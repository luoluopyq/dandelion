use super::events::{HookDecision, HookEvent};
use crate::config::Settings;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Duration;

pub trait Hook: Send + Sync {
    fn id(&self) -> &'static str;
    fn handle(&self, event: &HookEvent) -> HookDecision;
}

/// Test / gate hook: deny `write_file` on `pre_tool_call`.
pub struct DenyNamedTool {
    pub name: String,
}

impl Default for DenyNamedTool {
    fn default() -> Self {
        Self {
            name: "write_file".into(),
        }
    }
}

impl Hook for DenyNamedTool {
    fn id(&self) -> &'static str {
        "deny_named_tool"
    }

    fn handle(&self, event: &HookEvent) -> HookDecision {
        match event {
            HookEvent::PreToolCall { tool_name } if tool_name == &self.name => {
                HookDecision::deny(format!("deny_named_tool:{tool_name}"))
            }
            _ => HookDecision::Allow,
        }
    }
}

pub struct HookRuntime {
    hooks: Vec<Arc<dyn Hook>>,
    timeout: Duration,
}

impl HookRuntime {
    pub fn from_settings(settings: &Settings) -> Self {
        let mut hooks: Vec<Arc<dyn Hook>> = Vec::new();
        for id in &settings.hooks_enabled {
            if id == "deny_named_tool" {
                hooks.push(Arc::new(DenyNamedTool::default()));
            } else {
                tracing::warn!(hook = %id, "unknown hook id ignored");
            }
        }
        Self {
            hooks,
            timeout: Duration::from_millis(settings.hooks_timeout_ms),
        }
    }

    pub fn with_hooks(hooks: Vec<Arc<dyn Hook>>, timeout: Duration) -> Self {
        Self { hooks, timeout }
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub async fn fire(&self, event: HookEvent) -> HookDecision {
        let fail_closed = matches!(event, HookEvent::PreToolCall { .. });
        for hook in &self.hooks {
            let hook = hook.clone();
            let ev = event.clone();
            let timeout = self.timeout;
            let outcome = tokio::task::spawn_blocking(move || {
                catch_unwind(AssertUnwindSafe(|| hook.handle(&ev)))
            });
            let decision = match tokio::time::timeout(timeout, outcome).await {
                Ok(Ok(Ok(d))) => d,
                Ok(Ok(Err(_))) => {
                    if fail_closed {
                        HookDecision::deny("panic")
                    } else {
                        tracing::warn!("hook panicked (fail-open)");
                        HookDecision::Allow
                    }
                }
                Ok(Err(err)) => {
                    if fail_closed {
                        HookDecision::deny(format!("join: {err}"))
                    } else {
                        tracing::warn!(%err, "hook join failed (fail-open)");
                        HookDecision::Allow
                    }
                }
                Err(_) => {
                    if fail_closed {
                        HookDecision::deny("timeout")
                    } else {
                        tracing::warn!("hook timeout (fail-open)");
                        HookDecision::Allow
                    }
                }
            };
            match decision {
                HookDecision::Allow => {}
                other => return other,
            }
        }
        HookDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PanicHook;

    impl Hook for PanicHook {
        fn id(&self) -> &'static str {
            "panic"
        }
        fn handle(&self, event: &HookEvent) -> HookDecision {
            if matches!(event, HookEvent::PreToolCall { .. }) {
                panic!("boom");
            }
            HookDecision::Allow
        }
    }

    #[tokio::test]
    async fn deny_named_tool_blocks_write() {
        let rt = HookRuntime::with_hooks(
            vec![Arc::new(DenyNamedTool::default())],
            Duration::from_secs(1),
        );
        let d = rt
            .fire(HookEvent::PreToolCall {
                tool_name: "write_file".into(),
            })
            .await;
        assert!(matches!(d, HookDecision::Deny { .. }));
        let d = rt
            .fire(HookEvent::PreToolCall {
                tool_name: "read_file".into(),
            })
            .await;
        assert_eq!(d, HookDecision::Allow);
    }

    #[tokio::test]
    async fn pre_tool_panic_is_deny() {
        let rt = HookRuntime::with_hooks(vec![Arc::new(PanicHook)], Duration::from_secs(1));
        let d = rt
            .fire(HookEvent::PreToolCall {
                tool_name: "write_file".into(),
            })
            .await;
        assert!(matches!(d, HookDecision::Deny { reason } if reason == "panic"));
    }

    #[tokio::test]
    async fn pre_tool_timeout_is_deny() {
        struct Slow;
        impl Hook for Slow {
            fn id(&self) -> &'static str {
                "slow"
            }
            fn handle(&self, event: &HookEvent) -> HookDecision {
                if matches!(event, HookEvent::PreToolCall { .. }) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                HookDecision::Allow
            }
        }
        let rt = HookRuntime::with_hooks(vec![Arc::new(Slow)], Duration::from_millis(20));
        let d = rt
            .fire(HookEvent::PreToolCall {
                tool_name: "write_file".into(),
            })
            .await;
        assert!(matches!(d, HookDecision::Deny { reason } if reason == "timeout"));
    }

    struct AbortLlm;
    impl Hook for AbortLlm {
        fn id(&self) -> &'static str {
            "abort_llm"
        }
        fn handle(&self, event: &HookEvent) -> HookDecision {
            match event {
                HookEvent::PreLlmRequest { .. } => HookDecision::Abort {
                    reason: "stop".into(),
                },
                _ => HookDecision::Allow,
            }
        }
    }

    struct AlwaysPanic;
    impl Hook for AlwaysPanic {
        fn id(&self) -> &'static str {
            "always_panic"
        }
        fn handle(&self, _event: &HookEvent) -> HookDecision {
            panic!("nope");
        }
    }

    #[test]
    fn from_settings_unknown_id_ignored() {
        let mut s = Settings::defaults();
        s.hooks_enabled = vec!["not-a-hook".into(), "deny_named_tool".into()];
        let rt = HookRuntime::from_settings(&s);
        assert!(!rt.is_empty());
        let empty = HookRuntime::from_settings(&Settings::defaults());
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn pre_llm_abort_is_returned() {
        let rt = HookRuntime::with_hooks(vec![Arc::new(AbortLlm)], Duration::from_secs(1));
        let d = rt.fire(HookEvent::PreLlmRequest { iteration: 1 }).await;
        assert!(matches!(d, HookDecision::Abort { reason } if reason == "stop"));
    }

    #[tokio::test]
    async fn panic_on_session_start_is_fail_open() {
        let rt = HookRuntime::with_hooks(vec![Arc::new(AlwaysPanic)], Duration::from_secs(1));
        let d = rt
            .fire(HookEvent::SessionStart {
                session_id: "tsk_x".into(),
            })
            .await;
        assert_eq!(d, HookDecision::Allow);
    }

    #[tokio::test]
    async fn panic_on_load_tools_is_fail_open() {
        let rt = HookRuntime::with_hooks(vec![Arc::new(AlwaysPanic)], Duration::from_secs(1));
        let d = rt
            .fire(HookEvent::LoadTools {
                tool_names: vec!["read_file".into()],
            })
            .await;
        assert_eq!(d, HookDecision::Allow);
    }

    #[tokio::test]
    async fn timeout_on_post_tool_is_fail_open() {
        struct SlowPost;
        impl Hook for SlowPost {
            fn id(&self) -> &'static str {
                "slow_post"
            }
            fn handle(&self, event: &HookEvent) -> HookDecision {
                if matches!(event, HookEvent::PostToolCall { .. }) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                HookDecision::Allow
            }
        }
        let rt = HookRuntime::with_hooks(vec![Arc::new(SlowPost)], Duration::from_millis(20));
        let d = rt
            .fire(HookEvent::PostToolCall {
                tool_name: "read_file".into(),
                failed: false,
            })
            .await;
        assert_eq!(d, HookDecision::Allow);
    }

    #[tokio::test]
    async fn first_deny_stops_later_hooks() {
        struct SecondPanic;
        impl Hook for SecondPanic {
            fn id(&self) -> &'static str {
                "second"
            }
            fn handle(&self, event: &HookEvent) -> HookDecision {
                if matches!(event, HookEvent::PreToolCall { .. }) {
                    panic!("should not run");
                }
                HookDecision::Allow
            }
        }
        let rt = HookRuntime::with_hooks(
            vec![Arc::new(DenyNamedTool::default()), Arc::new(SecondPanic)],
            Duration::from_secs(1),
        );
        let d = rt
            .fire(HookEvent::PreToolCall {
                tool_name: "write_file".into(),
            })
            .await;
        assert!(matches!(d, HookDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn pre_tool_call_is_reentrant() {
        let rt = Arc::new(HookRuntime::with_hooks(
            vec![Arc::new(DenyNamedTool::default())],
            Duration::from_secs(1),
        ));
        let a = rt.clone();
        let b = rt.clone();
        let (da, db) = tokio::join!(
            a.fire(HookEvent::PreToolCall {
                tool_name: "write_file".into(),
            }),
            b.fire(HookEvent::PreToolCall {
                tool_name: "read_file".into(),
            }),
        );
        assert!(matches!(da, HookDecision::Deny { .. }));
        assert_eq!(db, HookDecision::Allow);
    }

    #[tokio::test]
    async fn subagent_start_panic_is_fail_open() {
        struct PanicOnSubagent;
        impl Hook for PanicOnSubagent {
            fn id(&self) -> &'static str {
                "panic_subagent"
            }
            fn handle(&self, event: &HookEvent) -> HookDecision {
                if matches!(event, HookEvent::SubagentStart { .. }) {
                    panic!("boom");
                }
                HookDecision::Allow
            }
        }
        let rt = HookRuntime::with_hooks(vec![Arc::new(PanicOnSubagent)], Duration::from_secs(1));
        let d = rt
            .fire(HookEvent::SubagentStart {
                parent_session_id: "tsk_p".into(),
                child_session_id: "tsk_c".into(),
                goal: "g".into(),
            })
            .await;
        assert_eq!(d, HookDecision::Allow);
    }
}
