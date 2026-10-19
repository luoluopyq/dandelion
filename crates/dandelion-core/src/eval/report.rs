//! H5: versioned JSONL evaluation reports and readable baseline/candidate
//! comparison.
//!
//! One JSON object per line, each carrying `"v": REPORT_VERSION`. Automation
//! consumes the file incrementally (`suite.started`, then one
//! `case.finished` per case, then `suite.finished`). Records are
//! content-safe by construction: they repeat only control-plane facts — case
//! names, pass/fail, failure *categories* already reduced by the scorer,
//! model call counts, stop reasons, changed path names — never prompts, tool
//! arguments, or tool output. `dln eval compare` turns two such files into a
//! human-readable transition table for CI.

use serde::{Deserialize, Serialize};

/// Bumped only on breaking changes to record shapes. Consumers should refuse
/// unknown majors, not guess.
pub const REPORT_VERSION: u32 = 1;

/// One tool call's side-effect record, as it appears in reports. Content-safe
/// by construction: result bytes are reduced to a hash plus a presence flag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallRecord {
    pub call_id: String,
    pub name: String,
    pub result_sha256: String,
    pub result_present: bool,
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvalEvent {
    SuiteStarted {
        v: u32,
        suite: String,
        case_count: usize,
        cases: Vec<String>,
        /// H5b: identity of the configuration under test for live suites,
        /// e.g. `model=… backend=… permission=Auto`. `None` for offline
        /// scripted suites, whose determinism needs no pairing claim.
        /// Additive (old readers ignore it; absent parses to `None`), so the
        /// report version stays 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pairing: Option<String>,
    },
    CaseFinished {
        v: u32,
        case: String,
        passed: bool,
        /// Scorer failure strings (categories + expected/actual file names).
        failures: Vec<String>,
        /// "completed" | "expected_failure".
        outcome: String,
        model_calls: usize,
        stop_reason: String,
        /// Workspace paths the run changed — names only, never contents.
        changed_paths: Vec<String>,
        duration_ms: u64,
        /// Baseline/replay pairing evidence, so `compare` can render which
        /// tool's pairing or side effect drifted instead of only “differs”.
        tool_calls: Vec<ToolCallRecord>,
    },
    SuiteFinished {
        v: u32,
        suite: String,
        passed: bool,
        passed_cases: usize,
        failed_cases: usize,
    },
}

impl EvalEvent {
    pub fn to_json_line(&self) -> String {
        let mut line = serde_json::to_string(self).unwrap_or_else(|_| "{}".into());
        line.push('\n');
        line
    }

    pub fn from_json_line(line: &str) -> std::result::Result<Self, String> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Err("blank line".into());
        }
        let value: serde_json::Value =
            serde_json::from_str(trimmed).map_err(|err| format!("invalid JSON: {err}"))?;
        let version = value.get("v").and_then(|v| v.as_u64()).unwrap_or(0);
        if version != REPORT_VERSION as u64 {
            return Err(format!("unsupported report version {version}"));
        }
        serde_json::from_value(value).map_err(|err| format!("invalid record: {err}"))
    }

    /// Parse an entire JSONL report file, refusing unknown versions.
    pub fn parse_report(raw: &str) -> std::result::Result<Vec<EvalEvent>, String> {
        raw.lines()
            .enumerate()
            .map(|(index, line)| {
                EvalEvent::from_json_line(line).map_err(|err| format!("line {}: {err}", index + 1))
            })
            .collect()
    }
}

/// What `dln eval compare` produces: per-case transitions plus a summary
/// string ready to print in a CI log.
#[derive(Clone, Debug)]
pub struct Comparison {
    pub regressions: Vec<String>,
    pub fixes: Vec<String>,
    pub still_failing: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Readable side-effect pairing drift between two otherwise-passing
    /// cases (a tool added/dropped, or a write's bytes changed).
    pub drift: Vec<String>,
    pub unchanged_passed: usize,
    pub unchanged_failed: usize,
}

impl Comparison {
    /// Diff two parsed reports. Cases missing on one side are reported; a
    /// report without `suite.finished` is truncated-but-comparable (CI job
    /// killed mid-run still yields a diff of whatever completed).
    pub fn between(
        baseline: &[EvalEvent],
        candidate: &[EvalEvent],
    ) -> std::result::Result<Comparison, String> {
        fn cases(events: &[EvalEvent]) -> Vec<(&str, bool, &[ToolCallRecord])> {
            events
                .iter()
                .filter_map(|event| match event {
                    EvalEvent::CaseFinished {
                        case,
                        passed,
                        tool_calls,
                        ..
                    } => Some((case.as_str(), *passed, tool_calls.as_slice())),
                    _ => None,
                })
                .collect()
        }
        // H5b pairing gate. A live report declares what it ran against
        // (`pairing`); comparing across different pairings (model, backend,
        // permission posture) measures the pairing, not the harness, so it
        // is refused outright. A live report and a scripted report are also
        // incomparable by construction. Between two *live* runs of the same
        // pairing, per-call tool drift (call ids, read/write byte hashes) is
        // model creativity, not harness behavior — only pass/fail
        // transitions are meaningful there, so the drift scan is skipped.
        fn pairing_of(events: &[EvalEvent]) -> Option<&str> {
            events.iter().find_map(|event| match event {
                EvalEvent::SuiteStarted {
                    pairing: Some(pairing),
                    ..
                } => Some(pairing.as_str()),
                _ => None,
            })
        }
        let (base_pairing, cand_pairing) = (pairing_of(baseline), pairing_of(candidate));
        match (base_pairing, cand_pairing) {
            (Some(b), Some(c)) if b != c => {
                return Err(format!(
                    "live reports must share the same pairing to compare: '{b}' vs '{c}'"
                ))
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(
                    "cannot compare a live report with a scripted one (different measurement)"
                        .into(),
                )
            }
            _ => {}
        }
        let live = base_pairing.is_some();
        let base = cases(baseline);
        let cand = cases(candidate);
        let mut out = Comparison {
            regressions: Vec::new(),
            fixes: Vec::new(),
            still_failing: Vec::new(),
            added: Vec::new(),
            removed: Vec::new(),
            drift: Vec::new(),
            unchanged_passed: 0,
            unchanged_failed: 0,
        };
        for (name, was, calls) in &base {
            match cand.iter().find(|(n, ..)| n == name) {
                None => out.removed.push((*name).to_string()),
                Some((_, now, new_calls)) => {
                    if !live {
                        out.drift.extend(tool_drift(name, calls, new_calls));
                    }
                    match (was, now) {
                        (true, false) => out
                            .regressions
                            .push(format!("{name}: PASS -> FAIL (regression)")),
                        (false, true) => out.fixes.push(format!("{name}: FAIL -> PASS (fixed)")),
                        (false, false) => out.still_failing.push((*name).to_string()),
                        (true, true) => out.unchanged_passed += 1,
                    }
                }
            }
        }
        for (name, now, _) in &cand {
            if !base.iter().any(|(n, ..)| n == name) {
                out.added.push(format!(
                    "{name}: new case, {}",
                    if *now { "PASS" } else { "FAIL" }
                ));
            }
        }
        Ok(out)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for line in self
            .regressions
            .iter()
            .chain(self.fixes.iter())
            .chain(self.added.iter())
            .chain(self.drift.iter())
        {
            out.push_str(line);
            out.push('\n');
        }
        if !self.still_failing.is_empty() {
            out.push_str(&format!(
                "still failing: {}\n",
                self.still_failing.join(", ")
            ));
        }
        if !self.removed.is_empty() {
            out.push_str(&format!(
                "cases removed from suite: {}\n",
                self.removed.join(", ")
            ));
        }
        out.push_str(&format!("unchanged: {} passing\n", self.unchanged_passed));
        out
    }

    /// CI gate: regressions, removed cases, and pairing/side-effect drift
    /// fail a comparison; fixes and new cases never do.
    pub fn is_acceptable(&self) -> bool {
        self.regressions.is_empty() && self.removed.is_empty() && self.drift.is_empty()
    }
}

/// Readable per-tool pairing drift between the same case in two reports:
/// calls added/dropped/reordered, or a non-read-only result changing bytes.
/// Read-only result bytes are scheduling-dependent (a read may legitimately
/// observe a concurrent write in one run and not the other), so they are
/// excluded — the eval scorer's file/git expectations own that semantics.
pub fn is_read_only(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "search_files" | "web_search" | "web_extract" | "process"
    )
}

fn tool_drift(
    case: &str,
    baseline: &[ToolCallRecord],
    candidate: &[ToolCallRecord],
) -> Vec<String> {
    let mut out = Vec::new();
    if baseline.len() != candidate.len()
        || baseline
            .iter()
            .zip(candidate)
            .any(|(b, c)| b.name != c.name || b.call_id != c.call_id)
    {
        let fmt = |calls: &[ToolCallRecord]| {
            calls
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };
        out.push(format!(
            "{case}: tool pairing drifted: [{}] -> [{}]",
            fmt(baseline),
            fmt(candidate)
        ));
        return out;
    }
    for (b, c) in baseline.iter().zip(candidate) {
        if !is_read_only(&b.name)
            && (b.result_sha256 != c.result_sha256 || b.cancelled != c.cancelled)
        {
            out.push(format!(
                "{case}: side effect drifted on '{}' (bytes or cancel state changed)",
                b.name
            ));
        }
        if b.result_present != c.result_present {
            out.push(format!("{case}: result presence drifted on '{}'", b.name));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(case: &str, passed: bool) -> EvalEvent {
        EvalEvent::CaseFinished {
            v: REPORT_VERSION,
            case: case.into(),
            passed,
            failures: if passed {
                vec![]
            } else {
                vec!["missing file: a.txt".into()]
            },
            outcome: "completed".into(),
            model_calls: 2,
            stop_reason: "end_turn".into(),
            changed_paths: vec!["a.txt".into()],
            duration_ms: 5,
            tool_calls: vec![ToolCallRecord {
                call_id: "c1".into(),
                name: "write_file".into(),
                result_sha256: "abc".into(),
                result_present: true,
                cancelled: false,
            }],
        }
    }

    #[test]
    fn roundtrip_jsonl_and_version_guard() {
        let events = vec![
            EvalEvent::SuiteStarted {
                v: REPORT_VERSION,
                suite: "smoke".into(),
                case_count: 1,
                cases: vec!["write-file".into()],
                pairing: None,
            },
            line("write-file", true),
        ];
        let raw: String = events.iter().map(EvalEvent::to_json_line).collect();
        let parsed = EvalEvent::parse_report(&raw).unwrap();
        assert_eq!(parsed, events);
        assert!(EvalEvent::from_json_line(r#"{"v":99,"type":"x"}"#).is_err());
        assert!(EvalEvent::parse_report("not json\n").is_err());
    }

    #[test]
    fn pairing_gate_and_live_drift_exemption() {
        let started = |pairing: Option<&str>| EvalEvent::SuiteStarted {
            v: REPORT_VERSION,
            suite: "live".into(),
            case_count: 1,
            cases: vec!["x".into()],
            pairing: pairing.map(str::to_string),
        };
        // Same case, same PASS, but the model chose different tool bytes:
        // scripted would call this drift, live must not.
        let live_pairing = "model=m1 backend=openai_compat permission=Auto";
        let live_base = vec![started(Some(live_pairing)), line("x", true)];
        let mut moved = line("x", true);
        if let EvalEvent::CaseFinished { tool_calls, .. } = &mut moved {
            tool_calls[0].result_sha256 = "different".into();
        }
        let live_cand = vec![started(Some(live_pairing)), moved];
        let ok = Comparison::between(&live_base, &live_cand).unwrap();
        assert!(ok.is_acceptable(), "live byte drift is model noise");
        let scripted_same = Comparison::between(
            &[line("x", true)],
            &[{
                let mut m = line("x", true);
                if let EvalEvent::CaseFinished { tool_calls, .. } = &mut m {
                    tool_calls[0].result_sha256 = "different".into();
                }
                m
            }],
        )
        .unwrap();
        assert!(!scripted_same.is_acceptable(), "scripted gate unchanged");

        // A different model under test measures the model, not the harness.
        let other = vec![
            started(Some("model=m2 backend=openai_compat permission=Auto")),
            line("x", true),
        ];
        assert!(Comparison::between(&live_base, &other)
            .unwrap_err()
            .contains("same pairing"));
        // Live vs scripted is a category error, not a comparison.
        assert!(Comparison::between(&live_base, &[line("x", true)])
            .unwrap_err()
            .contains("live report"));
        // Pass/fail transitions still gate live reports.
        let regressed = vec![started(Some(live_pairing)), line("x", false)];
        let bad = Comparison::between(&live_base, &regressed).unwrap();
        assert!(!bad.is_acceptable());
    }

    #[test]
    fn compare_reads_transitions_and_gates_regressions() {
        let baseline = vec![line("a", true), line("b", false), line("gone", true)];
        let candidate = vec![line("a", false), line("b", true), line("new", false)];
        let c = Comparison::between(&baseline, &candidate).unwrap();
        assert_eq!(c.regressions.len(), 1);
        assert!(c.regressions[0].contains("a: PASS -> FAIL"));
        assert!(c.fixes[0].contains("b: FAIL -> PASS"));
        assert_eq!(c.removed, vec!["gone".to_string()]);
        assert!(c.added[0].contains("new"));
        assert!(
            !c.is_acceptable(),
            "regression and removal must fail the gate"
        );
        let clean = Comparison::between(&[line("a", true)], &[line("a", true)]).unwrap();
        assert!(clean.is_acceptable());
        assert_eq!(clean.unchanged_passed, 1);
    }
}
