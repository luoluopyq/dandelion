//! G17 (phase 7): the external-engine bridge with SPACE attribution.
//!
//! | rule | test |
//! |------|------|
//! | a Space grant submits to the space ledger (pending) and queries space articles, no project anywhere | `space_grant_propose_and_query_over_http` |
//! | any attribution change (space→space, space→project, revoke) kills the previous token on the real route | `attribution_changes_kill_the_previous_token` |
//! | a session that has a project bound may never ride a space grant | `space_grant_refuses_a_project_bound_session` |
//! | grants for unknown / malformed space ids are refused at mint time | `space_grant_refuses_unknown_or_malformed_ids` |
//!
//! The mid-flight "space vanished between grant and call" branch stays
//! defensive on purpose: no public API deletes a space (archiving keeps the
//! row on purpose), so the only reachable refusal is the mint-time
//! `知识空间不存在` check below.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::ScriptedProvider;
use dandelion_core::wiki;
use dandelion_core::wiki_build as wb;
use dandelion_core::wiki_index as wi;
use dandelion_core::{PermissionMode, Runtime, SessionPrefs, Settings};
use serde_json::json;
use std::fs;
use std::sync::Arc;

/// One process-global loopback bridge on whichever runtime first started it;
/// bridge tests serialize and each re-ensures (same discipline as g3/g5).
static BRIDGE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn bridge_lock() -> std::sync::MutexGuard<'static, ()> {
    BRIDGE_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn rpc(url: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
    let body = json!({"jsonrpc": "2.0", "id": "1", "method": method, "params": params});
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

/// Full request URL for a minted token (the token map embeds the live host
/// and port, so no probe grant is needed here).
fn bridge_url_for(token: &str) -> String {
    dandelion_core::wiki_bridge::bridge_url(token).expect("bridge url")
}

fn runtime_at(ws: &std::path::Path, data: &std::path::Path) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(ws.to_path_buf());
    fs::create_dir_all(ws).unwrap();
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(data.to_path_buf())),
        settings,
        Arc::new(ScriptedProvider::new(Vec::new())),
    )
    .expect("runtime")
}

fn mint_space(rt: &Runtime, name: &str) -> String {
    let tenant = rt.knowledge_create_tenant("外贸事业部").unwrap();
    let customer = rt.knowledge_create_customer(&tenant.id, "外贸客户甲").unwrap();
    rt.knowledge_create_space(&tenant.id, &customer.id, name, "").unwrap().id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn space_grant_propose_and_query_over_http() {
    let _guard = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data, docs) = (
        tmp.path().join("fallback"),
        tmp.path().join("data"),
        tmp.path().join("docs-home"),
    );
    fs::create_dir_all(docs.join("docs")).unwrap();
    fs::write(docs.join("docs/notes.md"), "# 工艺笔记\n\n保压时间不足会出现缩痕。\n").unwrap();
    let rt = runtime_at(&fallback, &data);
    let space_id = mint_space(&rt, "工艺知识库");
    // The space gets published articles WITHOUT any project registration:
    // the query path must then serve the space's own pages (article side
    // only) — never a raw scan of some directory the grant never covered.
    let space_dir = wiki::space_data_dir(&data, &space_id);
    let idx = wi::build_index(&space_dir, &docs, 100, 65_536).unwrap();
    wb::publish_build(
        &space_dir,
        &idx,
        &[wb::StagedArticle {
            file: "TOPIC-注塑.md".into(),
            title: "注塑工艺".into(),
            focus: String::new(),
            content: "缩痕的成因见 `docs/notes.md:L1-L3`。SPACEART_HARVEST\n".into(),
            inputs: vec!["docs/notes.md".into()],
            ranges: Vec::new(),
        }],
    )
    .unwrap();

    let sid = rt.create_session(Some("外部引擎对话")).unwrap();
    let token = rt
        .wiki_bridge_grant_space(sid.as_str(), &space_id, PermissionMode::Auto)
        .await
        .expect("space grant mints");
    let url = bridge_url_for(&token);

    let list = rpc(&url, "tools/list", json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"dandelion_wiki_propose") && names.contains(&"dandelion_wiki_query"), "{names:?}");

    // Propose over the bridge lands PENDING in the space ledger.
    let call = rpc(
        &url,
        "tools/call",
        json!({"name": "dandelion_wiki_propose", "arguments": {"kind": "flow", "content": "订单变更必须先收回旧版图纸"}}),
    )
    .await;
    assert_eq!(call["result"]["isError"], false, "{call}");
    let text = call_text(&call);
    assert!(text.contains("待审核") && !text.contains("已写入："), "must not claim a write: {text}");
    let entries = wiki::load_distilled(&space_dir);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].content, "订单变更必须先收回旧版图纸");
    assert_eq!(entries[0].status, wiki::ENTRY_STATUS_PENDING);
    assert_eq!(entries[0].session_id, sid.as_str());
    assert!(rt.wiki_entries_for(Some(&fallback)).is_empty(), "the fallback project stays clean");

    // Query serves the space's own articles, framed as a space search.
    let hit = rpc(
        &url,
        "tools/call",
        json!({"name": "dandelion_wiki_query", "arguments": {"question": "SPACEART_HARVEST"}}),
    )
    .await;
    assert_eq!(hit["result"]["isError"], false, "{hit}");
    let htext = call_text(&hit);
    assert!(htext.contains("SPACEART_HARVEST"), "space article served: {htext}");
    assert!(htext.contains("知识空间"), "honest framing, no fake project: {htext}");
    let miss = rpc(
        &url,
        "tools/call",
        json!({"name": "dandelion_wiki_query", "arguments": {"question": "ZZXQ-绝无仅有的问题"}}),
    )
    .await;
    assert_eq!(miss["result"]["isError"], false, "{miss}");
    assert!(call_text(&miss).contains("未命中"), "misses stay honest: {}", call_text(&miss));

    rt.revoke_wiki_grants(sid.as_str());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attribution_changes_kill_the_previous_token() {
    let _guard = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let rt = runtime_at(&ws, &data);
    let (s1, s2) = (mint_space(&rt, "空间一"), mint_space(&rt, "空间二"));
    let sid = rt.create_session(Some("归属切换对话")).unwrap();

    let token_a = rt.wiki_bridge_grant_space(sid.as_str(), &s1, PermissionMode::Auto).await.unwrap();
    let ok = rpc(&bridge_url_for(&token_a), "tools/list", json!({})).await;
    assert!(ok["result"]["tools"].is_array(), "live token lists tools: {ok}");

    // Space → space: the OLD config line is dead the moment attribution moves.
    let token_b = rt.wiki_bridge_grant_space(sid.as_str(), &s2, PermissionMode::Auto).await.unwrap();
    let dead = rpc(&bridge_url_for(&token_a), "tools/list", json!({})).await;
    assert_eq!(dead["error"]["code"], -32001, "{dead}");
    assert!(dead["error"]["message"].as_str().unwrap().contains("unknown or expired wiki bridge token"), "{dead}");

    // Space → project: same kill switch across target kinds.
    let token_c = rt.wiki_bridge_grant(sid.as_str(), &ws, PermissionMode::Auto).await.unwrap();
    let dead_b = rpc(&bridge_url_for(&token_b), "tools/list", json!({})).await;
    assert_eq!(dead_b["error"]["code"], -32001, "{dead_b}");
    let alive_c = rpc(&bridge_url_for(&token_c), "tools/list", json!({})).await;
    assert!(alive_c["result"]["tools"].is_array(), "newest token works: {alive_c}");

    // Session close: everything goes.
    rt.revoke_wiki_grants(sid.as_str());
    let dead_c = rpc(&bridge_url_for(&token_c), "tools/list", json!({})).await;
    assert_eq!(dead_c["error"]["code"], -32001, "{dead_c}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn space_grant_refuses_a_project_bound_session() {
    let _guard = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data, bound) = (
        tmp.path().join("fallback"),
        tmp.path().join("data"),
        tmp.path().join("bound-project"),
    );
    fs::create_dir_all(&bound).unwrap();
    let rt = runtime_at(&fallback, &data);
    let space_id = mint_space(&rt, "工艺知识库");
    let sid = rt.create_session(Some("先空间后绑项目")).unwrap();
    let token = rt.wiki_bridge_grant_space(sid.as_str(), &space_id, PermissionMode::Auto).await.unwrap();
    let url = bridge_url_for(&token);

    // Mid-flight the user binds a project to the conversation.
    let store = dandelion_core::store::Store::open(&data, 5_000).unwrap();
    let prefs = SessionPrefs { workspace: Some(bound.clone()), ..Default::default() };
    assert!(store.set_session_prefs(&sid, &prefs).unwrap(), "session bound");

    let call = rpc(
        &url,
        "tools/call",
        json!({"name": "dandelion_wiki_propose", "arguments": {"kind": "term", "content": "口头改价不算数"}}),
    )
    .await;
    assert_eq!(call["result"]["isError"], true, "{call}");
    let text = call_text(&call);
    assert!(text.contains("对话已绑定项目") && text.contains("作废"), "{text}");
    assert!(wiki::load_distilled(&wiki::space_data_dir(&data, &space_id)).is_empty(), "refused call writes nothing");

    let query = rpc(
        &url,
        "tools/call",
        json!({"name": "dandelion_wiki_query", "arguments": {"question": "口头改价"}}),
    )
    .await;
    assert_eq!(query["result"]["isError"], true, "{query}");
    assert!(call_text(&query).contains("作废"), "{}", call_text(&query));

    rt.revoke_wiki_grants(sid.as_str());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn space_grant_refuses_unknown_or_malformed_ids() {
    let _guard = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let rt = runtime_at(&ws, &data);
    let sid = rt.create_session(Some("幽灵空间对话")).unwrap();
    for bad in ["space_ghost_does_not_exist", "", "bad/../etc", "空格 空间"] {
        let err = rt
            .wiki_bridge_grant_space(sid.as_str(), bad, PermissionMode::Auto)
            .await
            .expect_err(&format!("{bad:?} must be refused"));
        assert!(format!("{err}").contains("知识空间不存在"), "{bad:?} → {err}");
    }
    // No half-minted grant is left behind for the failed ids.
    let good = mint_space(&rt, "正常空间");
    let token = rt.wiki_bridge_grant_space(sid.as_str(), &good, PermissionMode::Auto).await.unwrap();
    let ok = rpc(&bridge_url_for(&token), "tools/list", json!({})).await;
    assert!(ok["result"]["tools"].is_array(), "{ok}");
    rt.revoke_wiki_grants(sid.as_str());
}
