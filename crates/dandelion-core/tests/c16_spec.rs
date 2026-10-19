//! C16 offline: write pin, SPEC/PLAN validate, ask/revise/plan/auto with Scripted-like Provider.

use async_trait::async_trait;
use dandelion_core::host::CliHost;
use dandelion_core::provider::types::{
    AssistantMessage, ChatMessage, CompletionRequest, StreamEvent, ToolCall,
};
use dandelion_core::provider::Provider;
use dandelion_core::spec::{
    self, SpecMode, SpecStatus, INVALID_PREFIX, PIN_MSG, PLAN_FILE, SPEC_FILE,
};
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings, SpecDiff};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn runtime(dir: &Path, provider: Arc<dyn Provider>) -> Runtime {
    let host = Arc::new(CliHost::with_data_dir(dir.join("data")));
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(dir.to_path_buf());
    settings.cron_tick_sec = 3_600;
    settings.spec_generate_max_iterations = 6;
    settings.spec_adapt_max_iterations = 6;
    Runtime::new_with_provider(host, settings, provider).expect("runtime")
}

fn extract_spec_id(system: &str) -> String {
    let marker = "id 用 ";
    let start = system
        .find(marker)
        .map(|i| i + marker.len())
        .expect("generator segment missing id");
    let rest = &system[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    let id = &rest[..end];
    assert!(id.starts_with("spc_"), "{id}");
    id.to_string()
}

fn extract_spec_id_from_adapter(system: &str) -> String {
    let marker = "spec_id（";
    if let Some(start) = system.find(marker) {
        let rest = &system[start + marker.len()..];
        let end = rest.find('）').unwrap_or(rest.len());
        return rest[..end].to_string();
    }
    extract_spec_id(system)
}

fn valid_spec(id: &str, goal: &str, version: u32, status: &str, extra_accept: &str) -> String {
    format!(
        "---\nid: {id}\nversion: {version}\nstatus: {status}\ngoal: {goal}\nnon_goals:\n  - 不做桌面\nconstraints:\n  - 只改 crates/dandelion-core\nacceptance:\n  - cargo test -p dandelion-core 过{extra_accept}\n---\n\n## 背景\n{goal}\n"
    )
}

fn valid_plan(id: &str, version: u32) -> String {
    format!(
        "---\nspec_id: {id}\nspec_version: {version}\nsteps:\n  - id: s1\n    title: 补齐校验\n    files: [\"crates/dandelion-core/src/spec/mod.rs\"]\n    done_when: \"单测绿\"\n---\n"
    )
}

fn write_call(call_id: &str, rel: &str, content: &str) -> ToolCall {
    ToolCall {
        id: call_id.into(),
        name: "write_file".into(),
        arguments: json!({ "path": rel, "content": content }).to_string(),
    }
}

fn assistant_tools(calls: Vec<ToolCall>) -> AssistantMessage {
    AssistantMessage {
        text: String::new(),
        tool_calls: calls,
        finish_reason: Some("tool_calls".into()),
    }
}

fn assistant_text(text: &str) -> AssistantMessage {
    AssistantMessage {
        text: text.into(),
        tool_calls: Vec::new(),
        finish_reason: Some("stop".into()),
    }
}

async fn send_ready(events: &mpsc::Sender<StreamEvent>, msg: &AssistantMessage) {
    if !msg.text.is_empty() {
        let _ = events.send(StreamEvent::TextDelta(msg.text.clone())).await;
    }
    for call in &msg.tool_calls {
        let _ = events.send(StreamEvent::ToolCallReady(call.clone())).await;
    }
}

fn tool_count(messages: &[ChatMessage]) -> usize {
    let last_user = messages
        .iter()
        .rposition(|m| matches!(m, ChatMessage::User { .. }))
        .unwrap_or(0);
    messages[last_user..]
        .iter()
        .filter(|m| {
            matches!(
                m,
                ChatMessage::Assistant {
                    tool_calls,
                    ..
                } if !tool_calls.is_empty()
            )
        })
        .count()
}

fn tool_has(messages: &[ChatMessage], needle: &str) -> bool {
    let last_user = messages
        .iter()
        .rposition(|m| matches!(m, ChatMessage::User { .. }))
        .unwrap_or(0);
    messages[last_user..]
        .iter()
        .any(|m| matches!(m, ChatMessage::Tool { content, .. } if content.contains(needle)))
}

/// 0 ask · 1 plan · 2 auto · 3 revise · 4 invalid-then-fix · 5 outside-pin-then-fix
struct SpecScriptProvider {
    completes: AtomicUsize,
    mode: u8,
}

#[async_trait]
impl Provider for SpecScriptProvider {
    async fn complete(
        &self,
        req: CompletionRequest,
        _cancel: CancellationToken,
        events: mpsc::Sender<StreamEvent>,
    ) -> dandelion_core::Result<AssistantMessage> {
        let _n = self.completes.fetch_add(1, Ordering::SeqCst);
        let is_adapter = req.system.contains("规格计划（Adapter）");
        let is_execute = req.system.contains("可执行计划（plan 段）");
        let is_generator = req.system.contains("规格计划（Generator）");

        let msg = match self.mode {
            0 => ask_turn(&req, is_generator),
            1 => plan_turn(&req, is_generator, is_adapter),
            2 => auto_turn(&req, is_generator, is_adapter, is_execute),
            3 => revise_turn(&req, is_generator),
            4 => invalid_turn(&req, is_generator),
            5 => pin_turn(&req, is_generator),
            _ => assistant_text("unexpected mode"),
        };
        send_ready(&events, &msg).await;
        Ok(msg)
    }
}

fn ask_turn(req: &CompletionRequest, is_generator: bool) -> AssistantMessage {
    if !is_generator {
        return assistant_text("ok");
    }
    let id = extract_spec_id(&req.system);
    let rel = format!(".dandelion/specs/{id}/{SPEC_FILE}");
    if tool_count(&req.messages) == 0 {
        assistant_tools(vec![write_call(
            "w1",
            &rel,
            &valid_spec(&id, "给 doctor 加一行", 1, "draft", ""),
        )])
    } else {
        assistant_text("SPEC ready")
    }
}

fn plan_turn(req: &CompletionRequest, is_generator: bool, is_adapter: bool) -> AssistantMessage {
    if is_adapter {
        let id = extract_spec_id_from_adapter(&req.system);
        let rel = format!(".dandelion/specs/{id}/{PLAN_FILE}");
        if tool_count(&req.messages) == 0 {
            assistant_tools(vec![write_call("p1", &rel, &valid_plan(&id, 1))])
        } else {
            assistant_text("PLAN ready")
        }
    } else if is_generator {
        let id = extract_spec_id(&req.system);
        let rel = format!(".dandelion/specs/{id}/{SPEC_FILE}");
        if tool_count(&req.messages) == 0 {
            assistant_tools(vec![write_call(
                "w1",
                &rel,
                &valid_spec(&id, "plan goal", 1, "draft", ""),
            )])
        } else {
            assistant_text("SPEC ready")
        }
    } else {
        assistant_text("ok")
    }
}

fn auto_turn(
    req: &CompletionRequest,
    is_generator: bool,
    is_adapter: bool,
    is_execute: bool,
) -> AssistantMessage {
    if is_execute {
        if tool_count(&req.messages) == 0 {
            assistant_tools(vec![write_call(
                "biz",
                "crates/note.txt",
                "executed from plan\n",
            )])
        } else {
            assistant_text("done")
        }
    } else if is_adapter {
        plan_turn(req, false, true)
    } else if is_generator {
        let id = extract_spec_id(&req.system);
        let rel = format!(".dandelion/specs/{id}/{SPEC_FILE}");
        if tool_count(&req.messages) == 0 {
            assistant_tools(vec![write_call(
                "w1",
                &rel,
                &valid_spec(&id, "auto goal", 1, "draft", ""),
            )])
        } else {
            assistant_text("SPEC ready")
        }
    } else {
        assistant_text("ok")
    }
}

fn revise_turn(req: &CompletionRequest, is_generator: bool) -> AssistantMessage {
    if !is_generator {
        return assistant_text("ok");
    }
    let id = extract_spec_id(&req.system);
    let rel = format!(".dandelion/specs/{id}/{SPEC_FILE}");
    let n = tool_count(&req.messages);
    // Across prompts: first prompt writes v1; revise prompt writes changed v2 content
    // Detect revise by user text mentioning Windows / clap, or existing compacted history
    let revise = req.messages.iter().any(|m| {
        matches!(m, ChatMessage::User { content, .. } if content.contains("Windows") || content.contains("clap"))
    });
    if n == 0 {
        if revise {
            let mut body = valid_spec(&id, "old goal", 2, "revising", "\n  - Windows CI");
            body = body.replace(
                "constraints:\n  - 只改 crates/dandelion-core",
                "constraints:\n  - 只改 crates/dandelion-core\n  - 不要改 CLI clap",
            );
            assistant_tools(vec![write_call("w2", &rel, &body)])
        } else {
            assistant_tools(vec![write_call(
                "w1",
                &rel,
                &valid_spec(&id, "old goal", 1, "draft", ""),
            )])
        }
    } else {
        assistant_text("ok")
    }
}

fn invalid_turn(req: &CompletionRequest, is_generator: bool) -> AssistantMessage {
    if !is_generator {
        return assistant_text("ok");
    }
    let id = extract_spec_id(&req.system);
    let rel = format!(".dandelion/specs/{id}/{SPEC_FILE}");
    let n = tool_count(&req.messages);
    if n == 0 {
        assistant_tools(vec![write_call(
            "w1",
            &rel,
            &valid_spec(&id, "   ", 1, "draft", ""),
        )])
    } else if tool_has(&req.messages, INVALID_PREFIX) && n == 1 {
        assistant_tools(vec![write_call(
            "w2",
            &rel,
            &valid_spec(&id, "fixed goal", 1, "draft", ""),
        )])
    } else {
        assistant_text("fixed")
    }
}

fn pin_turn(req: &CompletionRequest, is_generator: bool) -> AssistantMessage {
    if !is_generator {
        return assistant_text("ok");
    }
    let id = extract_spec_id(&req.system);
    let rel = format!(".dandelion/specs/{id}/{SPEC_FILE}");
    let n = tool_count(&req.messages);
    if n == 0 {
        assistant_tools(vec![write_call("bad", "crates/leak.rs", "should fail\n")])
    } else if tool_has(&req.messages, PIN_MSG) && n == 1 {
        assistant_tools(vec![write_call(
            "ok",
            &rel,
            &valid_spec(&id, "pinned ok", 1, "draft", ""),
        )])
    } else {
        assistant_text("pinned")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_writes_valid_spec_without_business_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("crates")).unwrap();
    let rt = runtime(
        dir.path(),
        Arc::new(SpecScriptProvider {
            completes: AtomicUsize::new(0),
            mode: 0,
        }),
    );
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid.clone(), "给 doctor 增加一行打印 skills 数");
    req.workspace = Some(dir.path().to_path_buf());
    let handle = rt
        .spec_start(SpecMode::Ask, req)
        .await
        .expect("spec_start ask");
    assert_eq!(handle.session_id, sid);
    assert!(handle.spec_id.starts_with("spc_"));
    let snap = rt.spec_show(&handle.spec_id).expect("show");
    assert_eq!(snap.status, SpecStatus::Draft);
    assert_eq!(snap.version, 1);
    assert!(snap.spec.is_some());
    assert!(snap.plan.is_none());
    assert!(PathBuf::from(&snap.path).join(SPEC_FILE).is_file());
    assert!(!dir.path().join("crates/note.txt").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_writes_plan_still_no_business_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("crates")).unwrap();
    let rt = runtime(
        dir.path(),
        Arc::new(SpecScriptProvider {
            completes: AtomicUsize::new(0),
            mode: 1,
        }),
    );
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "plan something");
    req.workspace = Some(dir.path().to_path_buf());
    let handle = rt
        .spec_start(SpecMode::Plan, req)
        .await
        .expect("spec_start plan");
    let snap = rt.spec_show(&handle.spec_id).expect("show");
    assert_eq!(snap.status, SpecStatus::Planned);
    assert!(snap
        .plan
        .as_ref()
        .map(|p| !p.steps.is_empty())
        .unwrap_or(false));
    assert!(!dir.path().join("crates/note.txt").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revise_produces_diff_and_bumps_version() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(
        dir.path(),
        Arc::new(SpecScriptProvider {
            completes: AtomicUsize::new(0),
            mode: 3,
        }),
    );
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid.clone(), "first");
    req.workspace = Some(dir.path().to_path_buf());
    let handle = rt.spec_start(SpecMode::Ask, req).await.expect("ask");
    let mut rev = PromptRequest::new(sid, "验收加上 Windows CI；不要改 CLI clap");
    rev.workspace = Some(dir.path().to_path_buf());
    let diff: SpecDiff = rt.spec_revise(&handle.spec_id, rev).await.expect("revise");
    assert_eq!(diff.version_from, 1);
    assert_eq!(diff.version_to, 2);
    assert!(!diff.is_empty());
    assert!(
        !diff.acceptance_added.is_empty()
            || !diff.constraints_added.is_empty()
            || diff.goal_changed,
        "{diff:?}"
    );
    let snap = rt.spec_show(&handle.spec_id).unwrap();
    assert_eq!(snap.version, 2);
    assert!(snap.diff_markdown.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_executes_and_writes_business_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("crates")).unwrap();
    let rt = runtime(
        dir.path(),
        Arc::new(SpecScriptProvider {
            completes: AtomicUsize::new(0),
            mode: 2,
        }),
    );
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "auto");
    req.workspace = Some(dir.path().to_path_buf());
    let handle = rt.spec_start(SpecMode::Auto, req).await.expect("auto");
    let snap = rt.spec_show(&handle.spec_id).unwrap();
    assert_eq!(snap.status, SpecStatus::Done);
    assert!(
        dir.path().join("crates/note.txt").is_file(),
        "execute should write business file"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approve_then_run_writes_business_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("crates")).unwrap();
    // Ask first with mode 0
    let rt = runtime(
        dir.path(),
        Arc::new(SpecScriptProvider {
            completes: AtomicUsize::new(0),
            mode: 0,
        }),
    );
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid.clone(), "ask then approve");
    req.workspace = Some(dir.path().to_path_buf());
    let handle = rt.spec_start(SpecMode::Ask, req).await.expect("ask");
    let snap = rt.spec_show(&handle.spec_id).unwrap();
    std::fs::write(
        PathBuf::from(&snap.path).join(PLAN_FILE),
        valid_plan(&handle.spec_id, 1),
    )
    .unwrap();
    let approved = rt.spec_approve(&handle.spec_id).await.expect("approve");
    assert_eq!(approved.status, SpecStatus::Planned);

    // New runtime sharing same data_dir + workspace files, execute-capable provider
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(dir.path().to_path_buf());
    settings.cron_tick_sec = 3_600;
    let rt2 = Runtime::new_with_provider(
        host,
        settings,
        Arc::new(SpecScriptProvider {
            completes: AtomicUsize::new(0),
            mode: 2,
        }),
    )
    .unwrap();
    let mut run = PromptRequest::new(sid, "Execute the approved PLAN.md");
    run.workspace = Some(dir.path().to_path_buf());
    rt2.spec_run(&handle.spec_id, run).await.expect("run");
    assert!(dir.path().join("crates/note.txt").is_file());
    let done = rt2.spec_show(&handle.spec_id).unwrap();
    assert_eq!(done.status, SpecStatus::Done);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_spec_tool_error_then_recover() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(
        dir.path(),
        Arc::new(SpecScriptProvider {
            completes: AtomicUsize::new(0),
            mode: 4,
        }),
    );
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "invalid then fix");
    req.workspace = Some(dir.path().to_path_buf());
    let handle = rt.spec_start(SpecMode::Ask, req).await.expect("recover");
    let snap = rt.spec_show(&handle.spec_id).unwrap();
    assert_eq!(snap.spec.as_ref().unwrap().goal, "fixed goal");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_outside_spec_dir_is_pinned() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("crates")).unwrap();
    let rt = runtime(
        dir.path(),
        Arc::new(SpecScriptProvider {
            completes: AtomicUsize::new(0),
            mode: 5,
        }),
    );
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "pin");
    req.workspace = Some(dir.path().to_path_buf());
    let handle = rt
        .spec_start(SpecMode::Ask, req)
        .await
        .expect("ask after pin");
    assert!(!dir.path().join("crates/leak.rs").exists());
    let snap = rt.spec_show(&handle.spec_id).unwrap();
    assert_eq!(snap.spec.as_ref().unwrap().goal, "pinned ok");
}

#[test]
fn validate_written_rejects_empty_goal() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let id = "spc_test";
    let dir = spec::spec_dir(ws, id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(SPEC_FILE), valid_spec(id, "   ", 1, "draft", "")).unwrap();
    let fs = dandelion_core::WorkspaceFs::new(ws.to_path_buf()).unwrap();
    let rel = format!(".dandelion/specs/{id}/{SPEC_FILE}");
    let err = spec::validate_written("write_file", &json!({ "path": rel }), &fs, &dir, 65536)
        .expect("must reject");
    assert!(err.contains(INVALID_PREFIX), "{err}");
    assert!(err.contains("goal"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_default_unchanged_no_spec_required() {
    use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(
        dir.path(),
        Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("hi")])),
    );
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "hello"))
        .await
        .expect("chat");
}
