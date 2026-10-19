use super::{Tool, ToolCtx};
use crate::acp::ToolKind;
use crate::ids::SessionId;
use crate::provider::types::{ToolSource, ToolSpec};
use crate::subagent::{self, DelegateAction, Delivery, SpawnRequest};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DelegateTaskTool;

impl DelegateTaskTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DelegateTaskTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DelegateTaskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delegate_task".into(),
            description: "Spawn a child agent on the same harness (new session, empty history). \
Put independent work in one call via tasks[]. The parent receives a short summary, not the child's tool log. \
By default (dandelion.delegation.worktreeIsolation) a child whose workspace is a git repository runs in an isolated git worktree and its changes become a reviewable changeset: nothing merges into your workspace silently. \
Use isolation=shared to opt this call out (child writes your workspace directly; do not parallel-delegate writers of the same file). \
For isolated children: action=changes shows the changeset, action=merge applies it, action=discard removes it (force=true to drop unmerged work); all take childSessionId. \
Children do not have terminal/process unless dandelion.delegation.inheritShell is true. \
action=list lists live children; action=stop cancels one childSessionId. steer is not supported."
                .into(),
            source: ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["spawn", "list", "stop", "changes", "merge", "discard"],
                        "description": "Default spawn"
                    },
                    "goal": { "type": "string" },
                    "context": { "type": "string" },
                    "max_iterations": { "type": "integer", "minimum": 1 },
                    "isolation": {
                        "type": "string",
                        "enum": ["worktree", "shared"],
                        "description": "Per-call override of the worktree isolation default for spawn"
                    },
                    "force": {
                        "type": "boolean",
                        "description": "discard only: drop a changeset that still has unmerged content"
                    },
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "goal": { "type": "string" },
                                "context": { "type": "string" }
                            },
                            "required": ["goal"]
                        }
                    },
                    "childSessionId": { "type": "string" }
                }
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let Some(spawner) = ctx.spawner.clone() else {
            return "Error: delegate_task is unavailable".into();
        };
        let action = match args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("spawn")
        {
            "list" => DelegateAction::List,
            "stop" => DelegateAction::Stop,
            "steer" => DelegateAction::Steer,
            "changes" => DelegateAction::Changes,
            "merge" => DelegateAction::Merge,
            "discard" => DelegateAction::Discard,
            "spawn" | "" => DelegateAction::Spawn,
            other => {
                return format!("Error: unknown delegate_task action {other}");
            }
        };
        match action {
            DelegateAction::Steer => {
                "Error: steer is not supported; stop and delegate again".into()
            }
            DelegateAction::Unknown => "Error: unknown delegate_task action".into(),
            DelegateAction::List => subagent::list_json(&ctx.subagents.list(&ctx.session_id)),
            DelegateAction::Stop => {
                let Some(id) = args.get("childSessionId").and_then(|v| v.as_str()) else {
                    return "Error: childSessionId required".into();
                };
                let Ok(child) = SessionId::parse(id) else {
                    return "Error: invalid childSessionId".into();
                };
                if ctx.subagents.cancel_child(&child) {
                    serde_json::json!({"status":"cancelled","childSessionId": id}).to_string()
                } else {
                    format!("Error: unknown childSessionId {id}")
                }
            }
            DelegateAction::Changes | DelegateAction::Merge | DelegateAction::Discard => {
                worktree_action(action, &args, &ctx).await
            }
            DelegateAction::Spawn => spawn_tasks(&args, &ctx, spawner.as_ref()),
        }
    }
}

/// H2: parent-side review of an isolated child's changeset. Git inspection
/// runs off-loop; ownership is enforced through the subagent registry, so a
/// session can only touch its own children.
async fn worktree_action(action: DelegateAction, args: &Value, ctx: &ToolCtx) -> String {
    let Some(id) = args.get("childSessionId").and_then(|v| v.as_str()) else {
        return "Error: childSessionId required".into();
    };
    let Ok(child) = SessionId::parse(id) else {
        return "Error: invalid childSessionId".into();
    };
    if ctx.subagents.worktree_parent(&child).as_ref() != Some(&ctx.session_id) {
        return format!("Error: {id} has no isolated worktree owned by this session");
    }
    let Some(handle) = ctx.subagents.worktree_handle(&child) else {
        return format!("Error: no worktree for {id}");
    };
    let (merged, pruned) = ctx
        .subagents
        .worktree_flags(&child)
        .unwrap_or((false, false));
    match action {
        DelegateAction::Changes => {
            let blocking = handle.clone();
            let report = match tokio::task::spawn_blocking(move || {
                crate::worktree::WorktreeHandle::inspect(&blocking)
            })
            .await
            {
                Ok(report) => report,
                Err(err) => return format!("Error: changeset inspection failed: {err}"),
            };
            let mut json = crate::worktree::WorktreeHandle::report_json(&report, merged, pruned);
            json["patch"] = serde_json::json!(report.patch);
            json.to_string()
        }
        DelegateAction::Merge => {
            if merged {
                return serde_json::json!({"status": "alreadyApplied"}).to_string();
            }
            let blocking = handle.clone();
            let outcome = match tokio::task::spawn_blocking(move || {
                crate::worktree::merge(&blocking)
            })
            .await
            {
                Ok(outcome) => outcome,
                Err(err) => return format!("Error: merge failed: {err}"),
            };
            if matches!(outcome, crate::worktree::MergeOutcome::Merged) {
                ctx.subagents.mark_worktree_merged(&child);
                // A merged worktree has served its purpose; clean it up so
                // repeated merges of other children do not pile up.
                let cleanup = handle.clone();
                if tokio::task::spawn_blocking(move || cleanup.remove())
                    .await
                    .map(|r| r.is_ok())
                    .unwrap_or(false)
                {
                    ctx.subagents.mark_worktree_pruned(&child);
                }
            }
            let mut json = outcome.to_json();
            json["branch"] = serde_json::json!(handle.branch);
            json.to_string()
        }
        DelegateAction::Discard => {
            let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            let blocking = handle.clone();
            let res =
                tokio::task::spawn_blocking(move || crate::worktree::discard(&blocking, force))
                    .await;
            match res {
                Ok(Ok(value)) => {
                    ctx.subagents.mark_worktree_pruned(&child);
                    value.to_string()
                }
                Ok(Err(err)) => format!("Error: {err}"),
                Err(err) => format!("Error: discard failed: {err}"),
            }
        }
        _ => "Error: internal worktree action mismatch".into(),
    }
}

fn spawn_tasks(
    args: &Value,
    ctx: &ToolCtx,
    spawner: &dyn Fn(Vec<SpawnRequest>) -> Result<Vec<SessionId>, String>,
) -> String {
    let max_depth = ctx.settings.delegation_max_depth.clamp(1, 2);
    let next_depth = ctx.depth + 1;
    if next_depth > max_depth {
        return "Error: subagent depth limit".into();
    }

    let tasks = parse_tasks(args);
    if let Err(err) = &tasks {
        return err.clone();
    }
    let tasks = tasks.unwrap();
    if tasks.is_empty() {
        return "Error: goal or tasks[] required".into();
    }
    let n = tasks.len() as u32;
    if n > ctx.subagents.max_concurrent() {
        return "Error: too many concurrent subagents".into();
    }
    if ctx.subagents.over_cap(&ctx.root_session, n) {
        return "Error: too many concurrent subagents".into();
    }

    let cap = ctx.settings.child_max_iterations().max(1);
    let per_task = args
        .get("max_iterations")
        .and_then(|v| v.as_u64())
        .map(|n| (n as u32).clamp(1, cap))
        .unwrap_or(cap);

    let isolation = match args.get("isolation").and_then(|v| v.as_str()) {
        Some("shared") => Some(false),
        Some("worktree") => Some(true),
        Some(other) => {
            return format!("Error: unknown isolation '{other}' (worktree or shared)");
        }
        None => None,
    };
    let goals: Vec<String> = tasks.iter().map(|(g, _)| g.clone()).collect();
    let reqs: Vec<SpawnRequest> = tasks
        .into_iter()
        .map(|(goal, context)| SpawnRequest {
            parent: ctx.session_id.clone(),
            root: ctx.root_session.clone(),
            parent_tool_call_id: ctx.tool_call_id.clone(),
            goal,
            context,
            depth: next_depth,
            delivery: Delivery::Handle,
            parent_cancel: ctx.cancel.clone(),
            max_iterations: per_task,
            isolation,
        })
        .collect();
    match spawner(reqs) {
        Ok(ids) => {
            let mut json: Value = serde_json::from_str(&subagent::handle_json(&ids, &goals))
                .unwrap_or_else(|_| Value::Null);
            if isolation == Some(true) {
                if let Some(obj) = json.as_object_mut() {
                    obj.insert("isolation".into(), json!("worktree"));
                }
            }
            json.to_string()
        }
        Err(err) => {
            if err.starts_with("Error:") {
                err
            } else {
                format!("Error: {err}")
            }
        }
    }
}

fn parse_tasks(args: &Value) -> Result<Vec<(String, String)>, String> {
    if let Some(arr) = args.get("tasks").and_then(|v| v.as_array()) {
        if args.get("goal").and_then(|v| v.as_str()).is_some() {
            return Err("Error: tasks and goal are mutually exclusive".into());
        }
        let mut out = Vec::new();
        for t in arr {
            let goal = t
                .get("goal")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if goal.is_empty() {
                return Err("Error: each task needs a goal".into());
            }
            let context = t
                .get("context")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            out.push((goal, context));
        }
        return Ok(out);
    }
    let goal = args
        .get("goal")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if goal.is_empty() {
        return Err("Error: goal or tasks[] required".into());
    }
    let context = args
        .get("context")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(vec![(goal, context)])
}

#[cfg(test)]
mod tests {
    use super::parse_tasks;
    use serde_json::json;

    #[test]
    fn parse_single_goal() {
        let tasks =
            parse_tasks(&json!({"goal":"summarize README","context":"keep it short"})).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].0, "summarize README");
        assert_eq!(tasks[0].1, "keep it short");
    }

    #[test]
    fn parse_tasks_array() {
        let tasks = parse_tasks(&json!({
            "tasks": [
                {"goal": "a"},
                {"goal": "b", "context": "c"}
            ]
        }))
        .unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[1].1, "c");
    }

    #[test]
    fn parse_rejects_goal_and_tasks_together() {
        let err = parse_tasks(&json!({
            "goal": "a",
            "tasks": [{"goal": "b"}]
        }))
        .unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn parse_rejects_empty_goal() {
        let err = parse_tasks(&json!({})).unwrap_err();
        assert!(err.contains("goal or tasks"), "{err}");
        let err = parse_tasks(&json!({"tasks": [{"goal": "  "}]})).unwrap_err();
        assert!(err.contains("each task needs a goal"), "{err}");
        let empty = parse_tasks(&json!({"tasks": []})).unwrap();
        assert!(empty.is_empty());
    }
}
