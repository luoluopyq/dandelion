//! H3 scope layering: user → project → session, **tightening only**.
//!
//! The user layer (`dandelion.mcp.servers` in settings.json) is the widest
//! possible fleet. The project layer (`{workspace}/.dandelion/mcp.json`) and
//! the session layer (the prompt's explicit `mcp_server_ids` selection) may
//! only shrink it — a project file can never add a server, re-enable one the
//! user disabled, or expose a tool the user denied. This mirrors where both
//! references converged (Codex `enabled_tools` is enforced as a strict subset
//! of the listing; Hermes ships a `disabled_tools` deny-list; Codex's own docs
//! warn that allow-lists are fail-open against newly advertised tools) while
//! keeping our merge strictly fail-closed: unknown or expansion-shaped input
//! is reported and ignored, never obeyed.
//!
//! Credentials are unaffected: scope files only ever *name* servers and tools.

use crate::config::McpServerConfig;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Project-layer file name, under `{workspace}/.dandelion/`.
pub const PROJECT_SCOPE_FILE: &str = "mcp.json";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMcpScope {
    /// Server ids the project refuses entirely (even if the user enabled them).
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Per-server tightening. Keys must be servers the user already configured.
    #[serde(default)]
    pub servers: BTreeMap<String, ProjectServerScope>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectServerScope {
    /// Raw (remote) tool names this project never exposes.
    #[serde(default, alias = "disabledTools")]
    pub disabled_tools: Vec<String>,
}

/// Result of folding the three layers together for one prompt.
#[derive(Debug, Default)]
pub struct ScopeDecision {
    /// Effective server ids, input order preserved, project-disabled removed.
    pub ids: Vec<String>,
    /// server id → raw tool names to drop. Union of user + project deny lists.
    pub disabled_tools: HashMap<String, HashSet<String>>,
    /// Human-readable notes (project refused a server, expansion attempt
    /// ignored, malformed file…). Goes to trace log and `McpSnapshot.skipped`.
    pub notes: Vec<String>,
}

impl ScopeDecision {
    pub fn is_denied(&self, server_id: &str, remote_name: &str) -> bool {
        self.disabled_tools
            .get(server_id)
            .is_some_and(|set| set.contains(remote_name))
    }
}

/// Read `{workspace}/.dandelion/mcp.json`. `Ok(None)` when absent; a
/// malformed file is reported as a note and treated as absent — a broken
/// project config must never break the loop or silently widen anything.
pub fn load_project_scope(ws_root: &std::path::Path) -> (Option<ProjectMcpScope>, Vec<String>) {
    let path = ws_root.join(".dandelion").join(PROJECT_SCOPE_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return (None, Vec::new()),
        Err(err) => {
            return (
                None,
                vec![format!("project mcp scope unreadable ({err}); ignored")],
            )
        }
    };
    // Unknown *values* are the expansion channel: a project trying to add
    // `command`/`url`/`env` keys. `deny_unknown_fields` turns every such key
    // into a parse error, so we get an honest note and widen nothing.
    match serde_json::from_str::<ProjectMcpScope>(&raw) {
        Ok(scope) => (Some(scope), Vec::new()),
        Err(err) => (
            None,
            vec![format!(
                "project mcp scope .dandelion/{PROJECT_SCOPE_FILE} rejected: {err}; no servers added, none restricted"
            )],
        ),
    }
}

/// Fold the project layer onto the user fleet for this prompt's requested ids.
///
/// Session layer needs no extra parameter: the caller already passes the
/// session's explicit selection as `requested`, and [`snapshot_for`] only ever
/// intersects it with user-enabled servers — a session can pick a subset,
/// never widen. Here we subtract the project's further restrictions.
pub fn resolve(
    servers: &[McpServerConfig],
    ws_root: Option<&std::path::Path>,
    requested: &[String],
) -> ScopeDecision {
    let (project, mut notes) = match ws_root {
        Some(root) => load_project_scope(root),
        None => (None, Vec::new()),
    };
    let user_ids: HashSet<&str> = servers.iter().map(|s| s.id.as_str()).collect();
    let mut out = ScopeDecision {
        ids: Vec::with_capacity(requested.len()),
        disabled_tools: HashMap::new(),
        notes: Vec::new(),
    };

    // User layer: a configured deny list is a floor no layer can lift.
    for cfg in servers {
        if !cfg.disabled_tools.is_empty() {
            out.disabled_tools
                .insert(cfg.id.clone(), cfg.disabled_tools.iter().cloned().collect());
        }
    }

    if let Some(project) = &project {
        for id in &project.disabled {
            if !user_ids.contains(id.as_str()) {
                notes.push(format!(
                    "project disabled unknown server '{id}' (not in user config; nothing to restrict)"
                ));
            }
        }
        for (id, over) in &project.servers {
            if !user_ids.contains(id.as_str()) {
                // The one shape that could smuggle capacity in: a project
                // "configuring" a server the user never added.
                notes.push(format!(
                    "project scope for unknown server '{id}' ignored (projects may only restrict user-configured servers)"
                ));
                continue;
            }
            if over.disabled_tools.is_empty() {
                continue;
            }
            let entry = out.disabled_tools.entry(id.clone()).or_default();
            let before = entry.len();
            entry.extend(over.disabled_tools.iter().cloned());
            if entry.len() > before {
                notes.push(format!(
                    "project restricts {} tool(s) on '{id}'",
                    over.disabled_tools.len()
                ));
            }
        }
    }

    let project_disabled: HashSet<&str> = project
        .as_ref()
        .map(|p| p.disabled.iter().map(String::as_str).collect())
        .unwrap_or_default();
    for id in requested {
        if project_disabled.contains(id.as_str()) {
            notes.push(format!("server '{id}' disabled by project scope"));
            continue;
        }
        out.ids.push(id.clone());
    }
    out.notes = notes;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn user_server(id: &str, deny: &[&str]) -> McpServerConfig {
        McpServerConfig {
            id: id.into(),
            disabled_tools: deny.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn write_project(root: &std::path::Path, json: &str) {
        fs::create_dir_all(root.join(".dandelion")).unwrap();
        fs::write(root.join(".dandelion").join(PROJECT_SCOPE_FILE), json).unwrap();
    }

    #[test]
    fn absent_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let d = resolve(&[user_server("a", &[])], Some(dir.path()), &["a".into()]);
        assert_eq!(d.ids, vec!["a".to_string()]);
        assert!(d.disabled_tools.is_empty());
        assert!(d.notes.is_empty());
    }

    #[test]
    fn project_disables_server_and_tools() {
        let dir = tempfile::tempdir().unwrap();
        write_project(
            dir.path(),
            r#"{"disabled":["github"],"servers":{"jira":{"disabledTools":["delete_issue"]}}}"#,
        );
        let servers = vec![user_server("github", &[]), user_server("jira", &[])];
        let d = resolve(
            &servers,
            Some(dir.path()),
            &["github".into(), "jira".into()],
        );
        assert_eq!(d.ids, vec!["jira".to_string()]);
        assert!(d.is_denied("jira", "delete_issue"));
        assert!(!d.is_denied("jira", "create_issue"));
        assert!(d.notes.iter().any(|n| n.contains("project scope")));
    }

    #[test]
    fn user_deny_is_a_floor_project_cannot_lift() {
        let dir = tempfile::tempdir().unwrap();
        // Project says nothing about the tool the user denied; nothing here
        // (or in any well-formed file) can re-expose it.
        write_project(dir.path(), r#"{"servers":{"jira":{}}}"#);
        let servers = vec![user_server("jira", &["delete_issue"])];
        let d = resolve(&servers, Some(dir.path()), &["jira".into()]);
        assert!(d.is_denied("jira", "delete_issue"));
    }

    #[test]
    fn project_cannot_add_servers() {
        let dir = tempfile::tempdir().unwrap();
        write_project(
            dir.path(),
            r#"{"servers":{"evil":{"disabledTools":[]}}, "disabled":[]}"#,
        );
        let d = resolve(&[user_server("ok", &[])], Some(dir.path()), &["ok".into()]);
        assert_eq!(d.ids, vec!["ok".to_string()]);
        assert!(d.disabled_tools.is_empty());
        assert!(
            d.notes.iter().any(|n| n.contains("unknown server 'evil'")),
            "{:?}",
            d.notes
        );
    }

    #[test]
    fn expansion_keys_are_rejected_not_obeyed() {
        let dir = tempfile::tempdir().unwrap();
        // The classic project-side power grab: command/url/env do not parse
        // at all, so the whole file is refused with a visible note.
        write_project(
            dir.path(),
            r#"{"servers":{"jira":{"command":"curl","args":["evil"]}}}"#,
        );
        let servers = vec![user_server("jira", &[])];
        let d = resolve(&servers, Some(dir.path()), &["jira".into()]);
        assert_eq!(d.ids, vec!["jira".to_string()]); // untouched, nothing added
        assert!(d.disabled_tools.is_empty());
        assert!(
            d.notes.iter().any(|n| n.contains("rejected")),
            "{:?}",
            d.notes
        );
    }

    #[test]
    fn malformed_file_warns_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write_project(dir.path(), "{ not json");
        let d = resolve(&[user_server("a", &[])], Some(dir.path()), &["a".into()]);
        assert_eq!(d.ids, vec!["a".to_string()]);
        assert!(d.notes.iter().any(|n| n.contains("rejected")));
    }

    #[test]
    fn deny_lists_union_across_layers() {
        let dir = tempfile::tempdir().unwrap();
        write_project(
            dir.path(),
            r#"{"servers":{"jira":{"disabledTools":["b"]}}}"#,
        );
        let servers = vec![user_server("jira", &["a"])];
        let d = resolve(&servers, Some(dir.path()), &["jira".into()]);
        assert!(d.is_denied("jira", "a") && d.is_denied("jira", "b"));
    }
}
