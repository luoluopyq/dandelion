//! G6 C14: stdio JSON-RPC ACP Agent host over `Runtime`.

use dandelion_core::acp::SessionUpdate;
use dandelion_core::{PromptRequest, Runtime, SessionId};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[serde(default = "rpc_version")]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn rpc_version() -> String {
    "2.0".into()
}

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let rt = Runtime::new(Arc::new(crate::app::cli_host()))?;
    let mut bus = rt.subscribe();
    let rt_bus = rt.clone();
    tokio::spawn(async move {
        loop {
            match bus.recv().await {
                Ok(frame) => {
                    if frame.method != "session/update" {
                        continue;
                    }
                    // Pass through unknown sessionUpdate shapes via serde Value if needed.
                    let _ = &rt_bus;
                    let note = json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": frame.params,
                    });
                    if let Ok(line) = serde_json::to_string(&note) {
                        let mut out = io::stdout().lock();
                        let _ = writeln!(out, "{line}");
                        let _ = out.flush();
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: RpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(err) => {
                write_err(None, -32700, &format!("parse error: {err}"))?;
                continue;
            }
        };
        if req.jsonrpc != "2.0" {
            write_err(req.id, -32600, "jsonrpc must be 2.0")?;
            continue;
        }
        match req.method.as_str() {
            "initialize" | "authenticate" => {
                write_ok(req.id, json!({ "protocolVersion": 1 }))?;
            }
            "session/new" => {
                let sid = rt.new_session(None);
                write_ok(req.id, json!({ "sessionId": sid.to_string() }))?;
            }
            "session/prompt" => {
                let sid = match req.params.get("sessionId").and_then(|v| v.as_str()) {
                    Some(s) => SessionId::parse(s)?,
                    None => {
                        write_err(req.id, -32602, "sessionId required")?;
                        continue;
                    }
                };
                let text = extract_prompt_text(&req.params);
                if text.trim().is_empty() {
                    write_err(req.id, -32602, "prompt text required")?;
                    continue;
                }
                let id = req.id.clone();
                let rt2 = rt.clone();
                tokio::spawn(async move {
                    let mut pr = PromptRequest::new(sid, text);
                    pr.persist_bindings = false;
                    match rt2.prompt(pr).await {
                        Ok(done) => {
                            let _ = write_ok(
                                id,
                                json!({ "sessionId": done.to_string(), "stopReason": "end_turn" }),
                            );
                        }
                        Err(err) => {
                            let _ = write_err(id, -32000, &err.to_string());
                        }
                    }
                });
            }
            "session/cancel" => {
                let sid = match req.params.get("sessionId").and_then(|v| v.as_str()) {
                    Some(s) => SessionId::parse(s)?,
                    None => {
                        write_err(req.id, -32602, "sessionId required")?;
                        continue;
                    }
                };
                let id = req.id.clone();
                match rt.cancel(sid).await {
                    Ok(()) => write_ok(id, json!({}))?,
                    Err(err) => write_err(id, -32000, &err.to_string())?,
                }
            }
            other => {
                // Unknown methods: respond method not found (do not crash).
                // Clients may probe; keep Agent alive.
                let _ = other;
                write_err(req.id, -32601, "method not found")?;
            }
        }
    }
    Ok(())
}

fn extract_prompt_text(params: &Value) -> String {
    if let Some(s) = params.get("prompt").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(arr) = params.get("prompt").and_then(|v| v.as_array()) {
        let mut out = String::new();
        for block in arr {
            if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            } else if let Some(t) = block.as_str() {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
        return out;
    }
    params
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn write_ok(id: Option<Value>, result: Value) -> io::Result<()> {
    let mut map = serde_json::Map::new();
    map.insert("jsonrpc".into(), Value::String("2.0".into()));
    if let Some(id) = id {
        map.insert("id".into(), id);
    }
    map.insert("result".into(), result);
    let mut out = io::stdout().lock();
    writeln!(out, "{}", Value::Object(map))?;
    out.flush()
}

fn write_err(id: Option<Value>, code: i64, message: &str) -> io::Result<()> {
    let mut map = serde_json::Map::new();
    map.insert("jsonrpc".into(), Value::String("2.0".into()));
    if let Some(id) = id {
        map.insert("id".into(), id);
    }
    map.insert("error".into(), json!({ "code": code, "message": message }));
    let mut out = io::stdout().lock();
    writeln!(out, "{}", Value::Object(map))?;
    out.flush()
}

#[allow(dead_code)]
fn _forward_unknown_update(update: &SessionUpdate) -> Value {
    serde_json::to_value(update).unwrap_or(Value::Null)
}
