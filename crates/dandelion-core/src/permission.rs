//! Three-gate permission. Gate 3 is reserved (no user rule file in G1).

use crate::acp::{RequestPermissionOutcome, RequestPermissionRequest};
use crate::config::{PermissionMode, PermissionRules, Settings};
use crate::host::AgentHost;
use crate::ids::{SessionId, ToolCallId};
use crate::sandbox::{command_targets_system_path, is_system_path, WorkspaceFs};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    AllowTurn,
    DenyHard,
    DenyPermission,
    Cancelled,
}

const HARD_DENY_NEEDLES: &[&str] = &[
    "format.com",
    "format ",
    "diskpart",
    "bcdedit",
    "cipher /w",
    "shutdown",
    "restart-computer",
    "stop-computer",
    "stop-service",
    "reg delete",
    "reg add",
    "rd /s",
    "rmdir /s",
    "del /s",
    "remove-item -recurse",
    "remove-item -r ",
    "takeown",
    "icacls",
    // POSIX — same gate 1 idea, different needles
    "rm -rf /",
    "rm -rf /*",
    "mkfs ",
    "mkfs.",
    ":(){",
    "dd if=/dev/zero of=/dev/",
];

pub fn normalize_command(command: &str) -> String {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn gate1_hard_deny_command(command: &str) -> bool {
    let n = normalize_command(command);
    if command_targets_system_path(&n) || command_targets_system_path(command) {
        return true;
    }
    HARD_DENY_NEEDLES.iter().any(|needle| n.contains(needle))
}

pub fn gate2_terminal_needs_ask(command: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)\b(remove-item|del |erase |rm |rmdir|rd )\b").expect("regex")
    });
    re.is_match(command)
}

pub fn gate1_path(fs: &WorkspaceFs, args: &Value) -> Option<String> {
    let path = args.get("path").and_then(|v| v.as_str())?;
    match fs.resolve(path) {
        Ok(p) if is_system_path(&p) => Some(format!(
            "Error: blocked: system path denied: {}",
            p.display()
        )),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("system path denied") {
                Some(crate::sandbox::sandbox_error_for_model(&err))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn needs_gate2(name: &str, args: &Value) -> bool {
    match name {
        "read_file" | "search_files" => false,
        "write_file" | "patch" => true,
        "terminal" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(gate2_terminal_needs_ask)
            .unwrap_or(false),
        "process" => args
            .get("action")
            .and_then(|v| v.as_str())
            .map(|a| a.eq_ignore_ascii_case("kill"))
            .unwrap_or(false),
        "delegate_task" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("spawn");
            // `changes` is a read-only review of a child changeset;
            // merge/discard mutate the workspace and keep the gate.
            !matches!(action, "list" | "stop" | "changes")
        }
        // A wiki proposal is inert until the user approves it, so it never
        // gates. `mode:"write"` mutates the formal ledger — same posture as
        // write_file: gate 2 decides by permission mode.
        "wiki_propose" => args
            .get("mode")
            .and_then(|v| v.as_str())
            == Some("write"),
        _ => name.starts_with("mcp__"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate3Decision {
    /// Deny list hit: do not ask, do not run.
    Deny,
    /// Ask list hit: prompt even when the default mode or another rule allows it.
    Ask,
    /// Allow list hit: skip gate 2 (cannot override gate 1).
    SkipGate2,
    NoMatch,
}

pub fn gate3_decision(name: &str, args: &Value, rules: &PermissionRules) -> Gate3Decision {
    for pat in &rules.deny {
        if rule_matches(pat, name, args) {
            return Gate3Decision::Deny;
        }
    }
    for pat in &rules.ask {
        if rule_matches(pat, name, args) {
            return Gate3Decision::Ask;
        }
    }
    for pat in &rules.allow {
        if rule_matches(pat, name, args) {
            return Gate3Decision::SkipGate2;
        }
    }
    Gate3Decision::NoMatch
}

pub const GATE3_BLOCKED: &str = "Error: blocked by rule";

fn rule_matches(pattern: &str, name: &str, args: &Value) -> bool {
    let pat = pattern.trim();
    if pat.is_empty() {
        return false;
    }
    if let Some(needle) = pat.strip_prefix("terminal:") {
        if name != "terminal" {
            return false;
        }
        let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let cmd = cmd.to_ascii_lowercase();
        let needle = needle.to_ascii_lowercase();
        return if needle.contains('*') || needle.contains('?') {
            glob::Pattern::new(&needle).is_ok_and(|p| p.matches(&cmd))
        } else {
            cmd.contains(&needle)
        };
    }
    for tool in ["read_file", "write_file", "patch", "search_files"] {
        if let Some(path_pattern) = pat.strip_prefix(&format!("{tool}:")) {
            if name != tool {
                return false;
            }
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            return glob::Pattern::new(path_pattern).is_ok_and(|p| p.matches(path));
        }
    }
    glob_tool_name(pat, name)
}

fn glob_tool_name(pattern: &str, name: &str) -> bool {
    match glob::Pattern::new(pattern) {
        Ok(p) => p.matches(name),
        Err(_) => pattern == name,
    }
}

pub fn gate1_block_message(
    name: &str,
    args: &Value,
    fs: &WorkspaceFs,
    allow_shell: bool,
) -> Option<String> {
    if matches!(name, "terminal" | "process") && !allow_shell {
        return Some("Error: shell disabled by settings".into());
    }
    if name == "terminal" {
        if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
            if gate1_hard_deny_command(cmd) {
                return Some("Error: blocked: command matches a hard-deny pattern".into());
            }
        }
        if let Some(workdir) = args.get("workdir").and_then(|v| v.as_str()) {
            if let Err(err) = fs.resolve(workdir) {
                let msg = err.to_string();
                if msg.contains("system path denied") {
                    return Some(crate::sandbox::sandbox_error_for_model(&err));
                }
            }
        }
    }
    if matches!(name, "read_file" | "write_file" | "patch" | "search_files") {
        if let Some(msg) = gate1_path(fs, args) {
            return Some(msg);
        }
    }
    None
}

pub async fn check_gates(
    name: &str,
    args: &Value,
    title: &str,
    session_id: &SessionId,
    tool_call_id: &ToolCallId,
    fs: &WorkspaceFs,
    host: &dyn AgentHost,
    mode: PermissionMode,
    force_ask: bool,
    settings: &Settings,
    cancel: &CancellationToken,
) -> GateDecision {
    if let Some(_) = gate1_block_message(name, args, fs, settings.allow_shell) {
        return GateDecision::DenyHard;
    }
    if !force_ask && !needs_gate2(name, args) {
        return GateDecision::Allow;
    }
    let mode = if force_ask { PermissionMode::Ask } else { mode };
    match mode {
        PermissionMode::Auto => GateDecision::Allow,
        PermissionMode::Deny => GateDecision::DenyPermission,
        PermissionMode::Ask => {
            if !host.is_interactive() {
                return GateDecision::DenyPermission;
            }
            let req = RequestPermissionRequest::tool_call(
                session_id.clone(),
                title.to_string(),
                tool_call_id.clone(),
            );
            let wait = Duration::from_secs(settings.permission_timeout_sec);
            let outcome = tokio::select! {
                _ = cancel.cancelled() => RequestPermissionOutcome::Cancelled,
                res = timeout(wait, host.request_permission(req, cancel.clone())) => {
                    match res {
                        Ok(Ok(o)) => o,
                        Ok(Err(_)) => RequestPermissionOutcome::Denied,
                        Err(_) => RequestPermissionOutcome::Denied,
                    }
                }
            };
            match outcome {
                RequestPermissionOutcome::Cancelled => GateDecision::Cancelled,
                RequestPermissionOutcome::Selected { option_id } if option_id == "allow_turn" => {
                    GateDecision::AllowTurn
                }
                other if other.is_allow() => GateDecision::Allow,
                _ => GateDecision::DenyPermission,
            }
        }
    }
}

/// Denial text for external-directory access. A bare "denied" gets
/// re-attributed by models as a session-wide hard ban and abandoned; the
/// message must state what happened and the concrete unblock path.
pub fn external_dir_denied_message(root: &std::path::Path, host_interactive: bool) -> String {
    if host_interactive {
        format!(
            "Error: external directory access denied or approval timed out: {}. \
             The directory is outside the workspace. Retry and approve the permission \
             prompt (\"Allow for this session\" skips further prompts for the rest of \
             this session), or add the directory to dandelion.sandbox.allowRoots in \
             settings.",
            root.display()
        )
    } else {
        format!(
            "Error: external directory access blocked: {} — this host has no approval \
             channel, so no permission prompt was shown. Add the directory to \
             dandelion.sandbox.allowRoots in settings, then retry.",
            root.display()
        )
    }
}

pub async fn ask_external_directory(
    path: &std::path::Path,
    session_id: &SessionId,
    tool_call_id: &ToolCallId,
    host: &dyn AgentHost,
    settings: &Settings,
    cancel: &CancellationToken,
) -> GateDecision {
    if !host.is_interactive() {
        return GateDecision::DenyPermission;
    }
    let request = RequestPermissionRequest::tool_call(
        session_id.clone(),
        format!("允许本次访问工作区外目录 {}？", path.display()),
        tool_call_id.clone(),
    );
    let wait = Duration::from_secs(settings.permission_timeout_sec);
    let outcome = tokio::select! {
        _ = cancel.cancelled() => RequestPermissionOutcome::Cancelled,
        res = timeout(wait, host.request_permission(request, cancel.clone())) => {
            match res {
                Ok(Ok(outcome)) => outcome,
                _ => RequestPermissionOutcome::Denied,
            }
        }
    };
    match outcome {
        RequestPermissionOutcome::Cancelled => GateDecision::Cancelled,
        RequestPermissionOutcome::Selected { option_id } if option_id == "allow_turn" => {
            GateDecision::AllowTurn
        }
        other if other.is_allow() => GateDecision::Allow,
        _ => GateDecision::DenyPermission,
    }
}

/// Permission title shown to the user (not written onto ToolCallUpdate).
pub fn permission_title(name: &str, args: &Value) -> String {
    match name {
        "write_file" | "patch" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Allow {name} to {path}?")
        }
        "terminal" => {
            let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Allow terminal command `{cmd}`?")
        }
        "process" => "Allow process kill?".into(),
        "delegate_task" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("spawn");
            match action {
                "merge" => "Allow merge of a subagent changeset into the workspace?".into(),
                "discard" => "Allow discarding a subagent changeset?".into(),
                _ => {
                    if let Some(tasks) = args.get("tasks").and_then(|v| v.as_array()) {
                        format!("Allow {} children?", tasks.len())
                    } else {
                        "Allow delegate_task?".into()
                    }
                }
            }
        }
        "wiki_propose" => {
            let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .chars()
                .take(60)
                .collect::<String>();
            format!("Allow writing to the project Wiki ({kind}): {content}?")
        }
        _ => format!("Allow {name}?"),
    }
}

pub fn hard_deny_message(name: &str, args: &Value, fs: &WorkspaceFs, allow_shell: bool) -> String {
    gate1_block_message(name, args, fs, allow_shell)
        .unwrap_or_else(|| "Error: blocked: denied".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RecordingHost;
    use serde_json::json;

    #[tokio::test]
    async fn unanswered_permission_times_out_as_denied() {
        let dir = tempfile::tempdir().unwrap();
        let host = RecordingHost::new(dir.path().to_path_buf()).hang_on_permission();
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let mut settings = Settings::defaults();
        settings.permission_timeout_sec = 0;
        let decision = check_gates(
            "write_file",
            &json!({"path": "foo.rs", "content": "x"}),
            "Allow write_file to foo.rs?",
            &SessionId::new(),
            &ToolCallId::new(),
            &fs,
            &host,
            PermissionMode::Ask,
            false,
            &settings,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(decision, GateDecision::DenyPermission);
    }

    #[tokio::test]
    async fn external_directory_requires_interactive_approval_even_in_auto_mode() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::defaults();
        let allowed = RecordingHost::new(dir.path().to_path_buf());
        let decision = ask_external_directory(
            dir.path(),
            &SessionId::new(),
            &ToolCallId::new(),
            &allowed,
            &settings,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(decision, GateDecision::Allow);
        assert!(allowed.call_titles()[0].contains(&dir.path().display().to_string()));

        let denied = RecordingHost::new(dir.path().to_path_buf()).non_interactive();
        let decision = ask_external_directory(
            dir.path(),
            &SessionId::new(),
            &ToolCallId::new(),
            &denied,
            &settings,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(decision, GateDecision::DenyPermission);
        assert!(denied.call_titles().is_empty());
    }

    #[test]
    fn external_dir_denied_message_names_path_and_unblock_path() {
        let interactive = external_dir_denied_message(std::path::Path::new("/data/logs"), true);
        assert!(interactive.contains("/data/logs"), "{interactive}");
        assert!(interactive.contains("allowRoots"), "{interactive}");
        assert!(interactive.contains("timed out"), "{interactive}");
        assert!(interactive.contains("Allow for this session"), "{interactive}");

        let headless = external_dir_denied_message(std::path::Path::new("/data/logs"), false);
        assert!(headless.contains("/data/logs"), "{headless}");
        assert!(headless.contains("allowRoots"), "{headless}");
        assert!(headless.contains("no approval channel"), "{headless}");
    }

    #[tokio::test]
    async fn explicit_ask_overrides_auto_mode_for_read() {
        let dir = tempfile::tempdir().unwrap();
        let host = RecordingHost::new(dir.path().to_path_buf()).deny();
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let decision = check_gates(
            "read_file",
            &json!({"path": "a.txt"}),
            "Allow read_file?",
            &SessionId::new(),
            &ToolCallId::new(),
            &fs,
            &host,
            PermissionMode::Auto,
            true,
            &Settings::defaults(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(decision, GateDecision::DenyPermission);
        assert_eq!(host.call_titles().len(), 1);
    }

    #[test]
    fn gate1_format_does_not_need_host() {
        assert!(gate1_hard_deny_command("Format.com c:"));
        assert!(gate1_hard_deny_command(
            "Remove-Item -Recurse C:\\Windows\\System32"
        ));
        assert!(!gate1_hard_deny_command("Get-ChildItem"));
        assert!(gate1_hard_deny_command("rm -rf /"));
        assert!(!gate1_hard_deny_command("rm -rf ./build"));
    }

    #[tokio::test]
    async fn gate2_write_asks_host() {
        let dir = tempfile::tempdir().unwrap();
        let host = RecordingHost::new(dir.path().to_path_buf());
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let settings = Settings::defaults();
        let cancel = CancellationToken::new();
        let args = json!({"path": "foo.rs", "content": "x"});
        let decision = check_gates(
            "write_file",
            &args,
            "Allow write_file to foo.rs?",
            &SessionId::new(),
            &ToolCallId::new(),
            &fs,
            &host,
            PermissionMode::Ask,
            false,
            &settings,
            &cancel,
        )
        .await;
        assert_eq!(decision, GateDecision::Allow);
        assert_eq!(host.call_titles().len(), 1);
    }

    #[tokio::test]
    async fn gate1_skips_host() {
        let dir = tempfile::tempdir().unwrap();
        let host = RecordingHost::new(dir.path().to_path_buf());
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let settings = Settings::defaults();
        let cancel = CancellationToken::new();
        let args = json!({"command": "Remove-Item -Recurse C:\\Windows\\System32"});
        let decision = check_gates(
            "terminal",
            &args,
            "Allow?",
            &SessionId::new(),
            &ToolCallId::new(),
            &fs,
            &host,
            PermissionMode::Ask,
            false,
            &settings,
            &cancel,
        )
        .await;
        assert_eq!(decision, GateDecision::DenyHard);
        assert!(host.call_titles().is_empty());
    }

    #[test]
    fn mcp_prefix_needs_gate2() {
        assert!(needs_gate2("mcp__echo__ping", &json!({})));
        assert!(!needs_gate2("read_file", &json!({"path": "a.rs"})));
        assert!(!needs_gate2("search_files", &json!({})));
        assert!(needs_gate2("write_file", &json!({"path": "a.rs"})));
        assert!(!needs_gate2("unknown_builtin_like", &json!({})));
    }

    #[tokio::test]
    async fn mcp_auto_allows_without_host() {
        let dir = tempfile::tempdir().unwrap();
        let host = RecordingHost::new(dir.path().to_path_buf());
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let settings = Settings::defaults();
        let cancel = CancellationToken::new();
        let decision = check_gates(
            "mcp__echo__ping",
            &json!({}),
            "Allow mcp__echo__ping?",
            &SessionId::new(),
            &ToolCallId::new(),
            &fs,
            &host,
            PermissionMode::Auto,
            false,
            &settings,
            &cancel,
        )
        .await;
        assert_eq!(decision, GateDecision::Allow);
        assert!(host.call_titles().is_empty());
    }

    #[tokio::test]
    async fn mcp_ask_mode_calls_host() {
        let dir = tempfile::tempdir().unwrap();
        let host = RecordingHost::new(dir.path().to_path_buf());
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let settings = Settings::defaults();
        let cancel = CancellationToken::new();
        let decision = check_gates(
            "mcp__echo__ping",
            &json!({}),
            "Allow mcp__echo__ping?",
            &SessionId::new(),
            &ToolCallId::new(),
            &fs,
            &host,
            PermissionMode::Ask,
            false,
            &settings,
            &cancel,
        )
        .await;
        assert_eq!(decision, GateDecision::Allow);
        assert_eq!(host.call_titles().len(), 1);
    }

    #[tokio::test]
    async fn mcp_deny_mode_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let host = RecordingHost::new(dir.path().to_path_buf());
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let settings = Settings::defaults();
        let cancel = CancellationToken::new();
        let decision = check_gates(
            "mcp__echo__ping",
            &json!({}),
            "Allow?",
            &SessionId::new(),
            &ToolCallId::new(),
            &fs,
            &host,
            PermissionMode::Deny,
            false,
            &settings,
            &cancel,
        )
        .await;
        assert_eq!(decision, GateDecision::DenyPermission);
        assert!(host.call_titles().is_empty());
    }

    #[test]
    fn mcp_not_gate1_system_path() {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        assert!(gate1_block_message(
            "mcp__echo__ping",
            &json!({"path": r"C:\Windows\System32\hosts"}),
            &fs,
            true,
        )
        .is_none());
    }

    #[test]
    fn gate3_deny_before_allow() {
        let rules = PermissionRules {
            deny: vec!["write_file".into()],
            ask: vec![],
            allow: vec!["write_file".into()],
        };
        assert_eq!(
            gate3_decision("write_file", &json!({"path": "a.rs"}), &rules),
            Gate3Decision::Deny
        );
    }

    #[test]
    fn gate3_allow_and_glob_and_terminal_substr() {
        let rules = PermissionRules {
            deny: vec!["mcp__evil__*".into(), "terminal:format".into()],
            ask: vec![],
            allow: vec!["read_file".into(), "mcp__docs__*".into()],
        };
        assert_eq!(
            gate3_decision("read_file", &json!({"path": "a.rs"}), &rules),
            Gate3Decision::SkipGate2
        );
        assert_eq!(
            gate3_decision("mcp__docs__get", &json!({}), &rules),
            Gate3Decision::SkipGate2
        );
        assert_eq!(
            gate3_decision("mcp__evil__x", &json!({}), &rules),
            Gate3Decision::Deny
        );
        assert_eq!(
            gate3_decision("terminal", &json!({"command": "Format.com c:"}), &rules),
            Gate3Decision::Deny
        );
        assert_eq!(
            gate3_decision("search_files", &json!({"pattern": "x"}), &rules),
            Gate3Decision::NoMatch
        );
    }

    #[test]
    fn gate3_path_and_command_rules_obey_precedence() {
        let rules = PermissionRules {
            deny: vec!["write_file:/Users/me/Desktop/secret*".into()],
            ask: vec![
                "write_file:/Users/me/Desktop/*".into(),
                "terminal:git push *".into(),
            ],
            allow: vec!["write_file".into(), "terminal".into()],
        };
        assert_eq!(
            gate3_decision(
                "write_file",
                &json!({"path": "/Users/me/Desktop/secret.txt"}),
                &rules
            ),
            Gate3Decision::Deny
        );
        assert_eq!(
            gate3_decision(
                "write_file",
                &json!({"path": "/Users/me/Desktop/report.html"}),
                &rules
            ),
            Gate3Decision::Ask
        );
        assert_eq!(
            gate3_decision(
                "write_file",
                &json!({"path": "/Users/me/Work/report.html"}),
                &rules
            ),
            Gate3Decision::SkipGate2
        );
        assert_eq!(
            gate3_decision(
                "terminal",
                &json!({"command": "git push origin main"}),
                &rules
            ),
            Gate3Decision::Ask
        );
    }

    #[test]
    fn todo_does_not_need_gate2() {
        assert!(!needs_gate2("todo", &json!({"todos": []})));
        assert!(!needs_gate2("todo", &json!({})));
        assert!(!needs_gate2("web_search", &json!({"query": "x"})));
        assert!(!needs_gate2(
            "web_extract",
            &json!({"urls": ["https://example.com"]})
        ));
    }

    #[test]
    fn delegate_spawn_needs_gate2_list_does_not() {
        assert!(needs_gate2("delegate_task", &json!({"goal": "x"})));
        assert!(!needs_gate2("delegate_task", &json!({"action": "list"})));
        assert!(!needs_gate2(
            "delegate_task",
            &json!({"action": "stop", "childSessionId": "tsk_1"})
        ));
    }

    #[test]
    fn delegate_permission_title() {
        assert_eq!(
            permission_title("delegate_task", &json!({"goal": "x"})),
            "Allow delegate_task?"
        );
        assert_eq!(
            permission_title("delegate_task", &json!({"tasks": [{}, {}]})),
            "Allow 2 children?"
        );
    }
}
