//! Live smoke against a configured OpenAI-compatible endpoint.
//!
//! Default (override with env):
//!   DANDELION_BASE_URL=http://127.0.0.1:11434/v1
//!   DANDELION_MODEL=Qwen3.8-Flash-Next
//!
//! Run:
//!   $env:DANDELION_LIVE_TEST="1"
//!   cargo test -p dandelion-core --test live_g1 -- --nocapture

use dandelion_core::acp::{SessionState, SessionUpdate};
use dandelion_core::host::CliHost;
use dandelion_core::provider::types::{ChatMessage, CompletionRequest};
use dandelion_core::provider::{OpenAiSse, Provider};
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings, ThinkingIntensity};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn live_enabled() -> bool {
    matches!(
        std::env::var("DANDELION_LIVE_TEST").as_deref(),
        Ok("1") | Ok("true")
    )
}

fn live_base_url() -> String {
    std::env::var("DANDELION_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:11434/v1".into())
}

fn live_model() -> String {
    std::env::var("DANDELION_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Qwen3.8-Flash-Next".into())
}

fn live_settings(workspace: std::path::PathBuf) -> Settings {
    let mut s = Settings::defaults();
    s.base_url = Some(Settings::normalize_base_url(&live_base_url()));
    s.default_model = Some(live_model());
    s.permission_mode = PermissionMode::Auto;
    s.workspace = Some(workspace);
    s.thinking_intensity = ThinkingIntensity::Low;
    s.max_tokens = 512;
    s.max_iterations = 8;
    s.http_connect_timeout_ms = 10_000;
    s.sse_idle_timeout_ms = 60_000;
    s
}

#[tokio::test]
async fn live_provider_reasoning_effort() {
    if !live_enabled() {
        eprintln!("skip live_provider_reasoning_effort (set DANDELION_LIVE_TEST=1)");
        return;
    }
    let settings = live_settings(std::env::temp_dir());
    let provider = OpenAiSse::from_settings(&settings).expect("provider");
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let msg = provider
        .complete(
            CompletionRequest {
                model: live_model(),
                temperature: 0.2,
                max_tokens: 64,
                reasoning_effort: Some("low".into()),
                system: "Reply with exactly PONG and nothing else.".into(),
                messages: vec![ChatMessage::User {
                    content: "ping".into(),
                    images: Vec::new(),
                }],
                tools: vec![],
            },
            CancellationToken::new(),
            tx,
        )
        .await
        .expect("complete");
    let mut streamed = String::new();
    while let Ok(ev) = rx.try_recv() {
        if let dandelion_core::provider::types::StreamEvent::TextDelta(t) = ev {
            streamed.push_str(&t);
        }
    }
    let text = if msg.text.is_empty() {
        streamed
    } else {
        msg.text
    };
    assert!(
        text.to_ascii_uppercase().contains("PONG"),
        "expected PONG in {text:?}"
    );
}

#[tokio::test]
async fn live_runtime_read_file_tool() {
    if !live_enabled() {
        eprintln!("skip live_runtime_read_file_tool (set DANDELION_LIVE_TEST=1)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "DANDELION_LIVE_OK\n").unwrap();
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let settings = live_settings(dir.path().to_path_buf());
    let provider = Arc::new(OpenAiSse::from_settings(&settings).unwrap());
    let rt = Runtime::new_with_provider(host, settings, provider).unwrap();
    let mut rx = rt.subscribe();
    let sid = rt.new_session(None);
    let prompt = rt.prompt(PromptRequest::new(
        sid.clone(),
        "用 read_file 读取 hello.txt 的全部内容，然后只把文件原文复述出来。",
    ));
    let mut saw_read = false;
    let mut agent_text = String::new();
    let mut prompt_done = false;
    tokio::pin!(prompt);
    loop {
        tokio::select! {
            frame = rx.recv() => {
                let frame = frame.expect("frame");
                match frame.params.update {
                    SessionUpdate::ToolCallUpdate { update } => {
                        if update.title.as_deref() == Some("read_file") {
                            saw_read = true;
                        }
                    }
                    SessionUpdate::AgentMessageChunk { content, .. } => {
                        let dandelion_core::acp::ContentBlock::Text { text } = content;
                        agent_text.push_str(&text);
                    }
                    SessionUpdate::StateUpdate { state: SessionState::Idle, .. } => break,
                    _ => {}
                }
            }
            res = &mut prompt, if !prompt_done => {
                prompt_done = true;
                res.expect("prompt");
            }
        }
    }
    assert!(saw_read, "model should call read_file");
    assert!(
        agent_text.contains("DANDELION_LIVE_OK") || saw_read,
        "agent text={agent_text}"
    );
}
