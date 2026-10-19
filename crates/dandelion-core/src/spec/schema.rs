//! SPEC.md / PLAN.md parse, validate, and deterministic Diff.
//!
//! YAML subset only (front matter). No seventh tool. No LLM in Diff.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

pub const SPEC_FILE: &str = "SPEC.md";
pub const PLAN_FILE: &str = "PLAN.md";
pub const DIFF_FILE: &str = "DIFF.md";
pub const META_FILE: &str = "meta.json";
pub const INVALID_PREFIX: &str = "Error: spec invalid: ";
pub const UNCHANGED: &str = "Error: spec unchanged";
pub const PIN_MSG: &str = "Error: spec pipeline cannot write outside spec dir";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecMode {
    Ask,
    Plan,
    Auto,
}

impl SpecMode {
    pub fn parse(raw: &str) -> crate::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ask" => Ok(Self::Ask),
            "plan" => Ok(Self::Plan),
            "auto" => Ok(Self::Auto),
            other => Err(crate::Error::Invalid(format!("unknown SpecMode '{other}'"))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Plan => "plan",
            Self::Auto => "auto",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecStatus {
    Intake,
    Draft,
    Revising,
    Approved,
    Planned,
    Executing,
    Done,
    Abandoned,
}

impl SpecStatus {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "intake" => Ok(Self::Intake),
            "draft" => Ok(Self::Draft),
            "revising" => Ok(Self::Revising),
            "approved" => Ok(Self::Approved),
            "planned" => Ok(Self::Planned),
            "executing" => Ok(Self::Executing),
            "done" => Ok(Self::Done),
            "abandoned" => Ok(Self::Abandoned),
            other => Err(format!("unknown status '{other}'")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intake => "intake",
            Self::Draft => "draft",
            Self::Revising => "revising",
            Self::Approved => "approved",
            Self::Planned => "planned",
            Self::Executing => "executing",
            Self::Done => "done",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecDoc {
    pub id: String,
    pub version: u32,
    pub status: SpecStatus,
    pub goal: String,
    #[serde(default)]
    pub non_goals: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    pub acceptance: Vec<String>,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub done_when: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDoc {
    pub spec_id: String,
    pub spec_version: u32,
    pub steps: Vec<PlanStep>,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecDiff {
    pub goal_changed: bool,
    pub acceptance_added: Vec<String>,
    pub acceptance_removed: Vec<String>,
    pub constraints_added: Vec<String>,
    pub constraints_removed: Vec<String>,
    pub non_goals_added: Vec<String>,
    pub non_goals_removed: Vec<String>,
    pub body_unified_diff: String,
    pub version_from: u32,
    pub version_to: u32,
}

impl SpecDiff {
    pub fn is_empty(&self) -> bool {
        !self.goal_changed
            && self.acceptance_added.is_empty()
            && self.acceptance_removed.is_empty()
            && self.constraints_added.is_empty()
            && self.constraints_removed.is_empty()
            && self.non_goals_added.is_empty()
            && self.non_goals_removed.is_empty()
            && self.body_unified_diff.trim().is_empty()
    }

    pub fn render_markdown(&self) -> String {
        let mut s = format!(
            "# Spec diff v{} → v{}\n\n- goal_changed: {}\n",
            self.version_from, self.version_to, self.goal_changed
        );
        s.push_str(&list_block("acceptance_added", &self.acceptance_added));
        s.push_str(&list_block("acceptance_removed", &self.acceptance_removed));
        s.push_str(&list_block("constraints_added", &self.constraints_added));
        s.push_str(&list_block(
            "constraints_removed",
            &self.constraints_removed,
        ));
        s.push_str(&list_block("non_goals_added", &self.non_goals_added));
        s.push_str(&list_block("non_goals_removed", &self.non_goals_removed));
        s.push_str("\n## Body\n\n```diff\n");
        s.push_str(&self.body_unified_diff);
        if !self.body_unified_diff.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("```\n");
        s
    }
}

fn list_block(name: &str, items: &[String]) -> String {
    if items.is_empty() {
        return format!("- {name}: []\n");
    }
    let mut s = format!("- {name}:\n");
    for i in items {
        s.push_str("  - ");
        s.push_str(i);
        s.push('\n');
    }
    s
}

pub fn new_spec_id() -> String {
    format!("spc_{}", Uuid::new_v4().simple())
}

pub fn parse_spec(text: &str, max_bytes: usize) -> Result<SpecDoc, String> {
    if text.len() > max_bytes {
        return Err(format!("file exceeds maxFileBytes ({max_bytes})"));
    }
    let (yaml, body) = split_front_matter(text)?;
    let map = parse_yaml_map(yaml)?;
    let id = req_string(&map, "id")?;
    if !id.starts_with("spc_") {
        return Err("id must start with spc_".into());
    }
    let version = req_u32(&map, "version")?;
    if version < 1 {
        return Err("version must be >= 1".into());
    }
    let status = SpecStatus::parse(&req_string(&map, "status")?)?;
    let goal = req_string(&map, "goal")?;
    if goal.trim().is_empty() {
        return Err("goal must not be empty".into());
    }
    let acceptance = req_string_list(&map, "acceptance")?;
    if acceptance.is_empty() {
        return Err("acceptance must have at least 1 item".into());
    }
    Ok(SpecDoc {
        id,
        version,
        status,
        goal,
        non_goals: opt_string_list(&map, "non_goals"),
        constraints: opt_string_list(&map, "constraints"),
        acceptance,
        body: body.trim().to_string(),
    })
}

pub fn parse_plan(text: &str, max_bytes: usize) -> Result<PlanDoc, String> {
    if text.len() > max_bytes {
        return Err(format!("file exceeds maxFileBytes ({max_bytes})"));
    }
    let (yaml, body) = split_front_matter(text)?;
    let map = parse_yaml_map(yaml)?;
    let spec_id = req_string(&map, "spec_id")?;
    if spec_id.trim().is_empty() {
        return Err("spec_id must not be empty".into());
    }
    let spec_version = req_u32(&map, "spec_version")?;
    if spec_version < 1 {
        return Err("spec_version must be >= 1".into());
    }
    let steps = parse_steps(&map)?;
    if steps.is_empty() {
        return Err("steps must have at least 1 item".into());
    }
    let mut seen = BTreeSet::new();
    for step in &steps {
        if step.id.trim().is_empty() {
            return Err("step id must not be empty".into());
        }
        if step.title.trim().is_empty() {
            return Err("step title must not be empty".into());
        }
        if !seen.insert(step.id.clone()) {
            return Err(format!("duplicate step id '{}'", step.id));
        }
    }
    if steps.iter().all(is_meta_only_step) {
        return Err("steps must be workspace-verifiable work, not only subagent/MCP meta".into());
    }
    Ok(PlanDoc {
        spec_id,
        spec_version,
        steps,
        body: body.trim().to_string(),
    })
}

fn is_meta_only_step(step: &PlanStep) -> bool {
    if !step.files.is_empty() {
        return false;
    }
    let blob = format!("{} {} {}", step.id, step.title, step.done_when).to_ascii_lowercase();
    blob.contains("子代理")
        || blob.contains("subagent")
        || blob.contains("delegate")
        || blob.contains("mcp")
}

pub fn diff_specs(from: &SpecDoc, to: &SpecDoc) -> SpecDiff {
    let (acceptance_added, acceptance_removed) = set_diff(&from.acceptance, &to.acceptance);
    let (constraints_added, constraints_removed) = set_diff(&from.constraints, &to.constraints);
    let (non_goals_added, non_goals_removed) = set_diff(&from.non_goals, &to.non_goals);
    SpecDiff {
        goal_changed: from.goal != to.goal,
        acceptance_added,
        acceptance_removed,
        constraints_added,
        constraints_removed,
        non_goals_added,
        non_goals_removed,
        body_unified_diff: unified_diff(&from.body, &to.body, 3),
        version_from: from.version,
        version_to: to.version,
    }
}

pub fn render_spec(doc: &SpecDoc) -> String {
    let mut s = String::from("---\n");
    s.push_str(&format!("id: {}\n", doc.id));
    s.push_str(&format!("version: {}\n", doc.version));
    s.push_str(&format!("status: {}\n", doc.status.as_str()));
    s.push_str(&format!("goal: {}\n", yaml_scalar(&doc.goal)));
    s.push_str(&yaml_list("non_goals", &doc.non_goals));
    s.push_str(&yaml_list("constraints", &doc.constraints));
    s.push_str(&yaml_list("acceptance", &doc.acceptance));
    s.push_str("---\n");
    if !doc.body.is_empty() {
        s.push('\n');
        s.push_str(&doc.body);
        if !doc.body.ends_with('\n') {
            s.push('\n');
        }
    }
    s
}

fn yaml_list(key: &str, items: &[String]) -> String {
    if items.is_empty() {
        return format!("{key}: []\n");
    }
    let mut s = format!("{key}:\n");
    for i in items {
        s.push_str("  - ");
        s.push_str(&yaml_scalar(i));
        s.push('\n');
    }
    s
}

fn yaml_scalar(s: &str) -> String {
    if s.is_empty()
        || s.contains(':')
        || s.contains('#')
        || s.contains('\n')
        || s.starts_with([' ', '"', '\''])
    {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

pub fn split_front_matter(text: &str) -> Result<(&str, &str), String> {
    let t = text.trim_start_matches('\u{feff}').trim_start();
    if !t.starts_with("---") {
        return Err("missing YAML front matter".into());
    }
    let after = &t[3..];
    let after = after.strip_prefix('\r').unwrap_or(after);
    let after = after.strip_prefix('\n').unwrap_or(after);
    let close = after
        .find("\n---")
        .or_else(|| after.find("\r\n---"))
        .ok_or_else(|| "unterminated YAML front matter".to_string())?;
    let yaml = &after[..close];
    let rest = &after[close..];
    let rest = rest
        .strip_prefix("\r\n---")
        .or_else(|| rest.strip_prefix("\n---"))
        .unwrap_or(rest);
    let rest = rest.strip_prefix('\r').unwrap_or(rest);
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    Ok((yaml, rest))
}

fn parse_yaml_map(yaml: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let value = parse_yaml_subset(yaml)?;
    match value {
        serde_json::Value::Object(map) => Ok(map),
        _ => Err("front matter must be a mapping".into()),
    }
}

fn req_string(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String, String> {
    match map.get(key) {
        Some(serde_json::Value::String(s)) => Ok(s.trim().to_string()),
        Some(serde_json::Value::Number(n)) => Ok(n.to_string()),
        Some(other) => Err(format!("{key} must be a string, got {other}")),
        None => Err(format!("missing {key}")),
    }
}

fn req_u32(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Result<u32, String> {
    match map.get(key) {
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| format!("{key} must be a positive integer")),
        Some(serde_json::Value::String(s)) => s
            .trim()
            .parse()
            .map_err(|_| format!("{key} must be an integer")),
        _ => Err(format!("missing or invalid {key}")),
    }
}

fn req_string_list(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Vec<String>, String> {
    match map.get(key) {
        Some(v) => json_string_list(v, key),
        None => Err(format!("missing {key}")),
    }
}

fn opt_string_list(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    map.get(key)
        .and_then(|v| json_string_list(v, key).ok())
        .unwrap_or_default()
}

fn json_string_list(v: &serde_json::Value, key: &str) -> Result<Vec<String>, String> {
    match v {
        serde_json::Value::Array(arr) => arr
            .iter()
            .map(|i| match i {
                serde_json::Value::String(s) => Ok(s.trim().to_string()),
                serde_json::Value::Number(n) => Ok(n.to_string()),
                other => Err(format!("{key} items must be strings, got {other}")),
            })
            .collect(),
        serde_json::Value::String(s) if s.trim().is_empty() => Ok(Vec::new()),
        _ => Err(format!("{key} must be a list")),
    }
}

fn parse_steps(map: &serde_json::Map<String, serde_json::Value>) -> Result<Vec<PlanStep>, String> {
    let Some(serde_json::Value::Array(arr)) = map.get("steps") else {
        return Err("missing steps".into());
    };
    let mut steps = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let serde_json::Value::Object(obj) = item else {
            return Err(format!("steps[{i}] must be a mapping"));
        };
        let id = req_string(obj, "id")?;
        let title = req_string(obj, "title")?;
        let files = opt_string_list(obj, "files");
        let done_when = obj
            .get("done_when")
            .and_then(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        steps.push(PlanStep {
            id,
            title,
            files,
            done_when,
        });
    }
    Ok(steps)
}

/// Indent-based YAML subset → JSON. Enough for frozen SPEC/PLAN front matter.
fn parse_yaml_subset(input: &str) -> Result<serde_json::Value, String> {
    let lines: Vec<(usize, String)> = input
        .lines()
        .map(|l| {
            let raw = l.trim_end().to_string();
            let indent = l.chars().take_while(|c| *c == ' ').count();
            (indent, raw.trim().to_string())
        })
        .filter(|(_, t)| !t.is_empty() && !t.starts_with('#'))
        .collect();
    parse_block(&lines, 0, 0).map(|(v, _)| v)
}

fn parse_block(
    lines: &[(usize, String)],
    start: usize,
    min_indent: usize,
) -> Result<(serde_json::Value, usize), String> {
    if start >= lines.len() {
        return Ok((serde_json::Value::Object(serde_json::Map::new()), start));
    }
    if lines[start].1.starts_with("- ") || lines[start].1 == "-" {
        return parse_list(lines, start, min_indent);
    }
    parse_map(lines, start, min_indent)
}

fn parse_map(
    lines: &[(usize, String)],
    mut i: usize,
    min_indent: usize,
) -> Result<(serde_json::Value, usize), String> {
    let mut map = serde_json::Map::new();
    while i < lines.len() {
        let (indent, text) = &lines[i];
        if *indent < min_indent {
            break;
        }
        if text.starts_with("- ") {
            break;
        }
        let Some((key, rest)) = split_key(text) else {
            return Err(format!("expected key: {text}"));
        };
        i += 1;
        let value = if rest.is_empty() {
            if i < lines.len() && lines[i].0 > *indent {
                let (child, next) = parse_block(lines, i, indent + 1)?;
                i = next;
                child
            } else {
                serde_json::Value::Null
            }
        } else {
            parse_scalar(&rest)?
        };
        map.insert(key, value);
    }
    Ok((serde_json::Value::Object(map), i))
}

fn parse_list(
    lines: &[(usize, String)],
    mut i: usize,
    min_indent: usize,
) -> Result<(serde_json::Value, usize), String> {
    let mut arr = Vec::new();
    while i < lines.len() {
        let (indent, text) = &lines[i];
        if *indent < min_indent || !(text.starts_with("- ") || text == "-") {
            break;
        }
        let rest = if text == "-" {
            String::new()
        } else {
            text[2..].trim().to_string()
        };
        i += 1;
        if rest.is_empty() {
            if i < lines.len() && lines[i].0 > *indent {
                let (child, next) = parse_block(lines, i, indent + 1)?;
                i = next;
                arr.push(child);
            } else {
                arr.push(serde_json::Value::Null);
            }
        } else if let Some((k, v)) = split_key(&rest) {
            let mut obj = serde_json::Map::new();
            obj.insert(
                k,
                if v.is_empty() {
                    serde_json::Value::Null
                } else {
                    parse_scalar(&v)?
                },
            );
            while i < lines.len() && lines[i].0 > *indent && !lines[i].1.starts_with("- ") {
                let nested_indent = lines[i].0;
                let Some((nk, nv)) = split_key(&lines[i].1) else {
                    return Err(format!("expected key in list item: {}", lines[i].1));
                };
                i += 1;
                let val = if nv.is_empty() {
                    if i < lines.len() && lines[i].0 > nested_indent {
                        let (child, next) = parse_block(lines, i, nested_indent + 1)?;
                        i = next;
                        child
                    } else {
                        serde_json::Value::Null
                    }
                } else {
                    parse_scalar(&nv)?
                };
                obj.insert(nk, val);
            }
            arr.push(serde_json::Value::Object(obj));
        } else {
            arr.push(parse_scalar(&rest)?);
        }
    }
    Ok((serde_json::Value::Array(arr), i))
}

fn split_key(text: &str) -> Option<(String, String)> {
    let mut in_quote = false;
    for (i, ch) in text.char_indices() {
        match ch {
            '"' => in_quote = !in_quote,
            ':' if !in_quote => {
                let key = text[..i].trim().to_string();
                if key.is_empty() {
                    return None;
                }
                let rest = text[i + 1..].trim().to_string();
                return Some((key, rest));
            }
            _ => {}
        }
    }
    None
}

fn parse_scalar(raw: &str) -> Result<serde_json::Value, String> {
    let s = raw.trim();
    if s == "[]" {
        return Ok(serde_json::Value::Array(Vec::new()));
    }
    if s == "{}" {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }
    if let Some(inner) = s.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        let mut arr = Vec::new();
        if !inner.trim().is_empty() {
            for part in split_csv(inner) {
                arr.push(parse_scalar(&part)?);
            }
        }
        return Ok(serde_json::Value::Array(arr));
    }
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        return Ok(serde_json::Value::String(unquote(&s[1..s.len() - 1])));
    }
    if s == "true" {
        return Ok(serde_json::Value::Bool(true));
    }
    if s == "false" {
        return Ok(serde_json::Value::Bool(false));
    }
    if let Ok(n) = s.parse::<u64>() {
        return Ok(serde_json::Value::Number(n.into()));
    }
    if let Ok(n) = s.parse::<i64>() {
        return Ok(serde_json::json!(n));
    }
    Ok(serde_json::Value::String(s.to_string()))
}

fn split_csv(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for ch in inner.chars() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                cur.push(ch);
            }
            ',' if !in_quote => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn unquote(s: &str) -> String {
    s.replace("\\\"", "\"").replace("\\\\", "\\")
}

fn set_diff(old: &[String], new: &[String]) -> (Vec<String>, Vec<String>) {
    let old_set: BTreeSet<_> = old.iter().cloned().collect();
    let new_set: BTreeSet<_> = new.iter().cloned().collect();
    let added: Vec<String> = new_set.difference(&old_set).cloned().collect();
    let removed: Vec<String> = old_set.difference(&new_set).cloned().collect();
    (added, removed)
}

pub fn unified_diff(old: &str, new: &str, context: usize) -> String {
    if old == new {
        return String::new();
    }
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let edits = myers(&a, &b);
    render_unified(&a, &b, &edits, context)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    Keep(usize, usize),
    Del(usize),
    Ins(usize),
}

fn myers(a: &[&str], b: &[&str]) -> Vec<Edit> {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if a[i] == b[j] {
                dp[i][j] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut edits = Vec::new();
    let mut i = n;
    let mut j = m;
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && a[i - 1] == b[j - 1] {
            edits.push(Edit::Keep(i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            edits.push(Edit::Ins(j - 1));
            j -= 1;
        } else {
            edits.push(Edit::Del(i - 1));
            i -= 1;
        }
    }
    edits.reverse();
    edits
}

fn render_unified(a: &[&str], b: &[&str], edits: &[Edit], context: usize) -> String {
    let mut hunks: Vec<Vec<Edit>> = Vec::new();
    let mut cur: Vec<Edit> = Vec::new();
    let mut keep_run = 0usize;
    for e in edits {
        match e {
            Edit::Keep(..) => {
                keep_run += 1;
                cur.push(*e);
                if keep_run > context * 2 && cur.iter().any(|x| !matches!(x, Edit::Keep(..))) {
                    let split_at = cur.len() - (keep_run - context);
                    let rest = cur.split_off(split_at);
                    while matches!(cur.last(), Some(Edit::Keep(..)))
                        && cur.iter().filter(|x| matches!(x, Edit::Keep(..))).count() > context
                        && cur
                            .iter()
                            .rev()
                            .take_while(|x| matches!(x, Edit::Keep(..)))
                            .count()
                            > context
                    {
                        // trim trailing keep beyond context
                        let trailing = cur
                            .iter()
                            .rev()
                            .take_while(|x| matches!(x, Edit::Keep(..)))
                            .count();
                        if trailing > context {
                            cur.truncate(cur.len() - (trailing - context));
                        }
                        break;
                    }
                    if cur.iter().any(|x| !matches!(x, Edit::Keep(..))) {
                        hunks.push(std::mem::take(&mut cur));
                    }
                    cur = rest
                        .into_iter()
                        .rev()
                        .take(context)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    keep_run = cur.len();
                }
            }
            _ => {
                keep_run = 0;
                cur.push(*e);
            }
        }
    }
    if cur.iter().any(|x| !matches!(x, Edit::Keep(..))) {
        let trailing = cur
            .iter()
            .rev()
            .take_while(|x| matches!(x, Edit::Keep(..)))
            .count();
        if trailing > context {
            cur.truncate(cur.len() - (trailing - context));
        }
        hunks.push(cur);
    }
    let mut out = String::new();
    for hunk in hunks {
        let mut a_start = a.len().saturating_add(1);
        let mut b_start = b.len().saturating_add(1);
        let mut a_count = 0usize;
        let mut b_count = 0usize;
        for e in &hunk {
            match e {
                Edit::Keep(i, j) => {
                    a_start = a_start.min(*i + 1);
                    b_start = b_start.min(*j + 1);
                    a_count += 1;
                    b_count += 1;
                }
                Edit::Del(i) => {
                    a_start = a_start.min(*i + 1);
                    a_count += 1;
                }
                Edit::Ins(j) => {
                    b_start = b_start.min(*j + 1);
                    b_count += 1;
                }
            }
        }
        if a_start > a.len() {
            a_start = 1;
        }
        if b_start > b.len() {
            b_start = 1;
        }
        out.push_str(&format!(
            "@@ -{a_start},{a_count} +{b_start},{b_count} @@\n"
        ));
        for e in hunk {
            match e {
                Edit::Keep(i, _) => {
                    out.push(' ');
                    out.push_str(a[i]);
                    out.push('\n');
                }
                Edit::Del(i) => {
                    out.push('-');
                    out.push_str(a[i]);
                    out.push('\n');
                }
                Edit::Ins(j) => {
                    out.push('+');
                    out.push_str(b[j]);
                    out.push('\n');
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_text(goal: &str, extra_accept: &str) -> String {
        format!(
            "---\nid: spc_abc\nversion: 1\nstatus: draft\ngoal: {goal}\nnon_goals:\n  - 不做桌面\nconstraints:\n  - 只改 core\nacceptance:\n  - cargo test -p dandelion-core 过{extra_accept}\n---\n\n## 背景\nhello\n"
        )
    }

    #[test]
    fn spec_parse_rejects_empty_goal() {
        let t = spec_text("   ", "");
        let err = parse_spec(&t, 65536).unwrap_err();
        assert!(err.contains("goal"), "{err}");
    }

    #[test]
    fn spec_parse_rejects_empty_acceptance() {
        let t = "---\nid: spc_abc\nversion: 1\nstatus: draft\ngoal: x\nacceptance: []\n---\n";
        let err = parse_spec(t, 65536).unwrap_err();
        assert!(err.contains("acceptance"), "{err}");
    }

    #[test]
    fn spec_parse_ok_roundtrip_fields() {
        let doc = parse_spec(&spec_text("给 doctor 加一行", ""), 65536).unwrap();
        assert_eq!(doc.id, "spc_abc");
        assert_eq!(doc.goal, "给 doctor 加一行");
        assert_eq!(doc.acceptance.len(), 1);
        assert_eq!(doc.body, "## 背景\nhello");
    }

    #[test]
    fn spec_parse_rejects_over_cap() {
        let t = spec_text("goal", "");
        let err = parse_spec(&t, 8).unwrap_err();
        assert!(err.contains("maxFileBytes"), "{err}");
    }

    #[test]
    fn plan_parse_rejects_missing_steps() {
        let t = "---\nspec_id: spc_abc\nspec_version: 1\nsteps: []\n---\n";
        let err = parse_plan(t, 65536).unwrap_err();
        assert!(err.contains("steps"), "{err}");
    }

    #[test]
    fn plan_parse_ok() {
        let t = r#"---
spec_id: spc_abc
spec_version: 3
steps:
  - id: s1
    title: 补齐校验
    files: ["crates/dandelion-core/src/spec/mod.rs"]
    done_when: "单测 spec_parse_rejects_empty_goal 绿"
---
"#;
        let plan = parse_plan(t, 65536).unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].id, "s1");
        assert_eq!(
            plan.steps[0].files[0],
            "crates/dandelion-core/src/spec/mod.rs"
        );
    }

    #[test]
    fn plan_rejects_meta_only_steps() {
        let t = r#"---
spec_id: spc_abc
spec_version: 1
steps:
  - id: s1
    title: 调用子代理
    files: []
    done_when: "spawn mcp"
---
"#;
        let err = parse_plan(t, 65536).unwrap_err();
        assert!(err.contains("workspace-verifiable"), "{err}");
    }

    #[test]
    fn diff_set_difference_and_goal() {
        let a = parse_spec(&spec_text("old", ""), 65536).unwrap();
        let b = parse_spec(&spec_text("new", "\n  - Windows CI"), 65536).unwrap();
        let d = diff_specs(&a, &b);
        assert!(d.goal_changed);
        assert_eq!(d.acceptance_added, vec!["Windows CI".to_string()]);
        assert!(d.acceptance_removed.is_empty());
        assert!(!d.is_empty());
    }

    #[test]
    fn diff_empty_when_identical() {
        let a = parse_spec(&spec_text("same", ""), 65536).unwrap();
        let d = diff_specs(&a, &a);
        assert!(d.is_empty());
        assert!(d.body_unified_diff.is_empty());
    }

    #[test]
    fn body_unified_diff_has_plus_minus() {
        let mut a = parse_spec(&spec_text("g", ""), 65536).unwrap();
        let mut b = a.clone();
        a.body = "line1\nkeep\nline3".into();
        b.body = "line1\nchanged\nline3".into();
        let d = diff_specs(&a, &b);
        assert!(d.body_unified_diff.contains("-keep"), "{d:?}");
        assert!(d.body_unified_diff.contains("+changed"), "{d:?}");
        assert!(d.body_unified_diff.contains("@@"), "{d:?}");
    }
}
