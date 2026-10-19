//! H5b: live-model evaluation against a real configured endpoint.
//!
//! Where the scripted suite pins the model's words to test the *harness*,
//! this suite sends real prompts through the same production Runtime →
//! agent_loop → ToolRegistry → Store chain and scores only the **files**
//! that come back — the observable contract — plus control-plane
//! invariants (every tool call paired with a result, the run reached
//! `run.finished`). A model may satisfy a prompt in different numbers of
//! turns, so turn counts and tool-choice details are recorded but never
//! gated; wall-clock duration is recorded and never gates.
//!
//! Comparability is enforced, not advised: each run embeds a `pairing`
//! string (model + backend + permission posture) in `suite.started`, and
//! `Comparison::between` refuses to diff reports from different pairings,
//! or a live report against a scripted one.
//!
//! Nothing here runs unless the caller explicitly constructs a live
//! provider; the CLI additionally requires `DANDELION_LIVE_TEST=1`.

use super::report::{self, EvalEvent, ToolCallRecord};
use super::{
    collect_git_diff, initialize_git_baseline, score_trace, tool_transcript, EvalFixture,
    EvalScore, ToolTranscript, TraceExpectation,
};
use crate::host::CliHost;
use crate::provider::Provider;
use crate::store::Store;
use crate::{
    AgentHost, PermissionMode, PromptRequest, Runtime, Settings, ThinkingIntensity, TraceEvent,
};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

/// Env gate, mirroring `tests/live_g1.rs`: no accidental network calls.
pub fn live_enabled() -> bool {
    live_enabled_from(std::env::var("DANDELION_LIVE_TEST").ok().as_deref())
}

/// Pure form of [`live_enabled`], so the gate itself is testable without
/// mutating process environment from a test thread.
pub fn live_enabled_from(raw: Option<&str>) -> bool {
    matches!(raw, Some("1") | Some("true"))
}

pub const DEFAULT_BASE_URL: &str = "http://192.168.183.25:9911/v1";
pub const DEFAULT_MODEL: &str = "Qwen3.8-Flash-Next";

/// CLI/env/flag resolution for the endpoint under test. Flag wins, then env,
/// then the same defaults `tests/live_g1.rs` uses.
pub fn resolve_endpoint(base_url: Option<String>, model: Option<String>) -> (String, String) {
    let base = base_url
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("DANDELION_BASE_URL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_BASE_URL.into());
    let model = model
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("DANDELION_MODEL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_MODEL.into());
    (Settings::normalize_base_url(&base), model)
}

/// What two live reports may be compared: identical model, backend, and
/// permission posture. Content-free by construction (no URL, no key).
pub fn live_pairing(model: &str) -> String {
    format!("model={model} backend=openai_compat permission=Auto")
}

/// Settings for one live scenario run in its own temporary workspace.
pub fn live_settings(workspace: PathBuf, base_url: &str, model: &str) -> Settings {
    let mut settings = Settings::defaults();
    settings.base_url = Some(base_url.to_string());
    settings.default_model = Some(model.to_string());
    settings.workspace = Some(workspace);
    settings.permission_mode = PermissionMode::Auto;
    settings.thinking_intensity = ThinkingIntensity::Low;
    // Cap runaway agent loops, not model quality: a scenario that needs more
    // iterations than this to touch its files has failed regardless of what
    // the final answer says.
    settings.max_iterations = 12;
    settings.http_connect_timeout_ms = 10_000;
    settings.sse_idle_timeout_ms = 120_000;
    // Temp fixture repos are throwaway; worktree isolation only adds a
    // second git layer with nothing to protect here (mirrors the scripted
    // suite's offline rationale).
    settings.delegation_worktree_isolation = false;
    settings
}

/// File-level success criteria for a live scenario. Deliberately weaker
/// than the scripted suite's byte-exact contract: a model may phrase file
/// content differently, so live cases assert presence, absence, and
/// required/forbidden substrings — the semantic contract — never bytes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LiveExpectation {
    pub exists: Vec<String>,
    pub absent: Vec<String>,
    /// path -> substring that must appear in the file.
    pub contains: BTreeMap<String, String>,
    /// path -> substring that must NOT appear in the file.
    pub not_contains: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct LiveScenario {
    pub name: String,
    pub prompt: String,
    pub fixture: EvalFixture,
    pub expected: LiveExpectation,
    /// Hard wall-clock cap; a timeout cancels the production session.
    pub timeout_ms: u64,
}

/// One live scenario: create a file from scratch.
fn case_create() -> LiveScenario {
    LiveScenario {
        name: "live-create-file".into(),
        prompt: "在工作区中创建文件 answer.txt，内容为文本 42（不要包含其它文字）。".into(),
        fixture: EvalFixture::default(),
        expected: LiveExpectation {
            exists: vec!["answer.txt".into()],
            absent: Vec::new(),
            contains: BTreeMap::from([("answer.txt".into(), "42".into())]),
            not_contains: BTreeMap::new(),
        },
        timeout_ms: 120_000,
    }
}

/// One live scenario: edit an existing file in place.
fn case_edit() -> LiveScenario {
    LiveScenario {
        name: "live-edit-file".into(),
        prompt: "把 greeting.txt 中的 world 改成 dandelion，只修改这个文件。".into(),
        fixture: EvalFixture {
            files: BTreeMap::from([("greeting.txt".into(), "hello, world\n".into())]),
        },
        expected: LiveExpectation {
            exists: vec!["greeting.txt".into()],
            absent: Vec::new(),
            contains: BTreeMap::from([("greeting.txt".into(), "dandelion".into())]),
            not_contains: BTreeMap::from([("greeting.txt".into(), "world".into())]),
        },
        timeout_ms: 120_000,
    }
}

/// One live scenario: read one file, then write the found fact into another
/// — exercises tool chaining through the real loop, not just one call.
fn case_chain() -> LiveScenario {
    LiveScenario {
        name: "live-read-then-summarize".into(),
        prompt: "读取 notes.txt，然后把其中的项目代号原样写入 summary.txt。".into(),
        fixture: EvalFixture {
            files: BTreeMap::from([(
                "notes.txt".into(),
                "本次会议确定了项目代号：SUNFLOWER-77。\n".into(),
            )]),
        },
        expected: LiveExpectation {
            exists: vec!["summary.txt".into()],
            absent: Vec::new(),
            contains: BTreeMap::from([("summary.txt".into(), "SUNFLOWER-77".into())]),
            not_contains: BTreeMap::new(),
        },
        timeout_ms: 180_000,
    }
}

pub fn live_suite() -> Vec<LiveScenario> {
    vec![case_create(), case_edit(), case_chain()]
}

/// Score a workspace against a live expectation. Failure strings name the
/// file and the check — never the file content — so they are report-safe.
pub fn score_live_files(root: &std::path::Path, expected: &LiveExpectation) -> Result<EvalScore> {
    let mut failures = Vec::new();
    for relative in &expected.exists {
        if !super::fixture_path(root, relative)?.exists() {
            failures.push(format!("missing file: {relative}"));
        }
    }
    for relative in &expected.absent {
        if super::fixture_path(root, relative)?.exists() {
            failures.push(format!("unexpected file: {relative}"));
        }
    }
    let read = |relative: &String| -> Result<Option<String>> {
        let path = super::fixture_path(root, relative)?;
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    };
    for (relative, needle) in &expected.contains {
        match read(relative)? {
            None => failures.push(format!("missing file for contains: {relative}")),
            Some(text) if !text.contains(needle.as_str()) => {
                failures.push(format!("missing substring in {relative}"))
            }
            Some(_) => {}
        }
    }
    for (relative, needle) in &expected.not_contains {
        if let Some(text) = read(relative)? {
            if text.contains(needle.as_str()) {
                failures.push(format!("forbidden substring still in {relative}"))
            }
        }
    }
    Ok(EvalScore {
        passed: failures.is_empty(),
        failures,
    })
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct LiveRunResult {
    pub score: EvalScore,
    pub trace: Vec<TraceEvent>,
    pub stop_reason: String,
    pub changed_paths: Vec<String>,
    pub tool_transcript: Vec<ToolTranscript>,
    pub duration_ms: u64,
}

/// Run one live scenario through the production Runtime with the given
/// provider. Callers create and own the workspace root (and must set
/// `settings.workspace` to it). A timeout first cancels the real session,
/// then waits briefly for the loop to settle before reporting the failure.
pub async fn run_live_scenario(
    root: &std::path::Path,
    scenario: LiveScenario,
    provider: Arc<dyn Provider>,
    settings: Settings,
) -> Result<LiveRunResult> {
    let started = std::time::Instant::now();
    scenario.fixture.materialize(root)?;
    initialize_git_baseline(root)?;
    let data_dir = root.parent().unwrap_or(root).join(format!(
        "dandelion-live-data-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let host: Arc<dyn AgentHost> = Arc::new(CliHost::with_data_dir(data_dir.clone()));
    let runtime = Runtime::new_with_provider(host, settings, provider)?;
    let session = runtime.new_session(None);
    let prompt = runtime.prompt(PromptRequest::new(session.clone(), scenario.prompt));
    tokio::pin!(prompt);
    let outcome = tokio::select! {
        result = &mut prompt => result,
        _ = tokio::time::sleep(std::time::Duration::from_millis(scenario.timeout_ms)) => {
            // Cancel the production session, give the loop (and children) a
            // moment to settle, then report the timeout as the failure.
            runtime.cancel(session.clone()).await?;
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), &mut prompt).await;
            return Err(Error::Invalid(format!(
                "live scenario timeout after {} ms",
                scenario.timeout_ms
            )));
        }
    };
    outcome?;
    let session_id = session.to_string();
    let trace = runtime.trace_events(&session_id, 5_000)?;
    let stop_reason = runtime
        .agent_runs(&session_id, 1)?
        .first()
        .and_then(|run| run.stop_reason.clone())
        .unwrap_or_else(|| "unknown".into());
    let store = Store::open(&data_dir, 50_000)?;
    let transcript = tool_transcript(&store, &session)?;
    let mut score = score_live_files(root, &scenario.expected)?;
    // Control-plane invariant: the loop reached its end, and every tool call
    // the model emitted received a result. Which calls it chose and in how
    // many turns is the model's business; unpaired calls are a harness bug.
    let trace_score = score_trace(
        &trace,
        &TraceExpectation {
            kinds: vec!["run.started".into(), "run.finished".into()],
        },
    );
    score.failures.extend(trace_score.failures);
    for call in &transcript {
        if !call.result_present {
            score.failures.push(format!(
                "tool call never paired with a result: {}",
                call.name
            ));
        }
        if call.cancelled {
            score.failures.push(format!(
                "tool call cancelled before completing: {}",
                call.name
            ));
        }
    }
    score.passed = score.failures.is_empty();
    let changed_paths = collect_git_diff(root)?.changed_paths;
    Ok(LiveRunResult {
        score,
        trace,
        stop_reason,
        changed_paths,
        tool_transcript: transcript,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

#[derive(Clone, Debug)]
pub struct LiveSuiteResult {
    pub passed: bool,
    pub passed_cases: usize,
    pub failed_cases: usize,
}

/// Run the live suite, streaming v1 JSONL events exactly like the scripted
/// runner (progress visible from CI; partial results survive a crash).
/// `provider_for` builds the provider per case — production shares one
/// live client across cases (the closure clones the same `Arc`), while
/// offline tests inject a scripted provider per case to exercise every
/// reporting path without network.
pub async fn run_live_suite_stream(
    root: &std::path::Path,
    pairing: &str,
    scenarios: Vec<LiveScenario>,
    provider_for: &dyn Fn() -> Result<Arc<dyn Provider>>,
    settings_for: &dyn Fn(PathBuf) -> Settings,
    emit: &mut dyn FnMut(&EvalEvent),
) -> Result<LiveSuiteResult> {
    let names: Vec<String> = scenarios.iter().map(|s| s.name.clone()).collect();
    emit(&EvalEvent::SuiteStarted {
        v: report::REPORT_VERSION,
        suite: "live".into(),
        case_count: names.len(),
        cases: names,
        pairing: Some(pairing.to_string()),
    });
    let mut passed = true;
    let mut passed_cases = 0usize;
    let mut failed_cases = 0usize;
    for (index, scenario) in scenarios.into_iter().enumerate() {
        let case_root = root.join(format!("{:03}-{}", index, scenario.name));
        fs::create_dir_all(&case_root)?;
        let started = std::time::Instant::now();
        // Provider construction failure and run failure (timeout, transport
        // error, store error) share one reporting path: a sanitized error
        // event, not a suite abort.
        let run = match provider_for() {
            Ok(provider) => {
                run_live_scenario(
                    &case_root,
                    scenario.clone(),
                    provider,
                    settings_for(case_root.clone()),
                )
                .await
            }
            Err(err) => Err(err),
        };
        let run = match run {
            Ok(run) => run,
            Err(err) => {
                let detail = crate::mcp::stdio::sanitize_error_text(&err.to_string());
                emit(&EvalEvent::CaseFinished {
                    v: report::REPORT_VERSION,
                    case: scenario.name.clone(),
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
                failed_cases += 1;
                continue;
            }
        };
        let case_passed = run.score.passed;
        passed &= case_passed;
        passed_cases += usize::from(case_passed);
        failed_cases += usize::from(!case_passed);
        let model_completed = run
            .trace
            .iter()
            .filter(|event| event.kind == "model.completed")
            .count();
        emit(&EvalEvent::CaseFinished {
            v: report::REPORT_VERSION,
            case: scenario.name.clone(),
            passed: case_passed,
            failures: run.score.failures.clone(),
            outcome: "completed".into(),
            model_calls: model_completed,
            // Duration is recorded here for observability only. Compare and
            // the CI gate never read it: latency must not flip a pass.
            duration_ms: run.duration_ms,
            stop_reason: run.stop_reason.clone(),
            changed_paths: run.changed_paths.clone(),
            tool_calls: run
                .tool_transcript
                .iter()
                .map(|t| ToolCallRecord {
                    call_id: t.call_id.clone(),
                    name: t.name.clone(),
                    result_sha256: if report::is_read_only(&t.name) {
                        // Live byte hashes of read-only results are model
                        // scheduling noise; keep the record but zero the hash.
                        String::new()
                    } else {
                        t.result_sha256.clone()
                    },
                    result_present: t.result_present,
                    cancelled: t.cancelled,
                })
                .collect(),
        });
    }
    emit(&EvalEvent::SuiteFinished {
        v: report::REPORT_VERSION,
        suite: "live".into(),
        passed,
        passed_cases,
        failed_cases,
    });
    Ok(LiveSuiteResult {
        passed,
        passed_cases,
        failed_cases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::ToolCall;
    use crate::provider::{ScriptedProvider, ScriptedTurn};

    fn scripted(turns: Vec<ScriptedTurn>) -> ScriptedProvider {
        ScriptedProvider::new(turns)
    }

    fn settings_for(root: PathBuf) -> Settings {
        live_settings(root, "http://127.0.0.1:9/v1", "scripted-under-test")
    }

    #[test]
    fn live_gate_is_explicit_and_pairing_is_content_free() {
        assert!(live_enabled_from(Some("1")));
        assert!(live_enabled_from(Some("true")));
        assert!(!live_enabled_from(None));
        assert!(!live_enabled_from(Some("yes")));
        let pairing = live_pairing("M1");
        assert!(pairing.contains("model=M1"));
        assert!(!pairing.contains("http"));
        assert!(!pairing.contains("key"));
    }

    #[tokio::test]
    async fn live_scoring_passes_on_semantic_contract() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("answer.txt"), "答案是 42\n").unwrap();
        let score = score_live_files(
            dir.path(),
            &LiveExpectation {
                exists: vec!["answer.txt".into()],
                contains: BTreeMap::from([("answer.txt".into(), "42".into())]),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(score.passed, "{:?}", score.failures);
        // Same file, forbidden substring still present -> failure named by
        // file and check, never by content.
        let score = score_live_files(
            dir.path(),
            &LiveExpectation {
                not_contains: BTreeMap::from([("answer.txt".into(), "42".into())]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            score.failures,
            vec!["forbidden substring still in answer.txt".to_string()]
        );
        assert!(!score.failures[0].contains('4'));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_suite_reports_pass_and_fail_through_jsonl_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let mut events: Vec<EvalEvent> = Vec::new();
        let create = case_create();
        let skip = LiveScenario {
            name: "live-would-skip".into(),
            prompt: "nothing to do".into(),
            fixture: EvalFixture::default(),
            expected: LiveExpectation {
                exists: vec!["never-written.txt".into()],
                ..Default::default()
            },
            timeout_ms: 5_000,
        };
        // The factory hands the first case a provider that writes the file
        // and the second one an inert text-only provider: scoring must fail
        // the second case while the suite keeps running and reporting.
        let turns = vec![
            ScriptedTurn::tools(
                "",
                vec![ToolCall {
                    id: "call_live_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path": "answer.txt", "content": "42\n"})
                        .to_string(),
                }],
            ),
            ScriptedTurn::text("done"),
        ];
        let seen = std::sync::atomic::AtomicUsize::new(0);
        let provider_factory = || -> Result<Arc<dyn Provider>> {
            let first = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            let provider = if first {
                scripted(turns.clone())
            } else {
                scripted(vec![ScriptedTurn::text("no action taken")])
            };
            Ok(Arc::new(provider) as Arc<dyn Provider>)
        };
        let suite = run_live_suite_stream(
            dir.path(),
            &live_pairing("scripted-under-test"),
            vec![create, skip],
            &provider_factory,
            &settings_for,
            &mut |event| events.push(event.clone()),
        )
        .await
        .expect("live suite runner completes");
        assert!(!suite.passed);
        assert_eq!((suite.passed_cases, suite.failed_cases), (1, 1));
        let started_pairing = events
            .iter()
            .find_map(|e| match e {
                EvalEvent::SuiteStarted { pairing, .. } => Some(pairing.clone()),
                _ => None,
            })
            .expect("suite.started");
        assert_eq!(
            started_pairing.as_deref(),
            Some(live_pairing("scripted-under-test").as_str())
        );
        let outcomes: Vec<(String, bool, String)> = events
            .iter()
            .filter_map(|e| match e {
                EvalEvent::CaseFinished {
                    case,
                    passed,
                    failures,
                    ..
                } => Some((case.clone(), *passed, failures.join(";"))),
                _ => None,
            })
            .collect();
        assert_eq!(
            outcomes,
            vec![
                ("live-create-file".to_string(), true, String::new()),
                (
                    "live-would-skip".to_string(),
                    false,
                    "missing file: never-written.txt".to_string()
                ),
            ]
        );
        // The report is a valid v1 document, and its pairing string makes it
        // non-comparable with any scripted report or other-model run.
        let raw: String = events.iter().map(EvalEvent::to_json_line).collect();
        EvalEvent::parse_report(&raw).expect("valid live report");
        let scripted_only = vec![EvalEvent::CaseFinished {
            v: report::REPORT_VERSION,
            case: "live-create-file".into(),
            passed: true,
            failures: vec![],
            outcome: "completed".into(),
            model_calls: 2,
            stop_reason: "end_turn".into(),
            changed_paths: vec![],
            duration_ms: 1,
            tool_calls: vec![],
        }];
        assert!(report::Comparison::between(&events, &scripted_only).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_timeout_is_an_error_case_not_an_abort() {
        let dir = tempfile::tempdir().unwrap();
        let mut events: Vec<EvalEvent> = Vec::new();
        let stuck = |name: &str| LiveScenario {
            name: name.into(),
            prompt: "wait forever".into(),
            fixture: EvalFixture::default(),
            expected: LiveExpectation::default(),
            timeout_ms: 100,
        };
        let hang = || -> Result<Arc<dyn Provider>> {
            Ok(Arc::new(scripted(vec![ScriptedTurn::hang_until_cancel()])) as Arc<dyn Provider>)
        };
        let suite = run_live_suite_stream(
            dir.path(),
            &live_pairing("scripted-under-test"),
            vec![stuck("live-stuck-a"), stuck("live-stuck-b")],
            &hang,
            &settings_for,
            &mut |event| events.push(event.clone()),
        )
        .await
        .expect("suite survives a stuck case");
        assert!(!suite.passed);
        assert_eq!(suite.failed_cases, 2);
        let errored = events
            .iter()
            .filter(|e| matches!(e, EvalEvent::CaseFinished { outcome, .. } if outcome == "error"))
            .count();
        assert_eq!(errored, 2, "both timeouts must be reported as errors");
        // A timed-out case must say so through the sanitizer, and the suite
        // still reaches suite.finished (abort would have returned Err).
        for event in &events {
            if let EvalEvent::CaseFinished { failures, .. } = event {
                assert!(
                    failures.iter().any(|f| f.contains("timeout")),
                    "{failures:?}"
                );
            }
        }
        assert!(matches!(
            events.last(),
            Some(EvalEvent::SuiteFinished { passed: false, .. })
        ));
    }

    #[test]
    fn suite_shape_is_reviewable() {
        let suite = live_suite();
        let names: Vec<&str> = suite.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "live-create-file",
                "live-edit-file",
                "live-read-then-summarize"
            ]
        );
        assert!(suite.iter().all(|s| s.timeout_ms > 0));
    }
}
