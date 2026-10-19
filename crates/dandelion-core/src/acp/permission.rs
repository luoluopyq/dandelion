use crate::ids::{SessionId, ToolCallId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionRequest {
    pub session_id: SessionId,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<PermissionSubject>,
    pub options: Vec<PermissionOption>,
}

impl RequestPermissionRequest {
    pub fn g1_options() -> Vec<PermissionOption> {
        vec![
            PermissionOption {
                option_id: "allow".into(),
                name: "Allow".into(),
                kind: "allow_once".into(),
            },
            PermissionOption {
                option_id: "allow_turn".into(),
                name: "Allow for this session".into(),
                kind: "allow_session".into(),
            },
            PermissionOption {
                option_id: "deny".into(),
                name: "Deny".into(),
                kind: "reject_once".into(),
            },
        ]
    }

    pub fn tool_call(session_id: SessionId, title: String, tool_call_id: ToolCallId) -> Self {
        Self {
            session_id,
            title,
            description: None,
            subject: Some(PermissionSubject {
                type_: "tool_call".into(),
                tool_call_id: Some(tool_call_id),
            }),
            options: Self::g1_options(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSubject {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestPermissionOutcome {
    Selected { option_id: String },
    Cancelled,
    Denied,
}

impl RequestPermissionOutcome {
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Selected { option_id } if option_id == "allow" || option_id == "allow_turn")
    }
}
