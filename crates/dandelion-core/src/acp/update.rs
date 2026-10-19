use super::content::ContentBlock;
use super::tool_call::ToolCallUpdate;
use crate::ids::{MessageId, SessionId};
use serde::{Deserialize, Serialize};

pub const STOP_REASONS: &[&str] = &["end_turn", "cancelled", "error", "max_iterations"];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    pub session_id: SessionId,
    pub update: SessionUpdate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate")]
pub enum SessionUpdate {
    #[serde(rename = "user_message_chunk")]
    UserMessageChunk {
        #[serde(rename = "messageId")]
        message_id: MessageId,
        content: ContentBlock,
    },
    #[serde(rename = "agent_message_chunk")]
    AgentMessageChunk {
        #[serde(rename = "messageId")]
        message_id: MessageId,
        content: ContentBlock,
    },
    /// Dandelion extension: image attachments persisted for the user message
    /// just streamed. `files` are `<data_dir>`-relative paths (never base64
    /// on the bus); hosts render them via `attachment_data_url`. Human-facing
    /// preview only — this never enters inference.
    #[serde(rename = "_user_images")]
    UserImages {
        #[serde(rename = "messageId")]
        message_id: MessageId,
        files: Vec<String>,
    },
    /// Dandelion extension for model reasoning deltas. Hosts must not merge
    /// this stream into the user-facing assistant message body.
    #[serde(rename = "_reasoning_delta")]
    ReasoningDelta {
        #[serde(rename = "messageId")]
        message_id: MessageId,
        text: String,
    },
    /// Dandelion extension: size-only progress for streamed tool arguments.
    /// Never includes the unfinished arguments or dispatches a tool.
    #[serde(rename = "_tool_call_progress")]
    ToolCallProgress {
        name: String,
        #[serde(rename = "argumentBytes")]
        argument_bytes: usize,
    },
    #[serde(rename = "tool_call_update")]
    ToolCallUpdate {
        #[serde(flatten)]
        update: ToolCallUpdate,
    },
    #[serde(rename = "state_update")]
    StateUpdate {
        state: SessionState,
        #[serde(rename = "stopReason", skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
    },
    /// G3 Cron. Prefer publishing with method `_dandelion/session/update`.
    #[serde(rename = "_job_update")]
    JobUpdate {
        #[serde(rename = "jobId")]
        job_id: String,
        #[serde(rename = "childSessionId", skip_serializing_if = "Option::is_none")]
        child_session_id: Option<String>,
        phase: String,
    },
    /// Wiki onboarding: a `wiki_propose` in a session without a bound project
    /// persisted a draft instead of writing anything. The desktop renders a
    /// project picker for this draft (choose / register / create + cancel).
    /// Carried on the REAL session so the prompt shows inside that chat.
    #[serde(rename = "_wiki_draft")]
    WikiDraft {
        #[serde(rename = "draftId")]
        draft_id: String,
        kind: String,
        content: String,
        mode: String,
    },
    /// C17 wiki compile. Prefer `_dandelion/session/update`; never inject into inference.
    /// `workspace` names the project being compiled so multi-project UIs can
    /// attribute "updating / failed" feedback to the right Wiki page.
    #[serde(rename = "_wiki_update")]
    WikiUpdate {
        phase: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    /// G5 C13 live terminal pane. G1 still returns Shell text via `tool_call_update`.
    #[serde(rename = "terminal_update")]
    TerminalUpdate {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        background: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<TerminalOutput>,
        #[serde(rename = "exitStatus", skip_serializing_if = "Option::is_none")]
        exit_status: Option<TerminalExitStatus>,
    },
    #[serde(rename = "terminal_output_chunk")]
    TerminalOutputChunk {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        data: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalOutput {
    pub data: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalExitStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

impl SessionUpdate {
    pub fn tool_call(update: ToolCallUpdate) -> Self {
        Self::ToolCallUpdate { update }
    }

    pub fn running() -> Self {
        Self::StateUpdate {
            state: SessionState::Running,
            stop_reason: None,
        }
    }

    pub fn queued() -> Self {
        Self::StateUpdate {
            state: SessionState::Queued,
            stop_reason: None,
        }
    }

    pub fn idle(stop_reason: &str) -> Self {
        Self::StateUpdate {
            state: SessionState::Idle,
            stop_reason: Some(stop_reason.to_string()),
        }
    }

    pub fn job_update(
        job_id: impl Into<String>,
        child_session_id: Option<String>,
        phase: &str,
    ) -> Self {
        Self::JobUpdate {
            job_id: job_id.into(),
            child_session_id,
            phase: phase.to_string(),
        }
    }

    pub fn wiki_update(
        phase: &str,
        version: Option<u32>,
        message: Option<String>,
        workspace: Option<String>,
    ) -> Self {
        Self::WikiUpdate {
            phase: phase.to_string(),
            version,
            message,
            workspace,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Running,
    Queued,
    Idle,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MessageId;

    #[test]
    fn agent_chunk_matches_acp_discriminator() {
        let update = SessionUpdate::AgentMessageChunk {
            message_id: MessageId::new(),
            content: ContentBlock::text("hello"),
        };
        let value = serde_json::to_value(&update).expect("json");
        assert_eq!(value["sessionUpdate"], "agent_message_chunk");
        assert_eq!(value["content"]["type"], "text");
        assert_eq!(value["content"]["text"], "hello");
        assert!(value.get("messageId").is_some());
    }

    #[test]
    fn state_update_idle_roundtrip() {
        let update = SessionUpdate::idle("end_turn");
        let json = serde_json::to_string(&update).expect("to json");
        let back: SessionUpdate = serde_json::from_str(&json).expect("from json");
        match back {
            SessionUpdate::StateUpdate { state, stop_reason } => {
                assert!(matches!(state, SessionState::Idle));
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn tool_call_update_keys() {
        use crate::acp::{ToolCallStatus, ToolKind};
        use crate::ids::ToolCallId;
        let update = SessionUpdate::tool_call(crate::acp::ToolCallUpdate {
            tool_call_id: ToolCallId::parse("call_1"),
            title: Some("read_file".into()),
            kind: Some(ToolKind::Read),
            status: Some(ToolCallStatus::Pending),
            raw_input: Some(serde_json::json!({"path": "README.md"})),
            content: None,
        });
        let value = serde_json::to_value(&update).expect("json");
        assert_eq!(value["sessionUpdate"], "tool_call_update");
        assert_eq!(value["toolCallId"], "call_1");
        assert_eq!(value["status"], "pending");
        assert_eq!(value["kind"], "read");
    }

    #[test]
    fn job_update_keys() {
        let update = SessionUpdate::job_update("cron_1", Some("tsk_child".into()), "fired");
        let value = serde_json::to_value(&update).expect("json");
        assert_eq!(value["sessionUpdate"], "_job_update");
        assert_eq!(value["jobId"], "cron_1");
        assert_eq!(value["childSessionId"], "tsk_child");
        assert_eq!(value["phase"], "fired");
        let idle = SessionUpdate::job_update("cron_1", None, "idle");
        let value = serde_json::to_value(&idle).expect("json");
        assert!(value.get("childSessionId").is_none());
    }

    #[test]
    fn terminal_update_keys() {
        let update = SessionUpdate::TerminalUpdate {
            terminal_id: "proc_1".into(),
            command: Some("echo hi".into()),
            cwd: Some("/tmp".into()),
            background: Some(true),
            output: None,
            exit_status: Some(TerminalExitStatus {
                exit_code: Some(0),
                signal: None,
            }),
        };
        let value = serde_json::to_value(&update).expect("json");
        assert_eq!(value["sessionUpdate"], "terminal_update");
        assert_eq!(value["terminalId"], "proc_1");
        assert_eq!(value["command"], "echo hi");
        assert_eq!(value["background"], true);
        assert_eq!(value["exitStatus"]["exitCode"], 0);
        let chunk = SessionUpdate::TerminalOutputChunk {
            terminal_id: "proc_1".into(),
            data: crate::acp::encode_base64(b"hi"),
        };
        let value = serde_json::to_value(&chunk).expect("json");
        assert_eq!(value["sessionUpdate"], "terminal_output_chunk");
        assert_eq!(value["data"], "aGk=");
    }
}
