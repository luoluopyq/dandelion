//! C6 subagent tree: spawn without awaiting the child loop; Park vs handle.

use crate::acp::{SessionUpdate, ToolCallStatus, ToolCallUpdate};
use crate::ids::{SessionId, ToolCallId};
use crate::provider::types::ToolCall;
use crate::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChildStatus {
    Running,
    Idle,
    Failed,
    Cancelled,
}

impl ChildStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildListItem {
    pub child_session_id: SessionId,
    pub goal: String,
    pub status: ChildStatus,
    pub depth: u32,
    pub started_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct ChildOutcome {
    pub summary: String,
    pub status: ChildStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Park,
    Handle,
}

pub struct SpawnRequest {
    pub parent: SessionId,
    pub root: SessionId,
    pub parent_tool_call_id: ToolCallId,
    pub goal: String,
    pub context: String,
    pub depth: u32,
    pub delivery: Delivery,
    pub parent_cancel: CancellationToken,
    pub max_iterations: u32,
    /// H2: `Some(true)` forces worktree isolation, `Some(false)` forces the
    /// shared workspace, `None` defers to `Settings::delegation_worktree_isolation`.
    pub isolation: Option<bool>,
}

struct ChildRec {
    parent: SessionId,
    root: SessionId,
    goal: String,
    depth: u32,
    status: ChildStatus,
    started_at: DateTime<Utc>,
    started_mono: Instant,
    last_activity: Instant,
    in_tool: bool,
    cancel: CancellationToken,
    parent_tool_call_id: ToolCallId,
    delivery: Delivery,
    outcome_tx: watch::Sender<Option<ChildOutcome>>,
    stale: bool,
    /// H2: set when the child runs in an isolated git worktree.
    worktree: Option<WorktreeState>,
}

/// H2 worktree state kept beside the child record so `changes`/`merge`/
/// `discard` can act on a finished child without a second registry.
pub struct WorktreeState {
    pub handle: crate::worktree::WorktreeHandle,
    /// Final report attached by `run_child` once the child loop ends.
    pub report: Option<serde_json::Value>,
    pub merged: bool,
    pub pruned: bool,
}

struct Inner {
    children: HashMap<SessionId, ChildRec>,
    /// Per-root FIFO of children waiting for a concurrency slot (G5/overflow).
    #[allow(dead_code)]
    queued: HashMap<SessionId, VecDeque<SessionId>>,
    pending_kicks: HashMap<SessionId, Vec<String>>,
    park_interrupt: HashMap<SessionId, CancellationToken>,
}

pub struct SubagentRegistry {
    inner: Mutex<Inner>,
    max_concurrent: u32,
}

impl SubagentRegistry {
    pub fn new(max_concurrent: u32) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                children: HashMap::new(),
                queued: HashMap::new(),
                pending_kicks: HashMap::new(),
                park_interrupt: HashMap::new(),
            }),
            max_concurrent: max_concurrent.max(1),
        })
    }

    pub fn register(
        &self,
        child: SessionId,
        req: &SpawnRequest,
        cancel: CancellationToken,
    ) -> watch::Receiver<Option<ChildOutcome>> {
        let (tx, rx) = watch::channel(None);
        let now = Instant::now();
        let rec = ChildRec {
            parent: req.parent.clone(),
            root: req.root.clone(),
            goal: req.goal.clone(),
            depth: req.depth,
            status: ChildStatus::Running,
            started_at: Utc::now(),
            started_mono: now,
            last_activity: now,
            in_tool: false,
            cancel,
            parent_tool_call_id: req.parent_tool_call_id.clone(),
            delivery: req.delivery,
            outcome_tx: tx,
            stale: false,
            worktree: None,
        };
        let mut g = self.inner.lock().expect("lock");
        g.children.insert(child, rec);
        rx
    }

    pub fn live_for_root(&self, root: &SessionId) -> u32 {
        let g = self.inner.lock().expect("lock");
        g.children
            .values()
            .filter(|c| &c.root == root && c.status == ChildStatus::Running)
            .count() as u32
    }

    pub fn over_cap(&self, root: &SessionId, adding: u32) -> bool {
        self.live_for_root(root) + adding > self.max_concurrent
    }

    pub fn max_concurrent(&self) -> u32 {
        self.max_concurrent
    }

    pub fn touch(&self, child: &SessionId) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(c) = g.children.get_mut(child) {
                c.last_activity = Instant::now();
            }
        }
    }

    pub fn set_in_tool(&self, child: &SessionId, in_tool: bool) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(c) = g.children.get_mut(child) {
                c.in_tool = in_tool;
                c.last_activity = Instant::now();
            }
        }
    }

    pub fn is_stale(
        &self,
        child: &SessionId,
        stale_idle: Duration,
        stale_in_tool: Duration,
        wall: Option<Duration>,
        cmd_running: bool,
    ) -> bool {
        let g = self.inner.lock().expect("lock");
        let Some(c) = g.children.get(child) else {
            return false;
        };
        if c.status != ChildStatus::Running {
            return false;
        }
        if let Some(wall) = wall {
            if c.started_mono.elapsed() >= wall {
                return true;
            }
        }
        let idle = c.last_activity.elapsed();
        if c.in_tool || cmd_running {
            idle >= stale_in_tool
        } else {
            idle >= stale_idle
        }
    }

    pub fn mark_stale(&self, child: &SessionId) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(c) = g.children.get_mut(child) {
                c.stale = true;
                c.cancel.cancel();
            }
        }
    }

    pub fn was_stale(&self, child: &SessionId) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.children.get(child).map(|c| c.stale))
            .unwrap_or(false)
    }

    pub fn cancel_child(&self, child: &SessionId) -> bool {
        let g = self.inner.lock().expect("lock");
        if let Some(c) = g.children.get(child) {
            c.cancel.cancel();
            true
        } else {
            false
        }
    }

    pub fn cancel_tree(&self, session: &SessionId) {
        let g = self.inner.lock().expect("lock");
        for (id, c) in &g.children {
            if id == session || &c.parent == session || &c.root == session {
                c.cancel.cancel();
            }
        }
    }

    pub fn cancel_all(&self) {
        if let Ok(g) = self.inner.lock() {
            for c in g.children.values() {
                c.cancel.cancel();
            }
        }
    }

    pub fn finish(&self, child: &SessionId, mut outcome: ChildOutcome) {
        let mut g = self.inner.lock().expect("lock");
        let (should_kick, parent) = {
            let Some(c) = g.children.get_mut(child) else {
                return;
            };
            if outcome.status == ChildStatus::Idle && outcome.summary.starts_with("Error:") {
                outcome.status = ChildStatus::Failed;
            }
            c.status = outcome.status;
            let _ = c.outcome_tx.send(Some(outcome.clone()));
            let should_kick = c.delivery == Delivery::Handle && !outcome.summary.is_empty();
            (should_kick, c.parent.clone())
        };
        if should_kick {
            g.pending_kicks
                .entry(parent)
                .or_default()
                .push(outcome.summary);
        }
    }

    pub fn take_kicks(&self, parent: &SessionId) -> Vec<String> {
        self.inner
            .lock()
            .expect("lock")
            .pending_kicks
            .remove(parent)
            .unwrap_or_default()
    }

    pub fn list(&self, parent: &SessionId) -> Vec<ChildListItem> {
        let g = self.inner.lock().expect("lock");
        let mut out: Vec<_> = g
            .children
            .iter()
            .filter(|(_, c)| &c.parent == parent && c.status == ChildStatus::Running)
            .map(|(id, c)| ChildListItem {
                child_session_id: id.clone(),
                goal: c.goal.clone(),
                status: c.status,
                depth: c.depth,
                started_at: c.started_at,
            })
            .collect();
        out.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        out
    }

    pub fn parent_tool_id(&self, child: &SessionId) -> Option<(SessionId, ToolCallId)> {
        let g = self.inner.lock().expect("lock");
        g.children
            .get(child)
            .map(|c| (c.parent.clone(), c.parent_tool_call_id.clone()))
    }

    pub fn child_ids_for_tool(&self, parent: &SessionId, tool: &ToolCallId) -> Vec<SessionId> {
        let g = self.inner.lock().expect("lock");
        g.children
            .iter()
            .filter(|(_, c)| &c.parent == parent && &c.parent_tool_call_id == tool)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn set_park_interrupt(&self, parent: &SessionId, token: CancellationToken) {
        self.inner
            .lock()
            .expect("lock")
            .park_interrupt
            .insert(parent.clone(), token);
    }

    pub fn take_park_interrupt(&self, parent: &SessionId) -> Option<CancellationToken> {
        self.inner
            .lock()
            .expect("lock")
            .park_interrupt
            .remove(parent)
    }

    pub fn interrupt_park(&self, parent: &SessionId) {
        if let Ok(g) = self.inner.lock() {
            if let Some(t) = g.park_interrupt.get(parent) {
                t.cancel();
            }
        }
    }

    pub fn is_parked(&self, parent: &SessionId) -> bool {
        self.inner
            .lock()
            .ok()
            .map(|g| g.park_interrupt.contains_key(parent))
            .unwrap_or(false)
    }

    pub fn token_of(&self, child: &SessionId) -> Option<CancellationToken> {
        self.inner
            .lock()
            .ok()?
            .children
            .get(child)
            .map(|c| c.cancel.clone())
    }

    pub fn outcome_rx(&self, child: &SessionId) -> Option<watch::Receiver<Option<ChildOutcome>>> {
        self.inner
            .lock()
            .ok()?
            .children
            .get(child)
            .map(|c| c.outcome_tx.subscribe())
    }

    /// H2: attach the created worktree to a registered child.
    pub fn attach_worktree(
        &self,
        child: &SessionId,
        handle: crate::worktree::WorktreeHandle,
    ) -> bool {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(c) = g.children.get_mut(child) {
                c.worktree = Some(WorktreeState {
                    handle,
                    report: None,
                    merged: false,
                    pruned: false,
                });
                return true;
            }
        }
        false
    }

    pub fn worktree_parent(&self, child: &SessionId) -> Option<SessionId> {
        self.inner
            .lock()
            .ok()?
            .children
            .get(child)
            .filter(|c| c.worktree.is_some())
            .map(|c| c.parent.clone())
    }

    pub fn worktree_handle(&self, child: &SessionId) -> Option<crate::worktree::WorktreeHandle> {
        self.inner
            .lock()
            .ok()?
            .children
            .get(child)?
            .worktree
            .as_ref()
            .map(|w| w.handle.clone())
    }

    /// Snapshot of a child's worktree state including merged/pruned flags.
    pub fn worktree_flags(&self, child: &SessionId) -> Option<(bool, bool)> {
        let g = self.inner.lock().ok()?;
        let w = g.children.get(child)?.worktree.as_ref()?;
        Some((w.merged, w.pruned))
    }

    /// Store the finalized report and return the JSON view for hosts.
    pub fn set_worktree_report(&self, child: &SessionId, report: serde_json::Value) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(c) = g.children.get_mut(child) {
                if let Some(w) = c.worktree.as_mut() {
                    w.report = Some(report);
                }
            }
        }
    }

    pub fn mark_worktree_merged(&self, child: &SessionId) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(w) = g.children.get_mut(child).and_then(|c| c.worktree.as_mut()) {
                w.merged = true;
            }
        }
    }

    pub fn mark_worktree_pruned(&self, child: &SessionId) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(w) = g.children.get_mut(child).and_then(|c| c.worktree.as_mut()) {
                w.pruned = true;
            }
        }
    }
}

pub fn cap_summary(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("\ntruncated");
    out
}

pub fn extract_summary(
    last_assistant: Option<&str>,
    status: ChildStatus,
    extra: Option<&str>,
    max_chars: usize,
) -> String {
    let mut body = last_assistant.unwrap_or("").trim().to_string();
    match status {
        ChildStatus::Cancelled => {
            if extra == Some("subagent stale") {
                body = "Error: subagent stale".into();
            } else if body.is_empty() {
                body = "Error: cancelled".into();
            } else {
                body = format!("{body}\nError: cancelled");
            }
        }
        ChildStatus::Failed => {
            let reason = extra.unwrap_or("failed");
            if body.is_empty() {
                body = format!("Error: {reason}");
            } else {
                body = format!("{body}\nError: {reason}");
            }
        }
        _ => {
            if let Some(extra) = extra {
                if !extra.is_empty() {
                    if body.is_empty() {
                        body = extra.to_string();
                    } else {
                        body = format!("{body}\n{extra}");
                    }
                }
            }
            if body.is_empty() {
                body = "(no summary)".into();
            }
        }
    }
    cap_summary(&body, max_chars)
}

pub fn is_park_batch(calls: &[ToolCall]) -> bool {
    if calls.is_empty() {
        return false;
    }
    let mut spawn = false;
    for c in calls {
        if c.name != "delegate_task" {
            return false;
        }
        match delegate_action(&c.arguments) {
            DelegateAction::Spawn => spawn = true,
            DelegateAction::List
            | DelegateAction::Stop
            | DelegateAction::Changes
            | DelegateAction::Merge
            | DelegateAction::Discard => {}
            DelegateAction::Steer | DelegateAction::Unknown => return false,
        }
    }
    spawn
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelegateAction {
    Spawn,
    List,
    Stop,
    /// H2 parent-side changeset review (read-only).
    Changes,
    /// H2 explicit merge of a finished child's worktree.
    Merge,
    /// H2 explicit discard of a finished child's worktree.
    Discard,
    Steer,
    Unknown,
}

pub fn delegate_action(arguments: &str) -> DelegateAction {
    let v: Value = serde_json::from_str(if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    })
    .unwrap_or(Value::Null);
    match v.get("action").and_then(|x| x.as_str()).unwrap_or("spawn") {
        "list" => DelegateAction::List,
        "stop" => DelegateAction::Stop,
        "steer" => DelegateAction::Steer,
        "changes" => DelegateAction::Changes,
        "merge" => DelegateAction::Merge,
        "discard" => DelegateAction::Discard,
        "spawn" | "" => DelegateAction::Spawn,
        _ => DelegateAction::Unknown,
    }
}

pub fn handle_json(child_ids: &[SessionId], goals: &[String]) -> String {
    if child_ids.len() == 1 {
        serde_json::json!({
            "status": "running",
            "childSessionId": child_ids[0].as_str(),
            "goal": goals.first().cloned().unwrap_or_default(),
        })
        .to_string()
    } else {
        serde_json::json!({
            "status": "running",
            "childSessionIds": child_ids.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        })
        .to_string()
    }
}

pub fn list_json(items: &[ChildListItem]) -> String {
    let arr: Vec<Value> = items
        .iter()
        .map(|i| {
            serde_json::json!({
                "childSessionId": i.child_session_id.as_str(),
                "goal": i.goal,
                "status": i.status.as_str(),
                "depth": i.depth,
                "startedAt": i.started_at.to_rfc3339(),
            })
        })
        .collect();
    serde_json::to_string(&arr).unwrap_or_else(|_| "[]".into())
}

/// Wait until every child has an outcome, or park is interrupted / cancelled.
pub async fn wait_children(
    watches: Vec<watch::Receiver<Option<ChildOutcome>>>,
    cancel: CancellationToken,
    park_interrupt: CancellationToken,
) -> Result<Vec<Option<ChildOutcome>>, WaitAbort> {
    let mut out = Vec::with_capacity(watches.len());
    for mut rx in watches {
        loop {
            if let Some(v) = rx.borrow().clone() {
                out.push(Some(v));
                break;
            }
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() {
                        out.push(None);
                        break;
                    }
                }
                _ = cancel.cancelled() => return Err(WaitAbort::Cancelled),
                _ = park_interrupt.cancelled() => return Err(WaitAbort::Interrupted),
            }
        }
    }
    Ok(out)
}

#[derive(Debug)]
pub enum WaitAbort {
    Cancelled,
    Interrupted,
}

#[allow(dead_code)]
pub fn oneshot_pair() -> (oneshot::Sender<()>, oneshot::Receiver<()>) {
    oneshot::channel()
}

pub fn progress_update(
    parent_tool: &ToolCallId,
    title: impl Into<String>,
    status: Option<ToolCallStatus>,
) -> SessionUpdate {
    SessionUpdate::tool_call(ToolCallUpdate {
        tool_call_id: parent_tool.clone(),
        title: Some(title.into()),
        kind: None,
        status,
        raw_input: None,
        content: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn park_batch_detects_spawn_only() {
        let spawn = ToolCall {
            id: "1".into(),
            name: "delegate_task".into(),
            arguments: r#"{"goal":"x"}"#.into(),
        };
        let read = ToolCall {
            id: "2".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"a"}"#.into(),
        };
        assert!(is_park_batch(&[spawn.clone()]));
        assert!(!is_park_batch(&[spawn, read]));
    }

    #[test]
    fn summary_caps_and_marks_truncated() {
        let s = cap_summary("abcdef", 3);
        assert!(s.starts_with("abc"));
        assert!(s.contains("truncated"));
    }

    #[test]
    fn park_batch_allows_list_stop_with_spawn() {
        let spawn = ToolCall {
            id: "1".into(),
            name: "delegate_task".into(),
            arguments: r#"{"goal":"x"}"#.into(),
        };
        let list = ToolCall {
            id: "2".into(),
            name: "delegate_task".into(),
            arguments: r#"{"action":"list"}"#.into(),
        };
        assert!(is_park_batch(&[list.clone(), spawn]));
        let steer = ToolCall {
            id: "3".into(),
            name: "delegate_task".into(),
            arguments: r#"{"action":"steer"}"#.into(),
        };
        assert!(!is_park_batch(&[steer]));
        assert_eq!(
            delegate_action(r#"{"action":"steer"}"#),
            DelegateAction::Steer
        );
        assert_eq!(delegate_action(r#"{"goal":"x"}"#), DelegateAction::Spawn);
    }

    #[test]
    fn extract_summary_stale_cancelled_and_empty() {
        assert_eq!(
            extract_summary(None, ChildStatus::Failed, Some("subagent stale"), 8000),
            "Error: subagent stale"
        );
        assert_eq!(
            extract_summary(None, ChildStatus::Cancelled, Some("subagent stale"), 8000),
            "Error: subagent stale"
        );
        assert_eq!(
            extract_summary(None, ChildStatus::Cancelled, None, 8000),
            "Error: cancelled"
        );
        assert_eq!(
            extract_summary(None, ChildStatus::Idle, None, 8000),
            "(no summary)"
        );
        let s = extract_summary(Some("hello"), ChildStatus::Idle, None, 8000);
        assert_eq!(s, "hello");
    }

    #[test]
    fn handle_and_list_json_shape() {
        let id = SessionId::parse("tsk_child").unwrap();
        let one = handle_json(&[id.clone()], &["do work".into()]);
        assert!(one.contains("\"status\":\"running\""));
        assert!(one.contains("tsk_child"));
        assert!(one.contains("do work"));
        let two = handle_json(
            &[id.clone(), SessionId::parse("tsk_b").unwrap()],
            &["a".into(), "b".into()],
        );
        assert!(two.contains("childSessionIds"));
        let items = [ChildListItem {
            child_session_id: id,
            goal: "g".into(),
            status: ChildStatus::Running,
            depth: 1,
            started_at: Utc::now(),
        }];
        let listed = list_json(&items);
        assert!(listed.contains("childSessionId"));
        assert!(listed.contains("startedAt"));
        assert!(listed.contains("\"depth\":1"));
    }

    #[test]
    fn over_cap_counts_running_children_for_root() {
        let reg = SubagentRegistry::new(2);
        let root = SessionId::new();
        let cancel = CancellationToken::new();
        for i in 0..2 {
            let req = SpawnRequest {
                parent: root.clone(),
                root: root.clone(),
                parent_tool_call_id: ToolCallId::parse(format!("call_{i}")),
                goal: "g".into(),
                context: String::new(),
                depth: 1,
                delivery: Delivery::Handle,
                parent_cancel: cancel.clone(),
                max_iterations: 10,
                isolation: None,
            };
            reg.register(SessionId::new(), &req, cancel.clone());
        }
        assert!(reg.over_cap(&root, 1));
        assert!(!reg.over_cap(&root, 0));
        assert_eq!(reg.live_for_root(&root), 2);
    }

    #[test]
    fn stale_wall_and_cancel_tree() {
        let reg = SubagentRegistry::new(4);
        let root = SessionId::new();
        let child = SessionId::new();
        let parent_cancel = CancellationToken::new();
        let child_cancel = CancellationToken::new();
        let req = SpawnRequest {
            parent: root.clone(),
            root: root.clone(),
            parent_tool_call_id: ToolCallId::parse("call_stale"),
            goal: "g".into(),
            context: String::new(),
            depth: 1,
            delivery: Delivery::Park,
            parent_cancel: parent_cancel.clone(),
            max_iterations: 10,
            isolation: None,
        };
        reg.register(child.clone(), &req, child_cancel.clone());
        assert!(!reg.is_stale(
            &child,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            None,
            false
        ));
        assert!(reg.is_stale(
            &child,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            Some(Duration::from_secs(0)),
            false
        ));
        reg.cancel_tree(&root);
        assert!(child_cancel.is_cancelled());
    }
}
