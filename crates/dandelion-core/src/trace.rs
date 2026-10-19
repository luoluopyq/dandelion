//! Low-sensitivity execution trace primitives for deterministic evaluation.
//!
//! Trace events deliberately contain control-plane metadata only.  Prompt text,
//! model output, and raw tool arguments remain in the existing session ledger
//! and are never copied into this stream.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub String);

impl RunId {
    pub fn new() -> Self {
        Self(format!("run_{}", uuid::Uuid::new_v4().simple()))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEvent {
    pub run_id: String,
    pub session_id: String,
    pub sequence: u64,
    pub kind: String,
    pub detail: Value,
    pub created_at: String,
}

impl TraceEvent {
    pub fn new(
        run_id: &RunId,
        session_id: impl Into<String>,
        kind: impl Into<String>,
        detail: Value,
    ) -> Self {
        Self {
            run_id: run_id.0.clone(),
            session_id: session_id.into(),
            sequence: 0,
            kind: kind.into(),
            detail: redact_value(&detail),
            created_at: Utc::now().to_rfc3339(),
        }
    }
}

/// Remove values whose field names commonly contain credentials before a trace
/// reaches SQLite or any future export path.
pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let sensitive = [
                        "authorization",
                        "token",
                        "secret",
                        "password",
                        "cookie",
                        "api_key",
                        "apikey",
                    ]
                    .iter()
                    .any(|needle| lower.contains(needle));
                    (
                        key.clone(),
                        if sensitive {
                            Value::String("[REDACTED]".into())
                        } else {
                            redact_value(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_value).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_nested_credentials_without_dropping_safe_metadata() {
        let out = redact_value(&json!({
            "tool": "terminal",
            "headers": {"Authorization": "Bearer very-secret"},
            "items": [{"apiKey": "abc", "ok": true}]
        }));
        assert_eq!(out["tool"], "terminal");
        assert_eq!(out["headers"]["Authorization"], "[REDACTED]");
        assert_eq!(out["items"][0]["apiKey"], "[REDACTED]");
        assert_eq!(out["items"][0]["ok"], true);
    }
}
