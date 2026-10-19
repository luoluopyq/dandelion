//! H3 catalog: the searchable local view of listed MCP tools.
//!
//! Large MCP fleets must not all land in `tools[]` (exit condition: 500
//! candidates ⇒ first turn stays lean). `snapshot_for` builds a catalog of
//! every usable candidate; when direct injection would be too wide, only
//! [`crate::tools::find_tools::FindToolsTool`] is registered and the model
//! promotes entries into the callable set on demand (Codex
//! `ToolExposure::Deferred` + `tool_search` parity; Hermes injects
//! everything, so this axis is strictly stronger).

use super::schema::{function_object_bytes, SchemaCandidate};
use crate::config::Settings;

/// Immutable listed-tool catalog for one prompt. Entries keep full parameter
/// schemas in memory, but a schema only reaches the model once the entry is
/// activated through [`super::selection::ToolSelection`].
#[derive(Clone, Debug, Default)]
pub struct McpCatalog {
    entries: Vec<SchemaCandidate>,
}

impl McpCatalog {
    pub fn new(mut entries: Vec<SchemaCandidate>) -> Self {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[SchemaCandidate] {
        &self.entries
    }

    /// Prefer an exact multi-term match. If a request names several distinct
    /// actions, return one useful tool per action instead of requiring one
    /// schema to describe the entire workflow.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SchemaCandidate> {
        let tokens = tokenize(query);
        if tokens.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(i64, &SchemaCandidate)> = Vec::new();
        for entry in &self.entries {
            let mut total = 0i64;
            let mut all_hit = true;
            for token in &tokens {
                let hit = literal_match_score(entry, token);
                if hit == 0 {
                    total = 0;
                    all_hit = false;
                    break;
                }
                total += hit;
            }
            if all_hit && total > 0 {
                scored.push((total, entry));
            }
        }
        // Score desc, then name asc so results are stable run to run.
        scored.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then(a.name.cmp(&b.name)));
        if !scored.is_empty() {
            return scored
                .into_iter()
                .take(limit)
                .map(|(_, e)| e.clone())
                .collect();
        }

        // A query such as "browser navigate click read page" is a workflow:
        // no single Playwright tool can satisfy all its verbs. Select the best
        // hit for each answerable term, starting with rare action terms rather
        // than ubiquitous server/page words. A lone known word plus nonsense
        // still returns no match instead of a misleading suggestion.
        let mut answerable: Vec<(&str, usize)> = tokens
            .iter()
            .filter_map(|token| {
                let count = self
                    .entries
                    .iter()
                    .filter(|entry| fallback_match_score(entry, token) > 0)
                    .count();
                (count > 0).then_some((token.as_str(), count))
            })
            .collect();
        if answerable.len() < 2 {
            return Vec::new();
        }
        answerable.sort_by_key(|(_, count)| *count);
        let has_action_term = answerable.iter().any(|(token, _)| !is_context_term(token));
        let has_specific_term = answerable
            .iter()
            .any(|(_, count)| *count * 2 <= self.entries.len());
        let mut selected = Vec::new();
        for (token, count) in answerable {
            if selected.len() >= limit {
                break;
            }
            if has_action_term && is_context_term(token) {
                continue;
            }
            if has_specific_term && count * 2 > self.entries.len() {
                continue;
            }
            let best = self
                .entries
                .iter()
                .filter(|entry| {
                    !selected
                        .iter()
                        .any(|hit: &SchemaCandidate| hit.name == entry.name)
                })
                .filter_map(|entry| {
                    let score = fallback_match_score(entry, token);
                    (score > 0).then_some((score, entry))
                })
                .max_by(|(sa, a), (sb, b)| {
                    sa.cmp(sb)
                        .then_with(|| b.remote_name.len().cmp(&a.remote_name.len()))
                        .then_with(|| b.name.cmp(&a.name))
                });
            if let Some((_, entry)) = best {
                selected.push(entry.clone());
            }
        }
        selected
    }

    /// Per-server histogram for the no-match hint: `github(34) jira(12)`.
    pub fn server_summary(&self) -> String {
        let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
        for entry in &self.entries {
            *counts.entry(entry.server_id.as_str()).or_default() += 1;
        }
        counts
            .iter()
            .map(|(server, n)| format!("{server}({n})"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn is_context_term(token: &str) -> bool {
    matches!(
        token,
        "browser" | "page" | "web" | "site" | "url" | "element" | "tool" | "current"
    )
}

fn literal_match_score(entry: &SchemaCandidate, token: &str) -> i64 {
    let local_name = entry
        .name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__").map(|(_, tool)| tool))
        .unwrap_or(entry.name.as_str());
    let name = format!("{} {}", local_name, entry.remote_name).to_ascii_lowercase();
    if name == token {
        8
    } else if name.contains(token) {
        4
    } else if entry.server_id.to_ascii_lowercase().contains(token) {
        3
    } else if entry.description.to_ascii_lowercase().contains(token) {
        2
    } else {
        0
    }
}

fn fallback_match_score(entry: &SchemaCandidate, token: &str) -> i64 {
    let literal = literal_match_score(entry, token);
    let aliases: &[&str] = match token {
        "read" | "inspect" | "view" => &["snapshot"],
        "open" | "goto" | "browse" => &["navigate"],
        "press" => &["click"],
        _ => &[],
    };
    aliases.iter().fold(literal, |best, alias| {
        let score = literal_match_score(entry, alias);
        best.max(if score >= 4 { score + 1 } else { score })
    })
}

/// Truncate descriptions and drop candidates whose single schema exceeds the
/// per-tool hard cap (they could never activate). Returns the usable catalog
/// plus the skipped tool names.
pub fn prepare_catalog(
    candidates: Vec<SchemaCandidate>,
    settings: &Settings,
) -> (McpCatalog, Vec<String>) {
    let mut usable = Vec::new();
    let mut skipped = Vec::new();
    for mut c in candidates {
        c.description = truncate_desc(&c.description, settings.mcp_description_max_chars);
        if function_object_bytes(&c) > settings.mcp_max_tool_schema_bytes {
            skipped.push(c.name);
            continue;
        }
        usable.push(c);
    }
    (McpCatalog::new(usable), skipped)
}

/// Direct injection vs `find_tools` laziness. Direct requires *all* usable
/// candidates to fit both the per-turn activation count and the run's total
/// schema soft cap — exactly the budgets an activated set must obey later.
pub fn plan_direct_injection(catalog: &McpCatalog, settings: &Settings) -> bool {
    if catalog.entries.is_empty() {
        return true; // nothing to defer
    }
    if catalog.entries.len() > settings.mcp_activate_max_per_turn as usize {
        return false;
    }
    let soft = settings.mcp_max_schema_bytes;
    if soft == 0 {
        return true;
    }
    catalog
        .entries
        .iter()
        .map(function_object_bytes)
        .sum::<usize>()
        <= soft
}

fn truncate_desc(desc: &str, max_chars: usize) -> String {
    let n = desc.chars().count();
    if n <= max_chars {
        return desc.to_string();
    }
    let mut s: String = desc.chars().take(max_chars.saturating_sub(1)).collect();
    s.push('…');
    s
}

fn tokenize(query: &str) -> Vec<String> {
    // ASCII words split on separators; non-ASCII runs (CJK descriptions match
    // better as whole runs than fake segmentation) are their own tokens.
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut current_ascii_word = true;
    for ch in query.chars() {
        let ascii_word = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-';
        let keep = ascii_word || (!ch.is_ascii() && !ch.is_whitespace());
        if !keep {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !current.is_empty() && ascii_word != current_ascii_word {
            tokens.push(std::mem::take(&mut current));
        }
        if current.is_empty() {
            current_ascii_word = ascii_word;
        }
        current.push(if ascii_word {
            ch.to_ascii_lowercase()
        } else {
            ch
        });
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cand(server: &str, tool: &str, desc: &str, read_only: bool) -> SchemaCandidate {
        SchemaCandidate {
            server_id: server.into(),
            remote_name: tool.into(),
            name: format!("mcp__{server}__{tool}"),
            description: desc.into(),
            parameters: json!({"type": "object"}),
            read_only,
        }
    }

    fn catalog() -> McpCatalog {
        McpCatalog::new(vec![
            cand("github", "create_issue", "Create a GitHub issue", false),
            cand("github", "list_issues", "List GitHub issues", true),
            cand("jira", "search", "Search jira tickets", true),
            cand("filesystem", "read_file", "读取文件内容", true),
        ])
    }

    #[test]
    fn name_beats_description() {
        let hits = catalog().search("issue", 10);
        assert_eq!(hits.len(), 2);
        assert!(hits[0].name.starts_with("mcp__github__"));
        assert!(hits.iter().all(|h| h.name.contains("issue")));
    }

    #[test]
    fn all_tokens_must_hit() {
        let hits = catalog().search("github issue", 10);
        assert_eq!(hits.len(), 2);
        assert!(catalog().search("github nonexistent", 10).is_empty());
    }

    #[test]
    fn multi_action_browser_query_selects_each_action_once() {
        let mut entries = vec![
            cand("playwright", "browser_navigate", "Navigate to a URL", false),
            cand(
                "playwright",
                "browser_navigate_back",
                "Go back in history",
                false,
            ),
            cand(
                "playwright",
                "browser_click",
                "Perform click on a web page",
                false,
            ),
            cand(
                "playwright",
                "browser_snapshot",
                "Capture accessibility snapshot of the current page",
                true,
            ),
            cand(
                "playwright",
                "browser_drop",
                "Drop data onto a page element",
                false,
            ),
        ];
        for i in 0..21 {
            entries.push(cand(
                "playwright",
                &format!("browser_extra_{i}"),
                "Manage a browser preference",
                false,
            ));
        }
        let hits = McpCatalog::new(entries).search("browser navigate click read page", 12);
        let names: Vec<_> = hits.iter().map(|hit| hit.remote_name.as_str()).collect();
        assert_eq!(
            names,
            ["browser_click", "browser_snapshot", "browser_navigate"]
        );
    }

    #[test]
    fn server_and_description_hits_rank_below_name() {
        let hits = catalog().search("jira", 10);
        assert_eq!(hits[0].name, "mcp__jira__search");
    }

    #[test]
    fn cjk_substring_matches() {
        let hits = catalog().search("读取", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "mcp__filesystem__read_file");
    }

    #[test]
    fn limit_and_empty_query() {
        assert_eq!(catalog().search("github", 1).len(), 1);
        assert!(catalog().search("", 10).is_empty());
        assert!(catalog().search("issue", 0).is_empty());
    }

    #[test]
    fn deterministic_order_for_equal_scores() {
        let first = catalog().search("github", 10);
        for _ in 0..5 {
            assert_eq!(
                catalog()
                    .search("github", 10)
                    .iter()
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>(),
                first.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn five_hundred_candidates_are_not_directly_injected() {
        let mut settings = Settings::defaults();
        let entries: Vec<SchemaCandidate> = (0..500)
            .map(|i| cand("big", &format!("tool_{i}"), &format!("tool {i}"), true))
            .collect();
        let (catalog, skipped) = prepare_catalog(entries, &settings);
        assert_eq!(catalog.len(), 500);
        assert!(skipped.is_empty());
        assert!(!plan_direct_injection(&catalog, &settings));

        // A fleet within count and byte budgets still injects directly.
        settings.mcp_activate_max_per_turn = 512;
        assert!(plan_direct_injection(&catalog, &settings));
        settings.mcp_activate_max_per_turn = 12;

        // Even a few tools go lazy when their combined schema busts the soft cap.
        let fat = McpCatalog::new(vec![cand("big", "fat", &"x".repeat(4000), true)]);
        settings.mcp_max_tool_schema_bytes = 16_384;
        settings.mcp_max_schema_bytes = 100;
        assert!(!plan_direct_injection(&fat, &settings));
    }

    #[test]
    fn prepare_drops_only_over_hard_cap() {
        let mut settings = Settings::defaults();
        settings.mcp_max_tool_schema_bytes = 200;
        let fat = {
            let mut c = cand("s", "fat", "x", true);
            let big: Vec<String> = (0..100)
                .map(|i| format!("value_{i}_padding_padding"))
                .collect();
            c.parameters = json!({"type":"object","properties":{"e":{"enum": big}}});
            c
        };
        let (catalog, skipped) =
            prepare_catalog(vec![fat, cand("s", "slim", "ok", false)], &settings);
        assert_eq!(catalog.len(), 1);
        assert_eq!(skipped, vec!["mcp__s__fat".to_string()]);
    }

    #[test]
    fn server_summary_counts() {
        assert_eq!(
            catalog().server_summary(),
            "filesystem(1) github(2) jira(1)"
        );
    }
}
