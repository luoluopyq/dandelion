//! Real persistence and retrieval tests for independent personal knowledge.
use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::store::{sqlite::KnowledgeItemInput, Store};
use dandelion_core::{PermissionMode, Runtime, Settings};
use std::{fs, sync::Arc};
const USER: &str = "local-user";
fn note(title: &str, body: &str) -> KnowledgeItemInput {
    KnowledgeItemInput {
        title: title.into(),
        kind: "note".into(),
        body: body.into(),
        notes: String::new(),
        document_id: None,
        expected_version_id: None,
        source_id: None,
    }
}
fn runtime(t: &tempfile::TempDir, p: Arc<ScriptedProvider>) -> Runtime {
    let mut s = Settings::defaults_configured();
    s.workspace = Some(t.path().into());
    s.permission_mode = PermissionMode::Auto;
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(t.path().join("data"))),
        s,
        p,
    )
    .unwrap()
}

#[test]
fn standalone_notes_are_published_and_retrievable_without_a_project() {
    let t = tempfile::tempdir().unwrap();
    let store = Store::open(t.path(), 5000).unwrap();
    let item = store
        .knowledge_save_item(
            USER,
            &note(
                "Go 取消任务",
                "用 context 传播取消信号，等待 goroutine 退出。",
            ),
        )
        .unwrap();
    assert!(item.enabled);
    assert!(!item.has_asset);
    assert!(item.topics.is_empty());
    let found = store
        .knowledge_retrieve("local", "local", &item.space_id, USER, "取消信号", 5)
        .unwrap();
    assert!(!found.is_empty());
    let citation = &found[0];
    assert_eq!(
        store
            .knowledge_chunk_source(&item.space_id, USER, &citation.chunk_id)
            .unwrap()
            .version_id,
        item.version_id
    );
    assert!(store.projects_list().unwrap().is_empty());
    assert!(store.knowledge_item("stranger", &item.id).is_err());
}

#[test]
fn empty_markdown_and_binary_survive_as_durable_originals() {
    let t = tempfile::tempdir().unwrap();
    let data = t.path().join("data");
    let source = t.path().join("README.md");
    fs::write(&source, b"").unwrap();
    let store = Store::open(&data, 5000).unwrap();
    let item = store.knowledge_collect_file(USER, &source, 65536).unwrap();
    assert_eq!(item.parse_status, "empty");
    assert!(item.has_asset);
    assert!(item.enabled);
    let binary = t.path().join("lesson.mp4");
    fs::write(&binary, [0, 255, 19, 7]).unwrap();
    let media = store.knowledge_collect_file(USER, &binary, 65536).unwrap();
    assert_eq!(media.kind, "video");
    assert_eq!(media.parse_status, "unsupported");
    assert!(media.body.is_empty());
    fs::remove_file(source).unwrap();
    fs::remove_file(binary).unwrap();
    drop(store);
    let store = Store::open(&data, 5000).unwrap();
    assert!(
        fs::read(store.knowledge_item_asset(USER, &item.id).unwrap())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fs::read(store.knowledge_item_asset(USER, &media.id).unwrap()).unwrap(),
        [0, 255, 19, 7]
    );
}

#[test]
fn snapshots_reimport_idempotently_and_source_updates_keep_personal_notes() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("snippet.go");
    fs::write(&p, "package main\n// context cancellation\n").unwrap();
    let store = Store::open(&t.path().join("data"), 5000).unwrap();
    let item = store.knowledge_collect_file(USER, &p, 65536).unwrap();
    assert_eq!(item.kind, "code");
    let same = store.knowledge_collect_file(USER, &p, 65536).unwrap();
    assert_eq!(same.id, item.id);
    assert_eq!(same.version_id, item.version_id);
    let mut edit = note(&item.title, &item.body);
    edit.kind = item.kind.clone();
    edit.notes = "这是我的退出模式".into();
    edit.document_id = Some(item.id.clone());
    edit.expected_version_id = Some(item.version_id.clone());
    let updated = store.knowledge_save_item(USER, &edit).unwrap();
    assert!(
        store.knowledge_save_item(USER, &edit).is_err(),
        "stale editor must not overwrite"
    );
    fs::write(&p, "package main\n// new source\n").unwrap();
    let new = store.knowledge_collect_file(USER, &p, 65536).unwrap();
    assert_eq!(new.id, item.id);
    assert_eq!(new.notes, updated.notes);
    assert!(new.body.contains("new source"));
    assert_eq!(
        fs::read_to_string(store.knowledge_item_asset(USER, &new.id).unwrap()).unwrap(),
        "package main\n// new source\n"
    );
}

#[test]
fn parsing_failure_retains_original_and_retry_works_after_source_is_removed() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("big.md");
    fs::write(&p, "模型离线也要能保存知识。".repeat(100)).unwrap();
    let store = Store::open(&t.path().join("data"), 5000).unwrap();
    let item = store.knowledge_collect_file(USER, &p, 10).unwrap();
    assert_eq!(item.parse_status, "failed");
    assert!(item.has_asset);
    fs::remove_file(&p).unwrap();
    let retry = store.knowledge_retry_item(USER, &item.id, 65536).unwrap();
    assert_eq!(retry.id, item.id);
    assert_eq!(retry.parse_status, "ready");
    assert!(retry.body.contains("模型离线"));
    assert_eq!(retry.source_id, item.source_id);
}

#[test]
fn late_classification_cannot_override_manual_topics_or_a_new_version() {
    let t = tempfile::tempdir().unwrap();
    let store = Store::open(t.path(), 5000).unwrap();
    let item = store
        .knowledge_save_item(USER, &note("Channel", "关闭 channel"))
        .unwrap();
    store
        .knowledge_item_set_organization(
            USER,
            &item.id,
            &item.version_id,
            Some(vec!["手动主题".into()]),
            Some(true),
        )
        .unwrap();
    assert!(!store
        .knowledge_item_classification_result(
            USER,
            &item.id,
            &item.version_id,
            vec!["错误主题".into()],
            vec![],
            None
        )
        .unwrap());
    let mut edit = note("Channel", "更新正文");
    edit.document_id = Some(item.id.clone());
    edit.expected_version_id = Some(item.version_id.clone());
    let new = store.knowledge_save_item(USER, &edit).unwrap();
    assert!(!store
        .knowledge_item_classification_result(
            USER,
            &item.id,
            &item.version_id,
            vec![],
            vec!["过期".into()],
            None
        )
        .unwrap());
    assert_eq!(new.topics, vec!["手动主题"]);
    assert!(new.favorite);
}

#[tokio::test]
async fn model_classification_respects_policy_and_proposes_new_topics() {
    let t = tempfile::tempdir().unwrap();
    let p = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text(
        r#"{"topics":["Go","并发编程"]}"#,
    )]));
    let rt = runtime(&t, p.clone());
    let item = rt
        .knowledge_item_save(USER, &note("Go 的并发", "goroutine 和 channel"))
        .unwrap();
    assert!(rt.knowledge_item_classify(USER, &item.id).await.is_err());
    assert_eq!(p.complete_count(), 0);
    rt.knowledge_item_ai_policy(USER, Some(true)).unwrap();
    let classified = rt.knowledge_item_classify(USER, &item.id).await.unwrap();
    assert!(classified.topics.is_empty());
    assert_eq!(classified.suggested_topics, vec!["Go", "并发编程"]);
    assert_eq!(classified.body, item.body);
}

#[test]
fn explicit_reply_is_saved_whole_with_only_one_notification() {
    let t = tempfile::tempdir().unwrap();
    let store = Store::open(t.path(), 5000).unwrap();
    let body = "Go 并发核心知识。".repeat(30);
    let drafts = dandelion_core::wiki::capture_requested_reply(
        &store,
        t.path(),
        "session",
        "reply",
        "保存到知识库",
        &body,
        80,
    )
    .unwrap();
    assert_eq!(drafts.len(), 1);
    let items = store.knowledge_items(USER).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].body, body);
    assert!(drafts
        .iter()
        .all(|d| d.knowledge_document_id.as_deref() == Some(items[0].id.as_str())));
    dandelion_core::wiki::capture_requested_reply(
        &store,
        t.path(),
        "session",
        "reply",
        "保存到知识库",
        &body,
        80,
    )
    .unwrap();
    assert_eq!(store.knowledge_items(USER).unwrap().len(), 1);
    assert!(dandelion_core::wiki::capture_requested_reply(
        &store,
        t.path(),
        "session",
        "other",
        "不要保存到知识库",
        &body,
        80
    )
    .unwrap()
    .is_empty());
}

#[test]
fn deleting_one_item_keeps_shared_assets_until_the_last_reference_is_removed() {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("a.md");
    let b = t.path().join("b.md");
    fs::write(&a, "同一份来源正文").unwrap();
    fs::write(&b, "同一份来源正文").unwrap();
    let store = Store::open(&t.path().join("data"), 5000).unwrap();
    let first = store.knowledge_collect_file(USER, &a, 65536).unwrap();
    let second = store.knowledge_collect_file(USER, &b, 65536).unwrap();
    let asset = store.knowledge_item_asset(USER, &first.id).unwrap();
    assert_eq!(asset, store.knowledge_item_asset(USER, &second.id).unwrap());
    store.knowledge_remove_item(USER, &first.id).unwrap();
    assert!(asset.exists());
    assert!(store.knowledge_item(USER, &first.id).is_err());
    store.knowledge_remove_item(USER, &second.id).unwrap();
    assert!(!asset.exists());
    assert!(a.exists() && b.exists(), "original files are never deleted");
}

#[test]
fn pasted_content_needs_no_title_and_manual_edits_remain_searchable() {
    let t = tempfile::tempdir().unwrap();
    let store = Store::open(t.path(), 5000).unwrap();
    let item = store
        .knowledge_save_item(USER, &note("", "## Go 取消信号\n正文内容"))
        .unwrap();
    assert_eq!(item.title, "Go 取消信号");
    let mut edit = note("新标题", "新的正文，关闭资源");
    edit.document_id = Some(item.id.clone());
    edit.expected_version_id = Some(item.version_id.clone());
    let updated = store.knowledge_save_item(USER, &edit).unwrap();
    assert_ne!(updated.version_id, item.version_id);
    assert!(!store
        .knowledge_retrieve("local", "local", &item.space_id, USER, "关闭资源", 5)
        .unwrap()
        .is_empty());
}

#[test]
fn legacy_imports_are_editable_in_place_and_summary_search_reads_the_entire_body() {
    let t = tempfile::tempdir().unwrap();
    let store = Store::open(t.path(), 5000).unwrap();
    let space = store
        .knowledge_create_local_space(USER, "旧资料库", "local_only")
        .unwrap();
    let text = format!("{}\n结尾包含一个特殊检索词", "前面的内容。".repeat(200));
    let (doc, version) = store
        .knowledge_import_document(
            "local",
            "local",
            &space.id,
            USER,
            "markdown",
            "README.md",
            &text,
            1200,
        )
        .unwrap();
    store
        .knowledge_publish_version("local", "local", &space.id, USER, &doc.id, &version.id)
        .unwrap();
    let found = store.knowledge_item_summaries(USER, "特殊检索词").unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].body.chars().count() <= 180);
    assert!(
        !found[0].body.contains("特殊检索词"),
        "query must search beyond the UI preview"
    );
    let mut edit = note("README 解释", "修改后的正文");
    edit.document_id = Some(doc.id.clone());
    edit.expected_version_id = Some(version.id);
    let saved = store.knowledge_save_item(USER, &edit).unwrap();
    assert_eq!(saved.id, doc.id);
    assert_eq!(store.knowledge_items(USER).unwrap().len(), 1);
}

#[tokio::test]
async fn working_in_a_project_does_not_force_general_knowledge_saves_into_its_wiki() {
    use dandelion_core::{provider::types::ToolCall, PromptRequest};
    let t = tempfile::tempdir().unwrap();
    let p=Arc::new(ScriptedProvider::new(vec![
   ScriptedTurn::tools("",vec![ToolCall{id:"save".into(),name:"wiki_propose".into(),arguments:serde_json::json!({"kind":"term","mode":"write","content":"Go 使用 context 取消后台任务"}).to_string()}]),ScriptedTurn::text("已保存。")
 ]));
    let rt = runtime(&t, p);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "整理并保存到我的知识库"))
        .await
        .unwrap();
    let items = rt.knowledge_item_list(USER, "").unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].title.contains("context"));
    assert!(rt.wiki_entries_for(Some(t.path())).is_empty());
}

#[tokio::test]
async fn unavailable_model_does_not_undo_saved_content_or_local_retrieval() {
    let t = tempfile::tempdir().unwrap();
    let mut fail = ScriptedTurn::text("");
    fail.fail = Some("model offline".into());
    let rt = runtime(&t, Arc::new(ScriptedProvider::new(vec![fail])));
    let item = rt
        .knowledge_item_save(USER, &note("资源清理", "取消任务后释放文件句柄"))
        .unwrap();
    rt.knowledge_item_ai_policy(USER, Some(true)).unwrap();
    assert!(rt.knowledge_item_classify(USER, &item.id).await.is_err());
    let saved = rt.knowledge_item_get(USER, &item.id).unwrap();
    assert_eq!(saved.body, item.body);
    assert_eq!(saved.classification_status, "failed");
    assert_eq!(rt.knowledge_item_list(USER, "文件句柄").unwrap().len(), 1);
}

#[test]
fn a_hundred_mixed_files_remain_independent_and_reimport_without_growth() {
    let t = tempfile::tempdir().unwrap();
    let store = Store::open(&t.path().join("data"), 5000).unwrap();
    let mut paths = Vec::new();
    for n in 0..100 {
        let extension = ["md", "go", "mp4", "txt"][n % 4];
        let path = t.path().join(format!("fragment-{n}.{extension}"));
        fs::write(
            &path,
            if n % 4 == 0 {
                Vec::new()
            } else {
                format!("知识片段 {n} context cancellation").into_bytes()
            },
        )
        .unwrap();
        let item = store.knowledge_collect_file(USER, &path, 65536).unwrap();
        assert!(item.has_asset);
        paths.push(path);
    }
    assert_eq!(store.knowledge_item_summaries(USER, "").unwrap().len(), 100);
    for path in paths {
        store.knowledge_collect_file(USER, &path, 65536).unwrap();
    }
    assert_eq!(store.knowledge_item_summaries(USER, "").unwrap().len(), 100);
}

#[test]
fn an_existing_personal_name_is_reused_and_archiving_never_hides_new_saves() {
    let t = tempfile::tempdir().unwrap();
    let store = Store::open(t.path(), 5000).unwrap();
    let existing = store
        .knowledge_create_local_space(USER, "我的知识", "local_only")
        .unwrap();
    assert_eq!(
        store.knowledge_personal_space(USER).unwrap().id,
        existing.id
    );
    store.knowledge_archive_space(&existing.id, true).unwrap();
    let saved = store
        .knowledge_save_item(USER, &note("新笔记", "归档以后仍能添加新内容"))
        .unwrap();
    assert_ne!(saved.space_id, existing.id);
    assert!(store
        .knowledge_item_summaries(USER, "")
        .unwrap()
        .iter()
        .any(|i| i.id == saved.id));
}

#[test]
fn changed_binary_originals_get_distinct_versions_even_without_extracted_text() {
    let t = tempfile::tempdir().unwrap();
    let file = t.path().join("lesson.mp4");
    let store = Store::open(&t.path().join("data"), 5000).unwrap();
    fs::write(&file, [0u8, 1, 2]).unwrap();
    let a = store.knowledge_collect_file(USER, &file, 65536).unwrap();
    let first_asset = store.knowledge_item_asset(USER, &a.id).unwrap();
    fs::write(&file, [0u8, 4, 9]).unwrap();
    let b = store.knowledge_collect_file(USER, &file, 65536).unwrap();
    let second_asset = store.knowledge_item_asset(USER, &b.id).unwrap();
    assert_eq!(a.id, b.id);
    assert_ne!(a.version_id, b.version_id);
    assert_ne!(first_asset, second_asset);
    assert!(first_asset.exists());
    store.knowledge_remove_item(USER, &a.id).unwrap();
    assert!(!first_asset.exists() && !second_asset.exists());
}

#[tokio::test]
async fn existing_topic_names_are_canonical_and_manual_topics_skip_model_calls() {
    let t = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text(
        r#"{"topics":["go"]}"#,
    )]));
    let rt = runtime(&t, provider.clone());
    let seed = rt
        .knowledge_item_save(USER, &note("原有内容", "Go 基础"))
        .unwrap();
    rt.knowledge_item_organize(
        USER,
        &seed.id,
        &seed.version_id,
        Some(vec!["Go".into()]),
        None,
    )
    .unwrap();
    rt.knowledge_item_ai_policy(USER, Some(true)).unwrap();
    let next = rt
        .knowledge_item_save(USER, &note("新片段", "channel 的关闭"))
        .unwrap();
    let result = rt.knowledge_item_classify(USER, &next.id).await.unwrap();
    assert_eq!(result.topics, vec!["Go"]);
    assert!(result.suggested_topics.is_empty());
    assert!(rt.knowledge_item_classify(USER, &seed.id).await.is_err());
    assert_eq!(provider.complete_count(), 1);
}

#[tokio::test]
async fn concurrent_classification_of_one_item_spends_only_one_model_turn() {
    let t = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::delayed_text(
        r#"{"topics":["Go"]}"#,
        std::time::Duration::from_millis(40),
    )]));
    let rt = runtime(&t, provider.clone());
    let item = rt
        .knowledge_item_save(USER, &note("并发笔记", "goroutine 和 channel"))
        .unwrap();
    rt.knowledge_item_ai_policy(USER, Some(true)).unwrap();
    let (first, second) = tokio::join!(
        rt.knowledge_item_classify(USER, &item.id),
        rt.knowledge_item_classify(USER, &item.id)
    );
    assert!(first.is_ok() || second.is_ok());
    assert!(first.is_err() || second.is_err());
    assert_eq!(provider.complete_count(), 1);
}

#[tokio::test]
async fn personal_classification_does_not_depend_on_a_project_directory_existing() {
    let t = tempfile::tempdir().unwrap();
    let ws = t.path().join("project");
    fs::create_dir(&ws).unwrap();
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(ws.clone());
    let rt = Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(t.path().join("data"))),
        settings,
        Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text(
            r#"{"topics":["Go"]}"#,
        )])),
    )
    .unwrap();
    let item = rt
        .knowledge_item_save(USER, &note("本地笔记", "Go 知识片段"))
        .unwrap();
    rt.knowledge_item_ai_policy(USER, Some(true)).unwrap();
    fs::remove_dir(&ws).unwrap();
    assert!(rt.knowledge_item_classify(USER, &item.id).await.is_ok());
}

#[test]
fn non_utf8_markdown_keeps_the_exact_original_and_notes_can_still_be_retrieved() {
    let t = tempfile::tempdir().unwrap();
    let file = t.path().join("README.md");
    let raw = [0xffu8, 0xfe, 0x41, 0x00];
    fs::write(&file, raw).unwrap();
    let store = Store::open(&t.path().join("data"), 5000).unwrap();
    let item = store.knowledge_collect_file(USER, &file, 65536).unwrap();
    assert_eq!(item.parse_status, "failed");
    assert!(item.parse_error.as_deref().unwrap().contains("UTF-8"));
    assert_eq!(
        fs::read(store.knowledge_item_asset(USER, &item.id).unwrap()).unwrap(),
        raw
    );
    let mut edit = note(&item.title, "");
    edit.notes = "该资料说明连接池回收方式".into();
    edit.document_id = Some(item.id.clone());
    edit.expected_version_id = Some(item.version_id);
    let edited = store.knowledge_save_item(USER, &edit).unwrap();
    assert!(edited.has_asset);
    assert!(!store
        .knowledge_retrieve("local", "local", &item.space_id, USER, "连接池", 5)
        .unwrap()
        .is_empty());
}
