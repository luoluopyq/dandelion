use super::{Tool, ToolCtx};
use crate::acp::ToolKind;
use crate::config::Settings;
use crate::provider::types::ToolSpec;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProcessTool {
    default_log_lines: u32,
}

impl ProcessTool {
    pub fn new(settings: &Settings) -> Self {
        Self {
            default_log_lines: settings.process_log_default_lines,
        }
    }
}

#[async_trait]
impl Tool for ProcessTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "process".into(),
            description:
                "Manage background terminal jobs started in this session: list, poll, log, kill."
                    .into(),
            source: crate::provider::types::ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "poll", "log", "kill"] },
                    "session_id": { "type": "string" },
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer" }
                },
                "required": ["action"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return "Error: invalid arguments: missing action".into(),
        };
        match action {
            "wait" | "write" | "submit" | "close" => "Error: action not supported in G1".into(),
            "list" => ctx.jobs.list(&ctx.session_id),
            "poll" | "log" | "kill" => {
                let Some(id) = args.get("session_id").and_then(|v| v.as_str()) else {
                    return "Error: invalid arguments: session_id required".into();
                };
                match action {
                    "poll" => ctx.jobs.poll(id, ctx.settings.terminal_output_max_bytes),
                    "log" => {
                        let limit = args
                            .get("limit")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(self.default_log_lines as u64)
                            as u32;
                        let offset = args.get("offset").and_then(|v| v.as_i64());
                        ctx.jobs.log(
                            id,
                            offset,
                            limit.max(1),
                            ctx.settings.terminal_output_max_bytes,
                        )
                    }
                    "kill" => ctx.jobs.kill(id),
                    _ => unreachable!(),
                }
            }
            other => format!("Error: unknown action '{other}'"),
        }
    }
}
