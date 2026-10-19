use super::{Tool, ToolCtx};
use crate::acp::ToolKind;
use crate::config::Settings;
use crate::jobs::LiveOpts;
use crate::provider::types::ToolSpec;
use crate::sandbox::sandbox_error_for_model;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TerminalTool {
    default_timeout: u64,
    max_timeout: u64,
}

impl TerminalTool {
    pub fn new(settings: &Settings) -> Self {
        Self {
            default_timeout: settings.terminal_foreground_timeout_sec,
            max_timeout: settings.terminal_foreground_timeout_max_sec,
        }
    }
}

#[async_trait]
impl Tool for TerminalTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal".into(),
            description: "Run a shell command in the workspace (PowerShell on Windows, bash/sh on macOS/Linux). Use background=true for long jobs and process poll/log/kill to manage them. Do not use this to read or edit files.".into(),
            source: crate::provider::types::ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "background": { "type": "boolean", "default": false },
                    "timeout": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": self.max_timeout
                    },
                    "workdir": { "type": "string" }
                },
                "required": ["command"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        if args.get("pty").is_some() || args.get("notify").is_some() {
            return "Error: pty/notify not supported in G1; use process poll/log".into();
        }
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return "Error: invalid arguments: missing command".into(),
        };
        let workdir = match args.get("workdir").and_then(|v| v.as_str()) {
            Some(w) => match ctx.workspace.resolve(w) {
                Ok(p) => p,
                Err(err) => return sandbox_error_for_model(&err),
            },
            None => ctx.workspace.root().to_path_buf(),
        };
        if !workdir.is_dir() {
            return format!("Error: workdir is not a directory: {}", workdir.display());
        }
        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let live = Some(LiveOpts {
            emit: ctx.emit.clone(),
            tool_call_id: ctx.tool_call_id.clone(),
            publish_tool_call: true,
            chunk_bytes: ctx.settings.terminal_live_chunk_bytes,
            flush_ms: ctx.settings.terminal_live_flush_ms,
        });
        if background {
            return ctx
                .jobs
                .spawn_background(&ctx.session_id, command, &workdir, live);
        }
        let timeout = args
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.default_timeout);
        if timeout == 0 {
            return "Error: timeout must be >= 1".into();
        }
        if timeout > self.max_timeout {
            return format!(
                "Error: foreground timeout max is {}; use background=true",
                self.max_timeout
            );
        }
        ctx.jobs
            .run_foreground(
                &ctx.session_id,
                command,
                &workdir,
                timeout,
                ctx.settings.as_ref(),
                ctx.cancel.clone(),
                live,
            )
            .await
    }
}
