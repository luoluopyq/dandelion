//! G3 offline gates (`docs/g3-harness.md` §10).
//!
//! | §10 | test |
//! |-----|------|
//! | 1+11 Park 树 / 父不空转 | `c6_park_tree_and_no_empty_parent_complete` |
//! | 2 不泄漏 MCP/shell | `c6_child_does_not_leak_mcp_or_shell` |
//! | 3 深度 | `c6_depth_limit_omits_delegate_on_child` |
//! | 4 取消杀树 | `c6_parent_cancel_kills_child_tree` |
//! | 5 并发帽 | `c6_tasks_over_cap_errors` |
//! | 6 Cron | `c7_due_job_fires_session_and_bus` |
//! | 7 wiki | `c8_wiki_not_in_system_and_unread` |
//! | 8 soul | `c8b_soul_replaces_identity_and_can_disable` |
//! | 9 会话 | `c9_list_search_export` |
//! | 10 门 3 | `c10_deny_and_allow_and_gate1` |
//! | 12 与读并行 | `c6_read_and_delegate_overlap` |
//! | 13 摘要帽 | `c6_summary_cap_truncated` |
//! | 14 心跳 | `c6_stale_idle_cancels_child` |

use dandelion_core::acp::{SessionState, SessionUpdate, ToolCallStatus};
use dandelion_core::agent_loop::prompt::DEFAULT_IDENTITY;
use dandelion_core::config::PermissionRules;
use dandelion_core::host::{CliHost, RecordingHost};
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::{ChatMessage, ToolCall};
use dandelion_core::store::Store;
use dandelion_core::{
    McpServerConfig, PermissionMode, PromptRequest, Runtime, SessionId, Settings,
};
use std::fs;
use std::sync::Arc;
use std::time::Duration;

fn sleep_args(seconds: u32) -> String {
    let command = if cfg!(windows) {
        format!("Start-Sleep -Seconds {seconds}")
    } else {
        format!("sleep {seconds}")
    };
    format!(r#"{{"command":"{command}","timeout":{seconds}}}"#)
}

fn delegate(id: &str, goal: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "delegate_task".into(),
        arguments: serde_json::json!({ "goal": goal }).to_string(),
    }
}

fn settings_auto() -> Settings {
    let mut s = Settings::defaults_configured();
    s.permission_mode = PermissionMode::Auto;
    s.cron_tick_sec = 3_600;
    s
}

fn runtime_with(
    dir: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    mut settings: Settings,
) -> Runtime {
    settings.workspace = Some(dir.to_path_buf());
    if settings.permission_mode == PermissionMode::Ask && settings.base_url.is_none() {
        settings.permission_mode = PermissionMode::Ask;
    }
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(dir.join("data"))),
        settings,
        provider,
    )
    .expect("runtime")
}

fn parent_complete_count(provider: &ScriptedProvider) -> usize {
    provider
        .history()
        .iter()
        .filter(|h| h.tool_names.iter().any(|n| n == "delegate_task"))
        .count()
}

fn child_histories(provider: &ScriptedProvider) -> Vec<Vec<String>> {
    provider
        .history()
        .into_iter()
        .filter(|h| !h.tool_names.iter().any(|n| n == "delegate_task"))
        .map(|h| h.tool_names)
        .collect()
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

/// 1 + 11: Park-only delegate; parent does not complete again until the child is idle;
/// first tool row is the summary; child tools[] has no delegate_task (maxDepth=1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c6_park_tree_and_no_empty_parent_complete() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![delegate("call_d", "summarize src")]),
        ScriptedTurn::delayed_text("CHILD_SUMMARY_OK", Duration::from_millis(400)),
        ScriptedTurn::text("parent after park"),
    ]));
    let rt = runtime_with(dir.path(), provider.clone(), settings_auto());
    let sid = rt.new_session(None);
    let handle = {
        let rt = rt.clone();
        let sid = sid.clone();
        tokio::spawn(async move { rt.prompt(PromptRequest::new(sid, "delegate")).await })
    };
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(
        parent_complete_count(&provider),
        1,
        "Park must not start a second parent complete while the child is running"
    );
    handle.await.expect("join").expect("prompt");
    assert_eq!(parent_complete_count(&provider), 2);
    for names in child_histories(&provider) {
        assert!(
            !names.iter().any(|n| n == "delegate_task"),
            "child tools must omit delegate_task when maxDepth=1: {names:?}"
        );
    }
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    assert_eq!(tools.len(), 1, "{tools:?}");
    assert!(
        tools[0].contains("CHILD_SUMMARY_OK"),
        "Park tool row must be the summary, got {}",
        tools[0]
    );
    assert!(
        !tools[0].contains("\"status\":\"running\""),
        "must not persist the running handle as the first tool row: {}",
        tools[0]
    );
}

/// 2: Unselected MCP and inheritShell=false must not leak into the child.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c6_child_does_not_leak_mcp_or_shell() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = settings_auto();
    settings.mcp_servers = vec![McpServerConfig {
        id: "echo".into(),
        enabled: true,
        command: "dandelion-mcp-does-not-exist-xyz".into(),
        ..Default::default()
    }];
    settings.delegation_inherit_shell = false;
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![delegate("call_d", "inspect")]),
        ScriptedTurn::text("child ok"),
        ScriptedTurn::text("parent ok"),
    ]));
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "go"))
        .await
        .expect("prompt");
    for names in child_histories(&provider) {
        assert!(
            names.iter().all(|n| !n.starts_with("mcp__")),
            "child leaked MCP: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "terminal" || n == "process"),
            "child leaked shell: {names:?}"
        );
    }
}

/// 3: maxDepth=1 → child schema has no delegate_task (depth limit).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c6_depth_limit_omits_delegate_on_child() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = settings_auto();
    settings.delegation_max_depth = 1;
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![delegate("call_d", "one level")]),
        ScriptedTurn::text("leaf"),
        ScriptedTurn::text("done"),
    ]));
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "depth"))
        .await
        .expect("prompt");
    let child = child_histories(&provider);
    assert!(!child.is_empty(), "child must have completed");
    assert!(
        child
            .iter()
            .all(|n| !n.iter().any(|t| t == "delegate_task")),
        "{child:?}"
    );
}

/// 4: Parent cancel cancels the child token and kills the child's process tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c6_parent_cancel_kills_child_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = settings_auto();
    settings.delegation_inherit_shell = true;
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![delegate("call_d", "sleep")]),
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_sleep".into(),
                name: "terminal".into(),
                arguments: sleep_args(20),
            }],
        ),
    ]));
    let rt = runtime_with(dir.path(), provider, settings);
    let sid = rt.new_session(None);
    let mut rx = rt.subscribe();
    let handle = {
        let rt = rt.clone();
        let sid = sid.clone();
        tokio::spawn(async move { rt.prompt(PromptRequest::new(sid, "sleep")).await })
    };
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(8) {
            panic!("child terminal never started");
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
    rt.cancel(sid).await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(8), handle)
        .await
        .expect("join")
        .expect("task");
    assert!(outcome.is_ok(), "{outcome:?}");
}

/// 5: tasks[] larger than maxConcurrent is an error, not a silent truncate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c6_tasks_over_cap_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = settings_auto();
    settings.delegation_max_concurrent = 2;
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_d".into(),
                name: "delegate_task".into(),
                arguments: serde_json::json!({
                    "tasks": [
                        {"goal": "a"},
                        {"goal": "b"},
                        {"goal": "c"}
                    ]
                })
                .to_string(),
            }],
        ),
        ScriptedTurn::text("ok"),
    ]));
    let rt = runtime_with(dir.path(), provider, settings);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "fanout"))
        .await
        .expect("prompt");
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    assert!(
        tools
            .iter()
            .any(|t| t.contains("too many concurrent subagents")),
        "{tools:?}"
    );
    let kids: Vec<_> = store
        .sessions_list(50, 0)
        .unwrap()
        .into_iter()
        .filter(|r| r.kind == "subagent")
        .collect();
    assert!(
        kids.is_empty(),
        "must not spawn a truncated subset: {kids:?}"
    );
}

/// 6: Due cron job creates kind=cron and emits _job_update fired.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c7_due_job_fires_session_and_bus() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("cron done")]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let job = rt
        .cron_add(
            "30m",
            "CRON_UNIQUE_PROMPT",
            Some(dir.path().to_path_buf()),
            None,
            "skip",
        )
        .unwrap();
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    store
        .cron_mark_run(
            &job.id,
            "1999-01-01T00:00:00+00:00",
            "2000-01-01T00:00:00+00:00",
        )
        .unwrap();
    let mut rx = rt.subscribe();
    rt.cron_tick_once().await.unwrap();
    let mut fired = false;
    while let Ok(frame) = rx.try_recv() {
        if frame.method != "_dandelion/session/update" {
            continue;
        }
        if let SessionUpdate::JobUpdate { phase, job_id, .. } = frame.params.update {
            if phase == "fired" && job_id == job.id {
                fired = true;
            }
        }
    }
    assert!(fired, "expected _job_update fired");
    let cron_sessions: Vec<_> = rt
        .sessions_list(None, 0)
        .unwrap()
        .into_iter()
        .filter(|r| r.kind == "cron")
        .collect();
    assert!(!cron_sessions.is_empty(), "kind=cron session missing");
}

/// 7: Wiki body stays out of system; Inference cannot read it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c8_wiki_not_in_system_and_unread() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(data.join("wiki")).unwrap();
    fs::write(
        data.join("wiki/ARCH.md"),
        "UNIQUE_WIKI_SENTENCE_DO_NOT_INJECT\n",
    )
    .unwrap();
    let wiki = data.join("wiki/ARCH.md");
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_r".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({ "path": wiki.to_string_lossy() }).to_string(),
            }],
        ),
        ScriptedTurn::text("ok"),
    ]));
    let rt = runtime_with(dir.path(), provider.clone(), settings_auto());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "wiki?"))
        .await
        .expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(
        !sys.contains("UNIQUE_WIKI_SENTENCE_DO_NOT_INJECT"),
        "wiki must not enter system:\n{sys}"
    );
    let store = Store::open(&data, 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    assert!(
        tools.iter().any(|t| t.contains("Error:")),
        "Inference must fail to read wiki: {tools:?}"
    );
    assert!(
        tools
            .iter()
            .all(|t| !t.contains("UNIQUE_WIKI_SENTENCE_DO_NOT_INJECT")),
        "{tools:?}"
    );
}

/// 8: Soul replaces G1 identity; memory.enabled=false skips it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c8b_soul_replaces_identity_and_can_disable() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("DANDELION.soul.md"), "SOUL_UNIQUE_IDENTITY\n").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("hi")]));
    let rt = runtime_with(dir.path(), provider.clone(), settings_auto());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid, "who"))
        .await
        .expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(sys.contains("SOUL_UNIQUE_IDENTITY"), "{sys}");
    assert!(
        !sys.contains(DEFAULT_IDENTITY),
        "must not stack G1 identity:\n{sys}"
    );

    let provider2 = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("hi")]));
    let mut off = settings_auto();
    off.memory_enabled = false;
    let rt2 = runtime_with(dir.path(), provider2.clone(), off);
    let sid2 = rt2.new_session(None);
    rt2.prompt(PromptRequest::new(sid2, "who"))
        .await
        .expect("prompt");
    let sys2 = provider2.last_system().expect("system");
    assert!(!sys2.contains("SOUL_UNIQUE_IDENTITY"), "{sys2}");
    assert!(sys2.contains(DEFAULT_IDENTITY), "{sys2}");
}

/// 9: list newest-first; search hits content; export has no apiKey.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c9_list_search_export() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = settings_auto();
    settings.api_key = Some("sk-export-secret-key".into());
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("one"),
        ScriptedTurn::text("two"),
    ]));
    let rt = runtime_with(dir.path(), provider, settings);
    let a = rt.new_session(None);
    rt.prompt(PromptRequest::new(a.clone(), "UNIQUE_C9_ALPHA hello"))
        .await
        .expect("a");
    let b = rt.new_session(None);
    rt.prompt(PromptRequest::new(b.clone(), "UNIQUE_C9_BETA hello"))
        .await
        .expect("b");
    let listed = rt.sessions_list(None, 0).unwrap();
    let ids: Vec<_> = listed.iter().map(|r| r.id.as_str()).collect();
    assert!(
        ids.contains(&a.as_str()) && ids.contains(&b.as_str()),
        "{ids:?}"
    );
    assert_eq!(listed[0].id, b.as_str(), "newest first: {ids:?}");
    let hits = rt.sessions_search("UNIQUE_C9_ALPHA", None).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].id, a.as_str());
    let dump = rt.session_export(&a).unwrap().to_string();
    assert!(!dump.contains("apiKey"), "{dump}");
    assert!(!dump.contains("sk-export-secret-key"), "{dump}");
}

/// 10: Gate 3 deny skips Host and disk; allow skips ask and still writes; gate 1 still holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c10_deny_and_allow_and_gate1() {
    let dir = tempfile::tempdir().unwrap();
    let rec = RecordingHost::new(dir.path().join("data"));
    let host = Arc::new(rec.clone());
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Ask;
    settings.workspace = Some(dir.path().to_path_buf());
    settings.permission_rules = PermissionRules {
        deny: vec!["write_file".into()],
        ask: vec![],
        allow: vec![],
    };
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_w".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"denied.txt","content":"nope"}"#.into(),
            }],
        ),
        ScriptedTurn::text("done"),
    ]));
    let rt = Runtime::new_with_provider(host, settings, provider).expect("runtime");
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "write"))
        .await
        .expect("prompt");
    assert!(rec.call_titles().is_empty(), "{:?}", rec.call_titles());
    assert!(!dir.path().join("denied.txt").exists());
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    assert!(
        tools.iter().any(|t| t.contains("Error: blocked by rule")),
        "{tools:?}"
    );

    let rec2 = RecordingHost::new(dir.path().join("data2"));
    let host2 = Arc::new(rec2.clone());
    let mut allow = Settings::defaults_configured();
    allow.permission_mode = PermissionMode::Ask;
    allow.workspace = Some(dir.path().to_path_buf());
    allow.permission_rules = PermissionRules {
        deny: vec![],
        ask: vec![],
        allow: vec!["write_file".into()],
    };
    let provider2 = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![
                ToolCall {
                    id: "call_ok".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"allowed.txt","content":"yes"}"#.into(),
                },
                ToolCall {
                    id: "call_sys".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"C:\\Windows\\g3-nope.txt","content":"x"}"#.into(),
                },
            ],
        ),
        ScriptedTurn::text("done"),
    ]));
    let rt2 = Runtime::new_with_provider(host2, allow, provider2).expect("runtime");
    let sid2 = rt2.new_session(None);
    rt2.prompt(PromptRequest::new(sid2.clone(), "write2"))
        .await
        .expect("prompt");
    assert!(rec2.call_titles().is_empty(), "allow must skip gate 2");
    assert_eq!(
        fs::read_to_string(dir.path().join("allowed.txt")).unwrap(),
        "yes"
    );
    let store2 = Store::open(&dir.path().join("data2"), 5000).unwrap();
    let tools2 = tool_contents(&store2, &sid2);
    assert!(
        tools2
            .iter()
            .any(|t| t.contains("Error:") && t.to_lowercase().contains("system")),
        "gate 1 must still block Windows: {tools2:?}"
    );
}

/// 「本次会话不再询问」= 问一次之后本会话默认通过：一次弹窗答复后，
/// 同一轮里的另一个文件、以及下一轮 prompt 都不得再弹（gate-1 硬拒绝、
/// deny 规则仍然生效，见 c10）。旧的 per-run 精确键缓存每轮换文件就
/// 重新弹窗，正是用户投诉的“弹个不停”。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_turn_prompts_once_for_the_rest_of_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let rec = RecordingHost::new(dir.path().join("data")).with_option("allow_turn");
    let host = Arc::new(rec.clone());
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Ask;
    settings.workspace = Some(dir.path().to_path_buf());
    let write = |id: &str, path: &str| ToolCall {
        id: id.into(),
        name: "write_file".into(),
        arguments: format!(r#"{{"path":"{path}","content":"x"}}"#),
    };
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![write("w1", "a.txt")]),
        ScriptedTurn::tools("", vec![write("w2", "b.txt")]),
        ScriptedTurn::text("first done"),
        // Second user turn: under the old per-run semantics this re-prompted.
        ScriptedTurn::tools("", vec![write("w3", "c.txt")]),
        ScriptedTurn::text("second done"),
    ]));
    let rt = Runtime::new_with_provider(host, settings, provider).expect("runtime");
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "write a and b"))
        .await
        .expect("prompt 1");
    rt.prompt(PromptRequest::new(sid.clone(), "write c"))
        .await
        .expect("prompt 2");
    assert_eq!(
        rec.call_titles().len(),
        1,
        "session must be prompted exactly once: {:?}",
        rec.call_titles()
    );
    for name in ["a.txt", "b.txt", "c.txt"] {
        assert!(dir.path().join(name).exists(), "{name} was not written");
    }
}

/// 12: read_file + delegate_task overlap; child is running before read completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c6_read_and_delegate_overlap() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("__dandelion_g3_overlap_read"), "x\n").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![
                ToolCall {
                    id: "call_r".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"__dandelion_g3_overlap_read"}"#.into(),
                },
                delegate("call_d", "while reading"),
            ],
        ),
        ScriptedTurn::text("child-parallel"),
        ScriptedTurn::text("parent-mixed"),
        ScriptedTurn::text("kick"),
    ]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let sid = rt.new_session(None);
    let mut rx = rt.subscribe();
    rt.prompt(PromptRequest::new(sid.clone(), "mix"))
        .await
        .expect("prompt");
    let mut child_running_seq: Option<u64> = None;
    let mut read_done_seq: Option<u64> = None;
    while let Ok(frame) = rx.try_recv() {
        match &frame.params.update {
            SessionUpdate::StateUpdate {
                state: SessionState::Running,
                ..
            } if frame.params.session_id != sid => {
                child_running_seq.get_or_insert(frame.seq);
            }
            SessionUpdate::ToolCallUpdate { update }
                if frame.params.session_id == sid
                    && update.tool_call_id.as_str() == "call_r"
                    && matches!(
                        update.status,
                        Some(ToolCallStatus::Completed | ToolCallStatus::Failed)
                    ) =>
            {
                read_done_seq.get_or_insert(frame.seq);
            }
            _ => {}
        }
    }
    let child_seq = child_running_seq.expect("child never running");
    let read_seq = read_done_seq.expect("read_file never completed");
    assert!(
        child_seq < read_seq,
        "child must start before read_file finishes (got child={child_seq} read={read_seq})"
    );
}

/// 13: Child summary is capped and marked truncated; child tool log is not inlined.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c6_summary_cap_truncated() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "UNIQUE_CHILD_TOOL_LOG\n").unwrap();
    let mut settings = settings_auto();
    settings.delegation_summary_max_chars = 256;
    let long: String = "S".repeat(400);
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![delegate("call_d", "read then rant")]),
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_r".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"note.txt"}"#.into(),
            }],
        ),
        ScriptedTurn::text(long),
        ScriptedTurn::text("parent"),
    ]));
    let rt = runtime_with(dir.path(), provider, settings);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "cap"))
        .await
        .expect("prompt");
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    let summary = tools
        .iter()
        .find(|t| t.contains("truncated"))
        .expect("{tools:?}");
    assert!(summary.contains("truncated"), "{summary}");
    assert!(
        !summary.contains("UNIQUE_CHILD_TOOL_LOG"),
        "must not dump child tool history: {summary}"
    );
    let chars = summary.chars().count();
    assert!(
        chars <= 256 + "\ntruncated".chars().count() + 8,
        "len={chars}"
    );
}

/// 14: Idle child with no chunks/tools is cancelled after staleIdleSec.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c6_stale_idle_cancels_child() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = settings_auto();
    settings.delegation_stale_idle_sec = 1;
    settings.delegation_stale_in_tool_sec = 2;
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![delegate("call_d", "hang")]),
        ScriptedTurn::hang_until_cancel(),
        ScriptedTurn::text("parent after stale"),
    ]));
    let rt = runtime_with(dir.path(), provider, settings);
    let sid = rt.new_session(None);
    tokio::time::timeout(
        Duration::from_secs(8),
        rt.prompt(PromptRequest::new(sid.clone(), "stale")),
    )
    .await
    .expect("stale prompt timed out")
    .expect("prompt");
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    assert!(
        tools.iter().any(|t| t.contains("Error: subagent stale")),
        "{tools:?}"
    );
}

/// 15: 取消在历史尾部留下持久中断标记（Codex `<turn_aborted>` 同款）：
/// 下一轮模型必须看到"上一轮是被中断的"，否则会把下一条普通消息
/// （哪怕只是打招呼）当成"继续旧任务"的信号。UI 不受影响。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_records_turn_aborted_note_for_next_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::hang_until_cancel(),
        ScriptedTurn::text("greeting answered"),
    ]));
    let rt = runtime_with(dir.path(), provider.clone(), settings_auto());
    let sid = rt.new_session(None);
    let handle = {
        let rt = rt.clone();
        let sid = sid.clone();
        tokio::spawn(async move { rt.prompt(PromptRequest::new(sid, "生成整个项目的 wiki")).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    rt.cancel(sid.clone()).await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(8), handle)
        .await
        .expect("join")
        .expect("task");
    assert!(outcome.is_ok(), "{outcome:?}");

    // 模型侧：标记以 user 身份进入历史，下一轮请求里紧贴新用户消息
    rt.prompt(PromptRequest::new(sid.clone(), "你好"))
        .await
        .expect("second prompt");
    let msgs = provider.last_messages();
    let note_positions: Vec<usize> = msgs
        .iter()
        .enumerate()
        .filter_map(|(i, m)| match m {
            ChatMessage::User { content, .. } if content.contains("<turn_aborted>") => Some(i),
            _ => None,
        })
        .collect();
    assert_eq!(note_positions.len(), 1, "{msgs:?}");
    let last_user = msgs
        .iter()
        .rposition(|m| matches!(m, ChatMessage::User { content, .. } if content == "你好"))
        .expect("greeting in request");
    assert_eq!(note_positions[0] + 1, last_user, "中断标记必须紧贴本轮用户消息");

    // UI 侧：导出里是独立的 turn_note role，不是 user/assistant 气泡
    let export = rt.session_export(&sid).unwrap();
    let marker_rows: Vec<&serde_json::Value> = export["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("<turn_aborted>"))
        })
        .collect();
    assert_eq!(marker_rows.len(), 1);
    assert_eq!(marker_rows[0]["role"].as_str(), Some("turn_note"));
}
