//! Session todo list (G5 C11). Same registry / loop as G1 tools. Not a Dock switch.
//!
//! Aligns with Hermes `todo_list` (replace vs merge, full list returned, no system
//! mutation). Persistence is SQLite so the list survives turns and compact.

use super::{Tool, ToolCtx};
use crate::acp::ToolKind;
use crate::provider::types::{ToolSource, ToolSpec};
use crate::store::TodoItem;
use async_trait::async_trait;
use serde_json::{json, Value};

pub const INJECTION_MARK: &str = "[任务列表]";

const VALID_STATUSES: &[&str] = &["pending", "in_progress", "completed", "cancelled"];
const TRUNCATION_MARKER: &str = "… [truncated]";

pub struct TodoTool;

impl TodoTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TodoTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "todo".into(),
            description: "Track a task list for multi-step work (3+ steps). \
Call with no parameters to read the current list. \
Provide todos to write: merge=false (default) replaces the whole list; \
merge=true updates by id and appends new items. \
List order is priority. Only ONE item in_progress at a time. \
Nested subtasks use parent. Mark completed only after the work is verified. \
Always returns the full current list. State is per session."
                .into(),
            source: ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "Task items to write. Omit to read.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "content": { "type": "string", "description": "Task description" },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed", "cancelled"]
                                },
                                "parent": {
                                    "type": "string",
                                    "description": "Optional id of another item (nested subtask)."
                                }
                            },
                            "required": ["id", "content", "status"]
                        }
                    },
                    "merge": {
                        "type": "boolean",
                        "description": "true: update by id and append new. false: replace entire list.",
                        "default": false
                    }
                }
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let max_items = ctx.settings.todo_max_items.max(1) as usize;
        let max_chars = ctx.settings.todo_max_content_chars.max(64);
        match apply(&ctx.store, &ctx.session_id, &args, max_items, max_chars) {
            Ok(text) => text,
            Err(err) => format!("Error: {err}"),
        }
    }
}

fn apply(
    store: &crate::store::Store,
    session_id: &crate::ids::SessionId,
    args: &Value,
    max_items: usize,
    max_chars: usize,
) -> crate::Result<String> {
    let current = store.list_todos(session_id)?;
    let revision = store.todo_revision(session_id)?;
    let merge = args.get("merge").and_then(|v| v.as_bool()).unwrap_or(false);
    let todos_arg = args.get("todos");
    let (items, new_rev) = if todos_arg.is_none() || todos_arg == Some(&Value::Null) {
        (current, revision)
    } else {
        let incoming = parse_todos(todos_arg.unwrap())?;
        let next = write_list(&current, &incoming, merge, max_items, max_chars);
        let changed = next != current;
        let rev = if changed {
            revision.saturating_add(1)
        } else {
            revision
        };
        store.save_todos(session_id, &next, rev)?;
        (next, rev)
    };
    Ok(result_json(&items, new_rev))
}

fn parse_todos(raw: &Value) -> crate::Result<Vec<Value>> {
    if let Some(s) = raw.as_str() {
        let parsed: Value = serde_json::from_str(s).map_err(|_| {
            crate::Error::Invalid("todos must be a list of objects, got unparseable string".into())
        })?;
        return parse_todos(&parsed);
    }
    raw.as_array()
        .cloned()
        .ok_or_else(|| crate::Error::Invalid("todos must be a list".into()))
}

fn write_list(
    existing: &[TodoItem],
    incoming: &[Value],
    merge: bool,
    max_items: usize,
    max_chars: usize,
) -> Vec<TodoItem> {
    let incoming = dedupe_by_id(incoming);
    let mut items = if !merge {
        incoming
            .iter()
            .map(|t| validate(t, max_chars))
            .collect::<Vec<_>>()
    } else {
        let mut map: std::collections::HashMap<String, TodoItem> = existing
            .iter()
            .cloned()
            .map(|i| (i.id.clone(), i))
            .collect();
        let mut order: Vec<String> = existing.iter().map(|i| i.id.clone()).collect();
        for t in &incoming {
            let id = t
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if id.is_empty() {
                continue;
            }
            if let Some(item) = map.get_mut(&id) {
                if let Some(c) = t.get("content").and_then(|v| v.as_str()) {
                    if !c.trim().is_empty() {
                        item.content = cap_content(c.trim(), max_chars);
                    }
                }
                if let Some(s) = t.get("status").and_then(|v| v.as_str()) {
                    let status = s.trim().to_ascii_lowercase();
                    if VALID_STATUSES.iter().any(|v| *v == status) {
                        item.status = status;
                    }
                }
                if t.get("parent").is_some() {
                    let parent = t
                        .get("parent")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    item.parent = if parent.is_empty() || parent == id {
                        None
                    } else {
                        Some(parent)
                    };
                }
            } else {
                let validated = validate(t, max_chars);
                order.push(validated.id.clone());
                map.insert(validated.id.clone(), validated);
            }
        }
        order.into_iter().filter_map(|id| map.remove(&id)).collect()
    };
    if items.len() > max_items {
        items.truncate(max_items);
    }
    sanitize_parents(&mut items);
    normalize_order(items)
}

fn validate(item: &Value, max_chars: usize) -> TodoItem {
    if !item.is_object() {
        return TodoItem {
            id: "?".into(),
            content: "(invalid item)".into(),
            status: "pending".into(),
            parent: None,
        };
    }
    let mut id = item
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if id.is_empty() {
        id = "?".into();
    }
    let content_raw = item
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let content = if content_raw.is_empty() {
        "(no description)".into()
    } else {
        cap_content(content_raw, max_chars)
    };
    let status = item
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending")
        .trim()
        .to_ascii_lowercase();
    let status = if VALID_STATUSES.iter().any(|v| *v == status) {
        status
    } else {
        "pending".into()
    };
    let parent = item
        .get("parent")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && *s != id);
    TodoItem {
        id,
        content,
        status,
        parent,
    }
}

fn cap_content(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let keep = max_chars.saturating_sub(TRUNCATION_MARKER.chars().count());
    let mut out: String = content.chars().take(keep).collect();
    out.push_str(TRUNCATION_MARKER);
    out
}

fn dedupe_by_id(todos: &[Value]) -> Vec<Value> {
    let mut last: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, item) in todos.iter().enumerate() {
        let key = if item.is_object() {
            item.get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        } else {
            format!("__invalid_{i}")
        };
        let key = if key.is_empty() { "?".into() } else { key };
        last.insert(key, i);
    }
    let mut idxs: Vec<usize> = last.into_values().collect();
    idxs.sort_unstable();
    idxs.into_iter().map(|i| todos[i].clone()).collect()
}

fn sanitize_parents(items: &mut [TodoItem]) {
    let ids: std::collections::HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
    for item in items.iter_mut() {
        if let Some(p) = &item.parent {
            if !ids.contains(p) {
                item.parent = None;
            }
        }
    }
    let by_id: std::collections::HashMap<String, Option<String>> = items
        .iter()
        .map(|i| (i.id.clone(), i.parent.clone()))
        .collect();
    for item in items.iter_mut() {
        let mut seen = std::collections::HashSet::new();
        seen.insert(item.id.clone());
        let mut cur = item.parent.clone();
        while let Some(p) = cur {
            if !seen.insert(p.clone()) {
                item.parent = None;
                break;
            }
            cur = by_id.get(&p).cloned().flatten();
        }
    }
}

fn normalize_order(items: Vec<TodoItem>) -> Vec<TodoItem> {
    if items.iter().any(|i| i.parent.is_some()) {
        return items;
    }
    let active = items.iter().position(|i| i.status == "in_progress");
    let Some(active_index) = active else {
        return items;
    };
    let pending_before = items[..active_index]
        .iter()
        .position(|i| i.status == "pending");
    let Some(pending_index) = pending_before else {
        return items;
    };
    let mut out = items;
    let active_item = out.remove(active_index);
    out.insert(pending_index, active_item);
    out
}

fn result_json(items: &[TodoItem], revision: u32) -> String {
    let pending = items.iter().filter(|i| i.status == "pending").count();
    let in_progress = items.iter().filter(|i| i.status == "in_progress").count();
    let completed = items.iter().filter(|i| i.status == "completed").count();
    let cancelled = items.iter().filter(|i| i.status == "cancelled").count();
    json!({
        "todos": items,
        "revision": revision,
        "summary": {
            "total": items.len(),
            "pending": pending,
            "in_progress": in_progress,
            "completed": completed,
            "cancelled": cancelled,
        }
    })
    .to_string()
}

/// Active items for post-compact user-segment injection. Never system.
pub fn format_for_injection(items: &[TodoItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut children: std::collections::HashMap<String, Vec<&TodoItem>> =
        std::collections::HashMap::new();
    let mut roots: Vec<&TodoItem> = Vec::new();
    for item in items {
        if let Some(p) = &item.parent {
            children.entry(p.clone()).or_default().push(item);
        } else {
            roots.push(item);
        }
    }
    let mut lines = vec![INJECTION_MARK.to_string()];
    for item in roots {
        render_active(item, 0, &children, &mut lines);
    }
    if lines.len() == 1 {
        return None;
    }
    Some(lines.join("\n"))
}

fn render_active(
    item: &TodoItem,
    depth: usize,
    children: &std::collections::HashMap<String, Vec<&TodoItem>>,
    out: &mut Vec<String>,
) -> bool {
    let mut kid_lines = Vec::new();
    let mut has_active_kid = false;
    if let Some(kids) = children.get(&item.id) {
        for kid in kids {
            has_active_kid |= render_active(kid, depth + 1, children, &mut kid_lines);
        }
    }
    let keep = matches!(item.status.as_str(), "pending" | "in_progress") || has_active_kid;
    if keep {
        let marker = match item.status.as_str() {
            "completed" => "[x]",
            "in_progress" => "[>]",
            "cancelled" => "[~]",
            _ => "[ ]",
        };
        out.push(format!(
            "{}- {marker} {}. {} ({})",
            "  ".repeat(depth),
            item.id,
            item.content,
            item.status
        ));
        out.extend(kid_lines);
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, content: &str, status: &str) -> Value {
        json!({"id": id, "content": content, "status": status})
    }

    #[test]
    fn write_replaces_and_lifts_in_progress() {
        let next = write_list(
            &[],
            &[
                item("1", "First", "pending"),
                item("2", "Second", "in_progress"),
            ],
            false,
            256,
            4000,
        );
        assert_eq!(next[0].id, "2");
        assert_eq!(next[1].id, "1");
    }

    #[test]
    fn merge_updates_status_keeps_content() {
        let existing = write_list(&[], &[item("1", "Original", "pending")], false, 256, 4000);
        let next = write_list(
            &existing,
            &[json!({"id": "1", "status": "completed"})],
            true,
            256,
            4000,
        );
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].status, "completed");
        assert_eq!(next[0].content, "Original");
    }

    #[test]
    fn injection_drops_completed_keeps_active() {
        let items = write_list(
            &[],
            &[
                item("1", "Done", "completed"),
                item("2", "Next", "pending"),
                item("3", "Now", "in_progress"),
            ],
            false,
            256,
            4000,
        );
        let text = format_for_injection(&items).expect("seg");
        assert!(text.contains(INJECTION_MARK));
        assert!(text.contains("Next"));
        assert!(text.contains("Now"));
        assert!(!text.contains("Done"));
        assert!(!text.contains("[x]"));
    }

    #[test]
    fn empty_injection_is_none() {
        assert!(format_for_injection(&[]).is_none());
        let done = write_list(&[], &[item("1", "Done", "completed")], false, 256, 4000);
        assert!(format_for_injection(&done).is_none());
    }
}
