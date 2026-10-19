//! Deterministic, offline evaluation primitives.
//!
//! These types deliberately do not invoke a provider. A future runner supplies
//! a temporary workspace and evaluates its resulting file tree against a small,
//! reviewable contract.

pub mod live;
pub mod report;

use crate::provider::{ScriptedProvider, ScriptedTurn};
use crate::store::Store;
use crate::{
    AgentHost, CliHost, McpServerConfig, PermissionMode, PromptRequest, RecordingHost, Runtime,
    Settings, TraceEvent,
};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EvalFixture {
    /// Workspace-relative paths and their UTF-8 file content.
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileExpectation {
    pub exact: BTreeMap<String, String>,
    pub absent: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalScore {
    pub passed: bool,
    pub failures: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TraceExpectation {
    /// Required control-plane event kinds. Text and tool arguments are
    /// intentionally excluded. Tool start may precede model completion when
    /// early dispatch is enabled, so concurrent events have no total order.
    pub kinds: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ScriptedScenario {
    pub prompt: String,
    pub fixture: EvalFixture,
    pub expected: FileExpectation,
    pub turns: Vec<ScriptedTurn>,
    pub expected_trace: Option<TraceExpectation>,
    /// Override the default Auto permission posture for a security fixture.
    pub permission_mode: Option<PermissionMode>,
    /// Cancel an in-flight scripted completion after this delay.
    pub cancel_after_ms: Option<u64>,
    /// Deterministic alternative to [`Self::cancel_after_ms`]: cancel once
    /// the trace contains `count` events of `kind`. Both sides of a
    /// baseline/replay pair then cancel at the same semantic point instead of
    /// racing a wall-clock timer against the scheduler — under suite load a
    /// fixed-ms cancel flips which side of a boundary the cancel lands on,
    /// which is exactly the nondeterminism this suite exists to exclude.
    pub cancel_after_trace: Option<(String, usize)>,
    /// Hard wall-clock cap for this individual scenario. A timeout first
    /// cancels the production Runtime session, then waits briefly for its
    /// loop (and any children) to settle before reporting the failure.
    pub timeout_ms: Option<u64>,
    /// Treat a known, deterministic failure as the expected result. The
    /// value is matched against the internal error only; reports expose a
    /// category, not provider or tool error contents.
    pub expected_failure: Option<String>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub mcp_server_ids: Vec<String>,
    /// MCP server IDs that must be isolated failures: unavailable themselves,
    /// but not fatal to the selected built-in tool path.
    pub expected_mcp_failure_ids: Vec<String>,
    /// Additional turns in the same session, optionally preceded by an
    /// explicit compact. This keeps compaction coverage in the real runtime.
    pub follow_up_prompts: Vec<String>,
    pub compact_before_follow_up: bool,
    pub permission_host: EvalPermissionHost,
    pub expected_tool_result_ids: Vec<String>,
    pub expected_cancelled_tool_ids: Vec<String>,
    pub subagent_stale_idle_sec: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum EvalPermissionHost {
    #[default]
    Cli,
    Allow,
    HangUntilCancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScriptedRunResult {
    pub outcome: ScenarioOutcome,
    pub score: EvalScore,
    pub trace: Vec<TraceEvent>,
    pub model_calls: usize,
    pub stop_reason: String,
    pub trace_score: Option<EvalScore>,
    pub git_diff: GitDiff,
    /// Content-free record used to prove a replay did not add, drop, or
    /// reorder a tool side effect. Tool output itself remains out of reports.
    pub tool_transcript: Vec<ToolTranscript>,
    pub mcp_transcript: Vec<McpTranscript>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioOutcome {
    Completed,
    ExpectedFailure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolTranscript {
    pub call_id: String,
    pub name: String,
    pub result_sha256: String,
    pub result_present: bool,
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTranscript {
    pub id: String,
    pub ready: bool,
    pub has_error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReplayScore {
    pub passed: bool,
    pub failures: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReplayResult {
    pub baseline: ScriptedRunResult,
    pub replay: ScriptedRunResult,
    pub score: ReplayScore,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiff {
    pub changed_paths: Vec<String>,
    pub patch: String,
}

#[derive(Clone, Debug)]
pub struct ScriptedSuiteCase {
    pub name: String,
    pub scenario: ScriptedScenario,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScriptedSuiteResult {
    pub passed: bool,
    pub cases: Vec<(String, ScriptedRunResult)>,
}

pub fn smoke_suite() -> Vec<ScriptedSuiteCase> {
    vec![ScriptedSuiteCase {
        name: "write-file".into(),
        scenario: ScriptedScenario {
            prompt: "create the answer file".into(),
            fixture: EvalFixture::default(),
            expected: FileExpectation {
                exact: BTreeMap::from([("answer.txt".into(), "42\n".into())]),
                absent: Vec::new(),
            },
            turns: vec![
                ScriptedTurn::tools("", vec![crate::provider::types::ToolCall {
                    id: "call_eval_write".into(), name: "write_file".into(),
                    arguments: serde_json::json!({"path":"answer.txt","content":"42\n"}).to_string(),
                }]),
                ScriptedTurn::text("done"),
            ],
            expected_trace: Some(TraceExpectation { kinds: vec![
                "run.started".into(), "model.requested".into(), "model.completed".into(),
                "tool.started".into(), "tool.finished".into(), "model.requested".into(),
                "model.completed".into(), "run.finished".into(),
            ]}),
            permission_mode: None,
            cancel_after_ms: None, cancel_after_trace: None,
            timeout_ms: Some(2_000), expected_failure: None,
            mcp_servers: Vec::new(), mcp_server_ids: Vec::new(), expected_mcp_failure_ids: Vec::new(), follow_up_prompts: Vec::new(), compact_before_follow_up: false, permission_host: EvalPermissionHost::Cli, expected_tool_result_ids: Vec::new(), expected_cancelled_tool_ids: Vec::new(), subagent_stale_idle_sec: None,
        },
    }, ScriptedSuiteCase {
        name: "patch-file".into(),
        scenario: ScriptedScenario {
            prompt: "update the greeting".into(),
            fixture: EvalFixture { files: BTreeMap::from([("greeting.txt".into(), "hello, world\n".into())]) },
            expected: FileExpectation { exact: BTreeMap::from([("greeting.txt".into(), "hello, dandelion\n".into())]), absent: Vec::new() },
            turns: vec![
                ScriptedTurn::tools("", vec![crate::provider::types::ToolCall {
                    id: "call_eval_patch".into(), name: "patch".into(),
                    arguments: serde_json::json!({"path":"greeting.txt", "old_string":"world", "new_string":"dandelion"}).to_string(),
                }]),
                ScriptedTurn::text("patched"),
            ],
            expected_trace: None,
            permission_mode: None,
            cancel_after_ms: None, cancel_after_trace: None,
            timeout_ms: Some(2_000), expected_failure: None,
            mcp_servers: Vec::new(), mcp_server_ids: Vec::new(), expected_mcp_failure_ids: Vec::new(), follow_up_prompts: Vec::new(), compact_before_follow_up: false, permission_host: EvalPermissionHost::Cli, expected_tool_result_ids: Vec::new(), expected_cancelled_tool_ids: Vec::new(), subagent_stale_idle_sec: None,
        },
    }, ScriptedSuiteCase {
        name: "schema-rejection".into(),
        scenario: ScriptedScenario {
            prompt: "attempt an invalid edit".into(),
            fixture: EvalFixture::default(),
            expected: FileExpectation { exact: BTreeMap::new(), absent: vec!["must-not-exist.txt".into()] },
            turns: vec![
                ScriptedTurn::tools("", vec![crate::provider::types::ToolCall {
                    id: "call_eval_invalid".into(), name: "write_file".into(),
                    arguments: serde_json::json!({"path":"must-not-exist.txt"}).to_string(),
                }]),
                ScriptedTurn::text("invalid request rejected"),
            ],
            expected_trace: None,
            permission_mode: None,
            cancel_after_ms: None, cancel_after_trace: None,
            timeout_ms: Some(2_000), expected_failure: None,
            mcp_servers: Vec::new(), mcp_server_ids: Vec::new(), expected_mcp_failure_ids: Vec::new(), follow_up_prompts: Vec::new(), compact_before_follow_up: false, permission_host: EvalPermissionHost::Cli, expected_tool_result_ids: Vec::new(), expected_cancelled_tool_ids: Vec::new(), subagent_stale_idle_sec: None,
        },
    }, ScriptedSuiteCase {
        name: "permission-denied-write".into(),
        scenario: ScriptedScenario {
            prompt: "attempt a denied edit".into(),
            fixture: EvalFixture::default(),
            expected: FileExpectation { exact: BTreeMap::new(), absent: vec!["denied.txt".into()] },
            turns: vec![
                ScriptedTurn::tools("", vec![crate::provider::types::ToolCall {
                    id: "call_eval_denied".into(), name: "write_file".into(),
                    arguments: serde_json::json!({"path":"denied.txt", "content":"no"}).to_string(),
                }]),
                ScriptedTurn::text("denied"),
            ],
            expected_trace: None,
            permission_mode: Some(PermissionMode::Deny),
            cancel_after_ms: None, cancel_after_trace: None,
            timeout_ms: Some(2_000), expected_failure: None,
            mcp_servers: Vec::new(), mcp_server_ids: Vec::new(), expected_mcp_failure_ids: Vec::new(), follow_up_prompts: Vec::new(), compact_before_follow_up: false, permission_host: EvalPermissionHost::Cli, expected_tool_result_ids: Vec::new(), expected_cancelled_tool_ids: Vec::new(), subagent_stale_idle_sec: None,
        },
    }, ScriptedSuiteCase {
        name: "cancelled-completion".into(),
        scenario: ScriptedScenario {
            prompt: "wait for cancellation".into(),
            fixture: EvalFixture::default(),
            expected: FileExpectation::default(),
            turns: vec![ScriptedTurn::hang_until_cancel()],
            expected_trace: None,
            permission_mode: None,
            cancel_after_ms: Some(10), cancel_after_trace: Some(("model.requested".into(), 1)),
            timeout_ms: Some(2_000), expected_failure: None,
            mcp_servers: Vec::new(), mcp_server_ids: Vec::new(), expected_mcp_failure_ids: Vec::new(), follow_up_prompts: Vec::new(), compact_before_follow_up: false, permission_host: EvalPermissionHost::Cli, expected_tool_result_ids: Vec::new(), expected_cancelled_tool_ids: Vec::new(), subagent_stale_idle_sec: None,
        },
    }, ScriptedSuiteCase {
        name: "mcp-failure-isolated".into(),
        scenario: ScriptedScenario {
            prompt: "write despite unavailable MCP".into(),
            fixture: EvalFixture::default(),
            expected: FileExpectation { exact: BTreeMap::from([("still-works.txt".into(), "ok\n".into())]), absent: Vec::new() },
            turns: vec![
                ScriptedTurn::tools("", vec![crate::provider::types::ToolCall {
                    id: "call_eval_builtin".into(), name: "write_file".into(),
                    arguments: serde_json::json!({"path":"still-works.txt", "content":"ok\n"}).to_string(),
                }]),
                ScriptedTurn::text("built-in completed"),
            ],
            expected_trace: None,
            permission_mode: None,
            cancel_after_ms: None, cancel_after_trace: None,
            timeout_ms: Some(2_000), expected_failure: None,
            mcp_servers: vec![McpServerConfig { id: "broken".into(), enabled: true, command: "dandelion-eval-mcp-not-found".into(), ..Default::default() }],
            mcp_server_ids: vec!["broken".into()],
            expected_mcp_failure_ids: vec!["broken".into()],
            follow_up_prompts: Vec::new(), compact_before_follow_up: false, permission_host: EvalPermissionHost::Cli, expected_tool_result_ids: Vec::new(), expected_cancelled_tool_ids: Vec::new(), subagent_stale_idle_sec: None,
        },
    }, ScriptedSuiteCase {
        name: "concurrent-read-write".into(),
        scenario: ScriptedScenario {
            prompt: "write and then inspect the shared file".into(), fixture: EvalFixture::default(),
            expected: FileExpectation { exact: BTreeMap::from([("shared.txt".into(), "fresh\n".into())]), absent: Vec::new() },
            turns: vec![
                ScriptedTurn::tools("", vec![
                    crate::provider::types::ToolCall { id: "call_conflict_write".into(), name: "write_file".into(), arguments: serde_json::json!({"path":"shared.txt","content":"fresh\n"}).to_string() },
                    crate::provider::types::ToolCall { id: "call_conflict_read".into(), name: "read_file".into(), arguments: serde_json::json!({"path":"shared.txt"}).to_string() },
                ]),
                ScriptedTurn::text("conflict handled"),
            ], expected_trace: None, permission_mode: None, cancel_after_ms: None, cancel_after_trace: None, timeout_ms: Some(2_000), expected_failure: None,
            mcp_servers: Vec::new(), mcp_server_ids: Vec::new(), expected_mcp_failure_ids: Vec::new(), follow_up_prompts: Vec::new(), compact_before_follow_up: false, permission_host: EvalPermissionHost::Cli,
            expected_tool_result_ids: vec!["call_conflict_write".into(), "call_conflict_read".into()], expected_cancelled_tool_ids: Vec::new(), subagent_stale_idle_sec: None,
        },
    }, ScriptedSuiteCase {
        name: "compact-then-continue".into(),
        scenario: ScriptedScenario {
            prompt: "establish context".into(), fixture: EvalFixture::default(), expected: FileExpectation::default(),
            turns: vec![ScriptedTurn::text("first turn"), ScriptedTurn::text("warm-up turn"), ScriptedTurn::text("summary"), ScriptedTurn::text("continued after compact")],
            expected_trace: Some(TraceExpectation { kinds: vec!["run.started".into(), "run.finished".into()] }), permission_mode: None, cancel_after_ms: None, cancel_after_trace: None, timeout_ms: Some(2_000), expected_failure: None,
            mcp_servers: Vec::new(), mcp_server_ids: Vec::new(), expected_mcp_failure_ids: Vec::new(), follow_up_prompts: vec!["warm up history".into(), "continue from the compacted context".into()], compact_before_follow_up: true, permission_host: EvalPermissionHost::Cli,
            expected_tool_result_ids: Vec::new(), expected_cancelled_tool_ids: Vec::new(), subagent_stale_idle_sec: None,
        },
    }, ScriptedSuiteCase {
        name: "permission-ask-allow".into(),
        scenario: ScriptedScenario {
            prompt: "write with user approval".into(), fixture: EvalFixture::default(), expected: FileExpectation { exact: BTreeMap::from([("approved.txt".into(), "yes\n".into())]), absent: Vec::new() },
            turns: vec![ScriptedTurn::tools("", vec![crate::provider::types::ToolCall { id: "call_ask_write".into(), name: "write_file".into(), arguments: serde_json::json!({"path":"approved.txt","content":"yes\n"}).to_string() }]), ScriptedTurn::text("approved")],
            expected_trace: None, permission_mode: Some(PermissionMode::Ask), cancel_after_ms: None, cancel_after_trace: None, timeout_ms: Some(2_000), expected_failure: None,
            mcp_servers: Vec::new(), mcp_server_ids: Vec::new(), expected_mcp_failure_ids: Vec::new(), follow_up_prompts: Vec::new(), compact_before_follow_up: false, permission_host: EvalPermissionHost::Allow,
            expected_tool_result_ids: vec!["call_ask_write".into()], expected_cancelled_tool_ids: Vec::new(), subagent_stale_idle_sec: None,
        },
    }, ScriptedSuiteCase {
        name: "cancel-pairs-tool-results".into(),
        scenario: ScriptedScenario {
            prompt: "start two permissioned operations".into(), fixture: EvalFixture::default(), expected: FileExpectation { exact: BTreeMap::new(), absent: vec!["cancelled.txt".into()] },
            turns: vec![ScriptedTurn::tools("", vec![
                crate::provider::types::ToolCall { id: "call_cancel_write".into(), name: "write_file".into(), arguments: serde_json::json!({"path":"cancelled.txt","content":"no"}).to_string() },
                crate::provider::types::ToolCall { id: "call_cancel_read".into(), name: "read_file".into(), arguments: serde_json::json!({"path":"cancelled.txt"}).to_string() },
            // Cancel once the first tool has started (the write, which then
            // hangs in the permission gate). The read is in a later segment,
            // so the cancelled-batch path pairs it with the cancelled
            // result — the same mechanism the fixed-ms trigger raced.
            ])], expected_trace: None, permission_mode: Some(PermissionMode::Ask), cancel_after_ms: None, cancel_after_trace: Some(("tool.started".into(), 1)), timeout_ms: Some(2_000), expected_failure: None,
            mcp_servers: Vec::new(), mcp_server_ids: Vec::new(), expected_mcp_failure_ids: Vec::new(), follow_up_prompts: Vec::new(), compact_before_follow_up: false, permission_host: EvalPermissionHost::HangUntilCancelled,
            // The case's invariant is pairing (every call gets a result) and
            // that the hung WRITE ends cancelled. Whether the read lands as
            // "cancelled" or as its own completed error depends on which side
            // of the gate cancel catches it — that is scheduling noise, not
            // behavior, so it is asserted via result_present only.
            expected_tool_result_ids: vec!["call_cancel_write".into(), "call_cancel_read".into()], expected_cancelled_tool_ids: vec!["call_cancel_write".into()], subagent_stale_idle_sec: None,
        },
    }, ScriptedSuiteCase {
        name: "subagent-success".into(),
        scenario: ScriptedScenario {
            prompt: "delegate a small investigation".into(), fixture: EvalFixture::default(), expected: FileExpectation::default(),
            turns: vec![ScriptedTurn::tools("", vec![crate::provider::types::ToolCall { id: "call_child".into(), name: "delegate_task".into(), arguments: serde_json::json!({"goal":"inspect the fixture"}).to_string() }]), ScriptedTurn::delayed_text("child summary", std::time::Duration::from_millis(50)), ScriptedTurn::text("parent completed")],
            expected_trace: None, permission_mode: None, cancel_after_ms: None, cancel_after_trace: None, timeout_ms: Some(5_000), expected_failure: None,
            mcp_servers: Vec::new(), mcp_server_ids: Vec::new(), expected_mcp_failure_ids: Vec::new(), follow_up_prompts: Vec::new(), compact_before_follow_up: false, permission_host: EvalPermissionHost::Cli,
            expected_tool_result_ids: vec!["call_child".into()], expected_cancelled_tool_ids: Vec::new(), subagent_stale_idle_sec: None,
        },
    }, ScriptedSuiteCase {
        name: "subagent-stale-failure".into(),
        scenario: ScriptedScenario {
            prompt: "delegate a stuck task".into(), fixture: EvalFixture::default(), expected: FileExpectation::default(),
            turns: vec![ScriptedTurn::tools("", vec![crate::provider::types::ToolCall { id: "call_child_stale".into(), name: "delegate_task".into(), arguments: serde_json::json!({"goal":"wait"}).to_string() }]), ScriptedTurn::hang_until_cancel(), ScriptedTurn::text("parent recovered")],
            expected_trace: None, permission_mode: None, cancel_after_ms: None, cancel_after_trace: None, timeout_ms: Some(4_000), expected_failure: None,
            mcp_servers: Vec::new(), mcp_server_ids: Vec::new(), expected_mcp_failure_ids: Vec::new(), follow_up_prompts: Vec::new(), compact_before_follow_up: false, permission_host: EvalPermissionHost::Cli,
            expected_tool_result_ids: vec!["call_child_stale".into()], expected_cancelled_tool_ids: Vec::new(), subagent_stale_idle_sec: Some(1),
        },
    }]
}

impl EvalFixture {
    /// Materialize only paths contained by `root`; fixture definitions may not
    /// escape their temporary workspace through an absolute or parent path.
    pub fn materialize(&self, root: &Path) -> Result<()> {
        for (relative, content) in &self.files {
            let path = fixture_path(root, relative)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, content)?;
        }
        Ok(())
    }
}

pub fn score_files(root: &Path, expected: &FileExpectation) -> Result<EvalScore> {
    let mut failures = Vec::new();
    for (relative, wanted) in &expected.exact {
        let path = fixture_path(root, relative)?;
        match fs::read_to_string(&path) {
            Ok(actual) if &actual == wanted => {}
            Ok(_) => failures.push(format!("content mismatch: {relative}")),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                failures.push(format!("missing file: {relative}"))
            }
            Err(err) => return Err(err.into()),
        }
    }
    for relative in &expected.absent {
        let path = fixture_path(root, relative)?;
        if path.exists() {
            failures.push(format!("unexpected file: {relative}"));
        }
    }
    Ok(EvalScore {
        passed: failures.is_empty(),
        failures,
    })
}

pub fn score_trace(events: &[TraceEvent], expected: &TraceExpectation) -> EvalScore {
    let actual: Vec<_> = events.iter().map(|event| event.kind.clone()).collect();
    let missing: Vec<_> = expected
        .kinds
        .iter()
        .filter(|kind| !actual.contains(kind))
        .cloned()
        .collect();
    let passed = missing.is_empty();
    EvalScore {
        passed,
        failures: (!passed)
            .then(|| format!("trace missing {:?}; got {:?}", missing, actual))
            .into_iter()
            .collect(),
    }
}

/// Execute a deterministic scenario through the production Runtime and its
/// only agent loop. Callers create and own the temporary workspace root.
pub async fn run_scripted_scenario(
    root: &Path,
    scenario: ScriptedScenario,
) -> Result<ScriptedRunResult> {
    scenario.fixture.materialize(root)?;
    initialize_git_baseline(root)?;
    let data_dir = root.parent().unwrap_or(root).join(format!(
        "dandelion-eval-data-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let host: Arc<dyn AgentHost> = match scenario.permission_host {
        EvalPermissionHost::Cli => Arc::new(CliHost::with_data_dir(data_dir.clone())),
        EvalPermissionHost::Allow => Arc::new(RecordingHost::new(data_dir.clone())),
        EvalPermissionHost::HangUntilCancelled => {
            Arc::new(RecordingHost::new(data_dir.clone()).hang_on_permission())
        }
    };
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(root.to_path_buf());
    settings.permission_mode = scenario.permission_mode.unwrap_or(PermissionMode::Auto);
    settings.mcp_servers = scenario.mcp_servers.clone();
    settings.mcp_spawn_timeout_ms = 1_000;
    // The H0 baseline is replay-deterministic: worktree branch names embed
    // the child session id, so H2 isolation stays off inside eval (H2 has
    // its own offline gates in tests/h2_worktree.rs).
    settings.delegation_worktree_isolation = false;
    if scenario.compact_before_follow_up {
        settings.compact_keep_turns = 1;
    }
    if let Some(seconds) = scenario.subagent_stale_idle_sec {
        settings.delegation_stale_idle_sec = seconds;
    }
    let provider = Arc::new(ScriptedProvider::new(scenario.turns));
    let runtime = Runtime::new_with_provider(host, settings, provider.clone())?;
    let session = runtime.new_session(None);
    let mut request = PromptRequest::new(session.clone(), scenario.prompt);
    request.permission_mode = scenario.permission_mode;
    request.mcp_server_ids = scenario.mcp_server_ids.clone();
    let cancellation = if let Some((kind, count)) = scenario.cancel_after_trace.clone() {
        let running = runtime.clone();
        let session = session.clone();
        let session_id = session.to_string();
        let deadline =
            std::time::Duration::from_millis(scenario.timeout_ms.unwrap_or(30_000) + 5_000);
        Some(tokio::spawn(async move {
            let started = std::time::Instant::now();
            loop {
                let seen = running
                    .trace_events(&session_id, 1_000)
                    .map(|events| events.iter().filter(|event| event.kind == kind).count())
                    .unwrap_or(0);
                if seen >= count {
                    break;
                }
                if started.elapsed() > deadline {
                    return; // the scenario's own timeout reports the stall
                }
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
            let _ = running.cancel(session).await;
        }))
    } else {
        scenario.cancel_after_ms.map(|after_ms| {
            let running = runtime.clone();
            let session = session.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(after_ms)).await;
                let _ = running.cancel(session).await;
            })
        })
    };
    let prompt = runtime.prompt(request);
    tokio::pin!(prompt);
    let prompt_result = if let Some(timeout_ms) = scenario.timeout_ms {
        tokio::select! {
            result = &mut prompt => result,
            _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => {
                runtime.cancel(session.clone()).await?;
                let _ = tokio::time::timeout(std::time::Duration::from_secs(1), &mut prompt).await;
                Err(Error::Invalid("evaluation scenario timeout".into()))
            }
        }
    } else {
        prompt.await
    };
    if let Some(task) = cancellation {
        task.abort();
    }
    if let Err(err) = prompt_result {
        if scenario
            .expected_failure
            .as_ref()
            .is_some_and(|needle| err.to_string().contains(needle))
        {
            return Ok(expected_failure_result());
        }
        return Err(err);
    }
    if scenario.expected_failure.is_some() {
        return Err(Error::Invalid(
            "evaluation expected failure but scenario completed".into(),
        ));
    }
    let prompt_follow_up = |text: &String| {
        let mut follow_up = PromptRequest::new(session.clone(), text.clone());
        follow_up.permission_mode = scenario.permission_mode;
        follow_up.mcp_server_ids = scenario.mcp_server_ids.clone();
        runtime.prompt(follow_up)
    };
    let mut remaining_follow_ups = scenario.follow_up_prompts.iter();
    if scenario.compact_before_follow_up {
        let warm_up = remaining_follow_ups.next().ok_or_else(|| {
            Error::Invalid("compact scenario requires a warm-up follow-up prompt".into())
        })?;
        prompt_follow_up(warm_up).await?;
        runtime.compact(&session).await?;
    }
    for text in remaining_follow_ups {
        prompt_follow_up(text).await?;
    }
    let runs = runtime.agent_runs(&session.to_string(), 1)?;
    let stop_reason = runs
        .first()
        .and_then(|run| run.stop_reason.clone())
        .unwrap_or_else(|| "unknown".into());
    let trace = runtime.trace_events(&session.to_string(), 1000)?;
    let trace_score = scenario
        .expected_trace
        .as_ref()
        .map(|expected| score_trace(&trace, expected));
    let transcript_store = Store::open(&data_dir, 5_000)?;
    let tool_transcript = tool_transcript(&transcript_store, &session)?;
    let mcp_transcript = runtime
        .mcp_list()
        .await?
        .into_iter()
        .map(|item| McpTranscript {
            id: item.id,
            ready: item.ready,
            has_error: item.error.is_some(),
        })
        .collect::<Vec<_>>();
    let mut score = score_files(root, &scenario.expected)?;
    for expected_id in &scenario.expected_mcp_failure_ids {
        if !mcp_transcript
            .iter()
            .any(|item| item.id == *expected_id && !item.ready && item.has_error)
        {
            score.failures.push(format!(
                "MCP server did not expose isolated failure: {expected_id}"
            ));
        }
    }
    for expected_id in &scenario.expected_tool_result_ids {
        if !tool_transcript
            .iter()
            .any(|item| item.call_id == *expected_id && item.result_present)
        {
            score
                .failures
                .push(format!("tool result missing: {expected_id}"));
        }
    }
    for expected_id in &scenario.expected_cancelled_tool_ids {
        if !tool_transcript
            .iter()
            .any(|item| item.call_id == *expected_id && item.cancelled)
        {
            score
                .failures
                .push(format!("cancelled tool result missing: {expected_id}"));
        }
    }
    score.passed = score.failures.is_empty();
    Ok(ScriptedRunResult {
        outcome: ScenarioOutcome::Completed,
        score,
        trace,
        model_calls: provider.complete_count(),
        stop_reason,
        trace_score,
        git_diff: collect_git_diff(root)?,
        tool_transcript,
        mcp_transcript,
    })
}

fn expected_failure_result() -> ScriptedRunResult {
    ScriptedRunResult {
        outcome: ScenarioOutcome::ExpectedFailure,
        score: EvalScore {
            passed: true,
            failures: Vec::new(),
        },
        trace: Vec::new(),
        model_calls: 0,
        stop_reason: "expected_failure".into(),
        trace_score: None,
        git_diff: GitDiff::default(),
        tool_transcript: Vec::new(),
        mcp_transcript: Vec::new(),
    }
}

/// Re-run an immutable scripted fixture in a fresh workspace and compare only
/// deterministic control-plane evidence. Raw prompts, tool arguments, and
/// tool output never enter the replay report.
pub async fn replay_scripted_scenario(
    root: &Path,
    scenario: ScriptedScenario,
) -> Result<ReplayResult> {
    let baseline_root = root.join("baseline");
    let replay_root = root.join("replay");
    fs::create_dir_all(&baseline_root)?;
    fs::create_dir_all(&replay_root)?;
    // Captured before `scenario` is consumed by the runs below.
    let cancel_races_tools = scenario.cancel_after_trace.is_some();
    let baseline = run_scripted_scenario(&baseline_root, scenario.clone()).await?;
    let replay = run_scripted_scenario(&replay_root, scenario).await?;
    // The transcript gate asserts the *invariant*: no tool added, dropped,
    // reordered, or lost its result, and every write side effect is
    // byte-identical. Per-call details of read-only calls (result bytes,
    // cancelled-vs-completed shape) may legitimately differ when the
    // scenario races a read against a concurrent write or against cancel —
    // file expectations and git diff still pin down the actual semantics.
    fn normalized(t: &ToolTranscript) -> (String, String, String, bool) {
        (
            t.call_id.clone(),
            t.name.clone(),
            if report::is_read_only(&t.name) {
                String::new()
            } else {
                t.result_sha256.clone()
            },
            t.result_present,
        )
    }
    let mut failures = Vec::new();
    let norm = |t: &[ToolTranscript]| t.iter().map(normalized).collect::<Vec<_>>();
    if norm(&baseline.tool_transcript) != norm(&replay.tool_transcript) {
        // Readable per-call diff: which call changed shape, bytes, or cancel
        // state — not just a boolean "differs".
        let replay_n = norm(&replay.tool_transcript);
        let shape = |t: &ToolTranscript| {
            format!(
                "{}:{}",
                t.name,
                if t.cancelled { "cancelled" } else { "done" }
            )
        };
        let mut detail = String::new();
        for (i, b) in baseline.tool_transcript.iter().enumerate() {
            match replay_n.get(i) {
                None => detail.push_str(&format!(" replay dropped call #{i} {};", b.name)),
                Some(r) if &normalized(b) != r => {
                    let r0 = &replay.tool_transcript[i];
                    detail.push_str(&format!(
                        " call #{} {}{};",
                        i,
                        shape(b),
                        if b.result_sha256 == r0.result_sha256 {
                            " bytes same".to_string()
                        } else {
                            " BYTES DIFFER".to_string()
                        }
                    ));
                }
                _ => {}
            }
        }
        if let Some(extra) = replay.tool_transcript.get(baseline.tool_transcript.len()) {
            detail.push_str(&format!(" replay added extra call {};", extra.name));
        }
        failures.push(format!("tool transcript differs:{detail}"));
    }
    if baseline.mcp_transcript != replay.mcp_transcript {
        let fmt = |t: &[McpTranscript]| {
            t.iter()
                .map(|m| format!("{}={}", m.id, if m.ready { "ready" } else { "down" }))
                .collect::<Vec<_>>()
                .join(",")
        };
        failures.push(format!(
            "MCP transcript differs: [{}] vs [{}]",
            fmt(&baseline.mcp_transcript),
            fmt(&replay.mcp_transcript)
        ));
    }
    if baseline.git_diff != replay.git_diff {
        let only = |a: &[String], b: &[String]| -> Vec<String> {
            a.iter().filter(|p| !b.contains(p)).cloned().collect()
        };
        failures.push(format!(
            "git diff differs: baseline-only {:?}, replay-only {:?}",
            only(
                &baseline.git_diff.changed_paths,
                &replay.git_diff.changed_paths
            ),
            only(
                &replay.git_diff.changed_paths,
                &baseline.git_diff.changed_paths
            )
        ));
    }
    if baseline.outcome != replay.outcome
        || baseline.model_calls != replay.model_calls
        || baseline.stop_reason != replay.stop_reason
    {
        failures.push("model completion count or stop reason differs".into());
    }
    // Determinism lives in event *counts and pairing*, not in a total order
    // across independent boundaries: with early dispatch, `tool.started` may
    // legitimately precede or follow `model.completed` depending on
    // scheduling (this module's own TraceExpectation contract says so).
    // Real behavioral drift is still caught exactly: the multiset comparison
    // below catches any missing/extra round or event, and the tool
    // transcript / git diff comparisons above catch semantic changes.
    // For scenarios that fire cancel on a trace event, even the tool.*
    // *counts* race: cancel can catch the loop between dispatching the first
    // tool and the second, so `tool.started` legitimately runs 1 in one run
    // and 2 in the next. Those cases still compare the deterministic
    // model.*/run.* counts; the pairing invariant, which tool actually
    // cancelled, and file absence are asserted per-run by
    // expected_tool_result_ids / expected_cancelled_tool_ids / FileExpectation.
    fn kind_counts(run: &ScriptedRunResult, skip_tool_events: bool) -> BTreeMap<&str, usize> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for event in &run.trace {
            if skip_tool_events && (event.kind == "tool.started" || event.kind == "tool.finished") {
                continue;
            }
            *counts.entry(event.kind.as_str()).or_default() += 1;
        }
        counts
    }
    if kind_counts(&baseline, cancel_races_tools) != kind_counts(&replay, cancel_races_tools) {
        failures.push(format!(
            "trace event counts differ: baseline {:?} replay {:?}",
            kind_counts(&baseline, cancel_races_tools),
            kind_counts(&replay, cancel_races_tools)
        ));
    }
    Ok(ReplayResult {
        baseline,
        replay,
        score: ReplayScore {
            passed: failures.is_empty(),
            failures,
        },
    })
}

fn tool_transcript(store: &Store, session: &crate::SessionId) -> Result<Vec<ToolTranscript>> {
    use crate::provider::types::ChatMessage;
    let messages = store.load_openai_messages(session)?;
    let results = messages
        .iter()
        .filter_map(|message| match message {
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => Some((
                tool_call_id.clone(),
                (sha256(content), content == "Error: cancelled"),
            )),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    Ok(messages
        .into_iter()
        .flat_map(|message| match message {
            ChatMessage::Assistant { tool_calls, .. } => tool_calls
                .into_iter()
                .map(|call| ToolTranscript {
                    result_sha256: results
                        .get(&call.id)
                        .map(|result| result.0.clone())
                        .unwrap_or_default(),
                    result_present: results.contains_key(&call.id),
                    cancelled: results.get(&call.id).is_some_and(|result| result.1),
                    call_id: call.id,
                    name: call.name,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect())
}

fn sha256(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

/// Initialize a repository local to an evaluation fixture. No global Git
/// configuration is read or written; the temporary repository gets its own
/// identity so its baseline commit is deterministic.
pub fn initialize_git_baseline(root: &Path) -> Result<()> {
    git(root, ["init", "--quiet"])?;
    git(root, ["config", "user.email", "eval@dandelion.invalid"])?;
    git(root, ["config", "user.name", "Dandelion Eval"])?;
    git(root, ["add", "--all"])?;
    git(
        root,
        [
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "evaluation baseline",
        ],
    )?;
    Ok(())
}

pub fn collect_git_diff(root: &Path) -> Result<GitDiff> {
    let status = git(root, ["status", "--porcelain"])?;
    let changed_paths = status
        .lines()
        .filter_map(|line| line.get(3..).map(str::trim))
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut patch = git(root, ["diff", "--no-ext-diff", "--binary", "HEAD"])?;
    for relative in status
        .lines()
        .filter(|line| line.starts_with("?? "))
        .filter_map(|line| line.get(3..))
    {
        patch.push_str(&git_no_index(root, "/dev/null", relative)?);
    }
    Ok(GitDiff {
        changed_paths,
        patch,
    })
}

fn git<const N: usize>(root: &Path, args: [&str; N]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|err| Error::Invalid(format!("evaluation requires git in PATH: {err}")))?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "evaluation git command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git_no_index(root: &Path, left: &str, right: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "--no-index", "--binary", "--", left, right])
        .current_dir(root)
        .output()
        .map_err(|err| Error::Invalid(format!("evaluation requires git in PATH: {err}")))?;
    // `git diff --no-index` returns 1 precisely when it found a difference.
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(Error::Invalid(format!(
            "evaluation git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub async fn run_scripted_suite(
    root: &Path,
    cases: Vec<ScriptedSuiteCase>,
) -> Result<ScriptedSuiteResult> {
    run_scripted_suite_stream(root, "smoke", cases, &mut |_| {}).await
}

/// H5 streaming runner: emits versioned JSONL events as cases finish so a CI
/// consumer sees progress (and partial results after a crash) rather than
/// only a final blob. `emit` receives every event including `suite.*`.
pub async fn run_scripted_suite_stream(
    root: &Path,
    suite_name: &str,
    cases: Vec<ScriptedSuiteCase>,
    emit: &mut dyn FnMut(&report::EvalEvent),
) -> Result<ScriptedSuiteResult> {
    let names: Vec<String> = cases.iter().map(|case| case.name.clone()).collect();
    emit(&report::EvalEvent::SuiteStarted {
        v: report::REPORT_VERSION,
        suite: suite_name.into(),
        case_count: names.len(),
        cases: names,
        pairing: None,
    });
    let mut results = Vec::new();
    let mut passed = true;
    let mut passed_cases = 0usize;
    let mut errored = 0usize;
    for (index, case) in cases.into_iter().enumerate() {
        let case_root = root.join(format!("{:03}-{}", index, case.name));
        fs::create_dir_all(&case_root)?;
        let started = std::time::Instant::now();
        // A case that dies (timeout, internal error) is a *reported failure*,
        // not a suite abort: CI gets the error detail in the JSONL and the
        // remaining cases still run. The sanitized message keeps secrets out.
        let replay = match replay_scripted_scenario(&case_root, case.scenario).await {
            Ok(replay) => replay,
            Err(err) => {
                let detail = crate::mcp::stdio::sanitize_error_text(&err.to_string());
                emit(&report::EvalEvent::CaseFinished {
                    v: report::REPORT_VERSION,
                    case: case.name.clone(),
                    passed: false,
                    failures: vec![format!("case error: {detail}")],
                    outcome: "error".into(),
                    model_calls: 0,
                    stop_reason: "error".into(),
                    changed_paths: Vec::new(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    tool_calls: Vec::new(),
                });
                passed = false;
                errored += 1;
                continue;
            }
        };
        let duration_ms = started.elapsed().as_millis() as u64;
        let result = replay.replay;
        let case_passed = replay.score.passed
            && result.score.passed
            && result.trace_score.as_ref().is_none_or(|score| score.passed);
        passed &= case_passed;
        passed_cases += usize::from(case_passed);
        let mut failures: Vec<String> = result.score.failures.clone();
        if let Some(score) = &result.trace_score {
            failures.extend(score.failures.clone());
        }
        failures.extend(replay.score.failures.clone());
        emit(&report::EvalEvent::CaseFinished {
            v: report::REPORT_VERSION,
            case: case.name.clone(),
            passed: case_passed,
            failures,
            outcome: match result.outcome {
                ScenarioOutcome::Completed => "completed".into(),
                ScenarioOutcome::ExpectedFailure => "expected_failure".into(),
            },
            model_calls: result.model_calls,
            stop_reason: result.stop_reason.clone(),
            changed_paths: result.git_diff.changed_paths.clone(),
            duration_ms,
            tool_calls: result
                .tool_transcript
                .iter()
                .map(|t| report::ToolCallRecord {
                    call_id: t.call_id.clone(),
                    name: t.name.clone(),
                    result_sha256: t.result_sha256.clone(),
                    result_present: t.result_present,
                    cancelled: t.cancelled,
                })
                .collect(),
        });
        results.push((case.name, result));
    }
    let failed_cases = results.len() - passed_cases + errored;
    emit(&report::EvalEvent::SuiteFinished {
        v: report::REPORT_VERSION,
        suite: suite_name.into(),
        passed,
        passed_cases,
        failed_cases,
    });
    Ok(ScriptedSuiteResult {
        passed,
        cases: results,
    })
}

/// Write the streaming JSONL report to disk (parents created).
pub fn write_jsonl_report(path: &Path, events: &[report::EvalEvent]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw: String = events.iter().map(report::EvalEvent::to_json_line).collect();
    fs::write(path, raw)?;
    Ok(())
}

/// Persist a portable, content-safe suite report. Full prompts and tool
/// arguments are never included: the report only serializes existing
/// control-plane trace metadata and the workspace diff.
pub fn write_suite_report(path: &Path, report: &ScriptedSuiteResult) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(report)?)?;
    Ok(())
}

fn fixture_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let candidate = Path::new(relative);
    if relative.trim().is_empty()
        || candidate.is_absolute()
        || candidate
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(Error::Invalid(format!("invalid fixture path: {relative}")));
    }
    Ok(root.join(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialize_and_score_are_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = EvalFixture {
            files: BTreeMap::from([("src/lib.rs".into(), "pub fn x() {}\n".into())]),
        };
        fixture.materialize(dir.path()).unwrap();
        let score = score_files(
            dir.path(),
            &FileExpectation {
                exact: BTreeMap::from([("src/lib.rs".into(), "pub fn x() {}\n".into())]),
                absent: vec!["target/leak.txt".into()],
            },
        )
        .unwrap();
        assert!(score.passed, "{:?}", score.failures);
    }

    #[test]
    fn fixture_paths_cannot_escape_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = EvalFixture {
            files: BTreeMap::from([("../outside.txt".into(), "no".into())]),
        };
        assert!(fixture.materialize(dir.path()).is_err());
    }

    #[tokio::test]
    async fn scripted_scenario_runs_through_agent_loop_and_scores_diff() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_scripted_scenario(
            dir.path(),
            ScriptedScenario {
                prompt: "create the answer file".into(),
                fixture: EvalFixture::default(),
                expected: FileExpectation {
                    exact: BTreeMap::from([("answer.txt".into(), "42\n".into())]),
                    absent: Vec::new(),
                },
                turns: vec![
                    ScriptedTurn::tools(
                        "",
                        vec![crate::provider::types::ToolCall {
                            id: "call_eval_write".into(),
                            name: "write_file".into(),
                            arguments: serde_json::json!({"path":"answer.txt","content":"42\n"})
                                .to_string(),
                        }],
                    ),
                    ScriptedTurn::text("done"),
                ],
                expected_trace: Some(TraceExpectation {
                    kinds: vec![
                        "run.started".into(),
                        "model.requested".into(),
                        "model.completed".into(),
                        "tool.started".into(),
                        "tool.finished".into(),
                        "model.requested".into(),
                        "model.completed".into(),
                        "run.finished".into(),
                    ],
                }),
                permission_mode: None,
                cancel_after_ms: None,
                cancel_after_trace: None,
                timeout_ms: Some(2_000),
                expected_failure: None,
                mcp_servers: Vec::new(),
                mcp_server_ids: Vec::new(),
                expected_mcp_failure_ids: Vec::new(),
                follow_up_prompts: Vec::new(),
                compact_before_follow_up: false,
                permission_host: EvalPermissionHost::Cli,
                expected_tool_result_ids: Vec::new(),
                expected_cancelled_tool_ids: Vec::new(),
                subagent_stale_idle_sec: None,
            },
        )
        .await
        .unwrap();
        assert!(result.score.passed, "{:?}", result.score.failures);
        assert_eq!(result.model_calls, 2);
        assert_eq!(result.stop_reason, "end_turn");
        assert!(result
            .trace
            .iter()
            .any(|event| event.kind == "tool.finished"));
        assert!(result.trace_score.as_ref().unwrap().passed);
        assert_eq!(result.git_diff.changed_paths, vec!["answer.txt"]);
        assert!(result.git_diff.patch.contains("+42"));
    }

    #[test]
    fn suite_report_is_portable_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reports/suite.json");
        let report = ScriptedSuiteResult {
            passed: true,
            cases: vec![(
                "smoke".into(),
                ScriptedRunResult {
                    outcome: ScenarioOutcome::Completed,
                    score: EvalScore {
                        passed: true,
                        failures: Vec::new(),
                    },
                    trace: Vec::new(),
                    model_calls: 1,
                    stop_reason: "end_turn".into(),
                    trace_score: None,
                    git_diff: GitDiff::default(),
                    tool_transcript: Vec::new(),
                    mcp_transcript: Vec::new(),
                },
            )],
        };
        write_suite_report(&path, &report).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(value["passed"], true);
        assert_eq!(value["cases"][0][0], "smoke");
    }

    #[tokio::test]
    async fn replay_preserves_tool_sequence_trace_and_diff() {
        let dir = tempfile::tempdir().unwrap();
        let scenario = smoke_suite().remove(0).scenario;
        let replay = replay_scripted_scenario(dir.path(), scenario)
            .await
            .unwrap();
        assert!(replay.score.passed, "{:?}", replay.score.failures);
        assert_eq!(replay.baseline.tool_transcript.len(), 1);
        assert_eq!(
            replay.baseline.tool_transcript,
            replay.replay.tool_transcript
        );
    }

    #[tokio::test]
    async fn expected_timeout_is_a_passing_failure_case() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_scripted_scenario(
            dir.path(),
            ScriptedScenario {
                prompt: "simulate a stuck provider".into(),
                fixture: EvalFixture::default(),
                expected: FileExpectation::default(),
                turns: vec![ScriptedTurn::hang_until_cancel()],
                expected_trace: None,
                permission_mode: None,
                cancel_after_ms: None,
                cancel_after_trace: None,
                timeout_ms: Some(10),
                expected_failure: Some("timeout".into()),
                mcp_servers: Vec::new(),
                mcp_server_ids: Vec::new(),
                expected_mcp_failure_ids: Vec::new(),
                follow_up_prompts: Vec::new(),
                compact_before_follow_up: false,
                permission_host: EvalPermissionHost::Cli,
                expected_tool_result_ids: Vec::new(),
                expected_cancelled_tool_ids: Vec::new(),
                subagent_stale_idle_sec: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, ScenarioOutcome::ExpectedFailure);
        assert!(result.score.passed);
    }

    #[tokio::test]
    async fn smoke_suite_enforces_replay_before_reporting_pass() {
        let dir = tempfile::tempdir().unwrap();
        let suite = run_scripted_suite(dir.path(), smoke_suite()).await.unwrap();
        assert!(suite.passed);
        assert_eq!(suite.cases.len(), 12);
    }
}
