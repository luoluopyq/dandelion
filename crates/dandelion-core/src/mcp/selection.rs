//! H3 selection: the per-run activated set of external MCP tools.
//!
//! One [`ToolSelection`] lives exactly as long as the prompt that created
//! it, so activations expire with the run — external tools never stay
//! resident in later turns unless re-selected (the plan's `expire_turn`
//! guarantee, implemented as run scope rather than a timer). Counts and
//! schema bytes are capped so a search can never bloat the next model
//! request beyond what direct injection would have allowed.

use super::schema::{function_object_bytes, SchemaCandidate};
use crate::config::Settings;
use std::collections::HashSet;
use std::sync::Mutex;

#[derive(Default)]
struct Active {
    names: HashSet<String>,
    bytes: usize,
}

/// Run-scoped activation ledger shared by `find_tools` and the loop.
#[derive(Default)]
pub struct ToolSelection {
    active: Mutex<Active>,
}

/// Outcome of one activation attempt, ready to render for the model.
#[derive(Debug, Default)]
pub struct ActivationReport {
    pub activated: Vec<SchemaCandidate>,
    /// Already callable (idempotent re-search).
    pub already: Vec<SchemaCandidate>,
    /// Rejected with a human-readable budget reason, in search order.
    pub rejected: Vec<(SchemaCandidate, String)>,
}

impl ToolSelection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_count(&self) -> usize {
        self.active.lock().expect("lock").names.len()
    }

    pub fn is_active(&self, name: &str) -> bool {
        self.active.lock().expect("lock").names.contains(name)
    }

    /// Admit candidates within the per-call count cap
    /// (`mcp_activate_max_per_turn`) and the run's total schema soft cap
    /// (`mcp_max_schema_bytes`). Order follows the search ranking; entries
    /// past a budget are rejected with reasons, never silently dropped.
    pub fn activate(
        &self,
        candidates: &[SchemaCandidate],
        settings: &Settings,
    ) -> ActivationReport {
        let mut report = ActivationReport::default();
        let mut active = self.active.lock().expect("lock");
        let cap = settings.mcp_activate_max_per_turn.max(1) as usize;
        let soft = settings.mcp_max_schema_bytes;
        let mut admitted_this_call = 0usize;
        for c in candidates {
            if active.names.contains(&c.name) {
                report.already.push(c.clone());
                continue;
            }
            if admitted_this_call >= cap {
                report
                    .rejected
                    .push((c.clone(), format!("per-search cap {cap} reached")));
                continue;
            }
            let bytes = function_object_bytes(c);
            if soft > 0 && active.bytes.saturating_add(bytes) > soft {
                report.rejected.push((
                    c.clone(),
                    format!("over {soft}-byte total schema budget ({bytes} bytes needed)"),
                ));
                continue;
            }
            active.bytes += bytes;
            active.names.insert(c.name.clone());
            admitted_this_call += 1;
            report.activated.push(c.clone());
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cand(tool: &str) -> SchemaCandidate {
        SchemaCandidate {
            server_id: "s".into(),
            remote_name: tool.into(),
            name: format!("mcp__s__{tool}"),
            description: "d".into(),
            parameters: json!({"type": "object"}),
            read_only: true,
        }
    }

    #[test]
    fn per_search_count_cap() {
        let s = Settings::defaults(); // default cap 12
        let sel = ToolSelection::new();
        let batch: Vec<_> = (0..30).map(|i| cand(&format!("t{i}"))).collect();
        let r = sel.activate(&batch, &s);
        assert_eq!(r.activated.len(), 12);
        assert_eq!(r.rejected.len(), 18);
        assert!(r.rejected[0].1.contains("per-search cap"));
        assert_eq!(sel.active_count(), 12);
    }

    #[test]
    fn total_schema_byte_budget_across_searches() {
        let mut s = Settings::defaults();
        s.mcp_max_schema_bytes = 200;
        s.mcp_max_tool_schema_bytes = 16_384;
        let sel = ToolSelection::new();
        let r1 = sel.activate(&[cand("a")], &s);
        assert_eq!(r1.activated.len(), 1);
        let big = {
            let mut c = cand("big");
            c.description = "y".repeat(180);
            c
        };
        let r2 = sel.activate(&[big], &s);
        assert!(r2.activated.is_empty());
        assert!(r2.rejected[0].1.contains("budget"));
        assert_eq!(sel.active_count(), 1);
    }

    #[test]
    fn re_activate_is_idempotent_not_double_counted() {
        let s = Settings::defaults();
        let sel = ToolSelection::new();
        sel.activate(&[cand("a")], &s);
        let r = sel.activate(&[cand("a")], &s);
        assert!(r.activated.is_empty());
        assert_eq!(r.already.len(), 1);
        assert_eq!(sel.active_count(), 1);
        assert!(sel.is_active("mcp__s__a"));
    }
}
