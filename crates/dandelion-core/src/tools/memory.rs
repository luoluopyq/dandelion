//! Agent-side memory proposals. Same registry / loop as the other session
//! tools; deliberately NOT a memory write.
//!
//! The proposal lands in the Context OS ledger as `Authority::ModelCandidate`
//! (retrieval weight 0.0) and never reaches a prompt until the user confirms
//! it in the Project Memory page — the confirmation is the authorization, so
//! the tool itself stays gate-2 free. Hermes and Codex expose no agent memory
//! tool at all; this seam is additive and the confirm path is pre-existing.

use super::{Tool, ToolCtx};
use crate::acp::ToolKind;
use crate::context_os::{
    Authority, MemoryDomain, MemoryEventDraft, MemoryEventKind, MemoryScope, Sensitivity, SourceRef,
};
use crate::provider::types::{ToolSource, ToolSpec};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

/// Kept in step with the 4000-byte composer cap in the Project Memory page.
const MAX_CONTENT_BYTES: usize = 4_000;

pub struct MemoryProposeTool;

impl MemoryProposeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MemoryProposeTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Pure draft builder so the scope/authority choices stay unit-testable
/// without a live ToolCtx.
pub(crate) fn candidate_draft(
    workspace_root: &Path,
    session_id: &str,
    content: &str,
    kind: MemoryEventKind,
) -> MemoryEventDraft {
    MemoryEventDraft {
        domain: MemoryDomain::Internal,
        scope: MemoryScope {
            project_id: Some(workspace_root.to_string_lossy().into_owned()),
            ..MemoryScope::default()
        },
        kind,
        payload: json!({ "content": content }),
        source_ref: SourceRef {
            kind: "agent_proposal".into(),
            id: format!("{}:{}", session_id, uuid::Uuid::new_v4()),
            version: None,
        },
        // A proposal is advice, not knowledge: it only becomes a memory the
        // moment the user confirms it (confirm_memory_candidate rewrites the
        // authority to user_confirmed).
        authority: Authority::ModelCandidate,
        valid_from: None,
        valid_to: None,
        confidence: 0.5,
        sensitivity: Sensitivity::Internal,
        revision_of: None,
    }
}

#[async_trait]
impl Tool for MemoryProposeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_propose".into(),
            description: "Propose one durable cross-session memory for this project: \
a stable convention, a fixed fact about the codebase, or a user preference the user stated. \
The proposal is stored as PENDING and is used by no task until the user confirms it in the \
Project Memory page; never claim it was saved. \
One concise sentence per call (what is true, not what was done this session). \
Do not propose task state, file contents, or anything secret."
                .into(),
            source: ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "One self-contained sentence, ≤4000 bytes."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["fact", "preference"],
                        "description": "fact: stable truth about the project. preference: user-stated convention or taste. Default fact."
                    }
                },
                "required": ["content"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let Some(raw) = args.get("content").and_then(Value::as_str) else {
            return "Error: content is required".into();
        };
        let content = raw.trim();
        if content.is_empty() {
            return "Error: content is empty".into();
        }
        if content.len() > MAX_CONTENT_BYTES {
            return format!(
                "Error: content exceeds {MAX_CONTENT_BYTES} bytes ({})",
                content.len()
            );
        }
        let kind = match args.get("kind").and_then(Value::as_str) {
            Some("preference") => MemoryEventKind::Preference,
            _ => MemoryEventKind::Fact,
        };
        let draft = candidate_draft(ctx.workspace.root(), ctx.session_id.as_str(), content, kind);
        match ctx.store.append_memory_event_once(draft) {
            Ok(Some(event)) => format!(
                "Proposed memory {} (pending). It stays unused until the user confirms it \
in the Project Memory page. Do not repeat it as saved.",
                event.id
            ),
            // Same source ref can only come from a retry of this call.
            Ok(None) => "Duplicate proposal; already pending confirmation.".into(),
            Err(err) => format!("Error: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_is_candidate_scoped_to_workspace() {
        let draft = candidate_draft(
            Path::new("/work/proj"),
            "sess_1",
            "界面文案统一使用简体中文",
            MemoryEventKind::Preference,
        );
        assert_eq!(draft.authority, Authority::ModelCandidate);
        assert_eq!(draft.kind, MemoryEventKind::Preference);
        assert_eq!(
            draft.scope.project_id.as_deref(),
            Some("/work/proj"),
            "proposals must live in the same per-project scope the panel lists"
        );
        assert_eq!(draft.source_ref.kind, "agent_proposal");
        assert!(draft.source_ref.id.starts_with("sess_1:"));
        assert_eq!(
            draft.payload.get("content").and_then(|v| v.as_str()),
            Some("界面文案统一使用简体中文")
        );
        draft.validate().expect("draft must validate");
    }

    #[test]
    fn source_ids_are_unique_per_proposal() {
        let a = candidate_draft(Path::new("/p"), "s", "one", MemoryEventKind::Fact);
        let b = candidate_draft(Path::new("/p"), "s", "one", MemoryEventKind::Fact);
        assert_ne!(a.source_ref.id, b.source_ref.id);
    }
}
