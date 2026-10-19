//! G5-H C13: live terminal ACP events on existing `terminal` tool. No new tool name.

use dandelion_core::acp::{decode_base64, SessionUpdate, ToolCallContent};
use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::ToolCall;
use dandelion_core::tools::ToolRegistry;
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings};
use std::sync::Arc;
use std::time::Duration;

fn runtime_with(dir: &std::path::Path, provider: Arc<ScriptedProvider>) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(dir.to_path_buf());
    settings.permission_mode = PermissionMode::Auto;
    settings.cron_tick_sec = 3_600;
    settings.terminal_foreground_timeout_sec = 8;
    settings.terminal_foreground_timeout_max_sec = 12;
    settings.terminal_live_chunk_bytes = 64;
    settings.terminal_live_flush_ms = 20;
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(dir.join("data"))),
        settings,
        provider,
    )
    .expect("runtime")
}

fn echo_args() -> String {
    let command = if cfg!(windows) {
        "Write-Output LIVE_TERM_HIT"
    } else {
        "echo LIVE_TERM_HIT"
    };
    serde_json::json!({ "command": command }).to_string()
}

fn sleep_args(seconds: u32) -> String {
    let command = if cfg!(windows) {
        format!("Start-Sleep -Seconds {seconds}")
    } else {
        format!("sleep {seconds}")
    };
    serde_json::json!({ "command": command, "timeout": 6 }).to_string()
}

fn decoded_chunks(frames: &[dandelion_core::bus::BusFrame]) -> String {
    let mut out = String::new();
    for frame in frames {
        if let SessionUpdate::TerminalOutputChunk { data, .. } = &frame.params.update {
            if let Some(bytes) = decode_base64(data) {
                out.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    }
    out
}

#[test]
fn live_terminal_does_not_add_tool_name() {
    let s = Settings::defaults();
    let g1 = ToolRegistry::g1(&s).names();
    assert_eq!(g1.len(), 6);
    assert!(g1.contains(&"terminal".to_string()));
    assert!(!g1.contains(&"live_terminal".to_string()));
    let session = ToolRegistry::session_tools(&s, true, false, vec![]).names();
    assert!(
        !session.contains(&"live_terminal".to_string()),
        "{session:?}"
    );
}

#[cfg(any(windows, unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_emits_live_chunks_and_exit() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_sh".into(),
                name: "terminal".into(),
                arguments: echo_args(),
            }],
        ),
        ScriptedTurn::text("ok"),
    ]));
    let rt = runtime_with(dir.path(), provider);
    let mut rx = rt.subscribe();
    let sid = rt.create_session(Some("live-term")).expect("session");
    rt.prompt(PromptRequest::new(sid.clone(), "echo"))
        .await
        .expect("prompt");

    let mut frames = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        if frame.params.session_id == sid {
            frames.push(frame);
        }
    }
    let mut saw_start = false;
    let mut saw_exit = false;
    let mut saw_terminal_content = false;
    for frame in &frames {
        match &frame.params.update {
            SessionUpdate::TerminalUpdate {
                terminal_id,
                command,
                exit_status,
                ..
            } => {
                assert!(terminal_id.starts_with("proc_"), "{terminal_id}");
                if command.is_some() {
                    saw_start = true;
                }
                if exit_status.is_some() {
                    saw_exit = true;
                }
            }
            SessionUpdate::ToolCallUpdate { update } => {
                if update
                    .content
                    .as_ref()
                    .map(|c| {
                        c.iter()
                            .any(|item| matches!(item, ToolCallContent::Terminal { .. }))
                    })
                    .unwrap_or(false)
                {
                    saw_terminal_content = true;
                }
            }
            _ => {}
        }
    }
    let text = decoded_chunks(&frames);
    assert!(saw_start, "missing terminal_update start {frames:?}");
    assert!(saw_exit, "missing exitStatus {frames:?}");
    assert!(saw_terminal_content, "tool card must link terminalId");
    assert!(
        text.contains("LIVE_TERM_HIT"),
        "chunks={text:?} frames={frames:?}"
    );
}

#[cfg(any(windows, unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_emits_exit_and_kills_tree() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::tools(
        "",
        vec![ToolCall {
            id: "call_sleep".into(),
            name: "terminal".into(),
            arguments: sleep_args(20),
        }],
    )]));
    let rt = runtime_with(dir.path(), provider);
    let mut rx = rt.subscribe();
    let sid = rt.create_session(Some("live-cancel")).expect("session");
    let rt2 = rt.clone();
    let sid2 = sid.clone();
    let handle = tokio::spawn(async move { rt2.prompt(PromptRequest::new(sid2, "sleep")).await });
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(8) {
            panic!("never got terminal_update");
        }
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Ok(frame)) if frame.params.session_id == sid => {
                if let SessionUpdate::TerminalUpdate {
                    command,
                    exit_status,
                    ..
                } = frame.params.update
                {
                    if command.is_some() && exit_status.is_none() {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    rt.cancel(sid.clone()).await.unwrap();
    let _ = handle.await;
    assert!(
        start.elapsed() < Duration::from_secs(8),
        "Stop must kill the tree, not wait out sleep"
    );
    let mut saw_exit = false;
    while let Ok(frame) = rx.try_recv() {
        if frame.params.session_id != sid {
            continue;
        }
        if let SessionUpdate::TerminalUpdate {
            exit_status: Some(_),
            ..
        } = frame.params.update
        {
            saw_exit = true;
        }
    }
    if !saw_exit {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if let Ok(Ok(frame)) = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await
            {
                if let SessionUpdate::TerminalUpdate {
                    exit_status: Some(_),
                    ..
                } = frame.params.update
                {
                    saw_exit = true;
                    break;
                }
            }
        }
    }
    assert!(saw_exit, "cancel must emit terminal_update exitStatus");
}
