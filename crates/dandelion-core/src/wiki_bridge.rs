//! wiki_bridge — the Dandelion-hosted MCP server that lets EXTERNAL agent
//! engines (Claude Code, OpenCode, Pi) search local knowledge and submit Wiki knowledge through the same
//! `dandelion-core` service the native tool uses.
//!
//! Shape: one loopback HTTP listener (127.0.0.1, ephemeral port) speaking
//! JSON-RPC 2.0 / MCP (`initialize`, `tools/list`, `tools/call`). Each
//! external-engine turn mints a session-scoped bearer token; the token map is
//! the ONLY trust anchor — project or space identity, session id and turn are
//! resolved host-side from the grant, exactly like the native tool. Tokens
//! are revoked when the session closes or its attribution (project, space,
//! permission mode) changes, so a leaked config line cannot keep writing
//! after the conversation moved on.
//!
//! This module is a transport, not a policy: every write goes through
//! `crate::wiki::submit_wiki` via `Runtime::wiki_submit_for_grant`, and every
//! read answers from `crate::wiki::gather` (raw evidence first, Wiki pages
//! alongside) via `Runtime::wiki_query_for_grant` — the exact surface native
//! QA uses. Local-knowledge reads use the shared runtime retrieval and a separate
//! model-route-aware capability, so external engines get the same query capability, not a weaker
//! copy. There is no second ledger path here, and nothing in this file runs
//! an agent loop.

use axum::extract::Path as AxPath;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::config::PermissionMode;
use crate::runtime::Runtime;

/// What a grant points the conversation's knowledge at (phase 7). Projects
/// keep working exactly as before; a space target is the project-less
/// attribution — the space id is the identity, no directory is implied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WikiGrantTarget {
    /// A canonicalized project directory.
    Project(PathBuf),
    /// A knowledge space id (`space_…`); existence is re-checked against the
    /// Store at call time by the runtime handlers.
    Space(String),
}

/// What the host vouches for when a token was minted: which conversation is
/// calling, which project or space it is attributed to, and the session's
/// permission mode. A plugin claiming `mode=write` proves nothing by itself:
/// the host applies the mode and requires an explicit approval before formal
/// publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WikiGrant {
    pub session_id: String,
    pub target: WikiGrantTarget,
    pub permission_mode: PermissionMode,
    /// Legacy grants stay project/space scoped; only engine launch grants
    /// explicitly carry this independent local-search capability.
    pub knowledge: Option<KnowledgeGrant>,
    pub wiki_tools: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnowledgeGrant {
    pub workspace: PathBuf,
    pub uses_cloud_model: bool,
}

#[derive(Default)]
struct BridgeGlobal {
    runtime: Option<Runtime>,
    port: Option<u16>,
    grants: HashMap<String, WikiGrant>,
}

fn bridge() -> &'static Mutex<BridgeGlobal> {
    // Same static-coordination pattern as `wiki::compile_locks`: one bridge
    // per process, created lazily on the first external-engine turn.
    static BRIDGE: OnceLock<Mutex<BridgeGlobal>> = OnceLock::new();
    BRIDGE.get_or_init(Default::default)
}

/// Start the loopback MCP listener once; later calls return the same port.
/// The state lock is never held across an await (the prompt future must stay
/// `Send`), so concurrent first-callers may both bind and one listener loses.
pub async fn ensure_server(runtime: Runtime) -> crate::Result<u16> {
    {
        let mut guard = bridge()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Always (re)bind the live runtime — the process normally has exactly one.
        guard.runtime = Some(runtime);
        if let Some(port) = guard.port {
            // Liveness check: the listener task lives on whatever tokio
            // runtime first started it. If that runtime is gone (test
            // binaries, host restarts in-process) the port is a corpse —
            // clear it and fall through to a fresh bind instead of handing
            // engines a dead URL.
            let alive = std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                std::time::Duration::from_millis(150),
            )
            .is_ok();
            if alive {
                return Ok(port);
            }
            guard.port = None;
            guard.grants.clear();
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let mut guard = bridge()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = guard.port {
        // Lost the race against a concurrent first call; drop our listener.
        return Ok(existing);
    }
    let router = Router::new().route("/mcp/{token}", post(handle));
    tokio::spawn(async move {
        // Serving errors only mean the bridge is dead; engine tool calls then
        // surface as connection failures, which the adapters report honestly.
        let _ = axum::serve(listener, router).await;
    });
    guard.port = Some(port);
    Ok(port)
}

pub fn bridge_url(token: &str) -> Option<String> {
    let port = bridge()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .port?;
    Some(format!("http://127.0.0.1:{port}/mcp/{token}"))
}

/// Stable per-session token: re-minting for the same (session, target,
/// permission mode) triple returns the SAME token so reused CLI processes keep their
/// config; any change revokes the old tokens first — including a switch
/// between a project and a space (phase 7): one session has ONE grant, so
/// switching attribution (or re-pointing at another space) kills the previous
/// token outright and any call with it fails the lookup.
pub fn grant_for_session(session_id: &str, workspace: &PathBuf, permission_mode: PermissionMode) -> String {
    grant_for(session_id, WikiGrantTarget::Project(workspace.clone()), permission_mode)
}

/// Space-attribution twin of [`grant_for_session`], same token discipline.
pub fn grant_space_for_session(session_id: &str, space_id: &str, permission_mode: PermissionMode) -> String {
    grant_for(session_id, WikiGrantTarget::Space(space_id.to_string()), permission_mode)
}

fn grant_for(session_id: &str, target: WikiGrantTarget, permission_mode: PermissionMode) -> String {
    grant_with_capabilities(WikiGrant {
        session_id: session_id.to_string(), target, permission_mode,
        knowledge: None, wiki_tools: true,
    })
}

pub(crate) fn grant_for_engine(
    session_id: &str, target: WikiGrantTarget, permission_mode: PermissionMode,
    workspace: PathBuf, uses_cloud_model: bool, wiki_tools: bool,
) -> String {
    grant_with_capabilities(WikiGrant {
        session_id: session_id.to_string(), target, permission_mode, wiki_tools,
        knowledge: Some(KnowledgeGrant { workspace, uses_cloud_model }),
    })
}

fn grant_with_capabilities(wanted: WikiGrant) -> String {
    let session_id = &wanted.session_id;
    let mut guard = bridge().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((token, _)) = guard
        .grants
        .iter()
        .find(|(_, grant)| grant.session_id == *session_id && **grant == wanted)
    {
        return token.clone();
    }
    guard.grants.retain(|_, grant| grant.session_id != *session_id);
    let token = uuid::Uuid::new_v4().simple().to_string();
    guard.grants.insert(token.clone(), wanted);
    token
}

/// Cancel / session close / project switch: every token of that session dies.
pub fn revoke_session(session_id: &str) {
    if let Ok(mut guard) = bridge().lock() {
        guard.grants.retain(|_, grant| grant.session_id != session_id);
    }
}

fn rpc_error(id: Value, code: i32, message: &str) -> Response {
    (StatusCode::OK, Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))).into_response()
}

async fn handle(AxPath(token): AxPath<String>, Json(request): Json<Value>) -> Response {
    let id = request.get("id").cloned();
    // A notification (no id) gets an accepted-with-no-body response.
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };
    let (grant, runtime) = {
        let guard = match bridge().lock() {
            Ok(guard) => guard,
            Err(_) => return rpc_error(id, -32000, "wiki bridge state unavailable"),
        };
        (guard.grants.get(&token).cloned(), guard.runtime.clone())
    };
    let Some(grant) = grant else {
        return rpc_error(id, -32001, "unknown or expired wiki bridge token");
    };
    let Some(runtime) = runtime else {
        return rpc_error(id, -32000, "dandelion runtime unavailable");
    };
    let method = request["method"].as_str().unwrap_or("");
    let result: std::result::Result<Value, String> = match method {
        "initialize" => Ok(json!({
            "protocolVersion": request["params"]["protocolVersion"].as_str().unwrap_or("2025-03-26"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "dandelion-wiki", "version": env!("CARGO_PKG_VERSION") },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools_for_grant(&grant) })),
        "tools/call" => {
            let name = request["params"]["name"].as_str().unwrap_or("");
            let arguments = request["params"]["arguments"].clone();
            let arguments = if arguments.is_object() { arguments } else { json!({}) };
            let (text, is_error) = match name {
                WIKI_TOOL_NAME if grant.wiki_tools => runtime.wiki_submit_for_grant(&grant, &arguments).await,
                WIKI_QUERY_TOOL_NAME if grant.wiki_tools => runtime.wiki_query_for_grant(&grant, &arguments).await,
                KNOWLEDGE_SEARCH_TOOL_NAME if grant.knowledge.is_some() => runtime.knowledge_search_for_grant(&grant, &arguments),
                _ => return rpc_error(id, -32602, "unknown tool"),
            };
            Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
        }
        _ => return rpc_error(id, -32601, "method not found"),
    };
    match result {
        Ok(result) => (StatusCode::OK, Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))).into_response(),
        Err(message) => rpc_error(id, -32603, &message),
    }
}

pub const WIKI_TOOL_NAME: &str = "dandelion_wiki_propose";
/// Read-side twin: query the bound project's evidence index + Wiki pages.
pub const WIKI_QUERY_TOOL_NAME: &str = "dandelion_wiki_query";
pub const KNOWLEDGE_SEARCH_TOOL_NAME: &str = "dandelion_knowledge_search";

fn knowledge_tool_spec() -> Value {
    let spec = crate::tools::knowledge::search_spec();
    json!({ "name": KNOWLEDGE_SEARCH_TOOL_NAME, "description": spec.description,
        "inputSchema": spec.parameters, "annotations": { "readOnlyHint": true, "destructiveHint": false } })
}

fn tools_for_grant(grant: &WikiGrant) -> Vec<Value> {
    let mut tools = Vec::new();
    if grant.wiki_tools { tools.extend([tool_spec(), query_tool_spec()]); }
    if grant.knowledge.is_some() { tools.push(knowledge_tool_spec()); }
    tools
}

/// Thin Pi extension (plain JS, no imports → resolvable anywhere): registers
/// the same tool semantics and forwards every call to this session's bridge
/// URL. All policy lives host-side; the extension is transport only.
pub fn pi_extension_source(url: &str) -> Vec<u8> {
    pi_extension_with_tools(url, vec![tool_spec(), query_tool_spec()], false)
}

/// Engine extension advertises exactly the capabilities in its own grant.
/// Pi and MCP clients consume identical descriptions and input schemas.
pub(crate) fn pi_extension_source_for_grant(url: &str, token: &str) -> Vec<u8> {
    let (tools, read_only) = bridge().lock().ok()
        .and_then(|guard| guard.grants.get(token).map(|grant| (tools_for_grant(grant), grant.permission_mode == PermissionMode::Deny)))
        .unwrap_or_default();
    pi_extension_with_tools(url, tools, read_only)
}

fn pi_extension_with_tools(url: &str, tools: Vec<Value>, read_only: bool) -> Vec<u8> {
    format!(r#"// Dandelion local tools bridge (generated — do not edit)
const ENDPOINT = {endpoint};
const TOOLS = {tools};
const READ_ONLY = {read_only};
export default function (pi) {{
  for (const spec of TOOLS) {{
    pi.registerTool({{
      name: spec.name, label: spec.name, description: spec.description,
      parameters: spec.inputSchema, annotations: spec.annotations,
      async execute(_toolCallId, args, signal) {{
        try {{
          const res = await fetch(ENDPOINT, {{
            method: "POST", signal, headers: {{ "content-type": "application/json", "accept": "application/json" }},
            body: JSON.stringify({{ jsonrpc: "2.0", id: String(Date.now()), method: "tools/call", params: {{ name: spec.name, arguments: args }} }}),
          }});
          if (!res.ok) throw new Error("HTTP " + res.status);
          const data = await res.json();
          if (data.error) throw new Error(data.error.message || "Dandelion tool bridge error");
          const result = data.result;
          if (result.isError) throw new Error((result.content || []).map(item => item.text || "").join("\n"));
          return {{ content: result.content || [], details: {{}} }};
        }} catch (error) {{
          throw new Error("Dandelion tool bridge error: " + String(error?.message || error));
        }}
      }},
    }});
  }}
  // --no-tools also disables extension tools in newer Pi versions. Restore
  // exactly this host-gated read capability after session initialization.
  if (READ_ONLY && TOOLS.some(spec => spec.name === "{KNOWLEDGE_SEARCH_TOOL_NAME}")) {{
    pi.on("session_start", () => {{ pi.setActiveTools(["{KNOWLEDGE_SEARCH_TOOL_NAME}"]); }});
  }}
}}
"#, endpoint = serde_json::to_string(url).unwrap_or_else(|_| "null".into()),
        tools = serde_json::to_string(&tools).unwrap_or_else(|_| "[]".into())).into_bytes()
}

/// Merge the bridge MCP server into OpenCode's `OPENCODE_CONFIG_CONTENT`
/// JSON. Existing provider config survives untouched; only the `mcp` object
/// gains (or replaces) the `dandelion` entry. Garbage input degrades to a
/// fresh object rather than failing the turn.
pub fn merge_opencode_config(current: Option<&str>, url: &str) -> Option<String> {
    let mut config: Value = current
        .and_then(|raw| serde_json::from_str(raw).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    if !config["mcp"].is_object() {
        config["mcp"] = json!({});
    }
    config["mcp"]["dandelion"] = json!({ "type": "remote", "url": url, "enabled": true });
    serde_json::to_string(&config).ok()
}

fn tool_spec() -> Value {
    json!({
        "name": WIKI_TOOL_NAME,
        "description": "Record durable project knowledge into the Dandelion project Wiki bound to THIS \
conversation. Two modes: \"propose\" (default) files the entry as PENDING for the user to review in \
Dandelion's Wiki page — until approved it is NOT part of the wiki and you must not tell the user it was \
written; \"write\" requests host confirmation for this specific item and otherwise lands as a \
pending candidate. Pass revises=<entry id> to correct an existing entry. One self-contained \
item per call, ≤2000 bytes, grounded in this conversation. There is no project/path argument: the Wiki \
is chosen by Dandelion from the conversation's bound project.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["flow", "decision", "pitfall", "term"] },
                "content": { "type": "string", "description": "One self-contained statement, ≤2000 bytes." },
                "mode": { "type": "string", "enum": ["propose", "write"] },
                "revises": { "type": "string", "description": "Optional existing entry id this corrects." },
                "idempotencyKey": { "type": "string", "description": "Optional: a retried call with the same key never duplicates an entry." }
            },
            "required": ["kind", "content"]
        }
    })
}

fn query_tool_spec() -> Value {
    json!({
        "name": WIKI_QUERY_TOOL_NAME,
        "description": "Search the Dandelion project Wiki bound to THIS conversation together with its raw \
evidence index (original project files, prioritized; every hit carries 文件:行号 provenance you can open \
and verify yourself). Read-only; returns numbered located hits, or an explicit 'not enough evidence' note \
— if that note appears, say so instead of guessing. There is no project/path argument: the project is \
chosen by Dandelion from the conversation's bound project.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "What to look up: a symbol, a file, or how something works." },
                "maxHits": { "type": "number", "description": "Optional, 1..=20, default 8." }
            },
            "required": ["question"]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_is_stable_until_identity_changes() {
        let a = grant_for_session("sess-stable", &PathBuf::from("C:/proj-a"), PermissionMode::Ask);
        let again = grant_for_session("sess-stable", &PathBuf::from("C:/proj-a"), PermissionMode::Ask);
        assert_eq!(a, again, "re-mint must reuse the token so CLI processes keep their config");
        // Same session, other permission mode → new token, and the old one dies.
        let auto = grant_for_session("sess-stable", &PathBuf::from("C:/proj-a"), PermissionMode::Auto);
        assert_ne!(a, auto);
        let guard = bridge().lock().unwrap();
        assert!(!guard.grants.contains_key(&a));
        assert_eq!(guard.grants.get(&auto).map(|g| &g.session_id).unwrap(), "sess-stable");
        drop(guard);
        revoke_session("sess-stable");
        assert!(bridge().lock().unwrap().grants.values().all(|g| g.session_id != "sess-stable"));
    }

    #[test]
    fn switching_attribution_revokes_the_previous_token() {
        // One session, one grant: every attribution change (space→space,
        // space→project, project→space) must kill the previous token —
        // g17 proves the refused-call consequence over the real HTTP route.
        let space = grant_space_for_session("sess-switch", "space_alpha", PermissionMode::Ask);
        let again = grant_space_for_session("sess-switch", "space_alpha", PermissionMode::Ask);
        assert_eq!(space, again, "same (session, space, mode) reuses the token");
        let other = grant_space_for_session("sess-switch", "space_beta", PermissionMode::Ask);
        assert_ne!(space, other, "re-pointing at another space must mint a new token");
        let proj = grant_for_session("sess-switch", &PathBuf::from("C:/proj-x"), PermissionMode::Ask);
        assert_ne!(proj, other);
        let guard = bridge().lock().unwrap();
        assert!(!guard.grants.contains_key(&space), "old space token died on re-point");
        assert!(!guard.grants.contains_key(&other), "space token died on switch to project");
        assert_eq!(guard.grants.get(&proj).map(|g| &g.session_id).unwrap(), "sess-switch");
        drop(guard);
        revoke_session("sess-switch");
    }

    #[test]
    fn opencode_merge_keeps_provider_config_and_adds_bridge() {
        let current = serde_json::json!({
            "provider": { "anthropic": { "options": { "apiKey": "k" } } },
            "mcp": { "other": { "type": "local", "command": ["x"] } },
        })
        .to_string();
        let merged = merge_opencode_config(Some(&current), "http://127.0.0.1:9/mcp/tok").unwrap();
        let value: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["provider"]["anthropic"]["options"]["apiKey"], "k");
        assert_eq!(value["mcp"]["other"]["type"], "local");
        assert_eq!(value["mcp"]["dandelion"]["type"], "remote");
        assert_eq!(value["mcp"]["dandelion"]["url"], "http://127.0.0.1:9/mcp/tok");
        assert_eq!(value["mcp"]["dandelion"]["enabled"], true);
        // Garbage / absent config still yields a usable object.
        for bad in [None, Some("not json"), Some("[1,2]")] {
            let value: Value = serde_json::from_str(&merge_opencode_config(bad, "http://h/mcp").unwrap()).unwrap();
            assert_eq!(value["mcp"]["dandelion"]["url"], "http://h/mcp");
        }
    }

    #[test]
    fn pi_extension_matches_the_granted_tool_catalog_and_cancels_fetch() {
        let token = grant_for_engine("sess-knowledge-spec", WikiGrantTarget::Project(PathBuf::from("/workspace")),
            PermissionMode::Deny, PathBuf::from("/workspace"), true, false);
        let source = String::from_utf8(pi_extension_source_for_grant("http://127.0.0.1:1/mcp/example", &token)).unwrap();
        let serialized = source.lines().find_map(|line| line.strip_prefix("const TOOLS = ")).unwrap().trim_end_matches(';');
        let specs: Value = serde_json::from_str(serialized).unwrap();
        let specs = specs.as_array().unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0]["name"], KNOWLEDGE_SEARCH_TOOL_NAME);
        assert_eq!(specs[0]["annotations"]["readOnlyHint"], true);
        assert_eq!(specs[0]["inputSchema"], crate::tools::knowledge::search_spec().parameters);
        assert!(source.contains("async execute(_toolCallId, args, signal)"));
        assert!(source.contains("method: \"POST\", signal"));
        assert!(source.contains("if (result.isError) throw new Error"));
        assert!(source.contains("const READ_ONLY = true;"));
        assert!(source.contains("pi.setActiveTools([\"dandelion_knowledge_search\"])") );
        let combined = grant_for_engine("sess-knowledge-spec", WikiGrantTarget::Project(PathBuf::from("/workspace")),
            PermissionMode::Deny, PathBuf::from("/workspace"), true, true);
        assert_ne!(token, combined, "changing capabilities invalidates the old token");
        let source = String::from_utf8(pi_extension_source_for_grant("http://127.0.0.1:1/mcp/example", &combined)).unwrap();
        let serialized = source.lines().find_map(|line| line.strip_prefix("const TOOLS = ")).unwrap().trim_end_matches(';');
        let specs: Value = serde_json::from_str(serialized).unwrap();
        assert_eq!(specs.as_array().unwrap().len(), 3);
        revoke_session("sess-knowledge-spec");
    }

    #[test]
    fn pi_extension_embeds_endpoint_and_tool_name() {
        let source = String::from_utf8(pi_extension_source("http://127.0.0.1:1/mcp/abc")).unwrap();
        assert!(source.contains("http://127.0.0.1:1/mcp/abc"));
        assert!(source.contains(WIKI_TOOL_NAME));
        assert!(source.contains(WIKI_QUERY_TOOL_NAME), "external engines get query, not only propose");
        // Both tools forward through the same token-scoped endpoint.
        assert_eq!(source.matches("tools/call").count(), 1, "one shared forwarding handler for every declared tool");
        // Must stay import-free (plain JS the Pi host loads from any dir).
        assert!(!source.contains("import "));
    }
}
