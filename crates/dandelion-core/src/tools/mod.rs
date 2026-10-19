use crate::acp::SessionUpdate;
use crate::acp::{ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolKind};
use crate::config::Settings;
use crate::context::ContextSession;
use crate::hooks::{HookDecision, HookEvent, HookRuntime};
use crate::host::AgentHost;
use crate::ids::{SessionId, ToolCallId};
use crate::jobs::{truncate_bytes, ProcessTable};
use crate::permission::{
    ask_external_directory, check_gates, external_dir_denied_message, gate1_block_message,
    gate3_decision, hard_deny_message, permission_title, Gate3Decision, GateDecision,
    GATE3_BLOCKED,
};
use crate::provider::types::ToolSpec;
use crate::sandbox::{validate_sandbox_root, WorkspaceFs};
use crate::store::Store;
use crate::subagent::{SpawnRequest, SubagentRegistry};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

pub mod delegate;
pub mod file;
pub mod find_tools;
pub mod knowledge;
pub mod memory;
pub mod process;
pub mod terminal;
pub mod todo;
pub mod web;
pub mod wiki;

pub type SubagentSpawner =
    std::sync::Arc<dyn Fn(Vec<SpawnRequest>) -> Result<Vec<SessionId>, String> + Send + Sync>;

/// Whether an Auto prompt explicitly asks to use an MCP-backed capability.
/// Hosts use this before starting a selected MCP server, so merely opening a
/// conversation does not create external processes or connections.
pub fn auto_mcp_requested(prompt: &str, server_ids: &[String]) -> bool {
    let p = prompt.to_lowercase();
    p.contains("mcp")
        || server_ids
            .iter()
            .map(|id| id.to_lowercase())
            .any(|id| !id.is_empty() && p.contains(&id))
        || ["visio", "pixso", "jira", "word 文档", "word document"]
            .iter()
            .any(|term| p.contains(term))
}

pub struct ToolCtx {
    pub session_id: SessionId,
    pub tool_call_id: ToolCallId,
    pub workspace: WorkspaceFs,
    pub jobs: ProcessTable,
    pub cancel: CancellationToken,
    pub settings: Arc<Settings>,
    pub store: Arc<Store>,
    pub data_dir: PathBuf,
    pub subagents: Arc<SubagentRegistry>,
    pub depth: u32,
    pub kind: String,
    pub root_session: SessionId,
    pub spawner: Option<SubagentSpawner>,
    pub emit: Arc<dyn Fn(SessionUpdate) + Send + Sync>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn kind(&self) -> ToolKind;
    async fn call(&self, args: Value, ctx: ToolCtx) -> String;
    /// MCP `annotations.readOnlyHint`: safe to run alongside other tools in a
    /// batch (dispatch_plan `parallel_unscoped`). Activated MCP tools report
    /// this so lazily enabled read-only tools keep the G2 parallel behavior.
    fn read_only_annotation(&self) -> bool {
        false
    }
}

/// The host-side "stop asking in this session" allow table (keyed by root
/// session id). One table per app, shared by the external-CLI bridge and by
/// every prompt's [`ToolRegistry`]: the dialog's "本次会话不再询问" inserts,
/// each gate checks. It outlives a single run — a registry is rebuilt per
/// prompt, and the old per-run approval cache re-asked every turn, which is
/// exactly the "keeps popping" behavior users rejected. Gate-1 hard denials
/// and gate-3 deny rules are evaluated before the table and cannot be
/// bypassed by it.
pub type AllowAllTable = Arc<Mutex<HashSet<String>>>;

#[derive(Clone, Debug)]
pub struct SessionAllow {
    session: String,
    table: AllowAllTable,
}

impl SessionAllow {
    pub fn new(session: &SessionId, table: AllowAllTable) -> Self {
        Self { session: session.to_string(), table }
    }

    fn bypassed(&self) -> bool {
        self.table.lock().is_ok_and(|t| t.contains(&self.session))
    }

    fn bypass(&self) {
        if let Ok(mut t) = self.table.lock() {
            t.insert(self.session.clone());
        }
    }
}

pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
    turn_approved: Mutex<HashSet<String>>,
    turn_roots: Mutex<HashSet<PathBuf>>,
    /// Session-scoped bypass wired to the shared allow table. None for
    /// registries that are not a user session's prompt loop (spec pipeline,
    /// compact) — those keep the per-run cache only.
    session_allow: Option<SessionAllow>,
    /// H3: externally activated tools (MCP proxies promoted by `find_tools`).
    /// The registry is built per prompt, so activations expire with the run —
    /// external tools never stay resident across turns.
    activated: std::sync::RwLock<Vec<Arc<dyn Tool>>>,
}

impl ToolRegistry {
    fn from_tools(tools: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            tools,
            turn_approved: Mutex::new(HashSet::new()),
            turn_roots: Mutex::new(HashSet::new()),
            session_allow: None,
            activated: std::sync::RwLock::new(Vec::new()),
        }
    }

    pub fn empty() -> Self {
        Self::from_tools(Vec::new())
    }

    /// Wire the session-scoped "stop asking" bypass into this registry.
    pub fn with_session_allow(mut self, allow: Option<SessionAllow>) -> Self {
        self.session_allow = allow;
        self
    }

    /// Work order 4bce §七: attach the on-demand `knowledge_search` tool to
    /// this turn's registry. Only the Runtime calls this, and only for a
    /// native Auto turn of a session WITH bound knowledge spaces; the closure
    /// is that turn's `knowledge_chat_segment` itself — same fail-closed
    /// gate, per-space external-policy filter, byte budget and render, so
    /// the tool cannot become a second retrieval path that dodges them.
    pub fn with_knowledge(mut self, search: knowledge::KnowledgeSegmentFn) -> Self {
        self.tools.push(Arc::new(knowledge::KnowledgeSearchTool::new(search)));
        self
    }

    /// True once the user chose "本次会话不再询问" for this session — in this
    /// run or any earlier one (the table lives on the runtime, not the run).
    /// An explicit `deny` permission mode outranks it: that click meant
    /// "trust, don't ask", not "override deny mode if the user later picks it".
    fn session_bypassed(&self, mode: crate::config::PermissionMode) -> bool {
        mode != crate::config::PermissionMode::Deny
            && self.session_allow.as_ref().is_some_and(|a| a.bypassed())
    }

    /// Record the choice; subsequent gate checks in this session skip asking.
    fn set_session_bypassed(&self) {
        if let Some(a) = &self.session_allow {
            a.bypass();
        }
    }

    /// Read-only workspace context for Plan mode.  A general planning question
    /// remains tool-free; an explicitly workspace-scoped plan may inspect but
    /// can never write, execute, reach the web, or start MCP.
    pub fn plan_for_prompt(settings: &Settings, prompt: &str) -> Self {
        if PlanIntent::from_prompt(prompt).files {
            Self::from_tools(vec![
                Arc::new(file::ReadFileTool::new(settings)),
                Arc::new(file::SearchFilesTool::new(settings)),
            ])
        } else {
            Self::empty()
        }
    }

    pub fn g1(settings: &Settings) -> Self {
        Self::build(settings, true, false, Vec::new(), false)
    }

    pub fn spec_pipeline(settings: &Settings) -> Self {
        // Spec generation is deliberately not a normal coding turn.  It may
        // inspect and write its isolated spec directory, but it must not get
        // patch/search/shell simply because those happen to be the usual
        // coding tools.
        Self::from_tools(vec![
            Arc::new(file::ReadFileTool::new(settings)),
            Arc::new(file::WriteFileTool::new(settings)),
        ])
    }

    /// Build the stable tool surface for an ordinary Auto turn.
    ///
    /// Auto is the coding posture.  Do not infer capabilities from prompt
    /// keywords here: wording, follow-up turns, and non-English prompts are
    /// all too lossy for an authorization boundary.  The model decides which
    /// exposed tool fits the task; sandbox and permission gates decide whether
    /// that invocation may run.
    pub fn auto_for_prompt(
        settings: &Settings,
        _prompt: &str,
        delegate: bool,
        mcp_tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        // Keep the terminal schema visible even when shell execution is
        // disabled. Dispatch then returns the explicit gate error instead of
        // the misleading "unknown tool" and never starts a process.
        Self::session_tools(settings, true, delegate, mcp_tools)
    }

    pub fn session_tools(
        settings: &Settings,
        shell: bool,
        delegate: bool,
        mcp_tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        Self::build(settings, shell, delegate, mcp_tools, true)
    }

    fn build(
        settings: &Settings,
        shell: bool,
        delegate: bool,
        mcp_tools: Vec<Arc<dyn Tool>>,
        include_session_extras: bool,
    ) -> Self {
        let mut tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(file::ReadFileTool::new(settings)),
            Arc::new(file::WriteFileTool::new(settings)),
            Arc::new(file::PatchTool::new(settings)),
            Arc::new(file::SearchFilesTool::new(settings)),
        ];
        if shell {
            tools.push(Arc::new(terminal::TerminalTool::new(settings)));
            tools.push(Arc::new(process::ProcessTool::new(settings)));
        }
        if include_session_extras {
            tools.push(Arc::new(todo::TodoTool::new()));
            tools.push(Arc::new(web::WebSearchTool::new()));
            tools.push(Arc::new(web::WebExtractTool::new()));
            // Proposals are ModelCandidate events gated behind user
            // confirmation in the Project Memory page, so they respect the
            // memory kill-switch only, not the permission mode.
            if settings.memory_enabled {
                tools.push(Arc::new(memory::MemoryProposeTool::new()));
            }
            // Proposals land pending (no gate needed); only `mode:"write"`
            // prompts, via the shared gate-2 path (permission::needs_gate2).
            if settings.wiki_proposal_enabled {
                tools.push(Arc::new(wiki::WikiProposeTool::new()));
            }
        }
        if delegate {
            tools.push(Arc::new(delegate::DelegateTaskTool::new()));
        }
        let builtin_names: Vec<String> = tools.iter().map(|t| t.spec().name).collect();
        for tool in mcp_tools {
            let name = tool.spec().name;
            if builtin_names.iter().any(|b| b == &name) {
                tracing::warn!(%name, "mcp tool dropped: reserved builtin name");
                continue;
            }
            tools.push(tool);
        }
        Self::from_tools(tools)
    }

    pub fn for_session(
        settings: &Settings,
        _builtins: &[Arc<dyn Tool>],
        mcp_tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        Self::session_tools(settings, true, false, mcp_tools)
    }

    /// H3 lazy activation: admit an externally discovered tool (a `find_tools`
    /// promoted MCP proxy). Rejected when any name already exists — builtins
    /// and direct MCP tools win, and double activation is a no-op.
    pub fn activate(&self, tool: Arc<dyn Tool>) -> bool {
        let name = tool.spec().name;
        let mut activated = self.activated.write().expect("lock");
        if self.tools.iter().any(|t| t.spec().name == name)
            || activated.iter().any(|t| t.spec().name == name)
        {
            return false;
        }
        activated.push(tool);
        true
    }

    fn activated(&self) -> Vec<Arc<dyn Tool>> {
        self.activated.read().expect("lock").clone()
    }

    /// Activated MCP tools carrying `readOnlyHint`; merged into
    /// `PlanOptions::parallel_unscoped` per batch so lazy tools keep the same
    /// parallel behavior as directly injected ones.
    pub fn activated_parallel_unscoped(&self) -> Vec<String> {
        self.activated()
            .iter()
            .filter(|t| t.read_only_annotation() && t.spec().name.starts_with("mcp__"))
            .map(|t| t.spec().name)
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|t| t.spec().name)
            .chain(self.activated().iter().map(|t| t.spec().name))
            .collect()
    }

    pub fn mcp_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools
            .iter()
            .chain(self.activated().iter())
            .filter(|t| t.spec().name.starts_with("mcp__"))
            .cloned()
            .collect()
    }

    pub fn has_shell(&self) -> bool {
        self.names().iter().any(|n| n == "terminal")
    }

    pub fn openai_tools(&self) -> Vec<Value> {
        self.tools
            .iter()
            .chain(self.activated().iter())
            .map(|t| {
                let spec = t.spec();
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": spec.name,
                        "description": spec.description,
                        "parameters": spec.parameters,
                    }
                })
            })
            .collect()
    }

    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .iter()
            .chain(self.activated().iter())
            .find(|t| t.spec().name == name)
            .cloned()
    }

    pub async fn dispatch(&self, name: &str, arguments: &str, mut env: DispatchEnv) -> String {
        let args: Value = match serde_json::from_str(if arguments.trim().is_empty() {
            "{}"
        } else {
            arguments
        }) {
            Ok(v) => v,
            Err(err) => return format!("Error: invalid arguments: {err}"),
        };

        let Some(tool) = self.get(name) else {
            return format!("Error: unknown tool '{name}'");
        };

        if let Err(msg) = validate_tool_arguments(&tool.spec().parameters, &args) {
            return format!("Error: invalid arguments: {msg}");
        }

        let tool_call_id = env.tool_call_id.clone();
        let kind = tool.kind();
        env.emit(SessionUpdate::tool_call(ToolCallUpdate {
            tool_call_id: tool_call_id.clone(),
            title: Some(name.to_string()),
            kind: Some(kind),
            status: Some(ToolCallStatus::Pending),
            raw_input: Some(args.clone()),
            content: None,
        }));

        // Explicit denial takes precedence over any request to widen the
        // workspace. Do not ask the user to grant a directory for a tool that
        // is already blocked by policy.
        if gate3_decision(name, &args, &env.settings.permission_rules) == Gate3Decision::Deny {
            fail(&env, &tool_call_id, GATE3_BLOCKED);
            return GATE3_BLOCKED.to_string();
        }

        // Hard denials also take precedence over a directory permission
        // request. gate1 ignores workspace escapes here; those are handled by
        // the scoped directory prompt immediately below.
        if let Some(msg) =
            gate1_block_message(name, &args, &env.workspace, env.settings.allow_shell)
        {
            fail(&env, &tool_call_id, &msg);
            return msg;
        }

        if let Some(raw_path) = scoped_path(name, &args) {
            if let Err(err) = env.workspace.resolve(raw_path) {
                if !err.to_string().contains("path escapes workspace") {
                    let msg = crate::sandbox::sandbox_error_for_model(&err);
                    fail(&env, &tool_call_id, &msg);
                    return msg;
                }
                let candidate = match env.workspace.candidate_path(raw_path) {
                    Ok(path) => path,
                    Err(err) => {
                        let msg = crate::sandbox::sandbox_error_for_model(&err);
                        fail(&env, &tool_call_id, &msg);
                        return msg;
                    }
                };
                let mut requested_dir = if candidate.is_dir() {
                    candidate.clone()
                } else {
                    candidate.parent().unwrap_or(&candidate).to_path_buf()
                };
                // A new file may sit under directories that do not exist yet.
                // Ask for the nearest existing parent, showing that exact scope
                // in the permission prompt.
                while !requested_dir.exists() {
                    let Some(parent) = requested_dir.parent() else {
                        break;
                    };
                    requested_dir = parent.to_path_buf();
                }
                let root = match validate_sandbox_root(&requested_dir) {
                    Ok(root) => root,
                    Err(err) => {
                        let msg = crate::sandbox::sandbox_error_for_model(&err);
                        fail(&env, &tool_call_id, &msg);
                        return msg;
                    }
                };
                let already_approved = self.session_bypassed(env.permission_mode)
                    || self.turn_roots.lock().expect("lock").contains(&root);
                if !already_approved {
                    match ask_external_directory(
                        &root,
                        &env.session_id,
                        &tool_call_id,
                        env.host.as_ref(),
                        env.settings.as_ref(),
                        &env.cancel,
                    )
                    .await
                    {
                        GateDecision::AllowTurn => {
                            self.turn_roots.lock().expect("lock").insert(root.clone());
                            // "不再询问" on any dialog — including a directory
                            // one — is the session-wide choice, not this path only.
                            self.set_session_bypassed();
                        }
                        GateDecision::Allow => {}
                        GateDecision::Cancelled => {
                            env.emit(SessionUpdate::tool_call(ToolCallUpdate {
                                tool_call_id: tool_call_id.clone(),
                                title: None,
                                kind: None,
                                status: Some(ToolCallStatus::Cancelled),
                                raw_input: None,
                                content: Some(vec![ToolCallContent::text("Error: cancelled")]),
                            }));
                            return "Error: cancelled".into();
                        }
                        _ => {
                            let msg = external_dir_denied_message(
                                &root,
                                env.host.is_interactive(),
                            );
                            fail(&env, &tool_call_id, &msg);
                            return msg;
                        }
                    }
                }
                let mut roots = env.workspace.extra_roots().to_vec();
                roots.extend(self.turn_roots.lock().expect("lock").iter().cloned());
                roots.push(root);
                env.workspace =
                    match WorkspaceFs::with_extra_roots(env.workspace.root().to_path_buf(), &roots)
                    {
                        Ok(fs) => fs,
                        Err(err) => {
                            let msg = crate::sandbox::sandbox_error_for_model(&err);
                            fail(&env, &tool_call_id, &msg);
                            return msg;
                        }
                    };
            }
        }

        match env
            .hooks
            .fire(HookEvent::PreToolCall {
                tool_name: name.to_string(),
            })
            .await
        {
            HookDecision::Deny { reason } => {
                let msg = format!("Error: hook denied: {reason}");
                fail(&env, &tool_call_id, &msg);
                return msg;
            }
            HookDecision::Abort { reason } => {
                let msg = format!("Error: hook denied: {reason}");
                fail(&env, &tool_call_id, &msg);
                return msg;
            }
            HookDecision::Allow => {}
        }

        match gate3_decision(name, &args, &env.settings.permission_rules) {
            Gate3Decision::Deny => {
                fail(&env, &tool_call_id, GATE3_BLOCKED);
                return GATE3_BLOCKED.to_string();
            }
            Gate3Decision::SkipGate2 => {}
            rule_decision @ (Gate3Decision::Ask | Gate3Decision::NoMatch) => {
                let approval_key = approval_key(name, &args);
                if self.session_bypassed(env.permission_mode) {
                    // The user said "本次会话不再询问" for this session — in
                    // this run or a previous one. Gate 1 and gate-3 deny ran
                    // above; nothing left to ask. This is what stops the
                    // per-command/per-turn re-prompting users reported as
                    // "弹个不停".
                } else if self
                    .turn_approved
                    .lock()
                    .expect("lock")
                    .contains(&approval_key)
                {
                    // A prior explicit approval for this exact command or file
                    // applies only while this tool registry serves this turn.
                } else {
                    let title = permission_title(name, &args);
                    let decision = check_gates(
                        name,
                        &args,
                        &title,
                        &env.session_id,
                        &tool_call_id,
                        &env.workspace,
                        env.host.as_ref(),
                        env.permission_mode,
                        rule_decision == Gate3Decision::Ask,
                        env.settings.as_ref(),
                        &env.cancel,
                    )
                    .await;

                    match decision {
                        GateDecision::DenyHard => {
                            let msg = hard_deny_message(
                                name,
                                &args,
                                &env.workspace,
                                env.settings.allow_shell,
                            );
                            fail(&env, &tool_call_id, &msg);
                            return msg;
                        }
                        GateDecision::DenyPermission => {
                            let msg = "Error: permission denied.".to_string();
                            fail(&env, &tool_call_id, &msg);
                            return msg;
                        }
                        GateDecision::Cancelled => {
                            env.emit(SessionUpdate::tool_call(ToolCallUpdate {
                                tool_call_id: tool_call_id.clone(),
                                title: None,
                                kind: None,
                                status: Some(ToolCallStatus::Cancelled),
                                raw_input: None,
                                content: Some(vec![ToolCallContent::text("Error: cancelled")]),
                            }));
                            return "Error: cancelled".into();
                        }
                        GateDecision::AllowTurn => {
                            self.turn_approved
                                .lock()
                                .expect("lock")
                                .insert(approval_key);
                            // Same semantics as the external-CLI bridge: the
                            // dialog option means "stop asking in this
                            // session", so record it session-wide and every
                            // later tool — other commands, other files, the
                            // external-directory prompt, and the next turn —
                            // runs without a dialog. Registries without a
                            // wired table (compact/spec) keep only the
                            // per-run exact-key cache.
                            self.set_session_bypassed();
                        }
                        GateDecision::Allow => {}
                    }
                }
            }
        }

        env.emit(SessionUpdate::tool_call(ToolCallUpdate {
            tool_call_id: tool_call_id.clone(),
            title: None,
            kind: None,
            status: Some(ToolCallStatus::InProgress),
            raw_input: None,
            content: None,
        }));

        if let Some(msg) = crate::wiki::inference_guard(name, &args, &env.workspace, &env.data_dir)
        {
            fail(&env, &tool_call_id, &msg);
            return msg;
        }

        if let Some(root) = &env.spec_write_root {
            if let Some(msg) = crate::spec::write_guard(name, &args, &env.workspace, root) {
                fail(&env, &tool_call_id, &msg);
                return msg;
            }
        }

        env.subagents.set_in_tool(&env.session_id, true);
        let ctx = ToolCtx {
            session_id: env.session_id.clone(),
            tool_call_id: tool_call_id.clone(),
            workspace: env.workspace.clone(),
            jobs: env.jobs.clone(),
            cancel: env.cancel.clone(),
            settings: env.settings.clone(),
            store: env.store.clone(),
            data_dir: env.data_dir.clone(),
            subagents: env.subagents.clone(),
            depth: env.depth,
            kind: env.kind.clone(),
            root_session: env.root_session.clone(),
            spawner: env.spawner.clone(),
            emit: env.emit.clone(),
        };
        let result = tool.call(args.clone(), ctx).await;
        env.subagents.set_in_tool(&env.session_id, false);
        let mut result = truncate_bytes(&result, env.settings.terminal_output_max_bytes);
        if !result.starts_with("Error:") {
            if let Some(root) = &env.spec_write_root {
                if let Some(msg) = crate::spec::validate_written(
                    name,
                    &args,
                    &env.workspace,
                    root,
                    env.settings.spec_max_file_bytes,
                ) {
                    result = msg;
                }
            }
        }
        let failed = result.starts_with("Error:");

        let _ = env
            .hooks
            .fire(HookEvent::PostToolCall {
                tool_name: name.to_string(),
                failed,
            })
            .await;

        if let Some(dir) = progressive_dir(name, &args, &env.workspace) {
            if let Ok(mut session) = env.context.lock() {
                let extra = session.take_for_dir(&env.workspace, &dir, env.settings.as_ref());
                if !extra.is_empty() {
                    result = format!("{result}\n\n{extra}");
                }
            }
        }

        env.emit(SessionUpdate::tool_call(ToolCallUpdate {
            tool_call_id,
            title: None,
            kind: None,
            status: Some(if failed {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            }),
            raw_input: None,
            content: Some(vec![ToolCallContent::text(&result)]),
        }));
        result
    }
}

fn approval_key(name: &str, args: &Value) -> String {
    let scope = match name {
        "read_file" | "write_file" | "patch" | "search_files" => {
            args.get("path").and_then(Value::as_str).unwrap_or(".")
        }
        "terminal" => {
            return format!(
                "terminal\0{}\0{}",
                args.get("command").and_then(Value::as_str).unwrap_or(""),
                args.get("workdir").and_then(Value::as_str).unwrap_or("")
            )
        }
        _ => return format!("{name}\0{args}"),
    };
    format!("{name}\0{scope}")
}

fn scoped_path<'a>(name: &str, args: &'a Value) -> Option<&'a str> {
    match name {
        "read_file" | "write_file" | "patch" => args.get("path").and_then(Value::as_str),
        "search_files" => args.get("path").and_then(Value::as_str),
        "terminal" => args.get("workdir").and_then(Value::as_str),
        _ => None,
    }
}

/// Read-only Plan mode still needs an explicit workspace reference.  This is
/// deliberately not used to select Auto capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PlanIntent {
    files: bool,
}

/// Validate the portable JSON-Schema subset used by builtin tools before a
/// handler or permission hook runs. Unknown JSON-Schema keywords remain the
/// provider/MCP tool's concern; this protects the common object, required,
/// primitive, array, and numeric-bound cases without rejecting valid MCP
/// extensions that use richer schemas.
fn validate_tool_arguments(schema: &Value, value: &Value) -> std::result::Result<(), String> {
    let Some(kind) = schema.get("type").and_then(Value::as_str) else {
        return Ok(());
    };
    match kind {
        "object" => {
            let Some(object) = value.as_object() else {
                return Err("expected object".into());
            };
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for key in required.iter().filter_map(Value::as_str) {
                    if !object.contains_key(key) {
                        return Err(format!("missing required field '{key}'"));
                    }
                }
            }
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (key, child) in properties {
                    if let Some(child_value) = object.get(key) {
                        validate_tool_arguments(child, child_value)
                            .map_err(|err| format!("field '{key}': {err}"))?;
                    }
                }
            }
        }
        "array" => {
            let Some(items) = value.as_array() else {
                return Err("expected array".into());
            };
            if let Some(item_schema) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    validate_tool_arguments(item_schema, item)
                        .map_err(|err| format!("item {index}: {err}"))?;
                }
            }
        }
        "string" if !value.is_string() => return Err("expected string".into()),
        "boolean" if !value.is_boolean() => return Err("expected boolean".into()),
        "integer" if value.as_i64().is_none() && value.as_u64().is_none() => {
            return Err("expected integer".into())
        }
        "number" if !value.is_number() => return Err("expected number".into()),
        _ => {}
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
        if value.as_f64().is_some_and(|number| number < minimum) {
            return Err(format!("must be >= {minimum}"));
        }
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
        if value.as_f64().is_some_and(|number| number > maximum) {
            return Err(format!("must be <= {maximum}"));
        }
    }
    Ok(())
}

impl PlanIntent {
    fn from_prompt(prompt: &str) -> Self {
        let p = prompt.to_lowercase();
        let has = |terms: &[&str]| terms.iter().any(|term| p.contains(term));
        let write = has(&[
            "修改",
            "修复",
            "实现",
            "重构",
            "编辑",
            "更新",
            "创建文件",
            "删除文件",
            "改代码",
            "写入",
            "write ",
            "edit ",
            "fix ",
            "implement ",
            "refactor ",
            "create file",
            "delete file",
        ]);
        let shell = has(&[
            "运行", "执行", "测试", "构建", "编译", "启动", "调试", "安装", "run ", "test",
            "build", "compile", "start", "debug", "install ", "cargo ", "npm ", "pnpm ", "pytest",
            "git ",
        ]);
        // Do not mistake a general "分析这个概念" for a request to inspect
        // the workspace.  Analysis verbs need an explicit workspace referent.
        let workspace_ref = has(&[
            "代码",
            "项目",
            "仓库",
            "文件",
            "目录",
            "工作区",
            "code",
            "repository",
            "repo",
            "file",
            "workspace",
        ]);
        let explicit_file_read = has(&[
            "读取文件",
            "搜索文件",
            "查看文件",
            "read file",
            "search files",
            "grep ",
        ]);
        let files = write || shell || workspace_ref || explicit_file_read;
        Self { files }
    }
}

fn progressive_dir(name: &str, args: &Value, fs: &WorkspaceFs) -> Option<PathBuf> {
    let raw = match name {
        "read_file" | "write_file" | "patch" => args.get("path")?.as_str()?,
        "search_files" => args
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("."),
        "terminal" => args.get("workdir").and_then(|v| v.as_str())?,
        _ => return None,
    };
    let resolved = fs.resolve(raw).ok()?;
    if resolved.is_dir() {
        Some(resolved)
    } else {
        resolved.parent().map(|p| p.to_path_buf())
    }
}

fn fail(env: &DispatchEnv, id: &ToolCallId, msg: &str) {
    env.emit(SessionUpdate::tool_call(ToolCallUpdate {
        tool_call_id: id.clone(),
        title: None,
        kind: None,
        status: Some(ToolCallStatus::Failed),
        raw_input: None,
        content: Some(vec![ToolCallContent::text(msg)]),
    }));
}

pub struct DispatchEnv {
    pub session_id: SessionId,
    pub tool_call_id: ToolCallId,
    pub host: Arc<dyn AgentHost>,
    pub workspace: WorkspaceFs,
    pub jobs: ProcessTable,
    pub cancel: CancellationToken,
    pub permission_mode: crate::config::PermissionMode,
    pub settings: Arc<Settings>,
    pub store: Arc<Store>,
    pub emit: Arc<dyn Fn(SessionUpdate) + Send + Sync>,
    pub hooks: Arc<HookRuntime>,
    pub context: Arc<Mutex<ContextSession>>,
    pub data_dir: PathBuf,
    pub subagents: Arc<SubagentRegistry>,
    pub depth: u32,
    pub kind: String,
    pub root_session: SessionId,
    pub spawner: Option<SubagentSpawner>,
    pub spec_write_root: Option<PathBuf>,
}

impl DispatchEnv {
    fn emit(&self, update: SessionUpdate) {
        (self.emit)(update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{ToolSource, ToolSpec};

    struct FakeTool {
        name: String,
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.clone(),
                description: "fake".into(),
                parameters: serde_json::json!({"type": "object"}),
                source: ToolSource::Mcp {
                    server_id: "echo".into(),
                },
            }
        }
        fn kind(&self) -> ToolKind {
            ToolKind::Execute
        }
        async fn call(&self, _args: Value, _ctx: ToolCtx) -> String {
            "ok".into()
        }
    }

    #[test]
    fn session_allow_bypass_is_shared_and_per_session() {
        let table: AllowAllTable = Arc::new(Mutex::new(HashSet::new()));
        let a = SessionAllow::new(&SessionId::new(), table.clone());
        let b = SessionAllow::new(&SessionId::new(), table.clone());
        assert!(!a.bypassed());
        a.bypass();
        // A registry rebuilt for the next prompt shares the same table.
        let a_next_turn = a.clone();
        assert!(a_next_turn.bypassed(), "bypass must outlive one run");
        assert!(!b.bypassed(), "bypass is per session");
    }

    #[test]
    fn for_session_drops_reserved_name_keeps_prefixed() {
        let s = Settings::defaults();
        let steal = Arc::new(FakeTool {
            name: "write_file".into(),
        });
        let mcp = Arc::new(FakeTool {
            name: "mcp__echo__ping".into(),
        });
        let names = ToolRegistry::for_session(&s, &[], vec![steal, mcp]).names();
        assert_eq!(names.iter().filter(|n| *n == "write_file").count(), 1);
        assert!(names.contains(&"mcp__echo__ping".to_string()));
        let openai = ToolRegistry::for_session(
            &s,
            &[],
            vec![Arc::new(FakeTool {
                name: "mcp__echo__ping".into(),
            })],
        )
        .openai_tools();
        assert!(openai
            .iter()
            .any(|t| t["function"]["name"] == "mcp__echo__ping"));
    }

    #[test]
    fn progressive_dir_file_uses_parent_mcp_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir(&pkg).unwrap();
        std::fs::write(pkg.join("a.txt"), "x").unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let dir = progressive_dir("read_file", &serde_json::json!({"path": "pkg/a.txt"}), &fs)
            .expect("dir");
        assert_eq!(dir.file_name().unwrap(), "pkg");
        assert!(progressive_dir("mcp__echo__ping", &serde_json::json!({}), &fs).is_none());
        assert!(
            progressive_dir("search_files", &serde_json::json!({"pattern": "fn "}), &fs).is_some()
        );
    }

    #[test]
    fn session_tools_delegate_flag_and_g1_stays_six() {
        let s = Settings::defaults();
        let g1 = ToolRegistry::g1(&s).names();
        assert_eq!(g1.len(), 6);
        assert!(!g1.contains(&"delegate_task".to_string()));
        let parent = ToolRegistry::session_tools(&s, true, true, vec![]).names();
        assert!(parent.contains(&"delegate_task".to_string()));
        assert!(parent.contains(&"todo".to_string()));
        assert!(parent.contains(&"web_search".to_string()));
        assert!(parent.contains(&"web_extract".to_string()));
        assert!(!g1.contains(&"todo".to_string()));
        assert!(!g1.contains(&"web_search".to_string()));
        let child = ToolRegistry::session_tools(&s, false, false, vec![]).names();
        assert!(!child.contains(&"terminal".to_string()));
        assert!(!child.contains(&"delegate_task".to_string()));
        assert!(child.contains(&"read_file".to_string()));
        assert!(child.contains(&"todo".to_string()));
        assert!(child.contains(&"web_extract".to_string()));
    }

    #[test]
    fn auto_tools_are_stable_regardless_of_wording() {
        let s = Settings::defaults();
        let expected = ToolRegistry::session_tools(&s, true, false, vec![]).names();
        for prompt in ["write", "read it", "echo", "睡眠", "分析一下这个概念"] {
            assert_eq!(
                ToolRegistry::auto_for_prompt(&s, prompt, false, vec![]).names(),
                expected,
                "Auto tool schema changed for prompt: {prompt}"
            );
        }

        let delegate = ToolRegistry::auto_for_prompt(&s, "普通问题", true, vec![]).names();
        assert!(delegate.contains(&"delegate_task".to_string()));

        let plan = ToolRegistry::plan_for_prompt(&s, "为这个项目制定重构计划").names();
        assert_eq!(plan, vec!["read_file", "search_files"]);
        assert!(ToolRegistry::plan_for_prompt(&s, "帮我想一个旅行计划")
            .names()
            .is_empty());
    }

    #[test]
    fn builtin_schema_validation_rejects_bad_arguments_before_dispatch() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "timeout": { "type": "integer", "minimum": 1, "maximum": 10 },
                "items": { "type": "array", "items": { "type": "boolean" } }
            },
            "required": ["path"]
        });
        assert!(validate_tool_arguments(
            &schema,
            &serde_json::json!({"path":"a.txt", "timeout": 3, "items": [true, false]})
        )
        .is_ok());
        assert_eq!(
            validate_tool_arguments(&schema, &serde_json::json!({"timeout": 3})).unwrap_err(),
            "missing required field 'path'"
        );
        assert!(validate_tool_arguments(
            &schema,
            &serde_json::json!({"path":"a.txt", "timeout": 11})
        )
        .unwrap_err()
        .contains("must be <= 10"));
    }

    #[test]
    fn mcp_is_started_only_for_an_explicit_request() {
        let servers = vec!["visio-mcp".to_string(), "jira".to_string()];
        assert!(!auto_mcp_requested("解释一下 Rust 的所有权", &servers));
        assert!(auto_mcp_requested("用 visio-mcp 画一个流程图", &servers));
        assert!(auto_mcp_requested("请调用 MCP", &servers));
    }
}
