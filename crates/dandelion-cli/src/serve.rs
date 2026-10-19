//! G6 C15: OpenAI-compat HTTP host over `Runtime`. Binds loopback by default.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use dandelion_core::acp::SessionUpdate;
use dandelion_core::{PromptRequest, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

#[derive(Clone)]
struct AppState {
    runtime: Runtime,
}

#[derive(Debug, Deserialize)]
struct ChatRequest {
    #[serde(default)]
    messages: Vec<ChatMessageIn>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ChatMessageIn {
    role: String,
    #[serde(default)]
    content: Value,
}

#[derive(Debug, Serialize)]
struct ChatResponse {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<Choice>,
}

#[derive(Debug, Serialize)]
struct Choice {
    index: u32,
    message: OutMessage,
    finish_reason: &'static str,
}

#[derive(Debug, Serialize)]
struct OutMessage {
    role: &'static str,
    content: String,
}

/// Returns Ok(bind) or Err(message) when non-loopback without explicit CLI override.
pub fn resolve_bind(settings_bind: &str, cli_bind: Option<&str>) -> Result<String, String> {
    let chosen = cli_bind
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| settings_bind.trim());
    if chosen.is_empty() {
        return Err("bind address is empty".into());
    }
    if is_loopback_bind(chosen) {
        return Ok(chosen.to_string());
    }
    match cli_bind {
        Some(explicit) if explicit.trim() == chosen => {
            eprintln!(
                "dln serve: WARNING binding non-loopback {chosen} (explicit --bind). Exposes tools to the network."
            );
            Ok(chosen.to_string())
        }
        _ => Err(format!(
            "refusing non-loopback bind `{chosen}` from settings; pass explicit --bind {chosen}"
        )),
    }
}

pub fn is_loopback_bind(addr: &str) -> bool {
    let host = addr
        .rsplit_once(':')
        .map(|(h, _)| h.trim())
        .unwrap_or(addr.trim());
    let host = host.trim_matches(|c| c == '[' || c == ']');
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "0:0:0:0:0:0:0:1")
}

pub async fn run(cli_bind: Option<String>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let rt = Runtime::new(Arc::new(crate::app::cli_host()))?;
    let bind = resolve_bind(&rt.settings().serve_bind, cli_bind.as_deref())?;
    let addr: SocketAddr = bind
        .parse()
        .map_err(|e| format!("invalid bind `{bind}`: {e}"))?;

    let state = AppState { runtime: rt };
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(state);

    eprintln!("dln serve listening on http://{addr} (agent loop in dandelion-core)");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn chat_completions(
    State(state): State<AppState>,
    Json(body): Json<ChatRequest>,
) -> impl IntoResponse {
    if body.stream {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "stream=true not supported yet; use stream=false" } })),
        )
            .into_response();
    }
    let text = last_user_text(&body.messages);
    if text.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "messages must include a user turn" } })),
        )
            .into_response();
    }
    let sid = state.runtime.new_session(None);
    let mut rx = state.runtime.subscribe();
    let mut req = PromptRequest::new(sid.clone(), text);
    if let Some(model) = body.model.filter(|m| !m.trim().is_empty()) {
        req.model = Some(model);
    }
    let collect = tokio::spawn(async move {
        let mut buf = String::new();
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    if !frame.belongs_to_session(&sid) {
                        continue;
                    }
                    match frame.params.update {
                        SessionUpdate::AgentMessageChunk { content, .. } => {
                            if let Some(t) = content_text(&content) {
                                buf.push_str(&t);
                            }
                        }
                        SessionUpdate::StateUpdate {
                            state: dandelion_core::acp::SessionState::Idle,
                            ..
                        } => break,
                        _ => {}
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
        buf
    });
    if let Err(err) = state.runtime.prompt(req).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": { "message": err.to_string() } })),
        )
            .into_response();
    }
    let content = collect.await.unwrap_or_default();
    let model = state
        .runtime
        .settings()
        .default_model
        .clone()
        .unwrap_or_else(|| "dandelion".into());
    let resp = ChatResponse {
        id: format!("chatcmpl_{}", uuid_like()),
        object: "chat.completion",
        created: now_secs(),
        model,
        choices: vec![Choice {
            index: 0,
            message: OutMessage {
                role: "assistant",
                content,
            },
            finish_reason: "stop",
        }],
    };
    (StatusCode::OK, Json(resp)).into_response()
}

fn last_user_text(messages: &[ChatMessageIn]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| value_to_text(&m.content))
        .unwrap_or_default()
}

fn value_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                p.get("text")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| p.as_str().map(|s| s.to_string()))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn content_text(block: &dandelion_core::acp::ContentBlock) -> Option<String> {
    match block {
        dandelion_core::acp::ContentBlock::Text { text } => Some(text.clone()),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn uuid_like() -> String {
    format!("{:x}", now_secs().wrapping_mul(0x9e37) ^ 0xdead_beef)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_ok_from_settings() {
        assert!(resolve_bind("127.0.0.1:3927", None).is_ok());
        assert!(is_loopback_bind("127.0.0.1:3927"));
        assert!(is_loopback_bind("localhost:9"));
        assert!(is_loopback_bind("[::1]:3927"));
    }

    #[test]
    fn non_loopback_needs_explicit_cli() {
        let err = resolve_bind("0.0.0.0:3927", None).unwrap_err();
        assert!(err.contains("--bind"), "{err}");
        assert!(resolve_bind("0.0.0.0:3927", Some("0.0.0.0:3927")).is_ok());
    }
}
