//! Agent-side Wiki write path (task "在对话中维护项目 Wiki").
//!
//! | gate | test |
//! |------|------|
//! | propose → pending, tool never claims "written" | `agent_propose_lands_pending_and_stays_off_pages` |
//! | explicit write (Auto) → accepted + page + provenance | `agent_write_lands_accepted_and_compiles_onto_page` |
//! | retried tool call dedups, ledger does not grow | `same_call_twice_is_duplicate_not_growth` |
//! | approve / undo from the panel move the pages | `review_approve_and_undo_move_the_pages` |
//! | project A's write never reaches project B | `wiki_write_is_bound_to_the_session_project` |

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::{ChatMessage, ToolCall};
use dandelion_core::store::Store;
use dandelion_core::wiki::{self, PAGE_DECISIONS};
use dandelion_core::{PermissionMode, PromptRequest, Runtime, SessionId, Settings};
use std::fs;
use std::sync::Arc;

fn propose_call(id: &str, kind: &str, content: &str, mode: Option<&str>) -> ToolCall {
    let mut args = serde_json::json!({ "kind": kind, "content": content });
    if let Some(m) = mode {
        args["mode"] = serde_json::json!(m);
    }
    ToolCall {
        id: id.into(),
        name: "wiki_propose".into(),
        arguments: args.to_string(),
    }
}

fn settings_auto() -> Settings {
    let mut s = Settings::defaults_configured();
    s.permission_mode = PermissionMode::Auto;
    s
}

/// Runtime whose workspace is `ws` but whose data dir is shared — lets one
/// test host two projects over the same `{data}/wiki/projects/` tree.
fn runtime_at(
    ws: &std::path::Path,
    data: &std::path::Path,
    provider: Arc<ScriptedProvider>,
) -> Runtime {
    let mut settings = settings_auto();
    settings.workspace = Some(ws.to_path_buf());
    fs::create_dir_all(ws).unwrap();
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider)
        .expect("runtime")
}

fn tool_contents(store: &Store, sid: &SessionId) -> Vec<String> {
    store
        .load_openai_messages(sid)
        .unwrap()
        .into_iter()
        .filter_map(|m| match m {
            ChatMessage::Tool { content, .. } => Some(content),
            _ => None,
        })
        .collect()
}

fn page_body(rt: &Runtime, ws: &std::path::Path, want: &str) -> Option<String> {
    let pages = rt.wiki_show_for(Some(ws), None).unwrap_or_default();
    pages
        .into_iter()
        .find(|p| p.name == want)
        .map(|p| p.body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_propose_lands_pending_and_stays_off_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![propose_call("p1", "decision", "报价统一走线上审批流", None)]),
        ScriptedTurn::text("已提交审核"),
    ]));
    let rt = runtime_at(&ws, &data, provider.clone());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "把这条经验记进 Wiki")).await.expect("prompt");

    let store = Store::open(&data, 5_000).unwrap();
    let tools = tool_contents(&store, &sid);
    let said = tools.iter().find(|c| c.contains("Wiki")).expect("tool reply {tools:?}");
    assert!(said.contains("待审核"), "tool must report pending: {said}");

    let entries = rt.wiki_entries_for(Some(&ws));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, wiki::ENTRY_STATUS_PENDING);
    assert_eq!(entries[0].session_id, sid.as_str(), "provenance binds the session");
    assert_eq!(entries[0].turn, Some(1), "turn locator recorded");

    rt.wiki_compile(Some(&ws)).unwrap();
    assert!(page_body(&rt, &ws, PAGE_DECISIONS).is_none(), "pending never reaches a page");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_write_lands_accepted_and_compiles_onto_page() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![propose_call("w1", "decision", "打样确认后再对外报价", Some("write"))],
        ),
        ScriptedTurn::text("已写入"),
    ]));
    let rt = runtime_at(&ws, &data, provider.clone());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "帮我把这条写进 Wiki")).await.expect("prompt");

    let entries = rt.wiki_entries_for(Some(&ws));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, wiki::ENTRY_STATUS_ACCEPTED);

    rt.wiki_compile(Some(&ws)).unwrap();
    let body = page_body(&rt, &ws, PAGE_DECISIONS).expect("accepted entry is on the page");
    assert!(body.contains("打样确认后再对外报价"));
    let sid8: String = sid.as_str().chars().take(8).collect();
    assert!(body.contains(&sid8), "page carries the source session: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_call_twice_is_duplicate_not_growth() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    // Same content again, then a re-punctuated near-duplicate: both must
    // collapse onto the first entry instead of growing the ledger.
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![
                propose_call("p1", "term", "SOP=标准作业程序", None),
                propose_call("p2", "term", "SOP=标准作业程序", None),
                propose_call("p3", "term", "SOP = 标准作业程序。", None),
            ],
        ),
        ScriptedTurn::text("done"),
    ]));
    let rt = runtime_at(&ws, &data, provider.clone());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "记一下术语")).await.expect("prompt");

    let entries = rt.wiki_entries_for(Some(&ws));
    assert_eq!(entries.len(), 1, "ledger must not grow: {entries:?}");

    let store = Store::open(&data, 5_000).unwrap();
    let tools = tool_contents(&store, &sid);
    assert_eq!(tools.iter().filter(|c| c.contains("重复")).count(), 2, "{tools:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_approve_and_undo_move_the_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![propose_call("p1", "decision", "客户投诉24h内必须响应", None)],
        ),
        ScriptedTurn::text("ok"),
    ]));
    let rt = runtime_at(&ws, &data, provider.clone());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "沉淀一下")).await.expect("prompt");
    let id = rt.wiki_entries_for(Some(&ws))[0].id.clone();

    rt.wiki_compile(Some(&ws)).unwrap();
    assert!(page_body(&rt, &ws, PAGE_DECISIONS).is_none());

    rt.wiki_review_entry(Some(&ws), &id, "approve").unwrap();
    rt.wiki_compile(Some(&ws)).unwrap();
    assert!(page_body(&rt, &ws, PAGE_DECISIONS)
        .expect("approved entry surfaces")
        .contains("客户投诉24h内必须响应"));

    rt.wiki_review_entry(Some(&ws), &id, "undo").unwrap();
    rt.wiki_compile(Some(&ws)).unwrap();
    assert!(rt.wiki_entries_for(Some(&ws)).is_empty(), "undo removes the ledger row");
    assert!(page_body(&rt, &ws, PAGE_DECISIONS).is_none(), "undo takes the page down");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wiki_write_is_bound_to_the_session_project() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws_a, ws_b) = (
        tmp.path().join("data"),
        tmp.path().join("projA"),
        tmp.path().join("projB"),
    );
    let provider_a = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![propose_call("w1", "decision", "A项目的返利走季度结算", Some("write"))],
        ),
        ScriptedTurn::text("ok"),
    ]));
    let rt_a = runtime_at(&ws_a, &data, provider_a);
    rt_a.prompt(PromptRequest::new(rt_a.new_session(None), "记入 Wiki")).await.expect("prompt");

    // Project B shares the data dir but is a different workspace: its wiki
    // tree must be untouched, and a compile there must not pick up A's fact.
    let provider_b = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("hi")]));
    let rt_b = runtime_at(&ws_b, &data, provider_b);
    assert!(rt_b.wiki_entries_for(Some(&ws_b)).is_empty());
    rt_b.wiki_compile(Some(&ws_b)).unwrap();
    assert!(page_body(&rt_b, &ws_b, PAGE_DECISIONS).is_none());

    rt_a.wiki_compile(Some(&ws_a)).unwrap();
    assert!(page_body(&rt_a, &ws_a, PAGE_DECISIONS)
        .expect("A's own page has the fact")
        .contains("A项目的返利走季度结算"));
    assert_ne!(
        wiki::project_data_dir(&data, &ws_a),
        wiki::project_data_dir(&data, &ws_b)
    );
}
