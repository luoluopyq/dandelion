//! H5 contract: the streaming runner emits the versioned JSONL event
//! sequence, the file round-trips through the parser, and `compare` turns
//! two reports into a gate-correct transition table. All offline — the same
//! H0 scripted machinery, no second loop and no network.

use dandelion_core::eval::report::{Comparison, EvalEvent, ToolCallRecord, REPORT_VERSION};
use dandelion_core::eval::{
    write_jsonl_report, EvalFixture, FileExpectation, ScriptedScenario, ScriptedSuiteCase,
};
use dandelion_core::provider::scripted::ScriptedTurn;
use dandelion_core::provider::types::ToolCall;
use std::collections::BTreeMap;

fn write_case(file: &str, write_it: bool) -> ScriptedSuiteCase {
    let turns = if write_it {
        vec![
            ScriptedTurn::tools(
                "",
                vec![ToolCall {
                    id: "c1".into(),
                    name: "write_file".into(),
                    arguments: format!(r#"{{"path":"{file}","content":"ok\n"}}"#),
                }],
            ),
            ScriptedTurn::text("done"),
        ]
    } else {
        vec![ScriptedTurn::text("I did nothing")]
    };
    ScriptedSuiteCase {
        name: format!("case-{file}"),
        scenario: ScriptedScenario {
            prompt: format!("create {file}"),
            fixture: EvalFixture::default(),
            expected: FileExpectation {
                exact: BTreeMap::from([(file.to_string(), "ok\n".into())]),
                absent: Vec::new(),
            },
            turns,
            ..ScriptedScenario::default()
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jsonl_report_streams_roundtrips_and_compares() {
    let dir = tempfile::tempdir().unwrap();
    let mut events: Vec<EvalEvent> = Vec::new();
    let suite = dandelion_core::eval::run_scripted_suite_stream(
        dir.path(),
        "h5-mini",
        vec![write_case("good.txt", true), write_case("bad.txt", false)],
        &mut |event| events.push(event.clone()),
    )
    .await
    .expect("suite runs");
    assert!(!suite.passed, "one scripted case must fail scoring");

    // Event shape: started, one per case, finished — all on one version.
    assert!(matches!(
        &events[0],
        EvalEvent::SuiteStarted { suite, case_count: 2, v, .. }
            if suite == "h5-mini" && *v == REPORT_VERSION
    ));
    let finished = events
        .iter()
        .filter(|e| matches!(e, EvalEvent::CaseFinished { .. }))
        .count();
    assert_eq!(finished, 2);
    match events.last().expect("suite.finished") {
        EvalEvent::SuiteFinished {
            passed,
            passed_cases,
            failed_cases,
            ..
        } => assert!(!passed && *passed_cases == 1 && *failed_cases == 1),
        other => panic!("unexpected last event {other:?}"),
    }
    let case_lines: Vec<(String, bool)> = events
        .iter()
        .filter_map(|e| match e {
            EvalEvent::CaseFinished { case, passed, .. } => Some((case.clone(), *passed)),
            _ => None,
        })
        .collect();
    assert_eq!(
        case_lines,
        vec![
            ("case-good.txt".to_string(), true),
            ("case-bad.txt".to_string(), false)
        ]
    );

    // File round-trip through the strict version-checked parser.
    let report = dir.path().join("candidate.jsonl");
    write_jsonl_report(&report, &events).unwrap();
    let raw = std::fs::read_to_string(&report).unwrap();
    let parsed = EvalEvent::parse_report(&raw).expect("valid v1 report");
    assert_eq!(parsed, events);

    // Compare: candidate regressed one case and dropped another versus a
    // hypothetical all-pass baseline — the CI gate must refuse it.
    let baseline_events = vec![
        EvalEvent::CaseFinished {
            v: REPORT_VERSION,
            case: "case-good.txt".into(),
            passed: true,
            failures: vec![],
            outcome: "completed".into(),
            model_calls: 2,
            stop_reason: "end_turn".into(),
            changed_paths: vec!["good.txt".into()],
            duration_ms: 1,
            // Hypothetical baseline where this case ran with no tool calls:
            // the real run's write_file must surface as pairing drift.
            tool_calls: vec![],
        },
        EvalEvent::CaseFinished {
            v: REPORT_VERSION,
            case: "case-bad.txt".into(),
            passed: true,
            failures: vec![],
            outcome: "completed".into(),
            model_calls: 1,
            stop_reason: "end_turn".into(),
            changed_paths: vec!["bad.txt".into()],
            duration_ms: 1,
            tool_calls: vec![],
        },
        EvalEvent::CaseFinished {
            v: REPORT_VERSION,
            case: "case-gone.txt".into(),
            passed: true,
            failures: vec![],
            outcome: "completed".into(),
            model_calls: 1,
            stop_reason: "end_turn".into(),
            changed_paths: vec![],
            duration_ms: 1,
            tool_calls: vec![],
        },
    ];
    let comparison = Comparison::between(&baseline_events, &events).unwrap();
    assert!(!comparison.is_acceptable());
    let rendered = comparison.render();
    assert!(
        rendered.contains("case-bad.txt: PASS -> FAIL"),
        "{rendered}"
    );
    assert!(rendered.contains("case-gone.txt"), "{rendered}");
    // The passing-but-drifted good case must be visible as pairing drift:
    // the baseline recorded no tool calls, the real run recorded one.
    assert!(
        rendered.contains("case-good.txt: tool pairing drifted"),
        "{rendered}"
    );

    // Self-comparison of the same run is always acceptable (and deterministic).
    let same = Comparison::between(&events, &events).unwrap();
    assert!(same.is_acceptable());
}

/// A case that PASSES on both sides but whose write side-effect bytes changed
/// must still fail the compare gate — file expectations can be green while
/// intermediate behavior drifted.
#[test]
fn side_effect_drift_fails_the_gate_even_when_both_pass() {
    let call = |sha: &str, name: &str| ToolCallRecord {
        call_id: "c1".into(),
        name: name.into(),
        result_sha256: sha.into(),
        result_present: true,
        cancelled: false,
    };
    let case = |sha: &str| EvalEvent::CaseFinished {
        v: REPORT_VERSION,
        case: "x".into(),
        passed: true,
        failures: vec![],
        outcome: "completed".into(),
        model_calls: 1,
        stop_reason: "end_turn".into(),
        changed_paths: vec![],
        duration_ms: 1,
        tool_calls: vec![call(sha, "write_file")],
    };
    let drifted = Comparison::between(&[case("aaa")], &[case("bbb")]).unwrap();
    assert!(!drifted.is_acceptable());
    assert!(drifted
        .render()
        .contains("side effect drifted on 'write_file'"));

    // Read-only bytes are scheduling noise, not drift.
    let read_case = |sha: &str| EvalEvent::CaseFinished {
        v: REPORT_VERSION,
        case: "x".into(),
        passed: true,
        failures: vec![],
        outcome: "completed".into(),
        model_calls: 1,
        stop_reason: "end_turn".into(),
        changed_paths: vec![],
        duration_ms: 1,
        tool_calls: vec![call(sha, "read_file")],
    };
    let noise = Comparison::between(&[read_case("aaa")], &[read_case("bbb")]).unwrap();
    assert!(noise.is_acceptable(), "{:?}", noise.render());
}
