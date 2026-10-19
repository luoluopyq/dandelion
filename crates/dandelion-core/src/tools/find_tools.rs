//! H3 `find_tools`: the single lightweight built-in that reaches the external
//! MCP catalog when the fleet is too wide to inject (see `mcp/catalog.rs`).
//!
//! Registered only when a prompt's MCP snapshot is lazy. A search activates
//! its hits within the run's count/byte budgets and pushes real dispatching
//! proxies into the registry's activated layer, so the tools become callable
//! on the *next* iteration of the same loop — no second loop, no separate
//! event protocol. Activations live exactly as long as the prompt (the
//! registry is built per prompt), which is the plan's `expire_turn` rule.

use super::{Tool, ToolCtx, ToolRegistry};
use crate::acp::ToolKind;
use crate::mcp::{McpCatalog, McpManager, ToolSelection};
use crate::provider::types::ToolSpec;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::{Arc, Weak};

pub struct FindToolsTool {
    catalog: McpCatalog,
    selection: Arc<ToolSelection>,
    manager: Arc<McpManager>,
    registry: Weak<ToolRegistry>,
}

impl FindToolsTool {
    pub fn new(
        catalog: McpCatalog,
        selection: Arc<ToolSelection>,
        manager: Arc<McpManager>,
        registry: Weak<ToolRegistry>,
    ) -> Self {
        Self {
            catalog,
            selection,
            manager,
            registry,
        }
    }
}

#[async_trait]
impl Tool for FindToolsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "find_tools".to_string(),
            description: "Search external MCP tools that are not in your tool list yet, and activate the matches. Activated tools become callable in your NEXT message (their full schemas appear in your tool list then). A multi-step query can select one tool per action; search once for the workflow, e.g. \"browser navigate click read page\". Exact tool names also work.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Capability keywords to search server names, tool names and descriptions",
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max tools to activate from this search (bounded by settings)",
                    },
                },
                "required": ["query"],
            }),
            source: crate::provider::types::ToolSource::Builtin,
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let query = args.get("query").and_then(Value::as_str).unwrap_or("");
        if query.trim().is_empty() {
            return "Error: find_tools: `query` must not be empty".into();
        }
        let settings = &ctx.settings;
        let cap = settings.mcp_activate_max_per_turn.max(1) as usize;
        let limit = match args.get("limit").and_then(Value::as_u64) {
            Some(n) => (n as usize).min(cap),
            None => cap,
        };
        let hits = self.catalog.search(query, limit);
        if hits.is_empty() {
            return format!(
                "No external tool matches \"{query}\". Catalog: {} tool(s) across [{}]. Try one exact tool name or a shorter action term.",
                self.catalog.len(),
                self.catalog.server_summary(),
            );
        }

        // Build dispatchers first so the budget only ever counts tools that
        // actually registered (a server that died after listing costs nothing).
        let mut proxies = Vec::new();
        let mut unavailable = Vec::new();
        for candidate in &hits {
            match self
                .manager
                .proxy_for_candidate(candidate, settings.mcp_failure_threshold)
                .await
            {
                Some(p) => proxies.push((candidate, p)),
                None => unavailable.push(candidate.server_id.clone()),
            }
        }
        let live: Vec<_> = proxies.iter().map(|(c, _)| (*c).clone()).collect();
        let report = self.selection.activate(&live, settings);

        let mut activated_names = Vec::new();
        for candidate in &report.activated {
            let Some((_, proxy)) = proxies.iter().find(|(c, _)| c.name == candidate.name) else {
                continue;
            };
            let Some(registry) = self.registry.upgrade() else {
                return "Error: find_tools: registry unavailable".into();
            };
            if registry.activate(proxy.clone()) {
                activated_names.push(candidate);
            }
        }

        let mut out = String::new();
        out.push_str(&format!(
            "Matched {} of {} external tool(s); activated {} — callable in your NEXT message:\n",
            hits.len(),
            self.catalog.len(),
            activated_names.len(),
        ));
        for c in &activated_names {
            out.push_str(&format!(
                "- {} [{}] — {}\n",
                c.name,
                if c.read_only { "read-only" } else { "write" },
                c.description,
            ));
        }
        for (c, reason) in &report.rejected {
            out.push_str(&format!("- {} NOT activated: {reason}\n", c.name));
        }
        for c in &report.already {
            out.push_str(&format!("- {} already active\n", c.name));
        }
        for server in unavailable.iter().collect::<std::collections::HashSet<_>>() {
            out.push_str(&format!(
                "- server '{server}' is reconnecting; search again shortly\n"
            ));
        }
        if !activated_names.is_empty() {
            out.push_str("Call the activated tools directly in your next message.");
        }
        out
    }
}
