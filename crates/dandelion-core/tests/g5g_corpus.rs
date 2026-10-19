//! G5-G: sqlite-vec corpus. Bound retrieval is a user segment; unbound skips; never system.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::{ChatMessage, ToolCall};
use dandelion_core::{MemoryKind, PermissionMode, PromptRequest, Runtime, Settings, TaskMode};
use std::sync::Arc;

fn ask(session: dandelion_core::ids::SessionId, text: &str) -> PromptRequest {
    let mut request = PromptRequest::new(session, text);
    request.task_mode = TaskMode::Ask;
    request
}

fn runtime_with(
    dir: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    mut settings: Settings,
) -> Runtime {
    settings.workspace = Some(dir.to_path_buf());
    settings.permission_mode = PermissionMode::Auto;
    settings.cron_tick_sec = 3_600;
    settings.corpus_min_disk_free_mb = 1;
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(dir.join("data"))),
        settings,
        provider,
    )
    .expect("runtime")
}

#[tokio::test]
async fn bound_corpus_injects_user_segment_not_system() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text(
        "cited UNIQUE_CORPUS_TOKEN_alpha",
    )]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let src = dir.path().join("notes.txt");
    std::fs::write(
        &src,
        "UNIQUE_CORPUS_TOKEN_alpha is the secret garden phrase.\n\nUnrelated filler.",
    )
    .unwrap();
    let doc = rt.corpus_import(&src).expect("import");
    let sid = rt.create_session(Some("kb")).expect("session");
    rt.session_replace_bindings(&sid, "corpus", &[doc.id.clone()])
        .unwrap();
    rt.prompt(PromptRequest::new(sid, "What is the secret garden phrase?"))
        .await
        .expect("prompt");
    let system = provider.last_system().unwrap_or_default();
    assert!(
        !system.contains("UNIQUE_CORPUS_TOKEN_alpha"),
        "corpus must not enter system: {system}"
    );
    assert!(
        !system.contains("[知识库检索]"),
        "corpus mark must not enter system"
    );
    let users: Vec<String> = provider
        .last_messages()
        .into_iter()
        .filter_map(|m| match m {
            ChatMessage::User { content, .. } => Some(content),
            _ => None,
        })
        .collect();
    assert!(
        users
            .iter()
            .any(|u| u.contains("[知识库检索]") && u.contains("UNIQUE_CORPUS_TOKEN_alpha")),
        "expected user segment, got {users:?}"
    );
}

#[tokio::test]
async fn memory_and_chinese_corpus_reach_the_same_agent_turn() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    rt.memory_set(MemoryKind::User, "用户偏好：回答要简短。")
        .unwrap();
    let source = dir.path().join("activity.txt");
    std::fs::write(&source, "周末活动预算360元，地点海风公园，参加人数6人。").unwrap();
    let doc = rt.corpus_import(&source).unwrap();
    let sid = rt.create_session(Some("联动验证")).unwrap();
    let mut request = PromptRequest::new(sid, "周末活动预算和地点是什么？");
    request.corpus_ids = vec![doc.id];
    rt.prompt(request).await.unwrap();
    let system = provider.last_system().unwrap();
    assert!(system.contains("回答要简短"), "{system}");
    let users: Vec<String> = provider
        .last_messages()
        .into_iter()
        .filter_map(|message| match message {
            ChatMessage::User { content, .. } => Some(content),
            _ => None,
        })
        .collect();
    assert!(
        users
            .iter()
            .any(|content| content.contains("海风公园") && content.contains("360")),
        "{users:?}"
    );
}

#[tokio::test]
async fn bound_published_local_knowledge_reaches_agent_with_memory() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let mut settings = Settings::defaults_configured();
    settings.default_backend = "sidecar".into();
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    rt.memory_set(MemoryKind::User, "用户偏好：简短回答。")
        .unwrap();
    let space = rt
        .knowledge_create_local_space("local-user", "周末资料", "local_only")
        .unwrap();
    let (doc, version) = rt
        .knowledge_import_document(
            "local",
            "local",
            &space.id,
            "local-user",
            "manual",
            "weekend",
            "周末活动预算360元，地点海风公园，参加人数6人。",
            256,
        )
        .unwrap();
    rt.knowledge_publish_version(
        "local",
        "local",
        &space.id,
        "local-user",
        &doc.id,
        &version.id,
    )
    .unwrap();
    let sid = rt.create_session(Some("绑定知识库")).unwrap();
    rt.session_replace_bindings(&sid, "knowledge", &[space.id.clone()])
        .unwrap();
    rt.prompt(ask(sid, "周末活动预算和地点是什么？"))
        .await
        .unwrap();
    let system = provider.last_system().unwrap();
    assert!(system.contains("简短回答"));
    assert!(!system.contains("海风公园"), "{system}");
    let users = provider.last_messages();
    let joined = format!("{users:?}");
    assert!(
        joined.contains("360") && joined.contains("海风公园"),
        "{joined}"
    );
    // Unified search rendering: every hit block names its surface and its
    // space; the lexical chunk block carries the source id.
    assert!(joined.contains("资料片段"), "{joined}");
    assert!(joined.contains("周末资料"), "{joined}");
    assert!(joined.contains("weekend · v1"), "{joined}");
}

/// The blanket "a bound space forces a local model" refusal is retired: a
/// cloud-model turn now filters per space, exactly like `space_ask`. A
/// `local_only` space is NAMED without content — never silently dropped,
/// never silently sent — while an `allow_summary` space contributes.
#[tokio::test]
async fn cloud_turn_filters_bound_spaces_by_external_policy() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    // Default backend (not sidecar): the turn goes to the cloud model.
    let rt = runtime_with(dir.path(), provider.clone(), Settings::defaults_configured());
    let open = rt
        .knowledge_create_local_space("local-user", "公开资料", "allow_summary")
        .unwrap();
    let closed = rt
        .knowledge_create_local_space("local-user", "私有资料", "local_only")
        .unwrap();
    for (space, body, source) in [
        (&open, "报价审批阈值为一万元，需总监复核。", "quote"),
        (&closed, "内部机密代号是青石计划。", "secret"),
    ] {
        let (doc, version) = rt
            .knowledge_import_document(
                "local",
                "local",
                &space.id,
                "local-user",
                "manual",
                source,
                body,
                256,
            )
            .unwrap();
        rt.knowledge_publish_version(
            "local",
            "local",
            &space.id,
            "local-user",
            &doc.id,
            &version.id,
        )
        .unwrap();
    }
    let sid = rt.create_session(Some("云端按空间过滤")).unwrap();
    rt.session_replace_bindings(&sid, "knowledge", &[open.id, closed.id.clone()])
        .unwrap();
    rt.prompt(ask(sid, "报价审批阈值是多少？"))
        .await
        .expect("cloud turn must not be refused wholesale");
    let joined = format!("{:?}", provider.last_messages());
    // The permitted space contributes content…
    assert!(joined.contains("报价审批阈值为一万元"), "{joined}");
    // …the restricted space is named without its content…
    assert!(joined.contains("「私有资料」受外部策略限制"), "{joined}");
    assert!(!joined.contains("青石计划"), "restricted content leaked: {joined}");
}

/// Archived spaces are excluded from automatic discovery without disrupting chat.
#[tokio::test]
async fn archived_knowledge_is_not_discovered_and_does_not_block_chat() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let mut settings = Settings::defaults_configured();
    settings.default_backend = "sidecar".into();
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let space = rt
        .knowledge_create_local_space("local-user", "封存库", "local_only")
        .unwrap();
    let (doc, version) = rt
        .knowledge_import_document(
            "local",
            "local",
            &space.id,
            "local-user",
            "manual",
            "note",
            "封存库内容代号是青石。",
            256,
        )
        .unwrap();
    rt.knowledge_publish_version(
        "local",
        "local",
        &space.id,
        "local-user",
        &doc.id,
        &version.id,
    )
    .unwrap();
    rt.knowledge_archive_space(&space.id, true).unwrap();
    let sid = rt.create_session(Some("闸门失败续谈")).unwrap();
    rt.session_replace_bindings(&sid, "knowledge", &[space.id])
        .unwrap();
    rt.prompt(PromptRequest::new(sid, "封存库里有什么？"))
        .await
        .expect("gate failure must not kill the conversation");
    let joined = format!("{:?}", provider.last_messages());
    assert!(!joined.contains(dandelion_core::corpus::SEGMENT_MARK), "{joined}");
    assert!(!joined.contains("青石"), "archived content leaked: {joined}");
}

/// 「它的数值呢？」 carries no searchable term of its own. The zero-hit
/// retry composes the previous RAW user turn, so the anaphoric follow-up
/// finds the evidence its topic continues.
#[tokio::test]
async fn followup_question_retries_with_previous_user_turn() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("下限为每秒18升。"),
        ScriptedTurn::text("同一系统的下限数值。"),
    ]));
    let mut settings = Settings::defaults_configured();
    settings.default_backend = "sidecar".into();
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let space = rt
        .knowledge_create_local_space("local-user", "设备手册", "local_only")
        .unwrap();
    let (doc, version) = rt
        .knowledge_import_document(
            "local",
            "local",
            &space.id,
            "local-user",
            "manual",
            "cooler",
            "冷却流量下限为每秒18升。",
            256,
        )
        .unwrap();
    rt.knowledge_publish_version(
        "local",
        "local",
        &space.id,
        "local-user",
        &doc.id,
        &version.id,
    )
    .unwrap();
    let sid = rt.create_session(Some("追问指代")).unwrap();
    rt.session_replace_bindings(&sid, "knowledge", &[space.id])
        .unwrap();
    rt.prompt(ask(sid.clone(), "请问冷却系统的流量下限是多少？"))
        .await
        .expect("first turn");
    rt.prompt(ask(sid, "那它的数值呢？"))
        .await
        .expect("follow-up turn");
    let joined = format!("{:?}", provider.last_messages());
    assert!(joined.contains("追问结合上一条用户消息"), "{joined}");
    assert!(joined.contains("18升"), "{joined}");
}

#[tokio::test]
async fn revoked_knowledge_is_not_sent_to_agent() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("无资料")]));
    let mut settings = Settings::defaults_configured();
    settings.default_backend = "sidecar".into();
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let space = rt
        .knowledge_create_local_space("local-user", "撤回测试", "local_only")
        .unwrap();
    let (doc, version) = rt
        .knowledge_import_document(
            "local",
            "local",
            &space.id,
            "local-user",
            "manual",
            "secret",
            "海风公园的秘密代号是青石。",
            256,
        )
        .unwrap();
    rt.knowledge_publish_version(
        "local",
        "local",
        &space.id,
        "local-user",
        &doc.id,
        &version.id,
    )
    .unwrap();
    rt.knowledge_revoke_version(
        "local",
        "local",
        &space.id,
        "local-user",
        &doc.id,
        &version.id,
    )
    .unwrap();
    let sid = rt.create_session(Some("撤回验证")).unwrap();
    rt.session_replace_bindings(&sid, "knowledge", &[space.id])
        .unwrap();
    rt.prompt(ask(sid, "海风公园的秘密代号是什么？"))
        .await
        .unwrap();
    let joined = format!("{:?}", provider.last_messages());
    assert!(!joined.contains("青石"), "{joined}");
    assert!(!joined.contains(dandelion_core::corpus::SEGMENT_MARK), "a miss must add no knowledge context: {joined}");
}

#[tokio::test]
async fn unbound_corpus_does_not_retrieve() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let src = dir.path().join("notes.txt");
    std::fs::write(&src, "UNIQUE_CORPUS_TOKEN_alpha should stay out.").unwrap();
    rt.corpus_import(&src).expect("import");
    let sid = rt.new_session(None);
    assert_eq!(rt.corpus_max_hits_for(&sid).unwrap(), 0);
    rt.prompt(PromptRequest::new(
        sid,
        "What is UNIQUE_CORPUS_TOKEN_alpha?",
    ))
    .await
    .expect("prompt");
    let system = provider.last_system().unwrap_or_default();
    assert!(!system.contains("UNIQUE_CORPUS_TOKEN_alpha"));
    let blob: String = provider
        .last_messages()
        .into_iter()
        .filter_map(|m| match m {
            ChatMessage::User { content, .. } => Some(content),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !blob.contains("[知识库检索]"),
        "unbound must not retrieve: {blob}"
    );
}

#[tokio::test]
async fn import_wiki_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(
        dir.path(),
        Arc::new(ScriptedProvider::new(vec![])),
        Settings::defaults_configured(),
    );
    let wiki = dir.path().join("data").join("wiki");
    std::fs::create_dir_all(&wiki).unwrap();
    let page = wiki.join("ARCH.md");
    std::fs::write(&page, "secret-wiki-sentence").unwrap();
    let err = rt.corpus_import(&page).unwrap_err();
    assert!(err.to_string().contains("wiki"), "{err}");
}

#[tokio::test]
async fn oversized_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = Settings::defaults_configured();
    settings.corpus_min_disk_free_mb = 1;
    settings.corpus_max_file_bytes = 1024;
    let rt = runtime_with(
        dir.path(),
        Arc::new(ScriptedProvider::new(vec![])),
        settings,
    );
    let src = dir.path().join("notes.txt");
    std::fs::write(&src, "x".repeat(1025)).unwrap();
    let err = rt.corpus_import(&src).unwrap_err();
    assert!(err.to_string().contains("too large"), "{err}");
}

/// §七 on-demand pull, end to end. The auto segment searches the user's
/// own words; here those words miss ("帮我查点东西" shares nothing with
/// the document). The model then calls `knowledge_search` with its own
/// query, and the tool's result carries the rendered evidence — same gate,
/// same render (资料片段 · space name · source label), through the tool path.
/// The evidence token appears ONLY in the tool result: the auto segment
/// held a miss, so this assertion cannot pass off the first pass's work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn knowledge_search_tool_answers_where_auto_segment_missed() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_ks1".into(),
                name: "knowledge_search".into(),
                arguments: r#"{"query":"预算下限"}"#.into(),
            }],
        ),
        ScriptedTurn::text("下限为每秒18升。"),
    ]));
    let mut settings = Settings::defaults_configured();
    settings.default_backend = "sidecar".into();
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let space = rt
        .knowledge_create_local_space("local-user", "设备资料", "local_only")
        .unwrap();
    let (doc, version) = rt
        .knowledge_import_document(
            "local",
            "local",
            &space.id,
            "local-user",
            "manual",
            "pump",
            "A-3水泵的预算下限为每秒18升。",
            256,
        )
        .unwrap();
    rt.knowledge_publish_version(
        "local",
        "local",
        &space.id,
        "local-user",
        &doc.id,
        &version.id,
    )
    .unwrap();
    let sid = rt.create_session(Some("按需检索")).unwrap();
    rt.session_replace_bindings(&sid, "knowledge", &[space.id.clone()])
        .unwrap();
    rt.prompt(PromptRequest::new(sid, "帮我查点东西"))
        .await
        .expect("turn");
    let offered = provider.last_tool_names();
    assert!(
        offered.iter().any(|n| n == "knowledge_search"),
        "a bound Auto turn must OFFER the tool: {offered:?}"
    );
    let joined = format!("{:?}", provider.last_messages());
    assert!(
        joined.contains("每秒18升"),
        "tool result must carry the evidence the auto pass missed: {joined}"
    );
    assert!(joined.contains("资料片段"), "{joined}");
    assert!(joined.contains("设备资料"), "hits name their space: {joined}");
}

/// The tool is the same closure as the auto segment, so on a CLOUD model it
/// cannot leak a restricted space either. The question misses everything;
/// the tool query digs exactly where the policy forbids ("机密代号" is
/// unique to the local_only space). Through the tool, the restricted space
/// is still named without content, and its body never reaches the provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn knowledge_search_tool_honors_per_space_policy_on_cloud() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_ks2".into(),
                name: "knowledge_search".into(),
                arguments: r#"{"query":"机密代号"}"#.into(),
            }],
        ),
        ScriptedTurn::text("收到。"),
    ]));
    // Default backend: this turn goes to the CLOUD model.
    let rt = runtime_with(dir.path(), provider.clone(), Settings::defaults_configured());
    let open = rt
        .knowledge_create_local_space("local-user", "公开资料", "allow_summary")
        .unwrap();
    let closed = rt
        .knowledge_create_local_space("local-user", "私有资料", "local_only")
        .unwrap();
    for (space, body, source) in [
        (&open, "报价审批阈值为一万元，需总监复核。", "quote"),
        (&closed, "内部机密代号是青石计划。", "secret"),
    ] {
        let (doc, version) = rt
            .knowledge_import_document(
                "local",
                "local",
                &space.id,
                "local-user",
                "manual",
                source,
                body,
                256,
            )
            .unwrap();
        rt.knowledge_publish_version(
            "local",
            "local",
            &space.id,
            "local-user",
            &doc.id,
            &version.id,
        )
        .unwrap();
    }
    let sid = rt.create_session(Some("云端工具过滤")).unwrap();
    rt.session_replace_bindings(&sid, "knowledge", &[open.id, closed.id.clone()])
        .unwrap();
    rt.prompt(PromptRequest::new(sid, "帮我看个数"))
        .await
        .expect("cloud turn must not be refused wholesale");
    let offered = provider.last_tool_names();
    assert!(
        offered.iter().any(|n| n == "knowledge_search"),
        "bound Auto turn offers the tool even on cloud: {offered:?}"
    );
    let joined = format!("{:?}", provider.last_messages());
    // The question missed, so this marker can only be the tool's own output:
    // the restricted space is NAMED, its content withheld.
    assert!(joined.contains("「私有资料」受外部策略限制"), "{joined}");
    assert!(
        !joined.contains("青石计划"),
        "restricted content must never ride the tool path: {joined}"
    );
}

/// Auto and Plan discover local knowledge independently of session bindings.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn knowledge_search_is_discovered_without_bindings_in_auto_and_plan() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("ok"),
        ScriptedTurn::text("ok"),
    ]));
    let mut settings = Settings::defaults_configured();
    settings.default_backend = "sidecar".into();
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let space = rt
        .knowledge_create_local_space("local-user", "绑定库", "local_only")
        .unwrap();
    // Auto, no binding: discover the local knowledge capability.
    let bare = rt.create_session(Some("无绑定")).unwrap();
    rt.prompt(PromptRequest::new(bare, "你好")).await.expect("turn");
    let offered = provider.last_tool_names();
    assert!(
        offered.iter().any(|n| n == "knowledge_search"),
        "unbound Auto turn must discover local knowledge: {offered:?}"
    );
    // Plan can consult the same read-only local knowledge search.
    let plan_sid = rt.create_session(Some("计划模式")).unwrap();
    rt.session_replace_bindings(&plan_sid, "knowledge", &[space.id])
        .unwrap();
    let mut req = PromptRequest::new(plan_sid, "先做个计划");
    req.task_mode = TaskMode::Plan;
    rt.prompt(req).await.expect("plan turn");
    let offered = provider.last_tool_names();
    assert!(
        offered.iter().any(|n| n == "knowledge_search"),
        "Plan mode can search local knowledge read-only: {offered:?}"
    );
}
