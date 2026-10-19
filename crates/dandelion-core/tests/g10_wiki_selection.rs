//! G10: evidence SELECTION correction (spec 五: 证据包不得被文档占满，
//! 核心流程要追到实现). The REA acceptance run proved the failure mode
//! live: the planner nominated README_zh/README_ja/README_zh-TW as the
//! entire evidence set of a code-flow topic, the pack rendered those
//! prose copies first under a byte budget, and the implementation files
//! never got shown. The production fix is `wiki_build::sanitize_inputs`
//! — translated duplicates swap for their canonical (existence-gated),
//! and doc-only topics get the top-priority code files prepended. These
//! tests pin the rule table; the runtime wires it at the single targets
//! choke point (covered here at the unit + pack level, and end-to-end by
//! the real-model REA rebuild in the acceptance record).

use dandelion_core::wiki_build as wb;
use dandelion_core::wiki_index as wi;
use std::fs;

fn touch(ws: &std::path::Path, rel: &str, body: &str) {
    let p = ws.join(rel);
    fs::create_dir_all(p.parent().unwrap_or(ws)).unwrap();
    fs::write(&p, body).unwrap();
}

/// Code repo: manifest + canonical README + three translations + a doc +
/// an orphan translation (no canonical) + entry/source/test files.
fn code_ws() -> (tempfile::TempDir, std::path::PathBuf) {
    let ws = tempfile::tempdir().unwrap();
    let w = ws.path();
    touch(w, "package.json", "{\n  \"name\": \"g10\",\n  \"scripts\": { \"test\": \"node --test\" }\n}\n");
    touch(w, "README.md", "# G10\n\nRun npm start.\n");
    touch(w, "README_zh.md", "# G10 中文\n\n运行 npm start。\n");
    touch(w, "README_ja.md", "# G10 日本語\n\nnpm start を実行。\n");
    touch(w, "README_fr.md", "# G10 en français\n\nLancez npm start.\n");
    touch(w, "docs/guide.md", "# Guide\n\n## Run\n\nnpm start\n");
    touch(w, "docs/notes_ko.md", "# Notes (한국어)\n\n외로운 번역:正本 없음。\n");
    touch(w, "src/index.ts", "import { run } from \"./core/flow\";\nrun();\n");
    touch(w, "src/core/flow.ts", "export function run(): void { /* the flow */ }\n");
    touch(w, "src/core/flow.test.ts", "import { run } from \"./flow\";\nrun();\n");
    let path = w.to_path_buf();
    (ws, path)
}

fn index_of(ws: &std::path::Path) -> wi::EvidenceIndex {
    // The index is a fully owned snapshot; the metadata dir is only the
    // build's scratch space.
    let data = tempfile::tempdir().unwrap();
    let pdir = data.path().join("proj");
    fs::create_dir_all(&pdir).unwrap();
    wi::build_index(&pdir, ws, 100, 65_536).unwrap()
}

fn category_of(idx: &wi::EvidenceIndex, rel: &str) -> Option<wi::Category> {
    idx.files.iter().find(|f| f.rel == rel).map(|f| f.category)
}

fn is_code(c: Option<wi::Category>) -> bool {
    matches!(c, Some(wi::Category::Entry) | Some(wi::Category::Source) | Some(wi::Category::Test))
}

// ---------------------------------------------------------------- rule (a)

#[test]
fn translated_duplicates_swap_for_their_canonical_once() {
    let (_t, ws) = code_ws();
    let idx = index_of(&ws);
    // A code input present => rule (b) stays quiet, isolating rule (a).
    let mut inputs = vec![
        "src/index.ts".to_string(),
        "README_zh.md".to_string(),
        "README.md".to_string(),
        "README_ja.md".to_string(),
    ];
    wb::sanitize_inputs(&mut inputs, &idx);
    assert_eq!(inputs, vec!["src/index.ts".to_string(), "README.md".to_string()],
        "both translations fold into the single canonical README, order-first-wins");
}

#[test]
fn translation_without_a_canonical_survives() {
    let (_t, ws) = code_ws();
    let idx = index_of(&ws);
    // docs/notes.md does not exist: docs/notes_ko.md is the ONLY copy of
    // those facts and must not be dropped by the swap.
    let mut inputs = vec!["docs/guide.md".to_string(), "docs/notes_ko.md".to_string()];
    wb::sanitize_inputs(&mut inputs, &idx);
    assert!(inputs.contains(&"docs/notes_ko.md".to_string()), "orphan translation kept: {}", inputs.join(","));
    assert!(inputs.contains(&"docs/guide.md".to_string()));
}

#[test]
fn pure_doc_repo_folds_translations_without_injecting_anything() {
    let t = tempfile::tempdir().unwrap();
    let w = t.path();
    touch(w, "README.md", "# Notes\n");
    touch(w, "README_zh.md", "# 笔记\n");
    touch(w, "docs/only.md", "# Only\n");
    let idx = index_of(w);
    let mut inputs = vec!["README_zh.md".to_string(), "docs/only.md".to_string()];
    wb::sanitize_inputs(&mut inputs, &idx);
    assert_eq!(inputs, vec!["README.md".to_string(), "docs/only.md".to_string()],
        "swap happens with zero code files, and nothing is invented");
}

// ---------------------------------------------------------------- rule (b)

#[test]
fn doc_only_topic_gets_top_priority_code_prepended() {
    let (_t, ws) = code_ws();
    let idx = index_of(&ws);
    let mut inputs = vec!["docs/guide.md".to_string()];
    wb::sanitize_inputs(&mut inputs, &idx);
    assert!(inputs.len() >= 2, "grounding must be added: {inputs:?}");
    let prepended = &inputs[..inputs.len() - 1];
    assert!(
        prepended.iter().all(|r| is_code(category_of(&idx, r))),
        "everything before the topic's own doc must be code: {inputs:?}"
    );
    assert!(prepended.len() <= 3, "grounding capped at 3: {inputs:?}");
    assert_eq!(inputs.last().unwrap(), "docs/guide.md", "the topic keeps its own nomination, last");
}

#[test]
fn a_topic_with_one_code_input_is_left_alone() {
    let (_t, ws) = code_ws();
    let idx = index_of(&ws);
    let mut inputs = vec!["README.md".to_string(), "src/core/flow.ts".to_string()];
    wb::sanitize_inputs(&mut inputs, &idx);
    assert_eq!(inputs, vec!["README.md".to_string(), "src/core/flow.ts".to_string()],
        "one implementation file already grounds the topic — hands off");
}

#[test]
fn empty_inputs_stay_empty_for_the_pack_floor() {
    let (_t, ws) = code_ws();
    let idx = index_of(&ws);
    let mut inputs: Vec<String> = Vec::new();
    wb::sanitize_inputs(&mut inputs, &idx);
    assert!(inputs.is_empty(), "system-fallback shape is the floor's job, not the sanitizer's");
}

// ------------------------------------------------------- prompt & end-to-end

#[test]
fn plan_prompt_teaches_the_same_rules_the_code_enforces() {
    let (_t, ws) = code_ws();
    let idx = index_of(&ws);
    let p = wb::plan_prompt(&idx, 8);
    assert!(p.contains("源码/入口/测试"), "prompt must demand implementation files: {p}");
    assert!(p.contains("翻译副本"), "prompt must warn against README_<lang> picks");
}

#[test]
fn the_rendered_pack_of_a_doc_only_topic_shows_code() {
    let (_t, ws) = code_ws();
    let idx = index_of(&ws);
    let topic = wb::TopicSpec {
        title: "核心流程".into(),
        focus: "一次典型操作依次经过哪些模块".into(),
        inputs: vec!["docs/guide.md".to_string()],
    };
    let mut sanitized = topic.clone();
    wb::sanitize_inputs(&mut sanitized.inputs, &idx);
    let (_pack, shown) = wb::render_topic_pack(&ws, &idx, &sanitized, 65_536);
    let shown_code = shown.iter().filter(|s| is_code(category_of(&idx, &s.rel))).count();
    assert!(shown_code >= 1, "the pack must actually SHOW implementation files, got: {:?}",
        shown.iter().map(|s| s.rel.clone()).collect::<Vec<_>>());
    // The topic's own doc still rides along — grounding supplements, never
    // censors the model's nomination.
    assert!(shown.iter().any(|s| s.rel == "docs/guide.md"));
}
