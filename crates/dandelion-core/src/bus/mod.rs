//! In-process fan-out of ACP session updates.

use crate::acp::SessionNotification;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

pub const BUS_CAPACITY: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusFrame {
    /// Local monotonic id for JSONL ordering. Not an ACP field.
    pub seq: u64,
    pub ts: DateTime<Utc>,
    /// Always `session/update` for G0/G1. Extensions would use `_dandelion/session/update`.
    pub method: String,
    pub params: SessionNotification,
}

impl BusFrame {
    /// Host adapters filter before emit so a child `idle` cannot end the parent UI.
    pub fn belongs_to_session(&self, session_id: &crate::ids::SessionId) -> bool {
        &self.params.session_id == session_id
    }

    pub fn session_update(seq: u64, params: SessionNotification) -> Self {
        Self {
            seq,
            ts: Utc::now(),
            method: "session/update".into(),
            params,
        }
    }

    pub fn dandelion_update(seq: u64, params: SessionNotification) -> Self {
        Self {
            seq,
            ts: Utc::now(),
            method: "_dandelion/session/update".into(),
            params,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Bus {
    tx: broadcast::Sender<BusFrame>,
}

impl Bus {
    pub fn new() -> Self {
        Self::with_capacity(BUS_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let cap = capacity.max(1);
        let (tx, _) = broadcast::channel(cap);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BusFrame> {
        self.tx.subscribe()
    }

    pub fn publish(&self, frame: BusFrame) {
        let _ = self.tx.send(frame);
    }

    pub fn sender(&self) -> broadcast::Sender<BusFrame> {
        self.tx.clone()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::{ContentBlock, SessionNotification, SessionUpdate};
    use crate::ids::{MessageId, SessionId};

    #[tokio::test]
    async fn subscriber_receives_published_frame() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        let session_id = SessionId::new();
        bus.publish(BusFrame::session_update(
            1,
            SessionNotification {
                session_id: session_id.clone(),
                update: SessionUpdate::AgentMessageChunk {
                    message_id: MessageId::new(),
                    content: ContentBlock::text("ping"),
                },
            },
        ));
        let frame = rx.recv().await.expect("frame");
        assert_eq!(frame.seq, 1);
        assert_eq!(frame.method, "session/update");
        assert_eq!(frame.params.session_id, session_id);
    }

    #[tokio::test]
    async fn dandelion_update_uses_extension_method() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        let session_id = SessionId::new();
        bus.publish(BusFrame::dandelion_update(
            2,
            SessionNotification {
                session_id: session_id.clone(),
                update: SessionUpdate::job_update("cron_1", Some("tsk_child".into()), "fired"),
            },
        ));
        let frame = rx.recv().await.expect("frame");
        assert_eq!(frame.seq, 2);
        assert_eq!(frame.method, "_dandelion/session/update");
        assert_eq!(frame.params.session_id, session_id);
    }

    #[test]
    fn belongs_to_session_ignores_child_idle() {
        let parent = SessionId::parse("tsk_parent").unwrap();
        let child = SessionId::parse("tsk_child").unwrap();
        let frame = BusFrame::session_update(
            3,
            SessionNotification {
                session_id: child,
                update: SessionUpdate::idle("end_turn"),
            },
        );
        assert!(!frame.belongs_to_session(&parent));
        assert!(frame.belongs_to_session(&SessionId::parse("tsk_child").unwrap()));
    }
}
