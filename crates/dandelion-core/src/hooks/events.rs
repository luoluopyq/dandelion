//! Hook events G2 actually fires. `on_subagent_*` exist on the enum but are never sent.

#[derive(Clone, Debug)]
pub enum HookEvent {
    SessionStart {
        session_id: String,
    },
    LoadTools {
        tool_names: Vec<String>,
    },
    PreLlmRequest {
        iteration: u32,
    },
    PreToolCall {
        tool_name: String,
    },
    PostToolCall {
        tool_name: String,
        failed: bool,
    },
    SubagentStart {
        parent_session_id: String,
        child_session_id: String,
        goal: String,
    },
    SubagentEnd {
        parent_session_id: String,
        child_session_id: String,
        status: String,
        duration_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookDecision {
    Allow,
    /// fail-closed (`pre_tool_call` only)
    Deny {
        reason: String,
    },
    /// fail-open abort of this `complete` (`pre_llm_request`)
    Abort {
        reason: String,
    },
}

impl HookDecision {
    pub fn allow() -> Self {
        Self::Allow
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_helper_and_equality() {
        assert_eq!(HookDecision::allow(), HookDecision::Allow);
        assert_eq!(
            HookDecision::deny("timeout"),
            HookDecision::Deny {
                reason: "timeout".into()
            }
        );
        assert_ne!(
            HookDecision::Abort {
                reason: "stop".into()
            },
            HookDecision::Allow
        );
    }
}
