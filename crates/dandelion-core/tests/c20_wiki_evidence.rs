//! C20: evidence-index contract (Wiki deep-build phase S1).
//! Covers the spec's verification items that are index-level: a wrong
//! project root is *detected and reported* (parent-of-repos), the coverage
//! report accounts for every file (budget + skip reasons, not "scanned N"),
//! citations locate real lines (symbol/section/content hits), and two
//! workspaces keep two isolated indexes.

use dandelion_core::wiki;
use dandelion_core::wiki_index::{self as wi, Category};
use std::fs;

fn touch(ws: &std::path::Path, rel: &str, body: &str) {
    let p = ws.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn build(data: &std::path::Path, ws: &std::path::Path, max_files: usize) -> wi::EvidenceIndex {
    wi::build_index(data, ws, max_files, 65_536).unwrap()
}

#[test]
fn wrong_root_is_detected_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ws = tmp.path().join("parent-of-repos");
    touch(&ws, "app-a/package.json", "{\"name\":\"a\"}");
    touch(&ws, "app-a/src/index.js", "export function a() {}\n");
    touch(&ws, "app-b/Cargo.toml", "[package]\nname = \"b\"\n");
    touch(&ws, "app-b/src/main.rs", "fn main() {}\n");
    let idx = build(&data, &ws, 100);
    assert!(!idx.coverage.root_manifest);
    let dirs: Vec<&str> = idx.coverage.child_repos.iter().map(|c| c.dir.as_str()).collect();
    assert!(dirs.contains(&"app-a") && dirs.contains(&"app-b"), "{dirs:?}");
    assert!(idx.coverage.root_suggestion.is_none());
    // A real project root is never flagged.
    let ws2 = tmp.path().join("real-app");
    touch(&ws2, "package.json", "{\"name\":\"x\"}");
    touch(&ws2, "src/index.js", "export const x = 1;\n");
    let idx2 = wi::build_index(&tmp.path().join("data2"), &ws2, 100, 65_536).unwrap();
    assert!(idx2.coverage.root_manifest);
    assert!(idx2.coverage.child_repos.is_empty());
    assert!(idx2.coverage.root_suggestion.is_none());
}

#[test]
fn coverage_report_accounts_for_every_outcome() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ws = tmp.path().join("ws");
    touch(&ws, "package.json", "{}");
    touch(&ws, "README.md", "# Demo\n");
    for i in 0..30 {
        touch(&ws, &format!("src/m{i}.ts"), "export const v = 1;\n");
    }
    touch(&ws, "node_modules/dep/index.js", "junk\n");
    touch(&ws, "assets/noise.png", "not text\n"); // never a candidate
    let idx = build(&data, &ws, 5);
    // included + over_budget covers every candidate; ignored trees named.
    assert_eq!(idx.files.len(), 5);
    assert!(idx.coverage.budget_hit);
    assert_eq!(
        idx.coverage.included_files + idx.coverage.over_budget + idx.coverage.too_large
            == idx.coverage.candidate_files,
        true,
        "coverage must add up: {:?}",
        idx.coverage
    );
    assert!(idx.coverage.ignored_dirs.iter().any(|d| d == "node_modules"));
    assert!(idx.coverage.over_budget_examples.len() <= 20);
    // Priority-ordered: manifests/docs before the plain sources.
    let cats: Vec<Category> = idx.files.iter().map(|f| f.category).collect();
    assert_eq!(cats[0], Category::Manifest, "{cats:?}");
    assert_eq!(cats[1], Category::Doc, "{cats:?}");
}

#[test]
fn citations_locate_real_lines_in_original_files() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ws = tmp.path().join("ws");
    touch(&ws, "package.json", "{\"name\":\"x\",\"main\":\"src/run.ts\"}");
    touch(
        &ws,
        "src/run.ts",
        "// boot file\nexport function launchRuntime() {\n  startLoops();\n}\n",
    );
    touch(&ws, "docs/setup.md", "# Setup\n\n## Install\n\nrun npm i\n");
    let idx = build(&data, &ws, 100);

    let hits = wi::lookup(&ws, &idx, "launchRuntime", 10);
    let sym = hits
        .iter()
        .find(|h| h.via == "symbol" && h.rel == "src/run.ts")
        .expect("symbol hit");
    // the cited line really holds the declaration.
    let content = fs::read_to_string(ws.join(&sym.rel)).unwrap();
    let line = content.lines().nth((sym.start - 1) as usize).unwrap();
    assert!(line.contains("launchRuntime"), "cited line: {line}");

    let hits = wi::lookup(&ws, &idx, "Install", 10);
    let sec = hits
        .iter()
        .find(|h| h.via == "section" && h.rel == "docs/setup.md")
        .expect("section hit");
    assert!(sec.start <= sec.end);
    assert!(sec.end <= idx.files.iter().find(|f| f.rel == "docs/setup.md").unwrap().lines);
}

#[test]
fn workspaces_keep_isolated_indexes() {
    let tmp = tempfile::tempdir().unwrap();
    let ws_a = tmp.path().join("a");
    let ws_b = tmp.path().join("b");
    touch(&ws_a, "package.json", "{\"name\":\"a\"}");
    touch(&ws_b, "package.json", "{\"name\":\"b\"}");
    // project_data_dir is per-workspace-hash, exactly like the Wiki itself.
    let data_a = wiki::project_data_dir(&tmp.path().join("store"), &ws_a);
    let data_b = wiki::project_data_dir(&tmp.path().join("store"), &ws_b);
    assert_ne!(data_a, data_b);
    let idx_a = wi::build_index(&data_a, &ws_a, 100, 65_536).unwrap();
    wi::build_index(&data_b, &ws_b, 100, 65_536).unwrap();
    assert!(idx_a.workspace.ends_with("a"));
    let loaded_b = wi::load_index(&data_b).unwrap();
    assert!(loaded_b.workspace.ends_with("b"));
}

/// Real-repo smoke (acceptance target). Run with:
/// `cargo test -p dandelion-core --test c20_wiki_evidence -- --ignored --nocapture`
#[test]
#[ignore = "requires DLN_ACC_REA to point to a REA checkout"]
fn rea_scan_smoke() {
    let Some(ws) = std::env::var_os("DLN_ACC_REA") else {
        eprintln!("DLN_ACC_REA is not set, skipping");
        return;
    };
    let ws = std::path::Path::new(&ws);
    if !ws.join("package.json").exists() {
        eprintln!("rea not present, skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let idx = wi::build_index(&tmp.path().join("d"), ws, 800, 65_536).unwrap();
    let c = &idx.coverage;
    eprintln!(
        "candidates={} included={} over_budget={} too_large={} ignored={:?} root_manifest={} children={}",
        c.candidate_files,
        c.included_files,
        c.over_budget,
        c.too_large,
        &c.ignored_dirs.len(),
        c.root_manifest,
        c.child_repos.len()
    );
    assert!(c.root_manifest);
    assert!(idx.files.iter().any(|f| f.rel == "package.json"));
    assert!(idx.files.iter().any(|f| f.rel.starts_with("docs/") && f.category == Category::Doc));
    // top of the list must be manifest/doc/entry, never a test.
    for f in idx.files.iter().take(10) {
        assert!(f.category != Category::Test, "test ranked too high: {f:?}");
    }
    let hits = wi::lookup(ws, &idx, "capture", 12);
    assert!(!hits.is_empty());
    eprintln!("first hits: {:?}", &hits[..hits.len().min(5)]);
}

#[test]
fn stale_file_still_lookupable_via_index_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ws = tmp.path().join("ws");
    touch(&ws, "package.json", "{}");
    touch(&ws, "src/alpha.ts", "export function alphaThing() {}\n");
    let idx = build(&data, &ws, 100);
    fs::remove_file(ws.join("src/alpha.ts")).unwrap();
    // Deleted file: no crash, metadata-only hits still available.
    let hits = wi::lookup(&ws, &idx, "alphaThing", 10);
    assert!(hits.iter().any(|h| h.via == "symbol"));
}
