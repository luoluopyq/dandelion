//! C23: git-true gitignore semantics for the Wiki scan (unification phase 3).
//!
//! The work order names the exact defects of the old basename matcher, and
//! these tests are the regression lock: `build/*` must never degrade into
//! "ignore everything" (a pattern with a slash anchors to the ignore file's
//! directory), `/public/main` must not strike `src/main`, `!` re-includes
//! really re-include, a nested `.gitignore` overrides the shallower layers,
//! and `Settings::wiki_ignore_patterns` sits on top of the tree's own rules
//! in both directions (escape-hatch `!` re-includes what the repo hides;
//! an added pattern outranks a repo whitelist). Skips stay explainable:
//! every ignored directory names the rule and the layer that killed it.

use dandelion_core::wiki_index::{self as wi, IndexBudget};
use std::fs;

fn touch(ws: &std::path::Path, rel: &str, body: &str) {
    let p = ws.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn budget(patterns: &[&str]) -> IndexBudget {
    IndexBudget {
        ignore_patterns: patterns.iter().map(|p| p.to_string()).collect(),
        ..IndexBudget::legacy(200, 65_536)
    }
}

fn rels(data: &std::path::Path, ws: &std::path::Path, b: &IndexBudget) -> Vec<String> {
    let idx = wi::build_index_with_budget(data, ws, b).unwrap();
    idx.files.iter().map(|f| f.rel.clone()).collect()
}

fn has(v: &[String], rel: &str) -> bool {
    v.iter().any(|f| f == rel)
}

/// The flagship defect: `build/*` used to degrade to the leaf pattern `*`
/// and swallow every file in the tree. Git semantics anchor a pattern that
/// contains a slash to the ignore file's directory, so only `build/`'s own
/// children are hit — sibling trees and a same-named directory deeper down
/// are untouched.
#[test]
fn slash_pattern_anchors_and_never_degrades() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch(&ws, ".gitignore", "generated/*\n");
    touch(&ws, "README.md", "# keep\n");
    touch(&ws, "generated/a.rs", "fn a() {}\n");
    touch(&ws, "generated/sub/deep.rs", "fn deep() {}\n");
    touch(&ws, "src/x.rs", "fn x() {}\n");
    touch(&ws, "src/generated/keep.rs", "fn keep() {}\n");

    let v = rels(&data, &ws, &budget(&[]));
    assert!(has(&v, "README.md") && has(&v, "src/x.rs") && has(&v, "src/generated/keep.rs"), "{v:?}");
    assert!(!has(&v, "generated/a.rs"), "`generated/*` hits build's own children: {v:?}");
    assert!(
        !v.iter().any(|f| f.starts_with("generated/sub/")),
        "ignored dir is not descended: {v:?}"
    );

    let idx = wi::build_index_with_budget(&data, &ws, &budget(&[])).unwrap();
    assert!(
        idx.coverage.ignored_dirs.iter().any(|d| d == "generated/sub"),
        "skipped tree is named: {:?}",
        idx.coverage.ignored_dirs
    );
    let explained = idx
        .coverage
        .ignore_origins
        .iter()
        .any(|o| o == "generated/sub ← .gitignore:generated/*");
    assert!(explained, "skip names rule and layer: {:?}", idx.coverage.ignore_origins);
}

/// `/public/main.rs` is anchored at the workspace root and can never strike
/// `src/main.rs` — the old matcher matched the bare leaf `main.rs` anywhere.
#[test]
fn anchored_file_pattern_cannot_hit_same_name_elsewhere() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch(&ws, ".gitignore", "/public/main.rs\n");
    touch(&ws, "public/main.rs", "fn hidden() {}\n");
    touch(&ws, "src/main.rs", "fn main() {}\n");

    let v = rels(&data, &ws, &budget(&[]));
    assert!(has(&v, "src/main.rs"), "/-prefix must anchor to the root: {v:?}");
    assert!(!has(&v, "public/main.rs"), "anchored pattern still ignores its target: {v:?}");
}

/// `!` re-includes at every depth, and only what it names: `keep.txt`
/// survives the tree-wide `*.txt`, `drop.txt` does not, and a merely
/// similar name is not rescued.
#[test]
fn negation_reincludes_exactly_what_it_names() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch(&ws, ".gitignore", "*.txt\n!keep.txt\n");
    touch(&ws, "keep.txt", "root keep\n");
    touch(&ws, "drop.txt", "root drop\n");
    touch(&ws, "notes-keep.txt", "similar name\n");
    touch(&ws, "docs/keep.txt", "deep keep\n");
    touch(&ws, "docs/drop.txt", "deep drop\n");

    let v = rels(&data, &ws, &budget(&[]));
    assert!(has(&v, "keep.txt") && has(&v, "docs/keep.txt"), "{v:?}");
    assert!(!has(&v, "drop.txt") && !has(&v, "docs/drop.txt"), "{v:?}");
    assert!(!has(&v, "notes-keep.txt"), "`!keep.txt` matches names, not substrings: {v:?}");
}

/// A nested `.gitignore` overrides the shallower layer for its own subtree
/// — here a deep `!important.txt` rescues it from the root's `*.txt`,
/// while the root rule keeps its bite everywhere else.
#[test]
fn nested_gitignore_overrides_shallower_layers() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch(&ws, ".gitignore", "*.txt\n");
    touch(&ws, "sub/.gitignore", "!important.txt\n");
    touch(&ws, "a.txt", "root\n");
    touch(&ws, "sub/important.txt", "rescued\n");
    touch(&ws, "sub/x.txt", "still ignored\n");

    let v = rels(&data, &ws, &budget(&[]));
    assert!(has(&v, "sub/important.txt"), "nested negation wins for its subtree: {v:?}");
    assert!(!has(&v, "a.txt") && !has(&v, "sub/x.txt"), "{v:?}");
}

/// The built-in vendor/build skips are the *lowest* layer, and they are
/// explainable: a skipped tree reports `built-in skips` as its rule source.
/// A repo that legitimately uses such a name reclaims it with `!build/`.
#[test]
fn builtin_skips_are_named_and_reclaimable() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch(&ws, "package.json", "{}");
    touch(&ws, "node_modules/dep/index.js", "junk\n");
    touch(&ws, "build/out.js", "generated\n");
    let idx = wi::build_index_with_budget(&data, &ws, &budget(&[])).unwrap();
    let v: Vec<String> = idx.files.iter().map(|f| f.rel.clone()).collect();
    assert!(!v.iter().any(|f| f.starts_with("node_modules/")), "{v:?}");
    assert!(!v.iter().any(|f| f.starts_with("build/")), "{v:?}");
    assert!(idx.coverage.ignored_dirs.iter().any(|d| d == "node_modules"));
    assert!(
        idx.coverage
            .ignore_origins
            .iter()
            .any(|o| o.starts_with("node_modules ← built-in skips:")),
        "the rule source is named: {:?}",
        idx.coverage.ignore_origins
    );

    let ws2 = tmp.path().join("ws2");
    touch(&ws2, ".gitignore", "!build/\n");
    touch(&ws2, "build/main.rs", "fn main() {}\n");
    let v2 = rels(&tmp.path().join("data2"), &ws2, &budget(&[]));
    assert!(has(&v2, "build/main.rs"), "repo policy outranks the built-in layer: {v2:?}");
}

/// Dot-directories stay hidden (with the `.github` exception) — behaviour
/// the legacy matcher had, kept byte-for-byte.
#[test]
fn dot_dirs_stay_hidden_except_github() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch(&ws, ".github/workflows/ci.yml", "on: push\n");
    touch(&ws, ".hidden/notes.txt", "private\n");
    let v = rels(&data, &ws, &budget(&[]));
    assert!(has(&v, ".github/workflows/ci.yml"), "{v:?}");
    assert!(!has(&v, ".hidden/notes.txt"), "{v:?}");
}

/// `Settings::wiki_ignore_patterns` is the top layer in both directions:
/// an escape-hatch `!` re-includes what the repo hides, and an added
/// pattern outranks the repo's own whitelist.
#[test]
fn user_patterns_sit_on_top_of_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch(&ws, ".gitignore", "docs-hidden/\n");
    touch(&ws, "docs-hidden/guide.md", "# guide\n");
    touch(&ws, "README.md", "# keep\n");

    let plain = rels(&data, &ws, &budget(&[]));
    assert!(!has(&plain, "docs-hidden/guide.md"), "repo ignore stands by default: {plain:?}");
    let rescued = rels(&data, &ws, &budget(&["!docs-hidden/"]));
    assert!(
        has(&rescued, "docs-hidden/guide.md"),
        "settings `!` re-includes what the repo hides: {rescued:?}"
    );

    let ws2 = tmp.path().join("ws2");
    touch(&ws2, ".gitignore", "!secret.txt\n");
    touch(&ws2, "secret.txt", "top secret\n");
    touch(&ws2, "open.txt", "fine\n");
    let v2 = rels(&tmp.path().join("data2"), &ws2, &budget(&["secret.txt"]));
    assert!(has(&v2, "open.txt"), "{v2:?}");
    assert!(!has(&v2, "secret.txt"), "user policy outranks a repo whitelist: {v2:?}");
}

/// Valid trees report zero pattern errors — the error channel exists for
/// honest reporting, not noise.
#[test]
fn valid_patterns_produce_no_error_noise() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch(&ws, ".gitignore", "*.tmp\n!keep.tmp\n/build/\nsub/*.txt\n");
    touch(&ws, "src/main.rs", "fn main() {}\n");
    let idx = wi::build_index_with_budget(&data, &ws, &budget(&["*.bak", "!docs/"])).unwrap();
    assert!(idx.coverage.ignore_pattern_errors.is_empty(), "{:?}", idx.coverage.ignore_pattern_errors);
}
