use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// ACP `sessionId`. Same id is used as the product `taskId` so CLI and Tauri filter one stream.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn new() -> Self {
        Self(format!("tsk_{}", Uuid::new_v4().simple()))
    }

    pub fn parse(raw: impl Into<String>) -> crate::Result<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(crate::Error::Invalid("session id must not be empty".into()));
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(String);

impl MessageId {
    pub fn new() -> Self {
        Self(format!("msg_{}", Uuid::new_v4().simple()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolCallId(String);

impl ToolCallId {
    pub fn new() -> Self {
        Self(format!("call_{}", Uuid::new_v4().simple()))
    }

    pub fn parse(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        if raw.trim().is_empty() {
            Self::new()
        } else {
            Self(raw)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ToolCallId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
