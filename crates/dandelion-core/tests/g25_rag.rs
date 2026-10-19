//! Local RAG: retrieval quality, immutable references, upgrades and revocation.
use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::store::Store;
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings, TaskMode};
use std::sync::Arc;

fn runtime(tmp: &tempfile::TempDir, provider: Arc<ScriptedProvider>) -> Runtime {
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(&ws).unwrap();
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(ws);
    settings.permission_mode = PermissionMode::Auto;
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(tmp.path().join("data"))),
        settings,
        provider,
    )
    .unwrap()
}
fn space(rt: &Runtime) -> (String, String, String) {
    let s = rt
        .knowledge_create_local_space("local-user", "RAG资料", "allow_summary")
        .unwrap();
    ("local".into(), "local".into(), s.id)
}
fn import(
    rt: &Runtime,
    scope: &(String, String, String),
    name: &str,
    text: &str,
) -> (String, String) {
    let (d, v) = rt
        .knowledge_import_document(
            &scope.0,
            &scope.1,
            &scope.2,
            "local-user",
            "file",
            name,
            text,
            16000,
        )
        .unwrap();
    (d.id, v.id)
}
fn publish(rt: &Runtime, scope: &(String, String, String), doc: &str, version: &str) {
    rt.knowledge_publish_version(&scope.0, &scope.1, &scope.2, "local-user", doc, version)
        .unwrap();
}

#[tokio::test]
async fn natural_chinese_question_retrieves_tail_passage_with_a_frozen_source() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = runtime(&tmp, Arc::new(ScriptedProvider::new(vec![])));
    let scope = space(&rt);
    let (d, v) = import(
        &rt,
        &scope,
        "注塑工艺.md",
        &format!(
            "{}\n保压时间不足会导致制品收缩，需延长保压。",
            "背景说明。".repeat(500)
        ),
    );
    assert!(rt
        .space_search(
            "local-user",
            std::slice::from_ref(&scope.2),
            "保压时间不足会怎样",
            5
        )
        .unwrap()
        .hits
        .is_empty());
    publish(&rt, &scope, &d, &v);
    let hits = rt
        .space_search(
            "local-user",
            std::slice::from_ref(&scope.2),
            "保压时间不足会怎样",
            5,
        )
        .unwrap()
        .hits;
    assert!(!hits.is_empty());
    assert!(
        hits[0].excerpt.contains("保压时间不足会导致制品收缩"),
        "{:?}",
        hits[0]
    );
    assert!(hits[0].excerpt.len() <= rt.settings().knowledge_excerpt_bytes);
    let url = hits[0].source_url.as_ref().unwrap();
    let chunk = url.rsplit('/').next().unwrap();
    let source = rt
        .space_chunk_source("local-user", &scope.2, chunk)
        .unwrap();
    assert_eq!(source.version_id, v);
    assert_eq!(source.source_id, "注塑工艺.md");
    assert!(source.content.contains("延长保压"));
    assert!(rt.space_chunk_source("outsider", &scope.2, chunk).is_err());
    let (d2, v2) = import(&rt, &scope, "注塑工艺.md", "保压时间的新规范规定另行试验。");
    publish(&rt, &scope, &d2, &v2);
    assert_eq!(
        rt.space_chunk_source("local-user", &scope.2, chunk)
            .unwrap()
            .version_id,
        v,
        "superseded citations remain inspectable"
    );
    assert!(rt
        .space_search(
            "local-user",
            std::slice::from_ref(&scope.2),
            "保压时间不足",
            5
        )
        .unwrap()
        .hits
        .iter()
        .all(|h| !h.excerpt.contains("制品收缩")));
    rt.knowledge_revoke_version(&scope.0, &scope.1, &scope.2, "local-user", &d, &v)
        .unwrap();
    assert!(rt
        .space_chunk_source("local-user", &scope.2, chunk)
        .is_err());
}

#[tokio::test]
async fn exact_relevant_passage_beats_a_generic_first_document_and_short_terms_work() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = runtime(&tmp, Arc::new(ScriptedProvider::new(vec![])));
    let scope = space(&rt);
    let (d, v) = import(
        &rt,
        &scope,
        "泛泛说明.md",
        "保压属于注塑环节。时间应当按现场情况调整。其他部分说明设备。 ",
    );
    publish(&rt, &scope, &d, &v);
    let (d, v) = import(
        &rt,
        &scope,
        "工艺参数.md",
        "保压时间不足会导致制品收缩和凹陷。",
    );
    publish(&rt, &scope, &d, &v);
    let hits = rt
        .space_search(
            "local-user",
            std::slice::from_ref(&scope.2),
            "保压时间不足",
            1,
        )
        .unwrap()
        .hits;
    assert_eq!(hits.len(), 1);
    assert!(hits[0].label.contains("工艺参数.md"));
    assert!(!rt
        .space_search("local-user", std::slice::from_ref(&scope.2), "保压", 3)
        .unwrap()
        .hits
        .is_empty());
    for query in ["\" OR NOT *", "()\"", "' OR 1=1 --"] {
        assert!(rt
            .space_search("local-user", std::slice::from_ref(&scope.2), query, 3)
            .is_ok());
    }
}

#[tokio::test]
async fn legacy_chunks_are_backfilled_and_sql_triggers_index_later_imports() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = runtime(&tmp, Arc::new(ScriptedProvider::new(vec![])));
    let scope = space(&rt);
    let (d, v) = import(&rt, &scope, "原有资料.md", "阀门调节压力应采用三级校验。");
    publish(&rt, &scope, &d, &v);
    drop(rt);
    let conn = rusqlite::Connection::open(tmp.path().join("data/sessions.sqlite")).unwrap();
    conn.execute_batch("DROP TRIGGER knowledge_chunks_fts_insert; DROP TRIGGER knowledge_chunks_fts_delete; DROP TRIGGER knowledge_chunks_fts_update; DROP TABLE knowledge_chunks_fts; DELETE FROM dandelion_meta WHERE key='knowledge_fts_v1';").unwrap();
    drop(conn);
    let store = Store::open(&tmp.path().join("data"), 5000).unwrap();
    assert!(!store
        .knowledge_retrieve(
            &scope.0,
            &scope.1,
            &scope.2,
            "local-user",
            "阀门压力怎样校验",
            5
        )
        .unwrap()
        .is_empty());
    let (d, v) = store
        .knowledge_import_document(
            &scope.0,
            &scope.1,
            &scope.2,
            "local-user",
            "file",
            "新资料.md",
            "三级校验完成后检查传感器。",
            500,
        )
        .unwrap();
    store
        .knowledge_publish_version(&scope.0, &scope.1, &scope.2, "local-user", &d.id, &v.id)
        .unwrap();
    assert!(!store
        .knowledge_retrieve(&scope.0, &scope.1, &scope.2, "local-user", "传感器检查", 5)
        .unwrap()
        .is_empty());
    store
        .knowledge_delete_document(&scope.0, &scope.1, &scope.2, "local-user", &d.id)
        .unwrap();
    assert!(store
        .knowledge_retrieve(&scope.0, &scope.1, &scope.2, "local-user", "传感器", 5)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn chat_receives_ranked_evidence_with_clickable_sources_through_the_existing_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text(
        "依据资料，保压时间不足会导致收缩。",
    )]));
    let rt = runtime(&tmp, provider.clone());
    let scope = space(&rt);
    let (d, v) = import(&rt, &scope, "工艺参数.md", "保压时间不足会导致制品收缩。");
    publish(&rt, &scope, &d, &v);
    let session = rt.create_session(None).unwrap();
    let mut request = PromptRequest::new(session, "保压时间不足会怎样");
    request.task_mode = TaskMode::Ask;
    rt.prompt(request)
        .await
        .unwrap();
    let messages = format!("{:?}", provider.last_messages());
    assert!(messages.contains("制品收缩"), "{messages}");
    assert!(messages.contains("#knowledge/"), "{messages}");
    assert!(messages.contains("v1"));
}

#[tokio::test]
async fn filenames_and_saved_drafts_have_a_complete_import_preview_enable_flow() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = runtime(&tmp, Arc::new(ScriptedProvider::new(vec![])));
    let scope = space(&rt);
    let (doc, version) = import(&rt, &scope, "售后流程.md", "签收后七天内凭订单号申请退货。");
    let preview = rt
        .space_document_preview("local-user", &scope.2, &doc)
        .unwrap();
    assert_eq!(preview.version_id, version);
    assert!(!preview.enabled);
    assert!(rt
        .space_document_preview("outsider", &scope.2, &doc)
        .is_err());
    publish(&rt, &scope, &doc, &version);
    assert!(
        rt.space_document_preview("local-user", &scope.2, &doc)
            .unwrap()
            .enabled
    );
    let found = rt
        .space_search(
            "local-user",
            std::slice::from_ref(&scope.2),
            "售后流程.md",
            3,
        )
        .unwrap();
    assert!(
        !found.hits.is_empty(),
        "file names should locate their source even if absent from the body"
    );
    assert!(found.hits[0].excerpt.contains("七天"));
}


#[tokio::test]
async fn auto_search_discovers_all_local_spaces_without_binding_or_preloading() {
    use dandelion_core::provider::types::ToolCall;
    let tmp = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("你好"),
        ScriptedTurn::tools("", vec![ToolCall {
            id: "local-search".into(), name: "knowledge_search".into(),
            arguments: r#"{"query":"保压时间不足"}"#.into(),
        }]),
        ScriptedTurn::text("找到资料"),
    ]));
    let rt = runtime(&tmp, provider.clone());
    // More spaces than the old manual-binding cap; the relevant source is last.
    for i in 0..7 {
        let s = rt.knowledge_create_local_space("local-user", &format!("资料{i}"), "allow_summary").unwrap();
        let scope = ("local".into(), "local".into(), s.id);
        let text = if i == 6 { "保压时间不足会导致制品收缩。" } else { "零件运输需使用纸箱。" };
        let (d,v) = import(&rt, &scope, &format!("工艺{i}.md"), text);
        publish(&rt, &scope, &d, &v);
    }
    // A local space owned by someone else is not part of this user's scope.
    let hidden = rt.knowledge_create_local_space("another-user", "私人资料", "allow_summary").unwrap();
    rt.knowledge_archive_space(&hidden.id, true).unwrap();
    let session = rt.create_session(None).unwrap();
    rt.prompt(PromptRequest::new(session.clone(), "保压时间不足会怎样")) .await.unwrap();
    let first = format!("{:?}", provider.last_messages());
    assert!(!first.contains("制品收缩"), "Auto must not preload evidence: {first}");
    assert!(!first.contains(dandelion_core::corpus::SEGMENT_MARK));
    assert!(provider.last_tool_names().iter().any(|name| name == "knowledge_search"));
    rt.prompt(PromptRequest::new(session, "查一下本地工艺资料")) .await.unwrap();
    let messages = format!("{:?}", provider.last_messages());
    assert!(messages.contains("制品收缩"), "{messages}");
    assert!(messages.contains("#knowledge/"), "{messages}");
    assert!(messages.contains("工艺6.md"), "{messages}");
}

#[tokio::test]
async fn ask_miss_and_unavailable_spaces_do_not_add_unrelated_context() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("你好")]));
    let rt = runtime(&tmp, provider.clone());
    let scope = space(&rt);
    let (d,v) = import(&rt, &scope, "工艺.md", "保压时间不足会导致制品收缩。");
    publish(&rt, &scope, &d, &v);
    let hidden = rt.knowledge_create_local_space("another-user", "非本人资料", "allow_summary").unwrap();
    let (d,v) = rt.knowledge_import_document("local","local",&hidden.id,"another-user","file","别人的资料.md","天气晴朗，气温二十度。",500).unwrap();
    rt.knowledge_publish_version("local","local",&hidden.id,"another-user",&d.id,&v.id).unwrap();
    let session = rt.create_session(None).unwrap();
    let mut request = PromptRequest::new(session, "天气和气温如何？");
    request.task_mode = TaskMode::Ask;
    rt.prompt(request).await.unwrap();
    let messages = format!("{:?}",provider.last_messages());
    assert!(!messages.contains("二十度"), "{messages}");
    assert!(!messages.contains("制品收缩"), "{messages}");
    assert!(!messages.contains(dandelion_core::corpus::SEGMENT_MARK), "{messages}");
}

#[tokio::test]
async fn an_existing_chat_discovers_later_imports_without_rebinding() {
    use dandelion_core::provider::types::ToolCall;
    let tmp = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("你好"),
        ScriptedTurn::tools("", vec![ToolCall {
            id: "later-import".into(), name: "knowledge_search".into(),
            arguments: r#"{"query":"售后七天退货"}"#.into(),
        }]),
        ScriptedTurn::text("可以申请退货"),
    ]));
    let rt = runtime(&tmp, provider.clone());
    let session = rt.create_session(None).unwrap();
    rt.prompt(PromptRequest::new(session.clone(), "你好")).await.unwrap();
    assert!(!provider.last_tool_names().iter().any(|name| name == "knowledge_search"));
    let scope = space(&rt);
    let (doc, version) = import(&rt, &scope, "售后流程.md", "签收后七天内凭订单号申请退货。");
    publish(&rt, &scope, &doc, &version);
    rt.prompt(PromptRequest::new(session, "我导入的售后资料怎么说？")).await.unwrap();
    let messages = format!("{:?}", provider.last_messages());
    assert!(messages.contains("签收后七天内凭订单号"), "{messages}");
    assert!(messages.contains("#knowledge/"), "{messages}");
}
