//! P2 preflight contracts.  A router is advisory only: it never grants a
//! permission, invokes a tool, or owns an agent loop.

use crate::config::Settings;
use crate::context_os::{RoutingDecision, RoutingRoute};
use crate::{Error, Result};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightStatus {
    pub enabled: bool,
    pub state: &'static str,
    pub timeout_ms: u64,
    pub detail: String,
}

pub fn status(enabled: bool, timeout_ms: u64) -> PreflightStatus {
    PreflightStatus {
        enabled,
        state: if enabled { "ready" } else { "disabled" },
        timeout_ms,
        detail: if enabled {
            "rule fallback active; local-model adapter not enabled".into()
        } else {
            "disabled by settings".into()
        },
    }
}

pub fn status_with_circuit(
    enabled: bool,
    timeout_ms: u64,
    circuit: &CircuitBreaker,
) -> PreflightStatus {
    if !enabled {
        return status(false, timeout_ms);
    }
    if circuit.allow() {
        return status(true, timeout_ms);
    }
    PreflightStatus {
        enabled: true,
        state: "degraded",
        timeout_ms,
        detail: circuit
            .detail()
            .unwrap_or_else(|| "circuit breaker is cooling down".into()),
    }
}

/// Candidate extraction is intentionally narrow and advisory. Every result is
/// stored as `model_candidate` and requires user confirmation before retrieval.
pub fn extract_memory_candidates(text: &str) -> Vec<String> {
    let text = text.trim();
    let markers = ["我喜欢", "我偏好", "记住", "以后请", "always", "prefer"];
    if text.len() <= 500
        && markers.iter().any(|marker| {
            text.to_ascii_lowercase()
                .contains(&marker.to_ascii_lowercase())
        })
    {
        vec![text.to_string()]
    } else {
        vec![]
    }
}

#[derive(Default)]
pub struct CircuitBreaker {
    inner: Mutex<CircuitState>,
}
#[derive(Default)]
struct CircuitState {
    failures: u32,
    open_until: Option<Instant>,
    last_error: Option<String>,
}
impl CircuitBreaker {
    pub fn allow(&self) -> bool {
        self.inner
            .lock()
            .map(|s| s.open_until.is_none_or(|until| until <= Instant::now()))
            .unwrap_or(false)
    }
    pub fn success(&self) {
        if let Ok(mut s) = self.inner.lock() {
            *s = CircuitState::default();
        }
    }
    pub fn failure(&self, error: &Error, threshold: u32, cooldown: Duration) {
        if let Ok(mut s) = self.inner.lock() {
            s.failures += 1;
            s.last_error = Some(error.to_string());
            if s.failures >= threshold {
                s.open_until = Some(Instant::now() + cooldown);
            }
        }
    }
    pub fn detail(&self) -> Option<String> {
        self.inner.lock().ok().and_then(|s| s.last_error.clone())
    }
}

#[async_trait]
pub trait LocalInference: Send + Sync {
    /// Returns one JSON object conforming to [`RoutingDecision`]. Implementors
    /// are transport adapters; policy and parsing stay in Rust.
    async fn complete_json(&self, prompt: &str, cancel: CancellationToken) -> Result<String>;
}

/// Cancellable OpenAI-compatible local transport. Runtime decides when it is
/// safe to select it; this adapter never starts or competes for a sidecar.
pub struct OpenAiCompatibleInference {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl OpenAiCompatibleInference {
    pub fn new(base_url: &str, model: &str, settings: &Settings) -> Result<Self> {
        let model = model.trim();
        if model.is_empty() {
            return Err(Error::Invalid("preflight model is required".into()));
        }
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_millis(settings.http_connect_timeout_ms))
                .build()
                .map_err(|e| Error::Provider(e.to_string()))?,
            base_url: Settings::normalize_base_url(base_url),
            model: model.into(),
        })
    }
}

#[async_trait]
impl LocalInference for OpenAiCompatibleInference {
    async fn complete_json(&self, prompt: &str, cancel: CancellationToken) -> Result<String> {
        let request = self.client.post(format!("{}/chat/completions", self.base_url)).json(&json!({"model": self.model, "stream": false, "temperature": 0, "messages": [{"role":"system","content":"Return only a JSON RoutingDecision."}, {"role":"user","content":prompt}]})).send();
        let response = tokio::select! { _ = cancel.cancelled() => return Err(Error::Cancelled), value = request => value.map_err(|e| Error::Provider(format!("preflight local request: {e}")))? };
        if !response.status().is_success() {
            return Err(Error::Provider(format!(
                "preflight local status {}",
                response.status()
            )));
        }
        let body: serde_json::Value = tokio::select! { _ = cancel.cancelled() => return Err(Error::Cancelled), value = response.json() => value.map_err(|e| Error::Provider(format!("preflight local JSON: {e}")))? };
        body.pointer("/choices/0/message/content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::Provider("preflight local response had no message content".into())
            })
    }
}

/// Deterministic offline fallback. It supplies useful classification now and
/// is also the safe behavior whenever a local model is unavailable.
#[derive(Default)]
pub struct RuleInference;

#[async_trait]
impl LocalInference for RuleInference {
    async fn complete_json(&self, prompt: &str, cancel: CancellationToken) -> Result<String> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let lower = prompt.to_ascii_lowercase();
        let sensitive = lower.contains("api_key")
            || lower.contains("password")
            || lower.contains("secret")
            || prompt.contains("密钥");
        let route = if lower.contains("delete")
            || lower.contains("删除")
            || lower.contains("push")
            || lower.contains("推送")
        {
            RoutingRoute::AskUser
        } else if lower.contains("plan") || lower.contains("方案") || lower.contains("设计") {
            RoutingRoute::LocalToolPlan
        } else {
            RoutingRoute::CloudAgent
        };
        Ok(serde_json::to_string(&RoutingDecision {
            route,
            intent: if lower.contains("git") {
                "git".into()
            } else {
                "general".into()
            },
            complexity: if prompt.chars().count() > 600 {
                "medium".into()
            } else {
                "low".into()
            },
            risk: if matches!(route, RoutingRoute::AskUser) {
                "elevated".into()
            } else {
                "normal".into()
            },
            project_memory_ids: vec![],
            knowledge_chunk_ids: vec![],
            plan_draft: vec![],
            redact: sensitive,
            privacy_reasons: if sensitive {
                vec!["possible_secret".into()]
            } else {
                vec!["rule_fallback".into()]
            },
        })?)
    }
}

pub async fn evaluate(
    inference: &dyn LocalInference,
    prompt: &str,
    timeout_ms: u64,
    cancel: CancellationToken,
) -> Result<RoutingDecision> {
    if timeout_ms == 0 {
        return Err(Error::Invalid("preflight timeout must be positive".into()));
    }
    let raw = tokio::select! {
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), inference.complete_json(prompt, cancel.clone())) => result.map_err(|_| Error::Provider("preflight router timed out".into()))??,
    };
    let decision: RoutingDecision = serde_json::from_str(&raw)
        .map_err(|err| Error::Invalid(format!("invalid preflight schema: {err}")))?;
    decision.validate()?;
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rule_router_is_advisory_and_cancellable() {
        let decision = evaluate(
            &RuleInference,
            "请设计一个方案",
            100,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(matches!(decision.route, RoutingRoute::LocalToolPlan));
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            evaluate(&RuleInference, "x", 100, cancelled).await,
            Err(Error::Cancelled)
        ));
    }
}
