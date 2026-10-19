//! Split a `tool_calls` array into ordered segments (Hermes-style).
//!
//! Independent reads/searches (and independent writes in `auto`) run concurrently.
//! `terminal` / `process` and unparseable calls are barriers. MCP names listed in
//! [`PlanOptions::parallel_unscoped`] (G2: `readOnlyHint`) run with other
//! unscoped/path-scoped calls. Results must still be persisted in the original
//! array order.

use crate::provider::types::ToolCall;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub struct PlanOptions {
    pub enabled: bool,
    pub max: usize,
    /// `true` when Host will not prompt (permission_mode = auto).
    pub allow_parallel_writes: bool,
    /// Tool names that may overlap in-flight with path-scoped work (G2 fills
    /// `mcp__…` entries whose MCP `annotations.readOnlyHint` is true).
    pub parallel_unscoped: Vec<String>,
}

pub fn plan_segments(calls: &[ToolCall], workspace: &Path, opt: &PlanOptions) -> Vec<Vec<usize>> {
    if !opt.enabled || opt.max <= 1 || calls.len() <= 1 {
        return (0..calls.len()).map(|i| vec![i]).collect();
    }

    let mut segments: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut reserved: Vec<(PathBuf, bool)> = Vec::new();
    let mut current_has_spawn = false;

    let flush = |segments: &mut Vec<Vec<usize>>,
                 current: &mut Vec<usize>,
                 reserved: &mut Vec<(PathBuf, bool)>,
                 current_has_spawn: &mut bool| {
        if !current.is_empty() {
            segments.push(std::mem::take(current));
            reserved.clear();
            *current_has_spawn = false;
        }
    };

    for (i, call) in calls.iter().enumerate() {
        let parsed = parse_args(&call.arguments);
        let Some(args) = parsed.as_ref().filter(|v| v.is_object()) else {
            flush(
                &mut segments,
                &mut current,
                &mut reserved,
                &mut current_has_spawn,
            );
            segments.push(vec![i]);
            continue;
        };

        match classify(&call.name, opt) {
            Class::Barrier => {
                flush(
                    &mut segments,
                    &mut current,
                    &mut reserved,
                    &mut current_has_spawn,
                );
                segments.push(vec![i]);
            }
            Class::Spawn => {
                let has_writer = reserved.iter().any(|(_, w)| *w);
                if has_writer || current.len() >= opt.max {
                    flush(
                        &mut segments,
                        &mut current,
                        &mut reserved,
                        &mut current_has_spawn,
                    );
                }
                current_has_spawn = true;
                current.push(i);
            }
            Class::UnscopedParallel => {
                if current.len() >= opt.max {
                    flush(
                        &mut segments,
                        &mut current,
                        &mut reserved,
                        &mut current_has_spawn,
                    );
                }
                current.push(i);
            }
            Class::PathScoped { writer } => {
                let paths = scope_paths(&call.name, args, workspace);
                if paths.is_empty() {
                    flush(
                        &mut segments,
                        &mut current,
                        &mut reserved,
                        &mut current_has_spawn,
                    );
                    segments.push(vec![i]);
                    continue;
                }
                let conflict = paths.iter().any(|p| {
                    reserved.iter().any(|(existing, existing_writer)| {
                        (writer || *existing_writer) && paths_overlap(p, existing)
                    })
                });
                if writer && current_has_spawn {
                    flush(
                        &mut segments,
                        &mut current,
                        &mut reserved,
                        &mut current_has_spawn,
                    );
                }
                if conflict || current.len() >= opt.max {
                    flush(
                        &mut segments,
                        &mut current,
                        &mut reserved,
                        &mut current_has_spawn,
                    );
                }
                reserved.extend(paths.into_iter().map(|p| (p, writer)));
                current.push(i);
            }
        }
    }
    flush(
        &mut segments,
        &mut current,
        &mut reserved,
        &mut current_has_spawn,
    );
    segments
}

/// Two calls may not overlap in-flight (Codex exclusive lock + our path rules).
pub fn calls_conflict(a: &ToolCall, b: &ToolCall, workspace: &Path, opt: &PlanOptions) -> bool {
    if !opt.enabled {
        return true;
    }
    let ca = classify(&a.name, opt);
    let cb = classify(&b.name, opt);
    match (ca, cb) {
        (Class::Barrier, _) | (_, Class::Barrier) => true,
        (Class::Spawn, Class::Spawn)
        | (Class::Spawn, Class::UnscopedParallel)
        | (Class::UnscopedParallel, Class::Spawn) => false,
        (Class::Spawn, Class::PathScoped { writer: false })
        | (Class::PathScoped { writer: false }, Class::Spawn) => false,
        (Class::Spawn, Class::PathScoped { writer: true })
        | (Class::PathScoped { writer: true }, Class::Spawn) => true,
        (Class::UnscopedParallel, Class::UnscopedParallel)
        | (Class::UnscopedParallel, Class::PathScoped { .. })
        | (Class::PathScoped { .. }, Class::UnscopedParallel) => false,
        (Class::PathScoped { writer: wa }, Class::PathScoped { writer: wb }) => {
            let Some(aa) = parse_args(&a.arguments).filter(|v| v.is_object()) else {
                return true;
            };
            let Some(ba) = parse_args(&b.arguments).filter(|v| v.is_object()) else {
                return true;
            };
            let pa = scope_paths(&a.name, &aa, workspace);
            let pb = scope_paths(&b.name, &ba, workspace);
            if pa.is_empty() || pb.is_empty() {
                return true;
            }
            pa.iter()
                .any(|p| pb.iter().any(|q| (wa || wb) && paths_overlap(p, q)))
        }
    }
}

enum Class {
    Barrier,
    PathScoped { writer: bool },
    UnscopedParallel,
    Spawn,
}

fn classify(name: &str, opt: &PlanOptions) -> Class {
    match name {
        "read_file" | "search_files" => Class::PathScoped { writer: false },
        "write_file" | "patch" if opt.allow_parallel_writes => Class::PathScoped { writer: true },
        "delegate_task" => Class::Spawn,
        "web_search" | "web_extract" => Class::UnscopedParallel,
        _ if name.starts_with("mcp__") && opt.parallel_unscoped.iter().any(|n| n == name) => {
            Class::UnscopedParallel
        }
        // terminal, process, MCP without readOnlyHint, unknown names.
        _ => Class::Barrier,
    }
}

fn parse_args(raw: &str) -> Option<Value> {
    let s = if raw.trim().is_empty() { "{}" } else { raw };
    serde_json::from_str(s).ok()
}

fn scope_paths(name: &str, args: &Value, workspace: &Path) -> Vec<PathBuf> {
    let raw = match name {
        "search_files" => args
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("."),
        _ => match args.get("path").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => s,
            _ => return Vec::new(),
        },
    };
    vec![normalize(workspace, raw)]
}

fn normalize(workspace: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace.join(p)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::Prefix(pre) => out.push(pre.as_os_str()),
            Component::RootDir => out.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    #[cfg(windows)]
    {
        PathBuf::from(out.to_string_lossy().replace('/', "\\").to_lowercase())
    }
    #[cfg(not(windows))]
    {
        out
    }
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args.into(),
        }
    }

    fn opt(writes: bool) -> PlanOptions {
        PlanOptions {
            enabled: true,
            max: 8,
            allow_parallel_writes: writes,
            parallel_unscoped: Vec::new(),
        }
    }

    fn opt_unscoped(writes: bool, names: &[&str]) -> PlanOptions {
        PlanOptions {
            enabled: true,
            max: 8,
            allow_parallel_writes: writes,
            parallel_unscoped: names.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn two_reads_same_segment() {
        let ws = Path::new("/ws");
        let calls = vec![
            call("1", "read_file", r#"{"path":"a.rs"}"#),
            call("2", "read_file", r#"{"path":"b.rs"}"#),
        ];
        assert_eq!(plan_segments(&calls, ws, &opt(false)), vec![vec![0, 1]]);
    }

    #[test]
    fn reader_reader_same_file_stays_parallel() {
        let ws = Path::new("/ws");
        let calls = vec![
            call("1", "read_file", r#"{"path":"a.rs"}"#),
            call("2", "read_file", r#"{"path":"a.rs"}"#),
        ];
        assert_eq!(plan_segments(&calls, ws, &opt(false)), vec![vec![0, 1]]);
    }

    #[test]
    fn write_then_read_same_path_is_ordered() {
        let ws = Path::new("/ws");
        let calls = vec![
            call("1", "write_file", r#"{"path":"a.rs","content":"x"}"#),
            call("2", "read_file", r#"{"path":"a.rs"}"#),
        ];
        assert_eq!(
            plan_segments(&calls, ws, &opt(true)),
            vec![vec![0], vec![1]]
        );
    }

    #[test]
    fn write_is_barrier_when_host_would_ask() {
        let ws = Path::new("/ws");
        let calls = vec![
            call("1", "read_file", r#"{"path":"a.rs"}"#),
            call("2", "write_file", r#"{"path":"b.rs","content":"x"}"#),
        ];
        assert_eq!(
            plan_segments(&calls, ws, &opt(false)),
            vec![vec![0], vec![1]]
        );
    }

    #[test]
    fn independent_writes_parallel_in_auto() {
        let ws = Path::new("/ws");
        let calls = vec![
            call("1", "write_file", r#"{"path":"a.rs","content":"x"}"#),
            call("2", "write_file", r#"{"path":"b.rs","content":"y"}"#),
        ];
        assert_eq!(plan_segments(&calls, ws, &opt(true)), vec![vec![0, 1]]);
    }

    #[test]
    fn terminal_is_barrier() {
        let ws = Path::new("/ws");
        let calls = vec![
            call("1", "read_file", r#"{"path":"a.rs"}"#),
            call("2", "terminal", r#"{"command":"echo hi"}"#),
            call("3", "read_file", r#"{"path":"b.rs"}"#),
        ];
        assert_eq!(
            plan_segments(&calls, ws, &opt(false)),
            vec![vec![0], vec![1], vec![2]]
        );
    }

    #[test]
    fn search_root_orders_behind_write() {
        let ws = Path::new("/ws");
        let calls = vec![
            call("1", "write_file", r#"{"path":"src/a.rs","content":"x"}"#),
            call(
                "2",
                "search_files",
                r#"{"pattern":"fn ","target":"content"}"#,
            ),
        ];
        assert_eq!(
            plan_segments(&calls, ws, &opt(true)),
            vec![vec![0], vec![1]]
        );
    }

    #[test]
    fn disabled_is_fully_sequential() {
        let ws = Path::new("/ws");
        let calls = vec![
            call("1", "read_file", r#"{"path":"a.rs"}"#),
            call("2", "read_file", r#"{"path":"b.rs"}"#),
        ];
        let opt = PlanOptions {
            enabled: false,
            max: 8,
            allow_parallel_writes: false,
            parallel_unscoped: Vec::new(),
        };
        assert_eq!(plan_segments(&calls, ws, &opt), vec![vec![0], vec![1]]);
    }

    #[test]
    fn conflict_reads_do_not_conflict() {
        let ws = Path::new("/ws");
        let a = call("1", "read_file", r#"{"path":"a.rs"}"#);
        let b = call("2", "read_file", r#"{"path":"b.rs"}"#);
        assert!(!calls_conflict(&a, &b, ws, &opt(false)));
    }

    #[test]
    fn conflict_write_read_same_path() {
        let ws = Path::new("/ws");
        let a = call("1", "write_file", r#"{"path":"a.rs","content":"x"}"#);
        let b = call("2", "read_file", r#"{"path":"a.rs"}"#);
        assert!(calls_conflict(&a, &b, ws, &opt(true)));
    }

    #[test]
    fn mcp_without_hint_is_barrier() {
        let ws = Path::new("/ws");
        let calls = vec![
            call("1", "read_file", r#"{"path":"a.rs"}"#),
            call("2", "mcp__echo__ping", r#"{"x":1}"#),
            call("3", "read_file", r#"{"path":"b.rs"}"#),
        ];
        assert_eq!(
            plan_segments(&calls, ws, &opt(false)),
            vec![vec![0], vec![1], vec![2]]
        );
    }

    #[test]
    fn web_search_and_extract_run_with_reads() {
        let ws = Path::new("/ws");
        let calls = vec![
            call("1", "read_file", r#"{"path":"a.rs"}"#),
            call("2", "web_search", r#"{"query":"dandelion"}"#),
            call("3", "web_extract", r#"{"url":"https://example.com"}"#),
        ];
        assert_eq!(plan_segments(&calls, ws, &opt(false)), vec![vec![0, 1, 2]]);
    }

    #[test]
    fn readonly_mcp_runs_with_read_file() {
        let ws = Path::new("/ws");
        let opt = opt_unscoped(false, &["mcp__docs__get"]);
        let calls = vec![
            call("1", "read_file", r#"{"path":"a.rs"}"#),
            call("2", "mcp__docs__get", r#"{"id":"1"}"#),
        ];
        assert_eq!(plan_segments(&calls, ws, &opt), vec![vec![0, 1]]);
        let a = call("1", "read_file", r#"{"path":"a.rs"}"#);
        let b = call("2", "mcp__docs__get", r#"{"id":"1"}"#);
        assert!(!calls_conflict(&a, &b, ws, &opt));
    }

    #[test]
    fn two_readonly_mcp_same_segment() {
        let ws = Path::new("/ws");
        let opt = opt_unscoped(false, &["mcp__a__get", "mcp__b__get"]);
        let calls = vec![
            call("1", "mcp__a__get", r#"{}"#),
            call("2", "mcp__b__get", r#"{}"#),
        ];
        assert_eq!(plan_segments(&calls, ws, &opt), vec![vec![0, 1]]);
        assert!(!calls_conflict(&calls[0], &calls[1], ws, &opt));
    }

    #[test]
    fn readonly_mcp_still_barriers_on_terminal() {
        let ws = Path::new("/ws");
        let opt = opt_unscoped(false, &["mcp__docs__get"]);
        let calls = vec![
            call("1", "mcp__docs__get", r#"{}"#),
            call("2", "terminal", r#"{"command":"echo hi"}"#),
            call("3", "read_file", r#"{"path":"a.rs"}"#),
        ];
        assert_eq!(
            plan_segments(&calls, ws, &opt),
            vec![vec![0], vec![1], vec![2]]
        );
    }

    #[test]
    fn parallel_max_splits() {
        let ws = Path::new("/ws");
        let calls = vec![
            call("1", "read_file", r#"{"path":"a.rs"}"#),
            call("2", "read_file", r#"{"path":"b.rs"}"#),
            call("3", "read_file", r#"{"path":"c.rs"}"#),
        ];
        let opt = PlanOptions {
            enabled: true,
            max: 2,
            allow_parallel_writes: false,
            parallel_unscoped: Vec::new(),
        };
        assert_eq!(plan_segments(&calls, ws, &opt), vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn delegate_runs_with_read_barriers_write() {
        let ws = Path::new("/ws");
        let read = call("1", "read_file", r#"{"path":"a.rs"}"#);
        let del = call("2", "delegate_task", r#"{"goal":"x"}"#);
        let write = call("3", "write_file", r#"{"path":"b.rs","content":"y"}"#);
        let term = call("4", "terminal", r#"{"command":"echo hi"}"#);
        assert!(!calls_conflict(&read, &del, ws, &opt(false)));
        assert!(calls_conflict(&write, &del, ws, &opt(true)));
        assert!(calls_conflict(&term, &del, ws, &opt(false)));
        let calls = vec![read.clone(), del.clone()];
        assert_eq!(plan_segments(&calls, ws, &opt(false)), vec![vec![0, 1]]);
        let calls = vec![del.clone(), write.clone()];
        assert_eq!(
            plan_segments(&calls, ws, &opt(true)),
            vec![vec![0], vec![1]]
        );
    }
}
