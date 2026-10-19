//! External-agent local knowledge: real MCP transport, shared evidence and live gates.
use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::ScriptedProvider;
use dandelion_core::store::{KnowledgeRole, Store};
use dandelion_core::{PermissionMode, Runtime, SessionId, SessionPrefs, Settings};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
const SEARCH: &str = "dandelion_knowledge_search";
fn runtime(tmp: &tempfile::TempDir, native_local: bool, wiki: bool) -> Runtime {
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(&ws).unwrap();
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(ws);
    settings.wiki_proposal_enabled = wiki;
    if native_local { settings.default_backend = "sidecar".into(); }
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(tmp.path().join("data"))), settings,
        Arc::new(ScriptedProvider::new(vec![]))).unwrap()
}
fn publish(rt: &Runtime, owner: &str, name: &str, policy: &str, text: &str) -> (String, String, String) {
    let space = rt.knowledge_create_local_space(owner, name, policy).unwrap();
    let (doc, version) = rt.knowledge_import_document("local", "local", &space.id, owner, "file", &format!("{name}.md"), text, 16000).unwrap();
    rt.knowledge_publish_version("local", "local", &space.id, owner, &doc.id, &version.id).unwrap();
    (space.id, doc.id, version.id)
}
async fn rpc(url: &str, method: &str, params: Value) -> Value {
    reqwest::Client::new().post(url).json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send().await.unwrap().json().await.unwrap()
}
async fn search(url: &str, query: &str) -> Value {
    rpc(url, "tools/call", json!({"name":SEARCH,"arguments":{"query":query}})).await
}
fn text(value: &Value) -> &str { value["result"]["content"][0]["text"].as_str().unwrap_or("") }
async fn grant(rt: &Runtime, sid: &SessionId, ws: &Path, cloud: bool) -> (String, String) {
    let token = rt.local_tools_bridge_grant_for_engine(sid.as_str(), ws, PermissionMode::Deny, cloud).await.unwrap();
    let url = dandelion_core::wiki_bridge::bridge_url(&token).unwrap();
    (token, url)
}

#[tokio::test(flavor="multi_thread", worker_threads=2)]
async fn knowledge_search_works_without_wiki_or_binding_and_preserves_sources() {
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let rt = runtime(&tmp, false, false);
    // Scope discovery is not the former four-space picker limit.
    for i in 0..6 { publish(&rt, "local-user", &format!("其他{i}"), "allow_summary", "物流用纸箱包装。"); }
    publish(&rt, "local-user", "最后的工艺资料", "allow_summary", "保压时间不足会导致制品收缩和凹陷。");
    let sid = rt.create_session(None).unwrap();
    let (_, url) = grant(&rt, &sid, &tmp.path().join("workspace"), true).await;
    let tools = rpc(&url,"tools/list",json!({})).await;
    let tools = tools["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "Wiki disabled still permits read-only local search");
    assert_eq!(tools[0]["name"], SEARCH);
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
    assert_eq!(tools[0]["inputSchema"]["additionalProperties"], false);
    let result = search(&url,"保压时间不足").await;
    assert_eq!(result["result"]["isError"], false, "{result}");
    assert!(text(&result).contains("制品收缩"),"{result}");
    assert!(text(&result).contains("#knowledge/"),"{result}");
    assert!(text(&result).contains("最后的工艺资料.md"),"{result}");
    let write = rpc(&url,"tools/call",json!({"name":"dandelion_wiki_propose","arguments":{"kind":"fact","content":"bad"}})).await;
    assert!(write.get("error").is_some(),"knowledge-only grant must not write: {write}");
    for args in [json!({}), json!({"query":"  "}), json!({"query":7}), json!({"query":"保压","spaceId":"other"})] {
        let bad = rpc(&url,"tools/call",json!({"name":SEARCH,"arguments":args})).await;
        assert_eq!(bad["result"]["isError"],true,"{bad}");
    }
}

#[tokio::test(flavor="multi_thread", worker_threads=2)]
async fn external_route_not_native_settings_controls_privacy_and_policy_is_live() {
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    // Native is local but the external engine uses a cloud model.
    let rt = runtime(&tmp, true, true);
    let (space,_,_) = publish(&rt,"local-user","私有工艺","local_only","机密代号是青石计划。");
    let sid = rt.create_session(None).unwrap();
    let (cloud_token, url) = grant(&rt,&sid,&tmp.path().join("workspace"),true).await;
    let denied = search(&url,"机密代号").await;
    assert!(!text(&denied).contains("青石计划"),"{denied}");
    assert!(text(&denied).contains("外部策略限制"),"{denied}");
    let store=Store::open(&tmp.path().join("data"),5000).unwrap();
    store.knowledge_set_space_external_policy(&space,"allow_summary").unwrap();
    assert!(text(&search(&url,"机密代号").await).contains("青石计划"));
    store.knowledge_set_space_external_policy(&space,"local_only").unwrap();
    assert!(!text(&search(&url,"机密代号").await).contains("青石计划"));
    let (local_token, local_url) = grant(&rt,&sid,&tmp.path().join("workspace"),false).await;
    assert_ne!(cloud_token,local_token,"model route changes must rotate capability");
    assert!(rpc(&url,"tools/list",json!({})).await.get("error").is_some());
    assert!(text(&search(&local_url,"机密代号").await).contains("青石计划"));
}

#[tokio::test(flavor="multi_thread", worker_threads=2)]
async fn local_external_model_can_read_local_only_even_if_native_is_cloud() {
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp=tempfile::tempdir().unwrap();let rt=runtime(&tmp,false,false);
    publish(&rt,"local-user","本机资料","local_only","阀门校验需要三次测量。");
    let sid=rt.create_session(None).unwrap();let(_,url)=grant(&rt,&sid,&tmp.path().join("workspace"),false).await;
    assert!(text(&search(&url,"阀门校验").await).contains("三次测量"));
}

#[tokio::test(flavor="multi_thread", worker_threads=2)]
async fn same_token_observes_new_imports_revocation_archiving_and_reader_removal() {
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp=tempfile::tempdir().unwrap();let rt=runtime(&tmp,false,false);
    let sid=rt.create_session(None).unwrap();let(_,url)=grant(&rt,&sid,&tmp.path().join("workspace"),true).await;
    assert!(!text(&search(&url,"退货").await).contains("七天内"));
    let(space,doc,ver)=publish(&rt,"local-user","售后流程","allow_summary","签收后七天内凭订单号申请退货。");
    assert!(text(&search(&url,"退货").await).contains("七天内"));
    rt.knowledge_archive_space(&space,true).unwrap();
    assert!(!text(&search(&url,"退货").await).contains("七天内"));
    rt.knowledge_archive_space(&space,false).unwrap();
    rt.knowledge_set_member(&space,"admin",KnowledgeRole::Owner).unwrap();
    rt.knowledge_remove_member("local","local",&space,"admin","local-user").unwrap();
    assert!(!text(&search(&url,"退货").await).contains("七天内"));
    rt.knowledge_set_member(&space,"local-user",KnowledgeRole::Reader).unwrap();
    assert!(text(&search(&url,"退货").await).contains("七天内"));
    rt.knowledge_revoke_version("local","local",&space,"admin",&doc,&ver).unwrap();
    assert!(!text(&search(&url,"退货").await).contains("七天内"));
}

#[tokio::test(flavor="multi_thread", worker_threads=2)]
async fn legacy_tokens_never_gain_global_search_and_workspace_changes_invalidate_reads() {
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp=tempfile::tempdir().unwrap();let rt=runtime(&tmp,false,true);
    let sid=rt.create_session(None).unwrap();let ws=tmp.path().join("workspace");
    let legacy=rt.wiki_bridge_grant(sid.as_str(),&ws,PermissionMode::Deny).await.unwrap();
    let legacy_url=dandelion_core::wiki_bridge::bridge_url(&legacy).unwrap();
    assert!(search(&legacy_url,"工艺").await.get("error").is_some());
    let(token,url)=grant(&rt,&sid,&ws,true).await;
    let(again,_)=grant(&rt,&sid,&ws,true).await;assert_eq!(token,again);
    assert!(rpc(&legacy_url,"tools/list",json!({})).await.get("error").is_some());
    let other=tmp.path().join("other");std::fs::create_dir_all(&other).unwrap();
    rt.session_set_prefs(&sid,SessionPrefs{workspace:Some(other),..Default::default()}).unwrap();
    assert_eq!(search(&url,"工艺").await["result"]["isError"],true);
    rt.cancel(sid.clone()).await.unwrap();
    assert!(search(&url,"工艺").await.get("error").is_some(), "cancellation revokes the host credential");
}
