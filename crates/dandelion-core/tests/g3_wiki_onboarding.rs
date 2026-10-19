//! Wiki onboarding for project-less conversations + the external-engine
//! bridge (task "无项目会话的 Wiki 引导" / "wiki_propose 多 Agent 支持").
//!
//! | gate | test |
//! |------|------|
//! | unbound session → draft, nothing claimed, onboarding lands the original draft | `unbound_propose_becomes_draft_then_onboarding_lands_it` |
//! | cancelled onboarding creates no directory / project / entry | `cancelled_draft_leaves_no_trace` |
//! | re-confirming a resolved draft is idempotent | same test, final block |
//! | external call: propose → pending; self-declared write without host approval → pending + honest note | `external_bridge_write_needs_verified_intent` |
//! | same idempotency key over the bridge never duplicates | same test, replay block |
//! | project switch mid-flight kills the old grant | `external_call_after_project_switch_is_refused` |
//! | manual distill cannot write a foreign/unbound session into a project | `distill_refuses_foreign_or_unbound_sessions` |

use dandelion_core::acp::RequestPermissionOutcome;
use dandelion_core::host::{CliHost, DesktopHost, RecordingHost};
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::ToolCall;
use dandelion_core::wiki::{self, WikiDraftTarget};
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings, SessionPrefs};
use std::fs;
use std::sync::Arc;

fn settings_auto() -> Settings {
    let mut s = Settings::defaults_configured();
    s.permission_mode = PermissionMode::Auto;
    s
}

fn runtime_at(ws: &std::path::Path, data: &std::path::Path, provider: Arc<ScriptedProvider>) -> Runtime {
    let mut settings = settings_auto();
    settings.workspace = Some(ws.to_path_buf());
    fs::create_dir_all(ws).unwrap();
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider)
        .expect("runtime")
}

fn propose_call(id: &str, content: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "wiki_propose".into(),
        arguments: serde_json::json!({ "kind": "decision", "content": content }).to_string(),
    }
}

/// The bridge is ONE process-global loopback server living on whichever
/// tokio runtime first started it. Tests run on private runtimes, so when a
/// test ends its server dies with it. Serialize the bridge tests (each then
/// re-ensures — `ensure_server` rebinds after a liveness check) and keep the
/// server's runtime = the running test's runtime. Production has one
/// long-lived runtime and does not hit this.
static BRIDGE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn bridge_lock() -> std::sync::MutexGuard<'static, ()> {
    BRIDGE_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn rpc(url: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
    let body = serde_json::json!({"jsonrpc": "2.0", "id": "1", "method": method, "params": params});
    reqwest::Client::new()
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .expect("bridge reachable")
        .json()
        .await
        .expect("json-rpc response")
}

fn call_text(response: &serde_json::Value) -> String {
    response["result"]["content"][0]["text"].as_str().unwrap_or("").to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unbound_propose_becomes_draft_then_onboarding_lands_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data) = (tmp.path().join("fallback"), tmp.path().join("data"));
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![propose_call("p1", "报价前必须先核对客户授信额度")]),
        ScriptedTurn::text("已保存草稿"),
    ]));
    // The runtime's fallback workspace exists, but the SESSION row must stay
    // unbound: a draft may never be written into the fallback project.
    let rt = runtime_at(&fallback, &data, provider);
    let sid = rt.create_session(Some("无项目对话")).unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "把这条经验记进 Wiki")).await.expect("prompt");

    // Draft landed on the session; no project wiki got an entry anywhere.
    let drafts = rt.wiki_drafts(&sid);
    assert_eq!(drafts.len(), 1, "the proposal must persist as a draft");
    assert_eq!(drafts[0].content, "报价前必须先核对客户授信额度");
    assert!(rt.wiki_entries_for(Some(&fallback)).is_empty(), "no fallback write");

    // User picks "new project directory" under an explicit parent.
    let parent = tmp.path().join("projects");
    fs::create_dir_all(&parent).unwrap();
    let outcome = rt
        .wiki_resolve_draft(
            &drafts[0].draft_id,
            WikiDraftTarget::NewProject { parent: parent.clone(), name: "外贸报价".into() },
        )
        .expect("onboarding resolves");
    assert_eq!(outcome.status, wiki::ENTRY_STATUS_PENDING, "original draft lands as a candidate");
    let project_dir = parent.join("外贸报价");
    assert!(project_dir.is_dir());
    assert_eq!(
        dunce::canonicalize(&outcome.workspace).unwrap(),
        dunce::canonicalize(&project_dir).unwrap()
    );

    // Project registered, session bound, draft consumed.
    let projects = rt.projects_list().unwrap();
    let bound = dunce::canonicalize(&outcome.workspace).unwrap();
    assert!(
        projects.iter().any(|p| dunce::canonicalize(&p.workspace).is_ok_and(|p| p == bound)),
        "project registered: {projects:?}"
    );
    let store = dandelion_core::store::Store::open(&data, 5_000).unwrap();
    let row = store.get_session(&sid).unwrap().unwrap();
    assert_eq!(
        dunce::canonicalize(&row.workspace).unwrap(),
        bound,
        "session bound to the chosen project"
    );
    assert!(rt.wiki_drafts(&sid).is_empty(), "draft is resolved, not left dangling");

    // The ORIGINAL draft content — not a re-entry — sits in the new project.
    let entries = rt.wiki_entries_for(Some(&project_dir));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].content, "报价前必须先核对客户授信额度");
    assert_eq!(entries[0].session_id, sid.as_str());

    // Double confirmation (retry after restart etc.) replays the same result.
    let again = rt
        .wiki_resolve_draft(
            &drafts[0].draft_id,
            WikiDraftTarget::NewProject { parent: parent.clone(), name: "外贸报价".into() },
        )
        .expect("idempotent replay");
    assert_eq!(again.entry_id, outcome.entry_id, "re-confirmation must not write twice");
    assert_eq!(rt.wiki_entries_for(Some(&project_dir)).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_draft_leaves_no_trace() {
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data) = (tmp.path().join("fallback"), tmp.path().join("data"));
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![propose_call("p1", "试产通过后才允许排大货")]),
        ScriptedTurn::text("好"),
    ]));
    let rt = runtime_at(&fallback, &data, provider);
    let sid = rt.create_session(Some("无项目对话")).unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "记录一下")).await.expect("prompt");
    let drafts = rt.wiki_drafts(&sid);
    let draft_id = drafts[0].draft_id.clone();
    drop(drafts);

    assert!(rt.wiki_cancel_draft(&draft_id).unwrap());
    assert!(rt.wiki_drafts(&sid).is_empty());
    assert!(wiki::load_resolved_draft(&data, &draft_id).is_none());
    assert!(rt.projects_list().unwrap().is_empty(), "cancelling creates no project");
    assert!(rt.wiki_entries_for(Some(&fallback)).is_empty());
    // Cancelling twice is a clean no-op, not an error.
    assert!(!rt.wiki_cancel_draft(&draft_id).unwrap());
}

#[test]
fn unbound_draft_retries_are_idempotent_and_storage_errors_are_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let submission = wiki::WikiSubmission {
        kind: "decision".into(),
        content: "核对数据后验收".into(),
        mode: "propose".into(),
        revises: None,
        idempotency_key: Some("same-tool-call".into()),
        session_id: "session-one".into(),
        turn: Some(1),
        workspace: None,
        space_id: None,
    };
    let first = wiki::submit_wiki(tmp.path(), &submission, 2_000).unwrap();
    let second = wiki::submit_wiki(tmp.path(), &submission, 2_000).unwrap();
    let (wiki::WikiSubmitResult::ProjectRequired { draft: a },
         wiki::WikiSubmitResult::ProjectRequired { draft: b }) = (first, second) else {
        panic!("both calls must have a durable draft");
    };
    assert_eq!(a.draft_id, b.draft_id);
    assert_eq!(wiki::list_drafts(tmp.path(), "session-one").len(), 1);

    let blocked = tempfile::tempdir().unwrap();
    fs::write(blocked.path().join("wiki"), b"not a directory").unwrap();
    assert!(wiki::submit_wiki(blocked.path(), &submission, 2_000).is_err(),
        "a failed disk write must never claim a saved draft");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn onboarding_does_not_publish_a_write_request() {
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data, project) =
        (tmp.path().join("fallback"), tmp.path().join("data"), tmp.path().join("project"));
    fs::create_dir_all(&project).unwrap();
    let tool = ToolCall {
        id: "write-draft".into(),
        name: "wiki_propose".into(),
        arguments: serde_json::json!({
            "kind": "decision", "content": "须先审核才能发布", "mode": "write"
        }).to_string(),
    };
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![tool]), ScriptedTurn::text("已保存草稿"),
    ]));
    let rt = runtime_at(&fallback, &data, provider);
    let sid = rt.create_session(None).unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "写入 Wiki")).await.unwrap();
    let draft = rt.wiki_drafts(&sid).remove(0);
    assert_eq!(draft.mode, "write");
    let outcome = rt.wiki_resolve_draft(
        &draft.draft_id, WikiDraftTarget::RegisterDirectory { path: project.clone() }
    ).unwrap();
    assert_eq!(outcome.status, wiki::ENTRY_STATUS_PENDING);
    assert_eq!(rt.wiki_entries_for(Some(&project))[0].status, wiki::ENTRY_STATUS_PENDING);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_onboarding_does_not_create_a_project_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data, other, parent) = (
        tmp.path().join("fallback"), tmp.path().join("data"),
        tmp.path().join("other"), tmp.path().join("projects"),
    );
    fs::create_dir_all(&other).unwrap();
    fs::create_dir_all(&parent).unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![propose_call("p1", "旧项目草稿")]),
        ScriptedTurn::text("草稿待绑定"),
    ]));
    let rt = runtime_at(&fallback, &data, provider);
    let sid = rt.create_session(None).unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "提议 Wiki 知识")).await.unwrap();
    let draft = rt.wiki_drafts(&sid).remove(0);
    rt.session_set_prefs(&sid, SessionPrefs { workspace: Some(other), ..SessionPrefs::default() })
        .unwrap();
    let result = rt.wiki_resolve_draft(
        &draft.draft_id, WikiDraftTarget::NewProject { parent: parent.clone(), name: "must-not-exist".into() },
    );
    assert!(result.is_err());
    assert!(!parent.join("must-not-exist").exists());
    assert!(rt.projects_list().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_wiki_tool_refuses_external_bridge_grants() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    let mut settings = settings_auto();
    settings.workspace = Some(ws.clone());
    settings.wiki_proposal_enabled = false;
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(tmp.path().join("data"))), settings, provider,
    ).unwrap();
    assert!(rt.wiki_bridge_grant("any-session", &ws, PermissionMode::Auto).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_bridge_write_needs_verified_intent() {
    let _serial = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("hi")]));
    let rt = runtime_at(&ws, &data, provider);
    // Bind the session the ordinary way (prompt stamps row.workspace = ws).
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "你好")).await.expect("prompt");

    // A headless host cannot confirm a self-declared `mode=write`, even when
    // the session's general permission mode is Ask.
    let token = rt.wiki_bridge_grant(&sid.to_string(), &ws, PermissionMode::Ask).await.unwrap();
    let url = format!("http://{}/mcp/{token}", bridge_host(&rt).await);
    let list = rpc(&url, "tools/list", serde_json::json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .map(|t| t.iter().filter_map(|x| x["name"].as_str()).collect())
        .unwrap_or_default();
    assert!(names.contains(&"dandelion_wiki_propose"), "bridge advertises the wiki tool: {names:?}");

    let response = rpc(
        &url,
        "tools/call",
        serde_json::json!({ "name": "dandelion_wiki_propose", "arguments": {
            "kind": "decision", "content": "海外订单必须预付30%定金", "mode": "write",
            "idempotencyKey": "ext-1"
        } }),
    )
    .await;
    let text = call_text(&response);
    assert_eq!(response["result"]["isError"].as_bool(), Some(false), "{response}");
    assert!(text.contains("待审核"), "write claim degraded honestly: {text}");
    let entries = rt.wiki_entries_for(Some(&ws));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, wiki::ENTRY_STATUS_PENDING);

    // Same idempotency key, different transport call: no second entry.
    let replay = rpc(
        &url,
        "tools/call",
        serde_json::json!({ "name": "dandelion_wiki_propose", "arguments": {
            "kind": "decision", "content": "海外订单必须预付30%定金", "mode": "write",
            "idempotencyKey": "ext-1"
        } }),
    )
    .await;
    assert!(call_text(&replay).contains("重复"), "replay is a duplicate: {replay}");
    assert_eq!(rt.wiki_entries_for(Some(&ws)).len(), 1);

    // Auto mode alone does not prove the user asked to publish this item.
    // A headless host cannot confirm it, so this too remains pending.
    let token_auto = rt.wiki_bridge_grant(&sid.to_string(), &ws, PermissionMode::Auto).await.unwrap();
    let url_auto = format!("http://{}/mcp/{token_auto}", bridge_host(&rt).await);
    let write = rpc(
        &url_auto,
        "tools/call",
        serde_json::json!({ "name": "dandelion_wiki_propose", "arguments": {
            "kind": "term", "content": "PPAP=生产件批准程序", "mode": "write"
        } }),
    )
    .await;
    assert!(call_text(&write).contains("待审核"), "headless auto session must not publish: {write}");
    let entries = rt.wiki_entries_for(Some(&ws));
    let ppap = entries.iter().find(|e| e.content.contains("PPAP")).expect("ppap entry");
    assert_eq!(ppap.status, wiki::ENTRY_STATUS_PENDING);
    assert_eq!(ppap.session_id, sid.as_str(), "host injected the session, not the plugin");

    // Revocation: after close, the token is dead (spec: 取消/关闭会话后失效).
    rt.revoke_wiki_grants(&sid.to_string());
    let dead = rpc(
        &url_auto,
        "tools/call",
        serde_json::json!({ "name": "dandelion_wiki_propose", "arguments": {
            "kind": "term", "content": "已撤销的 token 不能再写"
        } }),
    )
    .await;
    assert_eq!(dead["error"]["code"].as_i64(), Some(-32001), "revoked token rejected: {dead}");
    assert_eq!(rt.wiki_entries_for(Some(&ws)).len(), 2, "revoked call wrote nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_write_requires_specific_host_approval() {
    let _serial = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    fs::create_dir_all(&ws).unwrap();
    let mut settings = settings_auto();
    settings.workspace = Some(ws.clone());
    let host = Arc::new(RecordingHost::new(data.clone()));
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("hi")]));
    let rt = Runtime::new_with_provider(host.clone(), settings, provider).unwrap();
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "请正式写入 Wiki")).await.unwrap();

    let token = rt.wiki_bridge_grant(&sid.to_string(), &ws, PermissionMode::Ask).await.unwrap();
    let url = format!("http://{}/mcp/{token}", bridge_host(&rt).await);
    let response = rpc(&url, "tools/call", serde_json::json!({ "name": "dandelion_wiki_propose", "arguments": {
        "kind": "decision", "content": "正式条目需逐条确认", "mode": "write"
    } })).await;
    assert!(call_text(&response).contains("已写入"), "{response}");
    assert_eq!(rt.wiki_entries_for(Some(&ws))[0].status, wiki::ENTRY_STATUS_ACCEPTED);
    assert!(host.call_titles().iter().any(|title| title.contains("正式写入项目 Wiki")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_switch_during_write_approval_refuses_old_project() {
    let _serial = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (ws_a, ws_b, data) = (
        tmp.path().join("a"), tmp.path().join("b"), tmp.path().join("data"),
    );
    fs::create_dir_all(&ws_a).unwrap();
    fs::create_dir_all(&ws_b).unwrap();
    let mut settings = settings_auto();
    settings.workspace = Some(ws_a.clone());
    let host = Arc::new(DesktopHost::new(data));
    host.set_permission_sink(Arc::new(|_, _| {}));
    let rt = Runtime::new_with_provider(
        host.clone(), settings,
        Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("hi")])),
    ).unwrap();
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "请正式写入 Wiki")).await.unwrap();
    let token = rt.wiki_bridge_grant(&sid.to_string(), &ws_a, PermissionMode::Ask).await.unwrap();
    let url = format!("http://{}/mcp/{token}", bridge_host(&rt).await);
    let call = tokio::spawn(async move {
        rpc(&url, "tools/call", serde_json::json!({ "name": "dandelion_wiki_propose", "arguments": {
            "kind": "decision", "content": "切换项目后不得写入旧项目", "mode": "write"
        } })).await
    });
    let request_id = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some((id, _)) = host.pending_requests().into_iter().next() {
                break id;
            }
            tokio::task::yield_now().await;
        }
    }).await.expect("host approval dialog");
    rt.session_set_prefs(&sid, SessionPrefs {
        workspace: Some(ws_b.clone()), ..SessionPrefs::default()
    }).unwrap();
    host.respond(&request_id, RequestPermissionOutcome::Selected { option_id: "allow".into() });
    let response = call.await.unwrap();
    assert!(call_text(&response).contains("项目已变更"), "{response}");
    assert!(rt.wiki_entries_for(Some(&ws_a)).is_empty());
    assert!(rt.wiki_entries_for(Some(&ws_b)).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_call_after_project_switch_is_refused() {
    let _serial = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (ws_a, ws_b, data) =
        (tmp.path().join("A"), tmp.path().join("B"), tmp.path().join("data"));
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("hi")]));
    let rt = runtime_at(&ws_a, &data, provider);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "你好")).await.expect("prompt");
    fs::create_dir_all(&ws_b).unwrap();

    let token = rt.wiki_bridge_grant(&sid.to_string(), &ws_a, PermissionMode::Auto).await.unwrap();
    let url = format!("http://{}/mcp/{token}", bridge_host(&rt).await);

    // The session moves to project B mid-flight (rebind from the UI).
    let mut prefs = SessionPrefs::default();
    prefs.workspace = Some(ws_b.clone());
    rt.session_set_prefs(&sid, prefs).unwrap();

    let response = rpc(
        &url,
        "tools/call",
        serde_json::json!({ "name": "dandelion_wiki_propose", "arguments": {
            "kind": "decision", "content": "切换项目期间的旧调用"
        } }),
    )
    .await;
    let text = call_text(&response);
    assert_eq!(response["result"]["isError"].as_bool(), Some(true), "stale grant must fail: {response}");
    assert!(text.contains("项目已变更"), "actionable reason: {text}");
    assert!(rt.wiki_entries_for(Some(&ws_a)).is_empty(), "old project untouched");
    assert!(rt.wiki_entries_for(Some(&ws_b)).is_empty(), "new project untouched");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distill_refuses_foreign_or_unbound_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws_a, ws_b, data) =
        (tmp.path().join("A"), tmp.path().join("B"), tmp.path().join("data"));
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt_a = runtime_at(&ws_a, &data, provider.clone());
    let sid_a = rt_a.new_session(None);
    rt_a.prompt(PromptRequest::new(sid_a.clone(), "在A项目干活")).await.expect("prompt");

    // Same data dir, different project: distilling A's session into B is a
    // provenance lie and an isolation breach — it must fail before any
    // model call.
    let rt_b = runtime_at(&ws_b, &data, provider);
    let err = rt_b.wiki_distill(Some(&ws_b), &sid_a).await.expect_err("foreign session refused");
    assert!(err.to_string().contains("未绑定") || err.to_string().contains("不属于") || err.to_string().contains("项目"), "{err}");
    assert!(rt_b.wiki_entries_for(Some(&ws_b)).is_empty());

    // Unbound session (created without a project) also cannot be distilled.
    let sid_none = rt_a.create_session(Some("无项目")).unwrap();
    let err = rt_a.wiki_distill(Some(&ws_a), &sid_none).await.expect_err("unbound refused");
    assert!(!err.to_string().is_empty());
}

/// The bridge listens on a fixed loopback port once started; ask the runtime
/// to (re)ensure it and read the port back through a minted token's URL.
async fn bridge_host(rt: &Runtime) -> String {
    let token = rt.wiki_bridge_grant("probe-session", std::path::Path::new("C:/"), PermissionMode::Ask).await.unwrap();
    // grant_for_session is stable; bridge_url embeds the host+port.
    let url = dandelion_core::wiki_bridge::bridge_url(&token).expect("bridge url");
    let rest = url.strip_prefix("http://").unwrap_or(&url);
    let host = rest.split('/').next().unwrap_or(rest).to_string();
    // Remove the probe grant so it cannot linger as a wildcard-looking token.
    rt.revoke_wiki_grants("probe-session");
    host
}
