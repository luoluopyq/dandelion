use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::ToolCall;
use dandelion_core::tools::ToolRegistry;
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings, TaskMode};
use std::fs;
use std::sync::Arc;

fn runtime_with(
    dir: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    mut settings: Settings,
) -> Runtime {
    let host = Arc::new(CliHost::with_data_dir(dir.join("data")));
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(dir.to_path_buf());
    if settings.base_url.is_none() {
        settings.base_url = Some("http://127.0.0.1:9/v1".into());
    }
    if settings.default_model.is_none() {
        settings.default_model = Some("test-model".into());
    }
    Runtime::new_with_provider(host, settings, provider).expect("runtime")
}

#[tokio::test]
async fn hook_deny_write_file_does_not_touch_disk() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nope.txt");
    let mut settings = Settings::defaults_configured();
    settings.hooks_enabled = vec!["deny_named_tool".into()];
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_w".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"nope.txt","content":"secret"}"#.into(),
            }],
        ),
        ScriptedTurn::text("done"),
    ]));
    let rt = runtime_with(dir.path(), provider, settings);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "write"))
        .await
        .expect("prompt");
    assert!(!target.exists());
    let store = dandelion_core::store::Store::open(&dir.path().join("data"), 5000).unwrap();
    let msgs = store.load_openai_messages(&sid).unwrap();
    assert!(
        msgs.iter().any(|m| match m {
            dandelion_core::provider::types::ChatMessage::Tool { content, .. } => {
                content.contains("Error: hook denied:")
            }
            _ => false,
        }),
        "model must see hook deny"
    );
}

#[tokio::test]
async fn selected_skill_injects_body_without_leaking_to_other_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(data.join("skills/demo-skill")).unwrap();
    fs::write(
        data.join("skills/demo-skill/SKILL.md"),
        "---\nname: demo-skill\ndescription: demo\n---\nUNIQUE_SKILL_SENTENCE_XYZ\n",
    )
    .unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid.clone(), "have you got the skill?");
    req.skill_ids = vec!["demo-skill".into()];
    rt.prompt(req).await.expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(sys.contains("UNIQUE_SKILL_SENTENCE_XYZ"), "{sys}");

    let provider2 = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt2 = runtime_with(
        dir.path(),
        provider2.clone(),
        Settings::defaults_configured(),
    );
    let sid2 = rt2.new_session(None);
    rt2.prompt(PromptRequest::new(sid2, "have you got the skill?"))
        .await
        .expect("prompt");
    let sys2 = provider2.last_system().expect("system");
    assert!(!sys2.contains("UNIQUE_SKILL_SENTENCE_XYZ"), "{sys2}");
}

#[tokio::test]
async fn selected_skill_in_ask_mode_has_no_web_or_file_tools() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(data.join("skills/sql-query-generator")).unwrap();
    fs::write(
        data.join("skills/sql-query-generator/SKILL.md"),
        "---\nname: sql-query-generator\ndescription: SQLite query\n---\nSELECT_ONLY_SKILL\n",
    ).unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("SELECT 1;")]));
    let rt = runtime_with(dir.path(), provider.clone(), Settings::defaults_configured());
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "按表结构生成 SELECT");
    req.task_mode = TaskMode::Ask;
    req.skill_ids = vec!["sql-query-generator".into()];
    rt.prompt(req).await.expect("ask with skill");
    assert!(provider.last_system().unwrap().contains("SELECT_ONLY_SKILL"));
    assert!(provider.last_tool_names().is_empty());
}

#[tokio::test]
async fn context_file_injects_unique_sentence() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(".dandelion.md"),
        "UNIQUE_DANDELION_SENTENCE_QQQ\n",
    )
    .unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "quote project notes"))
        .await
        .expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(sys.contains("UNIQUE_DANDELION_SENTENCE_QQQ"), "{sys}");
}

#[tokio::test]
async fn developer_agents_md_not_injected() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("AGENTS.md"),
        "This file is for developers of Dandelion, not a runtime skill injected into dln.\nDEV_ONLY_MARKER_ZZZ\n",
    )
    .unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "quote agents"))
        .await
        .expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(!sys.contains("DEV_ONLY_MARKER_ZZZ"), "{sys}");
}

#[tokio::test]
async fn skill_over_cap_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(data.join("skills/huge-skill")).unwrap();
    let body = "H".repeat(4000);
    fs::write(
        data.join("skills/huge-skill/SKILL.md"),
        format!("---\nname: huge-skill\ndescription: huge\n---\n{body}\n"),
    )
    .unwrap();
    let mut settings = Settings::defaults_configured();
    settings.skills_max_inject_bytes = 1024;
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "skill?");
    req.skill_ids = vec!["huge-skill".into()];
    rt.prompt(req).await.expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(!sys.contains(&body), "huge skill should be skipped");
}

#[tokio::test]
async fn mcp_bad_command_keeps_builtins() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = Settings::defaults_configured();
    settings.mcp_servers = vec![dandelion_core::McpServerConfig {
        id: "echo".into(),
        enabled: true,
        command: "dandelion-mcp-does-not-exist-xyz".into(),
        ..Default::default()
    }];
    settings.mcp_spawn_timeout_ms = 2_000;
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let list = rt.mcp_list().await.expect("list");
    assert_eq!(list.len(), 1);
    assert!(!list[0].ready);
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "tools?");
    req.mcp_server_ids = vec!["echo".into()];
    rt.prompt(req).await.expect("prompt");
    let names = provider.last_tool_names();
    assert!(names.contains(&"read_file".to_string()), "{names:?}");
    assert!(names.iter().all(|n| !n.starts_with("mcp__")), "{names:?}");
}

#[tokio::test]
async fn mcp_bad_remote_url_keeps_builtins() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = Settings::defaults_configured();
    settings.mcp_spawn_timeout_ms = 2_000;
    settings.mcp_servers = vec![dandelion_core::McpServerConfig {
        id: "jira".into(),
        enabled: true,
        url: Some("http://127.0.0.1:1/mcp".into()),
        transport: "sse".into(),
        ..Default::default()
    }];
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let list = rt.mcp_list().await.expect("list");
    assert_eq!(list.len(), 1);
    assert!(!list[0].ready);
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "tools?");
    req.mcp_server_ids = vec!["jira".into()];
    rt.prompt(req).await.expect("prompt");
    let names = provider.last_tool_names();
    assert!(names.contains(&"read_file".to_string()), "{names:?}");
    assert!(names.iter().all(|n| !n.starts_with("mcp__")), "{names:?}");
}

#[test]
fn for_session_adds_todo_g1_stays_six() {
    let s = Settings::defaults();
    let a = ToolRegistry::g1(&s).names();
    let b = ToolRegistry::for_session(&s, &[], Vec::new()).names();
    assert_eq!(
        a,
        vec![
            "read_file",
            "write_file",
            "patch",
            "search_files",
            "terminal",
            "process"
        ]
    );
    assert!(b.contains(&"todo".to_string()), "{b:?}");
    assert!(b.contains(&"web_search".to_string()), "{b:?}");
    assert!(b.contains(&"web_extract".to_string()), "{b:?}");
    // memory_propose joins the session extras (memory kill-switch default-on);
    // it proposes weight-0 pending candidates and needs no permission gate.
    assert!(b.contains(&"memory_propose".to_string()), "{b:?}");
    // wiki_propose joins too (default-on): "propose" lands pending and is
    // ungated; only mode:"write" goes through gate 2 (permission::needs_gate2).
    assert!(b.contains(&"wiki_propose".to_string()), "{b:?}");
    assert_eq!(b.len(), 11);
}

#[tokio::test]
async fn available_skills_need_selection_and_framework_is_never_injected() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(data.join("skills/demo-skill")).unwrap();
    fs::write(
        data.join("skills/demo-skill/SKILL.md"),
        "---\nname: demo-skill\ndescription: demo\n---\nVISIBLE_SKILL\n",
    )
    .unwrap();
    fs::create_dir_all(data.join("skills/_framework/wiki")).unwrap();
    fs::write(
        data.join("skills/_framework/wiki/SKILL.md"),
        "---\nname: wiki\ndescription: fw\n---\nFRAMEWORK_SECRET\n",
    )
    .unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let listed = rt.skills_list(Some(dir.path())).unwrap();
    assert!(listed.iter().any(|i| i.id == "demo-skill" && !i.framework));
    assert!(listed.iter().any(|i| i.id == "wiki" && i.framework));
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "帮我处理这个任务"))
        .await
        .expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(!sys.contains("VISIBLE_SKILL"), "{sys}");
    assert!(!sys.contains("FRAMEWORK_SECRET"), "{sys}");
}

#[tokio::test]
async fn selected_skills_persist_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(data.join("skills/demo-skill")).unwrap();
    fs::write(
        data.join("skills/demo-skill/SKILL.md"),
        "---\nname: demo-skill\ndescription: demo\n---\nINHERIT_SKILL_BODY\n",
    )
    .unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("first"),
        ScriptedTurn::text("second"),
    ]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let sid = rt.new_session(None);
    let mut first = PromptRequest::new(sid.clone(), "one");
    first.skill_ids = vec!["demo-skill".into()];
    first.persist_bindings = true;
    rt.prompt(first).await.expect("first");
    rt.prompt(PromptRequest::new(sid, "two"))
        .await
        .expect("second");
    let sys = provider.last_system().expect("system");
    assert!(sys.contains("INHERIT_SKILL_BODY"), "{sys}");
}

#[tokio::test]
async fn context_disabled_skips_project_segment() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".dandelion.md"), "SHOULD_NOT_APPEAR_CTX\n").unwrap();
    let mut settings = Settings::defaults_configured();
    settings.context_enabled = false;
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "ctx?"))
        .await
        .expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(!sys.contains("SHOULD_NOT_APPEAR_CTX"), "{sys}");
}

#[tokio::test]
async fn progressive_context_appended_to_tool_result() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".dandelion.md"), "ROOT_ONLY\n").unwrap();
    let pkg = dir.path().join("pkg");
    fs::create_dir(&pkg).unwrap();
    fs::write(pkg.join(".dandelion.md"), "NESTED_PROGRESSIVE_UNIQUE\n").unwrap();
    fs::write(pkg.join("note.txt"), "hello nested\n").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_r".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"pkg/note.txt"}"#.into(),
            }],
        ),
        ScriptedTurn::text("done"),
    ]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "read nested"))
        .await
        .expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(sys.contains("ROOT_ONLY"), "{sys}");
    assert!(!sys.contains("NESTED_PROGRESSIVE_UNIQUE"), "{sys}");
    let store = dandelion_core::store::Store::open(&dir.path().join("data"), 5000).unwrap();
    let msgs = store.load_openai_messages(&sid).unwrap();
    let tool = msgs.iter().find_map(|m| match m {
        dandelion_core::provider::types::ChatMessage::Tool { content, .. } => Some(content),
        _ => None,
    });
    let content = tool.expect("tool row");
    assert!(content.contains("NESTED_PROGRESSIVE_UNIQUE"), "{content}");
}

#[tokio::test]
async fn enabled_mcp_is_considered_without_prompt_keyword() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = Settings::defaults_configured();
    settings.mcp_servers = vec![dandelion_core::McpServerConfig {
        id: "echo".into(),
        enabled: true,
        command: "dandelion-mcp-does-not-exist-xyz".into(),
        ..Default::default()
    }];
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "tools?"))
        .await
        .expect("prompt");
    let names = provider.last_tool_names();
    assert!(names.iter().all(|n| !n.starts_with("mcp__")), "{names:?}");
}

#[tokio::test]
async fn workspace_skill_overrides_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(data.join("skills/demo-skill")).unwrap();
    fs::write(
        data.join("skills/demo-skill/SKILL.md"),
        "---\nname: demo-skill\ndescription: global\n---\nGLOBAL_SKILL_BODY\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join(".dandelion/skills/demo-skill")).unwrap();
    fs::write(
        dir.path().join(".dandelion/skills/demo-skill/SKILL.md"),
        "---\nname: demo-skill\ndescription: local\n---\nLOCAL_SKILL_BODY\n",
    )
    .unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "skill?");
    req.skill_ids = vec!["demo-skill".into()];
    rt.prompt(req).await.expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(sys.contains("LOCAL_SKILL_BODY"), "{sys}");
    assert!(!sys.contains("GLOBAL_SKILL_BODY"), "{sys}");
}

#[tokio::test]
async fn unbound_skill_is_not_injected_on_a_later_turn() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(data.join("skills/demo-skill")).unwrap();
    fs::write(
        data.join("skills/demo-skill/SKILL.md"),
        "---\nname: demo-skill\ndescription: demo\n---\nNO_PERSIST_SKILL\n",
    )
    .unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("first"),
        ScriptedTurn::text("second"),
    ]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let sid = rt.new_session(None);
    let mut first = PromptRequest::new(sid.clone(), "one");
    first.skill_ids = vec!["demo-skill".into()];
    first.persist_bindings = false;
    rt.prompt(first).await.expect("first");
    rt.prompt(PromptRequest::new(sid, "two"))
        .await
        .expect("second");
    let sys = provider.last_system().expect("system");
    assert!(!sys.contains("NO_PERSIST_SKILL"), "{sys}");
}

#[tokio::test]
async fn unknown_skill_id_omitted_invalid_path_errors() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let sid = rt.new_session(None);
    let mut missing = PromptRequest::new(sid.clone(), "missing?");
    missing.skill_ids = vec!["missing-skill".into()];
    rt.prompt(missing).await.expect("unknown id is omitted");
    let sys = provider.last_system().expect("system");
    assert!(!sys.contains("已启用技能全文"), "{sys}");

    let mut bad = PromptRequest::new(sid, "bad?");
    bad.skill_ids = vec!["Not-A-Path".into()];
    assert!(rt.prompt(bad).await.is_err());
}

#[tokio::test]
async fn git_root_context_reaches_nested_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let _ = std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output();
    fs::write(
        dir.path().join(".dandelion.md"),
        "GIT_ROOT_RUNTIME_UNIQUE\n",
    )
    .unwrap();
    let ws = dir.path().join("pkg");
    fs::create_dir(&ws).unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(&ws, provider.clone(), Settings::defaults_configured());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "ctx?"))
        .await
        .expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(sys.contains("GIT_ROOT_RUNTIME_UNIQUE"), "{sys}");
}

#[tokio::test]
async fn hook_deny_skips_permission_host() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dandelion_core::host::RecordingHost::new(dir.path().join("data"));
    let host = Arc::new(rec.clone());
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Ask;
    settings.hooks_enabled = vec!["deny_named_tool".into()];
    settings.workspace = Some(dir.path().to_path_buf());
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_w".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"nope.txt","content":"secret"}"#.into(),
            }],
        ),
        ScriptedTurn::text("done"),
    ]));
    let rt = Runtime::new_with_provider(host, settings, provider).expect("runtime");
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "write"))
        .await
        .expect("prompt");
    assert!(rec.call_titles().is_empty());
    assert!(!dir.path().join("nope.txt").exists());
}
