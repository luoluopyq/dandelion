use dandelion_core::acp::{ContentBlock, SessionNotification, SessionUpdate};
use dandelion_core::bus::BusFrame;
use dandelion_core::ids::{MessageId, SessionId};

#[test]
fn golden_agent_message_chunk_keys() {
    let frame = BusFrame::session_update(
        1,
        SessionNotification {
            session_id: SessionId::parse("tsk_01").unwrap(),
            update: SessionUpdate::AgentMessageChunk {
                message_id: MessageId::new(),
                content: ContentBlock::text("Hello"),
            },
        },
    );
    let v = serde_json::to_value(&frame).unwrap();
    assert_eq!(v["method"], "session/update");
    assert!(v["params"]["sessionId"]
        .as_str()
        .unwrap()
        .starts_with("tsk_"));
    assert_eq!(
        v["params"]["update"]["sessionUpdate"],
        "agent_message_chunk"
    );
    assert!(v["params"]["update"].get("messageId").is_some());
}

#[test]
fn golden_tool_call_update_keys() {
    use dandelion_core::acp::{ToolCallStatus, ToolCallUpdate, ToolKind};
    use dandelion_core::ids::ToolCallId;
    let update = SessionUpdate::tool_call(ToolCallUpdate {
        tool_call_id: ToolCallId::parse("call_1"),
        title: Some("read_file".into()),
        kind: Some(ToolKind::Read),
        status: Some(ToolCallStatus::Completed),
        raw_input: None,
        content: None,
    });
    let v = serde_json::to_value(&update).unwrap();
    assert_eq!(v["sessionUpdate"], "tool_call_update");
    assert_eq!(v["toolCallId"], "call_1");
    assert_eq!(v["stopReason"], serde_json::Value::Null);
}

#[test]
fn golden_stop_reason_idle() {
    let v = serde_json::to_value(SessionUpdate::idle("end_turn")).unwrap();
    assert_eq!(v["sessionUpdate"], "state_update");
    assert_eq!(v["stopReason"], "end_turn");
    assert_eq!(v["state"], "idle");
}

#[test]
fn golden_job_update_keys() {
    let frame = BusFrame::dandelion_update(
        3,
        SessionNotification {
            session_id: SessionId::parse("tsk_cron").unwrap(),
            update: SessionUpdate::job_update("cron_1", Some("tsk_child".into()), "fired"),
        },
    );
    let v = serde_json::to_value(&frame).unwrap();
    assert_eq!(v["method"], "_dandelion/session/update");
    assert_eq!(v["params"]["update"]["sessionUpdate"], "_job_update");
    assert_eq!(v["params"]["update"]["jobId"], "cron_1");
    assert_eq!(v["params"]["update"]["childSessionId"], "tsk_child");
    assert_eq!(v["params"]["update"]["phase"], "fired");
}
