pub mod discover;
pub mod inject;

pub use discover::{
    context_status, discover_session_start, render_project_segment, ContextKind, ContextStatus,
    ProjectBundle,
};
pub use inject::ContextSession;
