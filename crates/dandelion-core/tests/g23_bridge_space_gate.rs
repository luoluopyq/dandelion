//! G23 (§七): the external-engine bridge launch gate.
//!
//! | rule | test |
//! |------|------|
//! | a workspace REGISTERED to a space rides a SPACE grant, cloud turn included while the space's policy allows `cloud_summary`; the registered binding no longer voids the call (query and submit) | `registered_workspace_cloud_turn_rides_the_space_grant` |
//! | a local-only space refuses a cloud-routed engine (honest degradation, no fake grant); a local engine passes; a policy flip takes effect on the next mint; an archived space falls back to the plain project grant | `local_only_space_gates_the_cloud_engine_only` |
//! | an unregistered workspace keeps the historic project grant, policy-gated by nothing | `unregistered_workspace_keeps_the_project_grant` |
//!
//! Identity is the registry hash, never the path: the relaxed call sites
//! re-resolve the session's binding through `knowledge_space_for_workspace`,
//! so g17's refusal (binding ≠ space-registered workspace) stays byte-identical.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::ScriptedProvider;
use dandelion_core::store::Store;
use dandelion_core::wiki;
use dandelion_core::wiki_build as wb;
use dandelion_core::wiki_index as wi;
use dandelion_core::{PermissionMode, Runtime, SessionPrefs, Settings};
use serde_json::json;
use std::fs;
use std::sync::Arc;

/// One process-global loopback bridge per process; bridge tests serialize
/// (same discipline as g3/g5/g17 — g23 is its own binary, its own listener).
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

/// Fresh connection to the same database (g17's discipline): registry rows
/// and the policy flip are written out-of-band, exactly like the migration
/// and the settings tab would, and every serve decision re-reads the row.
fn store_at(data: &std::path::Path) -> Store {
    Store::open(data, 5_000).unwrap()
}

fn mint_space(rt: &Runtime, name: &str) -> String {
    let tenant = rt.knowledge_create_tenant("外贸事业部").unwrap();
    let customer = rt.knowledge_create_customer(&tenant.id, "外贸客户甲").unwrap();
    rt.knowledge_create_space(&tenant.id, &customer.id, name, "").unwrap().id
}

fn register_workspace(data: &std::path::Path, space_id: &str, dir: &std::path::Path) {
    let store = store_at(data);
    store
        .knowledge_register_workspace(space_id, &wiki::workspace_hash(dir), &dir.to_string_lossy())
        .unwrap();
}

fn bind_session(data: &std::path::Path, sid: &dandelion_core::SessionId, dir: &std::path::Path) {
    let store = store_at(data);
    let prefs = SessionPrefs { workspace: Some(dir.to_path_buf()), ..Default::default() };
    assert!(store.set_session_prefs(sid, &prefs).unwrap(), "session bound");
}

/// Publish one space article WITHOUT any project, so the query has a space
/// page to serve through the relaxed call site.
fn publish_space_article(data: &std::path::Path, space_id: &str, docs: &std::path::Path) {
    fs::create_dir_all(docs.join("docs")).unwrap();
    fs::write(docs.join("docs/notes.md"), "# 工艺笔记\n\n保压时间不足会出现缩痕。\n").unwrap();
    let space_dir = wiki::space_data_dir(data, space_id);
    let idx = wi::build_index(&space_dir, docs, 100, 65_536).unwrap();
    wb::publish_build(
        &space_dir,
        &idx,
        &[wb::StagedArticle {
            file: "TOPIC-注塑.md".into(),
            title: "注塑工艺".into(),
            focus: String::new(),
            content: "缩痕的成因见 `docs/notes.md:L1-L3`。G23SPACEART\n".into(),
            inputs: vec!["docs/notes.md".into()],
            ranges: Vec::new(),
        }],
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_workspace_cloud_turn_rides_the_space_grant() {
    let _guard = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data, registered, docs) = (
        tmp.path().join("fallback"),
        tmp.path().join("data"),
        tmp.path().join("space-workspace"),
        tmp.path().join("docs"),
    );
    fs::create_dir_all(&registered).unwrap();
    let rt = runtime_at(&fallback, &data);
    let space_id = mint_space(&rt, "工艺知识库");
    register_workspace(&data, &space_id, &registered);
    store_at(&data).knowledge_set_space_external_policy(&space_id, "allow_summary").unwrap();
    publish_space_article(&data, &space_id, &docs);

    // The conversation IS bound to the registered workspace — the exact
    // session shape g17 refused. §七: this binding IS the space's identity.
    let sid = rt.create_session(Some("空间工作区对话")).unwrap();
    bind_session(&data, &sid, &registered);

    // Cloud-routed engine, policy allows cloud_summary → space grant.
    let token = rt
        .bridge_grant_for_engine(sid.as_str(), &registered, PermissionMode::Auto, true)
        .await
        .expect("registered + allow_summary rides the space grant even on a cloud turn");
    let url = bridge_url_for(&token);

    // Propose over the bridge lands PENDING in the SPACE ledger (relaxed
    // submit site) — not in the bound project's own ledger.
    let call = rpc(
        &url,
        "tools/call",
        json!({"name": "dandelion_wiki_propose", "arguments": {"kind": "flow", "content": "订单变更必须先收回旧版图纸"}}),
    )
    .await;
    assert_eq!(call["result"]["isError"], false, "{call}");
    assert!(call_text(&call).contains("待审核"), "{}", call_text(&call));
    let entries = wiki::load_distilled(&wiki::space_data_dir(&data, &space_id));
    assert_eq!(entries.len(), 1, "the space ledger takes the draft");
    assert_eq!(entries[0].status, wiki::ENTRY_STATUS_PENDING);
    // The registered directory resolves TO the space ledger (one store, no
    // fork), so the honest negative is a directory that is no space's: it
    // must have caught nothing.
    assert_eq!(
        rt.wiki_entries_for(Some(&registered)).len(),
        1,
        "reading through the registered path IS the space ledger — one store"
    );
    assert!(rt.wiki_entries_for(Some(&fallback)).is_empty(), "no stray project ledger anywhere else");

    // Query over the same bound session serves the space's own article
    // (relaxed query site), framed as a space search.
    let hit = rpc(
        &url,
        "tools/call",
        json!({"name": "dandelion_wiki_query", "arguments": {"question": "G23SPACEART"}}),
    )
    .await;
    assert_eq!(hit["result"]["isError"], false, "{hit}");
    let htext = call_text(&hit);
    assert!(htext.contains("G23SPACEART"), "space article served: {htext}");
    assert!(!htext.contains("作废"), "the registry-verified binding is no stale identity: {htext}");

    rt.revoke_wiki_grants(sid.as_str());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_only_space_gates_the_cloud_engine_only() {
    let _guard = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data, registered) = (
        tmp.path().join("fallback"),
        tmp.path().join("data"),
        tmp.path().join("space-workspace"),
    );
    fs::create_dir_all(&registered).unwrap();
    let rt = runtime_at(&fallback, &data);
    // mint_space's policy is "" ⇒ local_only: local-limited content may
    // never reach a cloud through the plugin.
    let space_id = mint_space(&rt, "保密工艺");
    register_workspace(&data, &space_id, &registered);
    let sid = rt.create_session(Some("本地限定对话")).unwrap();

    // Cloud-routed engine: refused, honestly, and NOTHING is minted.
    let err = rt
        .bridge_grant_for_engine(sid.as_str(), &registered, PermissionMode::Auto, true)
        .await
        .expect_err("local_only must gate a cloud turn");
    assert!(format!("{err}").contains("外部策略"), "{err}");

    // Local engine: the same space, same policy, no gate.
    let local = rt
        .bridge_grant_for_engine(sid.as_str(), &registered, PermissionMode::Auto, false)
        .await
        .expect("a local model is not an egress");
    let ok = rpc(&bridge_url_for(&local), "tools/list", json!({})).await;
    assert!(ok["result"]["tools"].is_array(), "space grant is live: {ok}");

    // Owner flips the policy: the NEXT mint re-reads the row (no restart,
    // no stale verdict) and the cloud turn passes.
    store_at(&data).knowledge_set_space_external_policy(&space_id, "allow_summary").unwrap();
    let cloud = rt
        .bridge_grant_for_engine(sid.as_str(), &registered, PermissionMode::Auto, true)
        .await
        .expect("allow_summary lifts the gate at once");
    let ok = rpc(&bridge_url_for(&cloud), "tools/list", json!({})).await;
    assert!(ok["result"]["tools"].is_array(), "{ok}");

    // Archived: the registry no longer vouches for anything here — the
    // workspace degrades to the plain project grant, even on a cloud turn.
    rt.knowledge_archive_space(&space_id, true).unwrap();
    let project = rt
        .bridge_grant_for_engine(sid.as_str(), &registered, PermissionMode::Auto, true)
        .await
        .expect("archived space falls back to the project grant");
    let ok = rpc(&bridge_url_for(&project), "tools/list", json!({})).await;
    assert!(ok["result"]["tools"].is_array(), "{ok}");

    rt.revoke_wiki_grants(sid.as_str());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unregistered_workspace_keeps_the_project_grant() {
    let _guard = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data, project) = (
        tmp.path().join("fallback"),
        tmp.path().join("data"),
        tmp.path().join("real-project"),
    );
    fs::create_dir_all(&project).unwrap();
    let rt = runtime_at(&fallback, &data);
    // A space exists in this tenant — irrelevant until the registry says so.
    let space_id = mint_space(&rt, "无关空间");
    let sid = rt.create_session(Some("真实项目对话")).unwrap();
    // A project grant is only project attribution while the session IS
    // bound to that project — an unbound session drafts, it writes nothing.
    bind_session(&data, &sid, &project);

    // Cloud turn, local_only policy — the project was never policy-gated,
    // so the historic bridge behaves exactly as before §七.
    let token = rt
        .bridge_grant_for_engine(sid.as_str(), &project, PermissionMode::Auto, true)
        .await
        .expect("unregistered workspaces are never gated");
    let url = bridge_url_for(&token);

    // Project attribution: the draft lands in the PROJECT ledger, the
    // untouched space stays clean.
    let call = rpc(
        &url,
        "tools/call",
        json!({"name": "dandelion_wiki_propose", "arguments": {"kind": "term", "content": "口头改价不算数"}}),
    )
    .await;
    assert_eq!(call["result"]["isError"], false, "{call}");
    let entries = rt.wiki_entries_for(Some(&project));
    assert_eq!(entries.len(), 1, "the project ledger takes the draft");
    assert_eq!(entries[0].status, wiki::ENTRY_STATUS_PENDING);
    assert!(wiki::load_distilled(&wiki::space_data_dir(&data, &space_id)).is_empty(), "the space keeps nothing");

    rt.revoke_wiki_grants(sid.as_str());
}
