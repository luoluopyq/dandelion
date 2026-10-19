use dandelion_core::acp::{SessionState, SessionUpdate};
use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::{ChatMessage, ToolCall};
use dandelion_core::store::Store;
use dandelion_core::{
    PermissionMode, PromptRequest, RecordingHost, Runtime, Settings, SettingsOverlay,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

fn sleep_args(seconds: u32, timeout: Option<u32>) -> String {
    let command = if cfg!(windows) {
        format!("Start-Sleep -Seconds {seconds}")
    } else {
        format!("sleep {seconds}")
    };
    match timeout {
        Some(t) => format!(r#"{{"command":"{command}","timeout":{t}}}"#),
        None => format!(r#"{{"command":"{command}"}}"#),
    }
}

fn runtime(dir: &std::path::Path, provider: ScriptedProvider, mode: PermissionMode) -> Runtime {
    let host = Arc::new(CliHost::with_data_dir(dir.join("data")));
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = mode;
    settings.workspace = Some(dir.to_path_buf());
    settings.terminal_foreground_timeout_sec = 3;
    settings.terminal_foreground_timeout_max_sec = 5;
    Runtime::new_with_provider(host, settings, Arc::new(provider)).expect("runtime")
}

async fn drain_idle(rt: &Runtime, req: PromptRequest) -> Vec<String> {
    let mut rx = rt.subscribe();
    let sid = req.session_id.clone();
    let fut = rt.prompt(req);
    tokio::pin!(fut);
    let mut kinds = Vec::new();
    let mut prompt_done = false;
    loop {
        tokio::select! {
            frame = rx.recv() => {
                let frame = frame.expect("frame");
                assert_eq!(frame.params.session_id, sid);
                kinds.push(match frame.params.update {
                    SessionUpdate::StateUpdate { state: SessionState::Running, .. } => "running".into(),
                    SessionUpdate::StateUpdate { state: SessionState::Queued, .. } => "queued".into(),
                    SessionUpdate::UserMessageChunk { .. } => "user".into(),
                    SessionUpdate::UserImages { .. } => "user_images".into(),
                    SessionUpdate::AgentMessageChunk { .. } => "agent".into(),
                    SessionUpdate::ReasoningDelta { .. } => "reasoning".into(),
                    SessionUpdate::ToolCallProgress { .. } => "tool_progress".into(),
                    SessionUpdate::ToolCallUpdate { update } => {
                        format!("tool:{}", update.title.unwrap_or_default())
                    }
                    SessionUpdate::JobUpdate { phase, .. } => format!("job:{phase}"),
                    SessionUpdate::WikiUpdate { phase, .. } => format!("wiki:{phase}"),
                    SessionUpdate::WikiDraft { .. } => "wiki_draft".into(),
                    SessionUpdate::TerminalUpdate { .. } => "term".into(),
                    SessionUpdate::TerminalOutputChunk { .. } => "term_chunk".into(),
                    SessionUpdate::StateUpdate { state: SessionState::Idle, stop_reason } => {
                        format!("idle:{}", stop_reason.unwrap_or_default())
                    }
                });
                if kinds.last().map(|s| s.starts_with("idle:")).unwrap_or(false) {
                    break;
                }
            }
            res = &mut fut, if !prompt_done => {
                prompt_done = true;
                res.expect("prompt");
            }
        }
    }
    kinds
}

#[tokio::test]
async fn native_run_reports_its_actual_engine_and_model() {
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(dir.path().to_path_buf());
    let rt = Runtime::new_with_provider(host, settings, provider.clone()).unwrap();
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "当前用哪个 Agent？")).await.unwrap();
    let system = provider.last_system().unwrap();
    assert!(system.contains("本轮执行引擎是蒲公英原生 Agent"));
    assert!(system.contains("当前模型配置标识"));
    let export = rt.session_export(&sid).unwrap();
    assert!(export["messages"].as_array().unwrap().iter().any(|row| {
        row["role"] == "assistant" && row["engineId"] == "native"
    }));
}

#[cfg(unix)]
#[tokio::test]
async fn pi_inherited_ask_runs_without_tool_permission() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("fake-pi");
    std::fs::write(&executable, "#!/bin/sh\ncase \" $* \" in *' --no-tools '*) ;; *) exit 87;; esac\nwhile IFS= read -r line; do\n  printf '%s\\n' '{\"id\":\"dandelion-prompt\",\"type\":\"response\",\"success\":true,\"data\":{\"disposition\":\"started\"}}' '{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"delta\":\"Pi 已运行\"}}' '{\"type\":\"agent_settled\",\"aborted\":false}'\ndone\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let rt = runtime(dir.path(), ScriptedProvider::new(vec![]), PermissionMode::Ask);
    rt.agent_plugin_connect("pi", Some(executable)).unwrap();
    rt.agent_plugin_set_model("pi", Some("test/model".into())).unwrap();
    let sid = rt.create_session(None).unwrap();
    rt.session_merge_prefs(&sid, dandelion_core::store::SessionPrefs {
        agent_engine: Some("pi".into()), ..Default::default()
    }).unwrap();
    tokio::time::timeout(Duration::from_secs(5), rt.prompt(PromptRequest::new(sid.clone(), "当前是哪一个 Agent？")))
        .await.expect("Pi RPC fixture should finish").unwrap();
    let export = rt.session_export(&sid).unwrap();
    assert!(export["messages"].as_array().unwrap().iter().any(|row| {
        row["role"] == "assistant" && row["engineId"] == "pi" && row["content"] == "Pi 已运行"
    }));
}

#[cfg(unix)]
#[tokio::test]
async fn claude_code_uses_its_own_configuration_with_native_sidecar_selected() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("fake-claude");
    std::fs::write(&executable, "#!/bin/sh\ncase \" $* \" in *' --print '*) ;; *) exit 87;; esac\ncase \" $* \" in *' --permission-mode dontAsk '*) exit 88;; esac\nwhile IFS= read -r line; do\n  printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Claude Code 已运行\"}]}}' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}'\ndone\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let mut settings = Settings::defaults_configured();
    settings.default_backend = "sidecar".into();
    settings.workspace = Some(dir.path().to_path_buf());
    settings.permission_mode = PermissionMode::Ask;
    let rt = Runtime::new_with_provider(host, settings, Arc::new(ScriptedProvider::new(vec![]))).unwrap();
    rt.agent_plugin_connect("claude_code", Some(executable)).unwrap();
    let sid = rt.create_session(None).unwrap();
    rt.session_merge_prefs(&sid, dandelion_core::store::SessionPrefs {
        agent_engine: Some("claude_code".into()), ..Default::default()
    }).unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "测试 Claude Code")).await.unwrap();
    let export = rt.session_export(&sid).unwrap();
    assert!(export["messages"].as_array().unwrap().iter().any(|row| {
        row["role"] == "assistant" && row["engineId"] == "claude_code" && row["content"] == "Claude Code 已运行"
    }));
}

#[cfg(unix)]
#[tokio::test]
async fn claude_code_keeps_one_process_across_conversation_turns() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("fake-claude-stream");
    std::fs::write(&executable, "#!/bin/sh\ncount=0\nwhile IFS= read -r line; do\n  count=$((count + 1))\n  printf '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"turn-%s\"}]}}\\n' \"$count\"\n  printf '%s\\n' '{\"type\":\"result\",\"is_error\":false}'\ndone\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let rt = runtime(dir.path(), ScriptedProvider::new(vec![]), PermissionMode::Ask);
    rt.agent_plugin_connect("claude_code", Some(executable)).unwrap();
    let sid = rt.create_session(None).unwrap();
    rt.session_merge_prefs(&sid, dandelion_core::store::SessionPrefs {
        agent_engine: Some("claude_code".into()), ..Default::default()
    }).unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "first")).await.unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "second")).await.unwrap();
    let export = rt.session_export(&sid).unwrap();
    let responses: Vec<_> = export["messages"].as_array().unwrap().iter()
        .filter(|row| row["role"] == "assistant")
        .map(|row| row["content"].as_str().unwrap().to_owned()).collect();
    assert_eq!(responses, ["turn-1", "turn-2"]);
}

#[cfg(unix)]
#[tokio::test]
async fn claude_code_auth_error_is_not_reported_as_success() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("fake-claude");
    std::fs::write(&executable, "#!/bin/sh\nIFS= read -r line\nprintf '%s\\n' '{\"type\":\"assistant\",\"error\":\"authentication_failed\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Not logged in\"}]}}' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"result\":\"Not logged in · Please run /login\"}'\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let rt = runtime(dir.path(), ScriptedProvider::new(vec![]), PermissionMode::Ask);
    rt.agent_plugin_connect("claude_code", Some(executable)).unwrap();
    let sid = rt.create_session(None).unwrap();
    rt.session_merge_prefs(&sid, dandelion_core::store::SessionPrefs {
        agent_engine: Some("claude_code".into()), ..Default::default()
    }).unwrap();
    let error = rt.prompt(PromptRequest::new(sid.clone(), "测试 Claude Code")).await.unwrap_err();
    assert!(error.to_string().contains("Not logged in"));
    let export = rt.session_export(&sid).unwrap();
    assert!(!export["messages"].as_array().unwrap().iter().any(|row| row["role"] == "assistant"));
}

#[cfg(unix)]
#[tokio::test]
async fn opencode_discovery_from_another_recent_workspace_executes() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let tool_workspace = dir.path().join("tools-workspace");
    let task_workspace = dir.path().join("task-workspace");
    let bin_dir = tool_workspace.join(".benchmarks/tools/node_modules/.bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&task_workspace).unwrap();
    let executable = bin_dir.join("opencode");
    std::fs::write(&executable, "#!/bin/sh\nif [ \"$1\" = serve ]; then\n  shift\n  while [ $# -gt 0 ]; do\n    if [ \"$1\" = --port ]; then printf '%s' \"$2\" > \"$(dirname \"$0\")/server-port\"; fi\n    shift\n  done\n  sleep 30\n  exit\nfi\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"text\",\"sessionID\":\"native-test\",\"part\":{\"text\":\"OpenCode 已运行\"}}'\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let marker = bin_dir.join("server-port");
    let health_server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        for _ in 0..500 {
            if marker.is_file() { break; }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let port: u16 = std::fs::read_to_string(marker).unwrap().parse().unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 1024];
        let _ = stream.read(&mut request);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap();
    });
    let rt = runtime(dir.path(), ScriptedProvider::new(vec![]), PermissionMode::Auto);
    let prior = rt.create_session(None).unwrap();
    rt.session_merge_prefs(&prior, dandelion_core::store::SessionPrefs {
        workspace: Some(tool_workspace), ..Default::default()
    }).unwrap();
    assert!(rt.agent_plugins_list().unwrap().iter().any(|item| item.id == "opencode" && item.ready));
    rt.agent_plugin_set_model("opencode", Some("test/model".into())).unwrap();
    let current = rt.create_session(None).unwrap();
    rt.session_merge_prefs(&current, dandelion_core::store::SessionPrefs {
        agent_engine: Some("opencode".into()),
        workspace: Some(task_workspace),
        ..Default::default()
    }).unwrap();
    rt.prompt(PromptRequest::new(current.clone(), "这次呢？")).await.unwrap();
    health_server.join().unwrap();
    let export = rt.session_export(&current).unwrap();
    assert!(export["messages"].as_array().unwrap().iter().any(|row| {
        row["role"] == "assistant" && row["engineId"] == "opencode" && row["content"] == "OpenCode 已运行"
    }));
}

#[test]
fn settings_unknown_keys_survive_save() {
    let dir = tempfile::tempdir().unwrap();
    let file = json!({
        "dandelion.serve.bind": "127.0.0.1:9",
        "dandelion.sseIdleTimeoutMs": 2222,
        "dandelion.baseUrl": "http://127.0.0.1:9/v1",
        "dandelion.defaultModel": "m"
    });
    let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default()).unwrap();
    assert_eq!(s.sse_idle_timeout_ms, 2222);
    s.save(dir.path()).unwrap();
    let loaded = Settings::load(dir.path(), &SettingsOverlay::default()).unwrap();
    assert_eq!(loaded.sse_idle_timeout_ms, 2222);
    assert_eq!(loaded.serve_bind, "127.0.0.1:9");
    assert_eq!(loaded.max_local_concurrency, 2);
}

#[tokio::test]
async fn overflow_compacts_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(dir.path().to_path_buf());
    settings.compact_keep_turns = 1;
    let rt = Runtime::new_with_provider(
        host,
        settings,
        Arc::new(ScriptedProvider::new(vec![
            ScriptedTurn::text("hello"),
            ScriptedTurn::overflow(),
            ScriptedTurn::text("summary-of-hello"),
            ScriptedTurn::text("recovered"),
        ])),
    )
    .expect("runtime");
    let sid = rt.new_session(None);
    let first = drain_idle(&rt, PromptRequest::new(sid.clone(), "hello")).await;
    assert!(first.iter().any(|k| k == "idle:end_turn"));
    let second = drain_idle(&rt, PromptRequest::new(sid.clone(), "second")).await;
    assert!(
        second.iter().any(|k| k == "agent"),
        "compact notice or recovered text: {second:?}"
    );
    assert!(second.iter().any(|k| k == "idle:end_turn"), "{second:?}");

    let store = Store::open(&rt.info().data_dir, 5000).unwrap();
    let msgs = store.load_openai_messages(&sid).unwrap();
    assert!(
        msgs.len() >= 3,
        "compact user + current user + recovered assistant, got {msgs:?}"
    );
    match &msgs[0] {
        ChatMessage::User { content, .. } => {
            assert!(
                content.contains("[compacted history]") && content.contains("summary-of-hello"),
                "{content}"
            );
        }
        other => panic!("expected compact user, got {other:?}"),
    }
    match &msgs[1] {
        ChatMessage::User { content, .. } => assert_eq!(content, "second"),
        other => panic!("expected current user, got {other:?}"),
    }
    match &msgs[2] {
        ChatMessage::Assistant { content, .. } => {
            assert_eq!(content.as_deref(), Some("recovered"));
        }
        other => panic!("expected recovered assistant, got {other:?}"),
    }
    let cps = store.compaction_checkpoints(&sid).unwrap();
    assert_eq!(cps.len(), 1);
    assert_eq!(cps[0].trigger, "overflow");
}

#[tokio::test]
async fn overflow_without_compact_errors() {
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(dir.path().to_path_buf());
    settings.compact_enabled = false;
    let rt = Runtime::new_with_provider(
        host,
        settings,
        Arc::new(ScriptedProvider::new(vec![
            ScriptedTurn::text("hello"),
            ScriptedTurn::overflow(),
        ])),
    )
    .expect("runtime");
    let sid = rt.new_session(None);
    drain_idle(&rt, PromptRequest::new(sid.clone(), "hello")).await;
    let err = rt
        .prompt(PromptRequest::new(sid, "second"))
        .await
        .expect_err("overflow must surface");
    assert!(err.is_context_overflow(), "{err}");
}

#[tokio::test]
async fn preflight_compacts_before_complete() {
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(dir.path().to_path_buf());
    settings.compact_keep_turns = 1;
    settings.compact_preflight_enabled = true;
    settings.compact_preflight_reserve_tokens = 60_000;
    let rt = Runtime::new_with_provider(
        host,
        settings,
        Arc::new(ScriptedProvider::new(vec![
            ScriptedTurn::text("hello"),
            ScriptedTurn::text("summary-of-hello"),
            ScriptedTurn::text("recovered"),
        ])),
    )
    .expect("runtime");
    let sid = rt.new_session(None);
    drain_idle(&rt, PromptRequest::new(sid.clone(), "hello")).await;
    let kinds = drain_idle(&rt, PromptRequest::new(sid.clone(), "second")).await;
    assert!(kinds.iter().any(|k| k == "agent"), "{kinds:?}");
    assert!(kinds.iter().any(|k| k == "idle:end_turn"), "{kinds:?}");
    let store = Store::open(&rt.info().data_dir, 5000).unwrap();
    let msgs = store.load_openai_messages(&sid).unwrap();
    match &msgs[0] {
        ChatMessage::User { content, .. } => {
            assert!(
                content.contains("[compacted history]") && content.contains("summary-of-hello"),
                "{content}"
            );
        }
        other => panic!("expected compact user, got {other:?}"),
    }
    match &msgs[1] {
        ChatMessage::User { content, .. } => assert_eq!(content, "second"),
        other => panic!("expected current user, got {other:?}"),
    }
    match &msgs[2] {
        ChatMessage::Assistant { content, .. } => {
            assert_eq!(content.as_deref(), Some("recovered"));
        }
        other => panic!("expected recovered assistant, got {other:?}"),
    }
    let cps = store.compaction_checkpoints(&sid).unwrap();
    assert_eq!(cps.len(), 1);
    assert_eq!(cps[0].trigger, "preflight");
}

#[tokio::test]
async fn preflight_off_skips_compact() {
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(dir.path().to_path_buf());
    settings.compact_keep_turns = 1;
    settings.compact_preflight_enabled = false;
    let rt = Runtime::new_with_provider(
        host,
        settings,
        Arc::new(ScriptedProvider::new(vec![
            ScriptedTurn::text("hello"),
            ScriptedTurn::text("plain-second"),
        ])),
    )
    .expect("runtime");
    let sid = rt.new_session(None);
    drain_idle(&rt, PromptRequest::new(sid.clone(), "hello")).await;
    drain_idle(&rt, PromptRequest::new(sid.clone(), "second")).await;
    let store = Store::open(&rt.info().data_dir, 5000).unwrap();
    let msgs = store.load_openai_messages(&sid).unwrap();
    let compact_users = msgs.iter().filter(|m| matches!(m, ChatMessage::User { content, .. } if content.contains("[compacted history]"))).count();
    assert_eq!(compact_users, 0, "{msgs:?}");
}

#[tokio::test]
async fn runtime_compact_rewrites_without_new_user_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(dir.path().to_path_buf());
    settings.compact_keep_turns = 1;
    let rt = Runtime::new_with_provider(
        host,
        settings,
        Arc::new(ScriptedProvider::new(vec![
            ScriptedTurn::text("hello"),
            ScriptedTurn::text("second-reply"),
            ScriptedTurn::text("summary-of-hello"),
        ])),
    )
    .expect("runtime");
    let sid = rt.new_session(None);
    drain_idle(&rt, PromptRequest::new(sid.clone(), "hello")).await;
    drain_idle(&rt, PromptRequest::new(sid.clone(), "second")).await;
    rt.compact(&sid).await.expect("compact");
    let store = Store::open(&rt.info().data_dir, 5000).unwrap();
    let msgs = store.load_openai_messages(&sid).unwrap();
    match &msgs[0] {
        ChatMessage::User { content, .. } => {
            assert!(
                content.contains("[compacted history]") && content.contains("summary-of-hello"),
                "{content}"
            );
        }
        other => panic!("expected compact user, got {other:?}"),
    }
    assert!(
        !msgs
            .iter()
            .any(|m| matches!(m, ChatMessage::User { content, .. } if content == "compact")),
        "{msgs:?}"
    );

    // The compaction left a manual checkpoint and the full history is intact.
    let cps = store.compaction_checkpoints(&sid).unwrap();
    assert_eq!(cps.len(), 1);
    assert_eq!(cps[0].trigger, "manual");
    assert!(!cps[0].source_truncated);
    let history = rt.session_history_window(&sid, 0, 50).unwrap();
    let texts: Vec<(&str, &str)> = history["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["source"] != "checkpoint")
        .map(|m| (m["role"].as_str().unwrap(), m["content"].as_str().unwrap()))
        .collect();
    assert_eq!(
        texts,
        vec![
            ("user", "hello"),
            ("assistant", "hello"),
            ("user", "second"),
            ("assistant", "second-reply"),
        ]
    );
}

#[tokio::test]
async fn compact_disabled_rejects_command_and_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(dir.path().to_path_buf());
    settings.compact_enabled = false;
    settings.compact_preflight_enabled = true;
    settings.compact_preflight_reserve_tokens = 60_000;
    settings.compact_keep_turns = 1;
    let rt = Runtime::new_with_provider(
        host,
        settings,
        Arc::new(ScriptedProvider::new(vec![
            ScriptedTurn::text("hello"),
            ScriptedTurn::text("still-there"),
        ])),
    )
    .expect("runtime");
    let sid = rt.new_session(None);
    drain_idle(&rt, PromptRequest::new(sid.clone(), "hello")).await;
    let err = rt.compact(&sid).await.expect_err("disabled");
    assert!(err.to_string().contains("compact.enabled"), "{err}");
    drain_idle(&rt, PromptRequest::new(sid.clone(), "second")).await;
    let store = Store::open(&rt.info().data_dir, 5000).unwrap();
    let msgs = store.load_openai_messages(&sid).unwrap();
    assert!(
        !msgs.iter().any(|m| matches!(m, ChatMessage::User { content, .. } if content.contains("[compacted history]"))),
        "{msgs:?}"
    );
}

#[tokio::test]
async fn loop_read_file_then_stop() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "hello dandelion\n").unwrap();
    let rt = runtime(
        dir.path(),
        ScriptedProvider::new(vec![
            ScriptedTurn::tools(
                "",
                vec![ToolCall {
                    id: "call_read".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md","limit":10}"#.into(),
                }],
            ),
            ScriptedTurn::text("summary"),
        ]),
        PermissionMode::Auto,
    );
    let sid = rt.new_session(None);
    let kinds = drain_idle(&rt, PromptRequest::new(sid, "read it")).await;
    assert!(kinds.iter().any(|k| k == "tool:read_file"));
    assert_eq!(kinds.last().unwrap(), "idle:end_turn");
}

#[tokio::test]
async fn independent_reads_persist_in_tool_call_order() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha.txt"), "alpha-token-111\n").unwrap();
    std::fs::write(dir.path().join("beta.txt"), "beta-token-222\n").unwrap();
    let rt = runtime(
        dir.path(),
        ScriptedProvider::new(vec![
            ScriptedTurn::tools(
                "",
                vec![
                    ToolCall {
                        id: "call_alpha".into(),
                        name: "read_file".into(),
                        arguments: r#"{"path":"alpha.txt"}"#.into(),
                    },
                    ToolCall {
                        id: "call_beta".into(),
                        name: "read_file".into(),
                        arguments: r#"{"path":"beta.txt"}"#.into(),
                    },
                ],
            ),
            ScriptedTurn::text("both files"),
        ]),
        PermissionMode::Auto,
    );
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "read both"))
        .await
        .unwrap();
    let store = Store::open(&rt.info().data_dir, 5000).unwrap();
    let msgs = store.load_openai_messages(&sid).unwrap();
    let tools: Vec<_> = msgs
        .iter()
        .filter_map(|m| match m {
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => Some((tool_call_id.as_str(), content.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].0, "call_alpha");
    assert_eq!(tools[1].0, "call_beta");
    assert!(tools[0].1.contains("alpha-token-111"), "{}", tools[0].1);
    assert!(tools[1].1.contains("beta-token-222"), "{}", tools[1].1);
    assert!(
        !tools[0].1.starts_with("Error:"),
        "alpha read failed: {}",
        tools[0].1
    );
    assert!(
        !tools[1].1.starts_with("Error:"),
        "beta read failed: {}",
        tools[1].1
    );
}

#[tokio::test]
async fn sqlite_restore_across_runtimes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("note.txt"), "secret-token-xyz").unwrap();
    let data = dir.path().join("data");
    let sid = {
        let rt = runtime(
            dir.path(),
            ScriptedProvider::new(vec![
                ScriptedTurn::tools(
                    "",
                    vec![ToolCall {
                        id: "call_read".into(),
                        name: "read_file".into(),
                        arguments: r#"{"path":"note.txt"}"#.into(),
                    }],
                ),
                ScriptedTurn::text("read note.txt"),
            ]),
            PermissionMode::Auto,
        );
        let sid = rt.new_session(None);
        rt.prompt(PromptRequest::new(sid.clone(), "read the note"))
            .await
            .unwrap();
        sid
    };
    let host = Arc::new(CliHost::with_data_dir(data));
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(dir.path().to_path_buf());
    let rt2 = Runtime::new_with_provider(
        host,
        settings,
        Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text(
            "you asked me to read note.txt which contained secret-token-xyz",
        )])),
    )
    .unwrap();
    rt2.prompt(PromptRequest::new(sid, "what file did I ask you to read?"))
        .await
        .unwrap();
}

#[cfg(any(windows, unix))]
#[tokio::test]
async fn terminal_echo_dandelion() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(
        dir.path(),
        ScriptedProvider::new(vec![
            ScriptedTurn::tools(
                "",
                vec![ToolCall {
                    id: "call_sh".into(),
                    name: "terminal".into(),
                    arguments: r#"{"command":"echo Dandelion"}"#.into(),
                }],
            ),
            ScriptedTurn::text("ok"),
        ]),
        PermissionMode::Auto,
    );
    let sid = rt.new_session(None);
    let mut rx = rt.subscribe();
    rt.prompt(PromptRequest::new(sid, "echo")).await.unwrap();
    let mut saw = false;
    while let Ok(frame) = rx.try_recv() {
        if let SessionUpdate::ToolCallUpdate { update } = frame.params.update {
            if let Some(content) = update.content {
                let text = format!("{content:?}");
                if text.contains("Dandelion") {
                    saw = true;
                }
            }
        }
    }
    assert!(saw, "terminal output should contain Dandelion");
}

#[cfg(any(windows, unix))]
#[tokio::test]
async fn terminal_timeout_kills_sleep() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(
        dir.path(),
        ScriptedProvider::new(vec![
            ScriptedTurn::tools(
                "",
                vec![ToolCall {
                    id: "call_sleep".into(),
                    name: "terminal".into(),
                    arguments: sleep_args(20, Some(1)),
                }],
            ),
            ScriptedTurn::text("timed out"),
        ]),
        PermissionMode::Auto,
    );
    let sid = rt.new_session(None);
    let mut rx = rt.subscribe();
    rt.prompt(PromptRequest::new(sid, "sleep")).await.unwrap();
    let mut saw_timeout = false;
    while let Ok(frame) = rx.try_recv() {
        if let SessionUpdate::ToolCallUpdate { update } = frame.params.update {
            if let Some(content) = update.content {
                let text = format!("{content:?}");
                if text.contains("timeout") {
                    saw_timeout = true;
                }
            }
        }
    }
    assert!(saw_timeout);
}

#[cfg(any(windows, unix))]
#[tokio::test]
async fn cancel_kills_foreground_sleep() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(
        dir.path(),
        ScriptedProvider::new(vec![ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_sleep".into(),
                name: "terminal".into(),
                arguments: sleep_args(20, Some(5)),
            }],
        )]),
        PermissionMode::Auto,
    );
    let sid = rt.new_session(None);
    let mut rx = rt.subscribe();
    let rt2 = rt.clone();
    let sid2 = sid.clone();
    let handle = tokio::spawn(async move { rt2.prompt(PromptRequest::new(sid2, "sleep")).await });
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(8) {
            panic!("never started tool");
        }
        if let Ok(frame) = rx.try_recv() {
            if let SessionUpdate::ToolCallUpdate { update } = frame.params.update {
                if update.title.as_deref() == Some("terminal") {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    break;
                }
            }
        } else {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    rt.cancel(sid.clone()).await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(8), handle)
        .await
        .expect("join")
        .expect("task");
    assert!(outcome.is_ok());
    let mut idle = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        while let Ok(frame) = rx.try_recv() {
            if let SessionUpdate::StateUpdate {
                state: SessionState::Idle,
                stop_reason,
            } = frame.params.update
            {
                idle = stop_reason;
            }
        }
        if idle.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(idle.as_deref(), Some("cancelled"));
}

#[tokio::test]
async fn system_path_read_returns_error_tool() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(
        dir.path(),
        ScriptedProvider::new(vec![
            ScriptedTurn::tools(
                "",
                vec![ToolCall {
                    id: "call_hosts".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"C:\\Windows\\System32\\drivers\\etc\\hosts"}"#.into(),
                }],
            ),
            ScriptedTurn::text("blocked"),
        ]),
        PermissionMode::Auto,
    );
    let sid = rt.new_session(None);
    let mut rx = rt.subscribe();
    rt.prompt(PromptRequest::new(sid, "read hosts"))
        .await
        .unwrap();
    let mut failed = false;
    while let Ok(frame) = rx.try_recv() {
        if let SessionUpdate::ToolCallUpdate { update } = frame.params.update {
            if let Some(content) = update.content {
                let text = format!("{content:?}");
                if text.contains("Error:") {
                    failed = true;
                }
            }
        }
    }
    assert!(failed);
}

#[tokio::test]
async fn cancel_during_length_continue_emits_idle() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(
        dir.path(),
        ScriptedProvider::new(vec![
            ScriptedTurn::truncated("partial answer"),
            ScriptedTurn::hang_until_cancel(),
        ]),
        PermissionMode::Auto,
    );
    let sid = rt.new_session(None);
    let mut rx = rt.subscribe();
    let rt2 = rt.clone();
    let sid2 = sid.clone();
    let handle = tokio::spawn(async move { rt2.prompt(PromptRequest::new(sid2, "go")).await });
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(5) {
            panic!("never streamed truncated text");
        }
        if let Ok(frame) = rx.try_recv() {
            if matches!(frame.params.update, SessionUpdate::AgentMessageChunk { .. }) {
                break;
            }
        } else {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    rt.cancel(sid.clone()).await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("join")
        .expect("task");
    assert!(outcome.is_ok(), "{outcome:?}");
    let mut idle = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        while let Ok(frame) = rx.try_recv() {
            if let SessionUpdate::StateUpdate {
                state: SessionState::Idle,
                stop_reason,
            } = frame.params.update
            {
                idle = stop_reason;
            }
        }
        if idle.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(idle.as_deref(), Some("cancelled"));
}

fn assert_tool_results_pair_calls(msgs: &[ChatMessage]) {
    let mut pending: Vec<String> = Vec::new();
    for msg in msgs {
        match msg {
            ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                pending.extend(tool_calls.iter().map(|c| c.id.clone()));
            }
            ChatMessage::Tool { tool_call_id, .. } => {
                let idx = pending
                    .iter()
                    .position(|id| id == tool_call_id)
                    .unwrap_or_else(|| panic!("orphan tool result {tool_call_id}"));
                pending.remove(idx);
            }
            _ => {}
        }
    }
    assert!(
        pending.is_empty(),
        "unpaired tool_calls after cancel: {pending:?}"
    );
}

#[tokio::test]
async fn cancel_mid_batch_pairs_remaining_tool_results() {
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(RecordingHost::new(dir.path().join("data")).hang_on_permission());
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Ask;
    settings.workspace = Some(dir.path().to_path_buf());
    let rt = Runtime::new_with_provider(
        host,
        settings,
        Arc::new(ScriptedProvider::new(vec![ScriptedTurn::tools(
            "",
            vec![
                ToolCall {
                    id: "call_write".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"out.txt","content":"x"}"#.into(),
                },
                ToolCall {
                    id: "call_read".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"out.txt"}"#.into(),
                },
            ],
        )])),
    )
    .expect("runtime");
    let sid = rt.new_session(None);
    let mut rx = rt.subscribe();
    let rt2 = rt.clone();
    let sid2 = sid.clone();
    let handle = tokio::spawn(async move {
        rt2.prompt(PromptRequest::new(sid2, "write then read"))
            .await
    });
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(5) {
            panic!("never saw write_file pending");
        }
        if let Ok(frame) = rx.try_recv() {
            if let SessionUpdate::ToolCallUpdate { update } = frame.params.update {
                if update.title.as_deref() == Some("write_file") {
                    break;
                }
            }
        } else {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    rt.cancel(sid.clone()).await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("join")
        .expect("task");
    assert!(outcome.is_ok(), "{outcome:?}");

    let store = Store::open(&rt.info().data_dir, 5000).unwrap();
    let msgs = store.load_openai_messages(&sid).unwrap();
    assert_tool_results_pair_calls(&msgs);
    let tools: Vec<_> = msgs
        .iter()
        .filter_map(|m| match m {
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => Some((tool_call_id.as_str(), content.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools,
        vec![
            ("call_write", "Error: cancelled"),
            ("call_read", "Error: cancelled"),
        ]
    );
    assert!(
        !dir.path().join("out.txt").exists(),
        "cancelled write_file must not create the file"
    );
}
