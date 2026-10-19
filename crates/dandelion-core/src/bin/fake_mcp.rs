//! Test/dev-only fake MCP server over stdio (newline-delimited JSON-RPC,
//! per the MCP spec — no rmcp needed on this side).
//!
//! `FAKE_MCP_TOOL_COUNT` (default 2) controls how many `tool_N` entries
//! `tools/list` returns, plus one always-present `screenshot_tool` marked
//! `readOnlyHint`. Integration tests spawn it via `env!("CARGO_BIN_EXE_fake_mcp")`
//! to exercise real list/call round-trips — the H3 lazy-catalog exit
//! condition needs a fleet that actually speaks MCP.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn tools(count: usize) -> Vec<Value> {
    let mut out = vec![json!({
        "name": "screenshot_tool",
        "description": "capture a screenshot of the screen",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": true},
    })];
    for i in 0..count {
        out.push(json!({
            "name": format!("tool_{i}"),
            "description": format!("generic catalog filler tool number {i}"),
            "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
        }));
    }
    out
}

fn main() {
    let count: usize = std::env::var("FAKE_MCP_TOOL_COUNT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(2);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Notifications carry no id and must not be answered.
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": msg
                        .pointer("/params/protocolVersion")
                        .and_then(Value::as_str)
                        .unwrap_or("2025-06-18"),
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "dandelion-fake-mcp", "version": "0.1.0"},
                },
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0", "id": id,
                "result": {"tools": tools(count)},
            }),
            "tools/call" => {
                let name = msg
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string();
                json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "content": [{"type": "text", "text": format!("fake ok from {name}")}],
                        "isError": false,
                    },
                })
            }
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            _ => json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("fake-mcp: unsupported method {method}")},
            }),
        };
        if writeln!(out, "{response}").is_err() {
            break;
        }
        let _ = out.flush();
    }
}
