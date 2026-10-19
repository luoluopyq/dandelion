//! ACP v2-shaped session updates (local types + golden JSON). Official crate is G2.

pub mod b64;
pub mod content;
pub mod permission;
pub mod tool_call;
pub mod update;

pub use b64::{decode_base64, encode_base64};
pub use content::ContentBlock;
pub use permission::{
    PermissionOption, PermissionSubject, RequestPermissionOutcome, RequestPermissionRequest,
};
pub use tool_call::{ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolKind};
pub use update::{
    SessionNotification, SessionState, SessionUpdate, TerminalExitStatus, TerminalOutput,
    STOP_REASONS,
};
