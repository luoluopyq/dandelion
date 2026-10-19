//! Evidence index (Wiki deep-build phase S1).
//!
//! A two-pass project scan: first enumerate every candidate cheaply (path,
//! size, kind), then include files in *importance order* — manifests,
//! readme/entry docs, detected entrypoints, core sources, then tests and
//! config — until the file/byte budget runs out. The result is an evidence
//! index: per-file hash, line count, category, symbol table and document
//! section ranges. It is the shared fact base both for grounded article
//! generation (which must cite `path:Lx-Ly`) and for question answering
//! that verifies claims against the original files.
//!
//! The index never stores whole file bodies: only metadata and line ranges.
//! Excerpts are read fresh from the user's workspace at lookup time, which
//! is also where "open the source" navigates. Full coverage accounting is
//! part of the contract: the caller must be able to say exactly what was
//! skipped and why — never just "scanned N files".
//!
//! Three separate budgets, each reporting its own gap (never one mixed
//! cap where doc overflow silently starves sources):
//! * **discovery** — `enumerate` walks every relevant file (hard-capped);
//! * **metadata** — hash/lines/symbols/sections/imports for every discovered
//!   file up to `metaFiles`/`metaFileBytes`: lookup and QA *see* these even
//!   when their text never enters an evidence pack;
//! * **body** — `bodyFiles` whole-text slots (per-category quota) that an
//!   evidence pack may carry verbatim; everything else is fetched as
//!   line-range windows around hits.

use crate::error::{Error, Result};
use crate::wiki;
use ignore::gitignore::{Gitignore, GitignoreBuilder, Glob};
use ignore::Match;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Candidate hard cap so a pathological tree cannot blow memory before the
/// real (configured) budget is reached.
const MAX_CANDIDATES: usize = 50_000;
/// Directory names never descended into (dependency/vendor/build output).
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".idea",
    ".vscode",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    ".gradle",
    ".venv",
    "venv",
    "node_modules",
    "bower_components",
    "target",
    "build",
    "dist",
    "out",
    "bin",
    "obj",
    "coverage",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    "third_party",
    "vendor",
    "Pods",
    "licenses",
    "tmp",
    "logs",
];
/// Extensions scanned as source (symbol-extracted).
const SOURCE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "vue", "svelte", "py", "go",
    "java", "kt", "c", "h", "cpp", "hpp", "cc", "cxx", "rb", "php", "swift", "scala", "cs", "zig",
];
/// Extensions scanned as documentation.
const DOC_EXTS: &[&str] = &["md", "mdx", "rst", "txt", "adoc"];
/// Extensions scanned as manifest/config (parsed for entry hints when small).
const CONFIG_EXTS: &[&str] = &["toml", "json", "jsonc", "yaml", "yml", "json5"];
/// Manifest files that mark a directory as a real project root or package.
const MANIFEST_NAMES: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "setup.py",
    "setup.cfg",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "composer.json",
    "CMakeLists.txt",
    "Package.swift",
    "mix.exs",
    "Gemfile",
];
/// Root-level documents that explain the project; read before any source.
const DOC_FIRST_NAMES: &[&str] = &[
    "readme",
    "agents",
    "dandelion",
    "contributing",
    "getting-started",
    "quickstart",
    "architecture",
    "changelog",
    "security",
];

// ------------------------------------------------------------------- types

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Category {
    /// package manifest (package.json, Cargo.toml, …)
    Manifest,
    /// readme / guide / doc tree
    Doc,
    /// executable or library entrypoint (main/index/cli/bin target)
    Entry,
    /// ordinary source file
    Source,
    /// test file or test directory
    Test,
    /// build/run configuration (compose, dockerfile, ci, settings)
    Config,
    /// anything else that was included
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    /// fn | class | const | type | mod | def …
    pub kind: String,
    /// 1-based line of the declaration.
    pub line: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Section {
    pub title: String,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceFile {
    /// workspace-relative, forward slashes.
    pub rel: String,
    /// sha256 of the included content (hex) — the staleness primitive.
    pub hash: String,
    pub bytes: u64,
    pub lines: u32,
    pub category: Category,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<Symbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<Section>,
    /// scan ranking weight; higher was included earlier.
    pub priority: u32,
    /// The file's full text fits the body tier and may enter an evidence
    /// pack whole. `false` = metadata-indexed only (bigger than the body
    /// size cap, or displaced by the per-category quota): evidence for it
    /// is fetched as line-range windows around lookup hits.
    #[serde(default)]
    pub body_budget: bool,
    /// Repo-relative files this file imports (deterministically resolved
    /// against the scan; never model-guessed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
    /// Import specifiers found but not resolved — external packages,
    /// build-time aliases, or languages we do not parse. Counted, never
    /// silently dropped.
    #[serde(default)]
    pub unresolved_imports: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChildRepo {
    /// workspace-relative directory ("." is never used here).
    pub dir: String,
    pub manifest: String,
}

/// Honest scan coverage: what was seen, taken, skipped and why.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageReport {
    pub scanned_at: String,
    pub workspace: String,
    /// relevant files found before budget (excluding ignored trees).
    pub candidate_files: u32,
    /// discovery tier: what was seen.
    pub included_files: u32,
    /// metadata tier: files the index knows (lookup/QA reach).
    #[serde(default)]
    pub meta_indexed_files: u32,
    /// body tier: files whose full text may enter an evidence pack.
    #[serde(default)]
    pub body_budget_files: u32,
    /// metadata-indexed but displaced from the body tier (size, quota or
    /// plain slot overflow) — visible via `EvidenceFile::body_budget`.
    #[serde(default)]
    pub body_over: u32,
    /// displaced from the body tier specifically by the per-category
    /// doc quota (multilingual READMEs and doc trees).
    #[serde(default)]
    pub body_quota_dropped: u32,
    /// import specifiers across the index that could not be resolved.
    #[serde(default)]
    pub unresolved_imports_total: u32,
    /// ignored directory trees that were NOT walked (name, not file count —
    /// counting inside them would defeat the purpose of skipping).
    #[serde(default)]
    pub ignored_dirs: Vec<String>,
    /// why each entry in `ignored_dirs` was skipped, in the form
    /// `"<rel> ← <rule source>:<pattern>"` — every ignore layer records
    /// its origin, so a skipped tree can name the exact rule that killed
    /// it (explainable ignores).
    #[serde(default)]
    pub ignore_origins: Vec<String>,
    /// ignore patterns that failed to compile (bad globs, unreadable
    /// ignore files) — counted and reported, never silently dropped.
    #[serde(default)]
    pub ignore_pattern_errors: Vec<String>,
    pub too_large: u32,
    #[serde(default)]
    pub too_large_examples: Vec<String>,
    /// present but unreadable (binary / invalid UTF-8 / io error).
    pub unreadable: u32,
    /// relevant files that fit none of the budgets (pure queue overflow).
    pub over_budget: u32,
    #[serde(default)]
    pub over_budget_examples: Vec<String>,
    pub budget_hit: bool,
    /// a package manifest sits at the workspace root itself.
    pub root_manifest: bool,
    /// manifests found in subdirectories (monorepo packages or, together with
    /// `root_manifest == false`, "you opened the parent of several repos").
    #[serde(default)]
    pub child_repos: Vec<ChildRepo>,
    /// single-child fallback suggestion when the root is not a project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_suggestion: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceIndex {
    pub version: u32,
    pub generated_at: String,
    pub workspace: String,
    pub files: Vec<EvidenceFile>,
    pub coverage: CoverageReport,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceHit {
    pub rel: String,
    pub start: u32,
    pub end: u32,
    pub excerpt: String,
    /// "symbol" | "section" | "path" | "content" | "related"
    pub via: String,
    pub score: u32,
}

// ------------------------------------------------------------------ paths

pub fn index_path(project_data_dir: &Path) -> PathBuf {
    wiki::wiki_dir(project_data_dir).join("index").join("evidence.json")
}

pub fn load_index(project_data_dir: &Path) -> Option<EvidenceIndex> {
    let raw = fs::read_to_string(index_path(project_data_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save_index(project_data_dir: &Path, index: &EvidenceIndex) -> Result<()> {
    let path = index_path(project_data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(index)?)?;
    Ok(())
}

/// Case-insensitive manifest detection (Cargo.toml ≠ cargo.toml by name).
fn is_manifest_name(name_lower: &str) -> bool {
    MANIFEST_NAMES.iter().any(|m| name_lower == m.to_ascii_lowercase())
}

// -------------------------------------------------------------------- scan

struct Candidate {
    path: PathBuf,
    rel: String,
    bytes: u64,
    ext: String,
    priority: u32,
}

// ----------------------------------------------------------- ignore engine
//
// Git-true gitignore semantics — a leading `/` anchors a pattern to the
// ignore file's directory, `**` spans path separators, `!` re-includes, a
// nested `.gitignore` overrides shallower layers — come from the `ignore`
// crate's matcher; Dandelion keeps the walk itself so budget, coverage
// accounting and explainability stay here. Every layer records where its
// rules came from, so each skipped tree can name the exact rule that
// killed it (`CoverageReport::ignore_origins`). Matching is
// case-insensitive, preserving the behavior of the basename matcher this
// replaces (Windows-first desktop; identical results on every platform).

/// One layer of the ignore stack. Later layers win, mirroring git's rule
/// that a deeper `.gitignore` overrides shallower ones.
struct IgnoreLayer {
    /// rule source shown in `CoverageReport::ignore_origins` — a
    /// workspace-relative ignore file, or a named built-in layer.
    origin: String,
    /// the directory whose rules these are; patterns match paths under it.
    root: PathBuf,
    gi: Gitignore,
}

impl IgnoreLayer {
    fn matched(&self, path: &Path, is_dir: bool) -> Match<&Glob> {
        if !path.starts_with(&self.root) {
            return Match::None; // this file's rules say nothing about other trees
        }
        self.gi.matched(path, is_dir)
    }
}

fn ci_builder(root: &Path) -> GitignoreBuilder {
    let mut b = GitignoreBuilder::new(root);
    let _ = b.case_insensitive(true);
    b
}

fn build_layer(
    b: GitignoreBuilder,
    origin: &str,
    root: &Path,
    rejected: &mut Vec<String>,
) -> Option<IgnoreLayer> {
    match b.build() {
        Ok(gi) if !gi.is_empty() => Some(IgnoreLayer {
            origin: origin.to_string(),
            root: root.to_path_buf(),
            gi,
        }),
        Ok(_) => None,
        Err(e) => {
            rejected.push(format!("{origin}: {e}"));
            None
        }
    }
}

/// Layer from pattern lines. Invalid globs are reported (they land in
/// `ignore_pattern_errors`), never silently dropped.
fn layer_from_lines(
    origin: &str,
    root: &Path,
    lines: &[String],
) -> (Option<IgnoreLayer>, Vec<String>) {
    let mut b = ci_builder(root);
    let mut rejected = Vec::new();
    for line in lines {
        if let Err(e) = b.add_line(None, line) {
            rejected.push(format!("{origin}: {line}: {e}"));
        }
    }
    let layer = build_layer(b, origin, root, &mut rejected);
    (layer, rejected)
}

/// Layer from a `.gitignore` file on disk (absent file → silently no layer).
fn layer_from_file(
    rel_name: &str,
    file: &Path,
    root: &Path,
) -> (Option<IgnoreLayer>, Vec<String>) {
    if !file.is_file() {
        return (None, Vec::new());
    }
    let mut b = ci_builder(root);
    let mut rejected = Vec::new();
    if let Some(e) = b.add(file) {
        rejected.push(format!("{rel_name}: {e}"));
    }
    let layer = build_layer(b, rel_name, root, &mut rejected);
    (layer, rejected)
}

/// Ignore decision for one path, git-style: the deepest layer that says
/// anything decides; a `!` whitelist cancels the shallower ignores it
/// answers (within one layer the matcher already lets a later `!` rule win).
/// `Some((origin, pattern))` = skip, and who is responsible.
fn decide(layers: &[Arc<IgnoreLayer>], path: &Path, is_dir: bool) -> Option<(String, String)> {
    let mut win: Option<(String, String)> = None;
    for l in layers {
        match l.matched(path, is_dir) {
            Match::None => {}
            Match::Ignore(g) => {
                win = Some((l.origin.clone(), g.original().to_string()));
            }
            Match::Whitelist(_) => win = None,
        }
    }
    win
}

/// Walk the workspace and collect every plausible candidate (no reads yet).
/// The ignore stack, in git precedence order (lowest first): the built-in
/// vendor/build skips, the tree's own `.gitignore` files (root first, a
/// nested file overrides the shallower layers), and finally
/// `Settings::wiki_ignore_patterns` on top — user policy wins in both
/// directions, so a `!rule` re-includes what the repo hides and an added
/// pattern outranks a repo whitelist.
fn enumerate(ws: &Path, user_patterns: &[String]) -> (Vec<Candidate>, CoverageReport) {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut coverage = CoverageReport::default();

    let mut base: Vec<Arc<IgnoreLayer>> = Vec::new();
    let builtin: Vec<String> = SKIP_DIRS.iter().map(|d| format!("{d}/")).collect();
    let (l, errs) = layer_from_lines("built-in skips", ws, &builtin);
    coverage.ignore_pattern_errors.extend(errs);
    if let Some(l) = l {
        base.push(Arc::new(l));
    }
    let (l, errs) = layer_from_file(".gitignore", &ws.join(".gitignore"), ws);
    coverage.ignore_pattern_errors.extend(errs);
    if let Some(l) = l {
        base.push(Arc::new(l));
    }
    if !user_patterns.is_empty() {
        let (l, errs) = layer_from_lines("wiki_ignore_patterns", ws, user_patterns);
        coverage.ignore_pattern_errors.extend(errs);
        if let Some(l) = l {
            base.push(Arc::new(l));
        }
    }
    coverage.ignore_pattern_errors.truncate(40);

    // Iterative DFS so a pathologically deep tree cannot overflow the
    // stack; each frame carries the ignore layers active for its children.
    let mut stack: Vec<(PathBuf, Vec<Arc<IgnoreLayer>>)> = vec![(ws.to_path_buf(), base)];
    let mut seen_manifests: BTreeSet<String> = BTreeSet::new();
    while let Some((dir, layers)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        let rel_dir = dir
            .strip_prefix(ws)
            .unwrap_or(Path::new(""))
            .to_string_lossy()
            .replace('\\', "/");
        let depth = rel_dir.split('/').filter(|s| !s.is_empty()).count();
        // A nested `.gitignore` extends the stack for this dir's children
        // (deeper file wins), recorded under the path it came from.
        let mut layers = layers;
        if dir != ws {
            let rel_name = format!("{rel_dir}/.gitignore");
            let (l, errs) = layer_from_file(&rel_name, &dir.join(".gitignore"), &dir);
            if !errs.is_empty() && coverage.ignore_pattern_errors.len() < 40 {
                coverage.ignore_pattern_errors.extend(errs);
            }
            if let Some(l) = l {
                layers.push(Arc::new(l));
            }
        }
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            let name_lower = name.to_ascii_lowercase();
            let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if name_lower.starts_with('.') && name_lower != ".github" {
                    continue;
                }
                if let Some((origin, pat)) = decide(&layers, &ent.path(), true) {
                    let shown = if rel_dir.is_empty() {
                        name.clone()
                    } else {
                        format!("{rel_dir}/{name}")
                    };
                    if coverage.ignored_dirs.len() < 40 {
                        coverage.ignore_origins.push(format!("{shown} ← {origin}:{pat}"));
                        coverage.ignored_dirs.push(shown);
                    }
                    continue;
                }
                stack.push((ent.path(), layers.clone()));
                continue;
            }
            if !relevant(&name) {
                continue;
            }
            let rel = if rel_dir.is_empty() {
                name.clone()
            } else {
                format!("{rel_dir}/{name}")
            };
            if decide(&layers, &ent.path(), false).is_some() {
                continue;
            }
            let bytes = ent.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
            if is_manifest_name(&name_lower) {
                seen_manifests.insert(if rel_dir.is_empty() {
                    rel.clone()
                } else {
                    format!("{rel_dir}/{}", name_lower)
                });
            }
            coverage.candidate_files += 1;
            if candidates.len() < MAX_CANDIDATES {
                candidates.push(Candidate {
                    path: ent.path(),
                    priority: score(&rel, depth, &name, &rel_dir),
                    bytes,
                    ext: name_lower
                        .rsplit('.')
                        .next()
                        .filter(|e| *e != name_lower)
                        .unwrap_or("")
                        .to_owned(),
                    rel,
                });
            }
        }
    }

    coverage.root_manifest = seen_manifests
        .iter()
        .any(|m| !m.contains('/') && is_manifest_name(m));
    coverage.child_repos = seen_manifests
        .iter()
        .filter(|m| m.contains('/'))
        .map(|m| ChildRepo {
            dir: m.rsplit('/').next().map(|_| trim_last(m)).unwrap_or_default(),
            manifest: m.clone(),
        })
        .collect();
    if !coverage.root_manifest {
        let dirs: BTreeSet<String> = coverage.child_repos.iter().map(|c| c.dir.clone()).collect();
        if dirs.len() == 1 {
            coverage.root_suggestion = dirs.into_iter().next();
        }
    }
    (candidates, coverage)
}

/// "a/b/package.json" -> "a/b"
fn trim_last(rel: &str) -> String {
    rel.rsplit_once('/').map(|(head, _)| head.to_owned()).unwrap_or_default()
}

fn relevant(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if is_manifest_name(&lower) {
        return true;
    }
    if matches!(
        lower.as_str(),
        "makefile" | "dockerfile" | "justfile" | "procfile" | "cmakelists.txt" | "go.mod" | "go.work"
    ) || lower.starts_with("dockerfile.")
    {
        return true;
    }
    if lower.ends_with(".sh") || lower.ends_with(".ps1") || lower.ends_with(".bat") {
        return true;
    }
    let Some(ext) = lower.rsplit('.').next() else { return false };
    if ext == lower {
        return false; // no extension and not a known name
    }
    SOURCE_EXTS.contains(&ext) || DOC_EXTS.contains(&ext) || CONFIG_EXTS.contains(&ext)
}

/// Importance ranking. Higher is included earlier. Deliberately coarse:
/// it decides what a budget cut keeps, so docs/entry/core beat tests/config.
fn score(rel: &str, depth: usize, name: &str, rel_dir: &str) -> u32 {
    let lower = rel.to_ascii_lowercase();
    let name_lower = name.to_ascii_lowercase();
    let is_manifest = is_manifest_name(&name_lower)
        || matches!(name_lower.as_str(), "go.mod" | "go.work");
    if is_manifest {
        return if rel_dir.is_empty() { 1000 } else { 780 };
    }
    // Translated duplicates (README_zh.md, docs/guide-ja.md …) must not
    // consume the body budget the canonical doc + core sources need: they
    // carry the same facts in another language. Still metadata-indexed and
    // searchable — just ranked below primary docs and near-core sources.
    {
        let ext0 = name_lower.rsplit('.').next().unwrap_or("");
        if DOC_EXTS.contains(&ext0) && is_translated_doc(&name_lower) {
            return 560usize.saturating_sub(depth).max(300) as u32;
        }
    }
    if rel_dir.is_empty() {
        let stem = name_lower.split('.').next().unwrap_or(&name_lower);
        if DOC_FIRST_NAMES.iter().any(|d| stem.starts_with(d)) {
            return 950;
        }
    }
    if is_entry_name(&lower, depth) {
        return 900;
    }
    let ext = lower.rsplit('.').next().unwrap_or("");
    let in_docs = lower.starts_with("docs/")
        || lower.contains("/docs/")
        || lower.starts_with("doc/")
        || lower.contains("/doc/");
    if in_docs && DOC_EXTS.contains(&ext) {
        return 800u32.saturating_sub(depth as u32 * 10);
    }
    let is_test = lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || lower.starts_with("test/")
        || lower.starts_with("tests/")
        || name_lower.contains(".test.")
        || name_lower.contains(".spec.")
        || stem_last(&lower).starts_with("test_");
    if SOURCE_EXTS.contains(&ext) {
        let base = if is_test { 240 } else { 600 };
        let near = if depth <= 2 { 50 } else { 0 };
        return base + near - depth as u32;
    }
    if DOC_EXTS.contains(&ext) {
        return 750 - depth as u32;
    }
    if is_test {
        return 220;
    }
    if CONFIG_EXTS.contains(&ext) {
        if lower.contains(".lock") || name_lower.contains("lock") {
            return 80;
        }
        return 300 - depth as u32;
    }
    400 - depth as u32
}

/// `README_zh.md`, `readme.ja.md`, `guide-ko.md`, `architecture_zh-tw.txt`:
/// a doc whose basename ends in a language code — a translated duplicate of
/// a canonical document. Broad on purpose (multilingual trees must not eat
/// the doc budget); false positives lose only ranking, never index presence.
fn is_translated_doc(name_lower: &str) -> bool {
    let Some((stem, _)) = name_lower.rsplit_once('.') else {
        return false;
    };
    for sep in ['_', '-', '.'] {
        if let Some((head, tail)) = stem.rsplit_once(sep) {
            if !head.is_empty() && is_lang_code(tail) {
                return true;
            }
        }
    }
    false
}

pub(crate) fn is_lang_code(s: &str) -> bool {
    matches!(
        s,
        "zh" | "zh-cn" | "zh-tw" | "zh-hans" | "zh-hant" | "ja" | "jp" | "ko" | "de" | "es"
            | "fr" | "ru" | "pt" | "it" | "nl" | "pl" | "tr" | "ar" | "uk" | "vi" | "th" | "id"
            | "cs" | "sv" | "da" | "fi" | "no" | "hu" | "ro" | "el" | "he" | "hi"
    )
}

fn stem_last(lower: &str) -> String {
    lower
        .rsplit('/')
        .next()
        .unwrap_or(lower)
        .rsplit('.')
        .next_back()
        .unwrap_or("")
        .to_owned()
}

fn is_entry_name(lower: &str, depth: usize) -> bool {
    if depth > 3 {
        return false;
    }
    let leaf = lower.rsplit('/').next().unwrap_or(lower);
    let head = leaf.split('.').next().unwrap_or(leaf);
    matches!(
        head,
        "main" | "index" | "cli" | "mod" | "app" | "server" | "lib" | "start" | "run" | "server_main"
    )
}

/// Cheap entry hints from the root manifest, if one is present and readable.
fn entry_hints(ws: &Path) -> Vec<String> {
    let mut hints = Vec::new();
    if let Ok(raw) = fs::read_to_string(ws.join("package.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            let mut push = |s: Option<&str>| {
                if let Some(s) = s {
                    hints.push(s.trim_start_matches("./").replace('\\', "/"));
                }
            };
            push(v.get("main").and_then(|m| m.as_str()));
            push(v.get("module").and_then(|m| m.as_str()));
            if let Some(bins) = v.get("bin").and_then(|b| b.as_object()) {
                for path in bins.values().filter_map(|b| b.as_str()) {
                    push(Some(path));
                }
            }
        }
    }
    hints
}

// ------------------------------------------------------------------ build

/// The separate tier budgets (never one mixed cap): discovery is a hard
/// constant inside `enumerate`; metadata and body are configured here, so a
/// large doc tree overflowing the body tier cannot hide files from lookup.
#[derive(Clone, Debug)]
pub struct IndexBudget {
    /// metadata tier: max files to hash/parse (symbols, sections, imports).
    pub meta_files: usize,
    /// metadata tier: per-file read cap for parsing.
    pub meta_file_bytes: usize,
    /// body tier: max files whose full text may enter an evidence pack.
    pub body_files: usize,
    /// body tier: per-file size to qualify for the body tier.
    pub body_file_bytes: usize,
    /// `Some(pct)`: docs get `body_files * pct / 100` body slots, tests a
    /// quarter — multilingual README/doc trees cannot starve core sources.
    /// `None`: no per-category quota (legacy behaviour).
    pub doc_budget_pct: Option<u32>,
    /// Extra gitignore-syntax patterns (`Settings::wiki_ignore_patterns`)
    /// applied on top of the tree's own `.gitignore` layers — `!` rules
    /// re-include, added rules outrank repo whitelists.
    pub ignore_patterns: Vec<String>,
}

impl IndexBudget {
    /// Legacy single-budget shape: metadata and body share one file/byte cap
    /// and no quota — byte-for-byte the pre-tiering behaviour.
    pub fn legacy(max_files: usize, max_file_bytes: usize) -> Self {
        IndexBudget {
            meta_files: max_files,
            meta_file_bytes: max_file_bytes,
            body_files: max_files,
            body_file_bytes: max_file_bytes,
            doc_budget_pct: None,
            ignore_patterns: Vec::new(),
        }
    }

    /// The production tiered shape, mapped once from user Settings so every
    /// caller (deep build, explicit compile, background scheduler, tool)
    /// indexes under identical budgets.
    pub fn from_settings(s: &crate::config::Settings) -> Self {
        Self {
            ignore_patterns: s.wiki_ignore_patterns.clone(),
            ..Self::tiered(
                s.wiki_max_index_files as usize,
                s.wiki_meta_max_file_bytes,
                s.wiki_max_scan_files as usize,
                s.wiki_max_file_bytes,
                s.wiki_build_doc_budget_pct,
            )
        }
    }

    /// Tiered budgets from Settings (deep-build path).
    pub fn tiered(
        meta_files: usize,
        meta_file_bytes: usize,
        body_files: usize,
        body_file_bytes: usize,
        doc_budget_pct: u32,
    ) -> Self {
        IndexBudget {
            meta_files,
            meta_file_bytes,
            body_files,
            body_file_bytes,
            doc_budget_pct: Some(doc_budget_pct),
            ignore_patterns: Vec::new(),
        }
    }
}

/// Scan `ws` under the legacy single budget (see `IndexBudget::legacy`).
/// Deterministic: same tree ⇒ same index (bar clock).
pub fn build_index(
    project_data_dir: &Path,
    ws: &Path,
    max_files: usize,
    max_file_bytes: usize,
) -> Result<EvidenceIndex> {
    build_index_with_budget(project_data_dir, ws, &IndexBudget::legacy(max_files, max_file_bytes))
}

/// Scan `ws` under tiered budgets and (re)build the evidence index.
///
/// Pass 1 (metadata) parses every candidate that fits the metadata caps in
/// priority order — including files too big for any evidence pack, whose
/// symbols/sections/imports still make lookup and windowed fetch work.
/// Pass 2 (body) marks which indexed files may be carried whole into a pack
/// (per-category quota); the rest are fetched as line-range windows around
/// hits. Every tier's overflow is counted separately in the report.
pub fn build_index_with_budget(
    project_data_dir: &Path,
    ws: &Path,
    budget: &IndexBudget,
) -> Result<EvidenceIndex> {
    let ws = dunce::canonicalize(ws).unwrap_or_else(|_| ws.to_path_buf());
    if !ws.is_dir() {
        return Err(Error::Invalid(format!(
            "wiki index: workspace missing: {}",
            ws.display()
        )));
    }
    let (mut candidates, mut coverage) = enumerate(&ws, &budget.ignore_patterns);
    let hints = entry_hints(&ws);
    for cand in candidates.iter_mut() {
        let rel = cand.rel.to_ascii_lowercase();
        if hints.iter().any(|h| !h.is_empty() && (rel == *h || rel.ends_with(&format!("/{h}")))) {
            cand.priority = cand.priority.max(920);
        }
        // entry docs referenced from the manifest dir still win over deep src.
    }
    candidates.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.rel.cmp(&b.rel))
    });

    // ---- pass 1: metadata (hash / symbols+routes / sections / import specs)
    let mut files: Vec<EvidenceFile> = Vec::new();
    let mut pending_specs: Vec<Vec<String>> = Vec::new();
    let mut over_budget = 0u32;
    for cand in &candidates {
        if files.len() >= budget.meta_files {
            over_budget += 1;
            if coverage.over_budget_examples.len() < 20 {
                coverage.over_budget_examples.push(cand.rel.clone());
            }
            continue;
        }
        if cand.bytes as usize > budget.meta_file_bytes {
            coverage.too_large += 1;
            if coverage.too_large_examples.len() < 20 {
                coverage.too_large_examples.push(cand.rel.clone());
            }
            continue;
        }
        let Ok(text) = fs::read_to_string(&cand.path) else {
            coverage.unreadable += 1;
            continue;
        };
        let lines = text.lines().count() as u32;
        let category = categorize(&cand.rel, &cand.ext);
        let symbols = extract_symbols(&cand.rel, &cand.ext, &text);
        let sections = if DOC_EXTS.contains(&cand.ext.as_str()) {
            extract_sections(&text)
        } else {
            Vec::new()
        };
        let specs = import_specs(&cand.ext, &text);
        files.push(EvidenceFile {
            rel: cand.rel.clone(),
            hash: format!("{:x}", Sha256::digest(text.as_bytes())),
            bytes: cand.bytes,
            lines,
            category,
            symbols,
            sections,
            priority: cand.priority,
            body_budget: false,
            imports: Vec::new(),
            unresolved_imports: 0,
        });
        pending_specs.push(specs);
    }

    // ---- pass 2: body tier slots, priority order, per-category quota
    let mut body_used = 0usize;
    let mut doc_used = 0usize;
    let mut test_used = 0usize;
    let (doc_cap, test_cap) = match budget.doc_budget_pct {
        Some(pct) => (
            budget.body_files * pct as usize / 100,
            budget.body_files / 4,
        ),
        None => (usize::MAX, usize::MAX),
    };
    let mut body_over = 0u32;
    let mut body_quota_dropped = 0u32;
    for f in files.iter_mut() {
        let quota_hit = match f.category {
            Category::Doc => doc_used >= doc_cap,
            Category::Test => test_used >= test_cap,
            _ => false,
        };
        if quota_hit {
            body_over += 1;
            body_quota_dropped += 1;
            continue;
        }
        if body_used >= budget.body_files || f.bytes as usize > budget.body_file_bytes {
            body_over += 1;
            continue;
        }
        f.body_budget = true;
        body_used += 1;
        match f.category {
            Category::Doc => doc_used += 1,
            Category::Test => test_used += 1,
            _ => {}
        }
    }

    // ---- relations: resolve collected import specs against the whole tree
    // (edges may legitimately point at metadata-skipped files; never invent
    // targets that do not exist in the scan).
    let all_rels: BTreeSet<String> = candidates.iter().map(|c| c.rel.clone()).collect();
    let mut unresolved_imports_total = 0u32;
    for (f, specs) in files.iter_mut().zip(pending_specs.into_iter()) {
        let (edges, unresolved) = resolve_import_specs(&f.rel, &specs, &all_rels);
        f.imports = edges;
        f.unresolved_imports = unresolved;
        unresolved_imports_total += unresolved;
    }

    coverage.workspace = ws.to_string_lossy().into_owned();
    coverage.scanned_at = now_rfc3339();
    coverage.candidate_files = coverage.candidate_files.min(u32::MAX as u32);
    coverage.included_files = files.len() as u32;
    coverage.meta_indexed_files = files.len() as u32;
    coverage.body_budget_files = files.iter().filter(|f| f.body_budget).count() as u32;
    coverage.body_over = body_over;
    coverage.body_quota_dropped = body_quota_dropped;
    coverage.unresolved_imports_total = unresolved_imports_total;
    coverage.over_budget = over_budget;
    coverage.budget_hit = over_budget > 0 || coverage.too_large > 0 || body_over > 0;
    // child_repos can be huge in monorepos; keep a bounded, sorted summary.
    if coverage.child_repos.len() > 60 {
        coverage.child_repos.truncate(60);
    }

    let index = EvidenceIndex {
        version: 1,
        generated_at: now_rfc3339(),
        workspace: coverage.workspace.clone(),
        files,
        coverage,
    };
    save_index(project_data_dir, &index)?;
    Ok(index)
}

fn categorize(rel: &str, ext: &str) -> Category {
    let lower = rel.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if is_manifest_name(name) || matches!(name, "go.mod" | "go.work" | "dockerfile")
    {
        return Category::Manifest;
    }
    if DOC_EXTS.contains(&ext) {
        return Category::Doc;
    }
    let is_test = lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.starts_with("test_");
    if is_test {
        return Category::Test;
    }
    if SOURCE_EXTS.contains(&ext) {
        let stem = name.split('.').next().unwrap_or(name);
        if matches!(stem, "main" | "index" | "cli" | "app" | "server" | "start" | "run") {
            return Category::Entry;
        }
        return Category::Source;
    }
    if matches!(
        name,
        "makefile" | "justfile" | "procfile"
    ) || lower.contains("/.github/")
        || lower.contains("compose")
    {
        return Category::Config;
    }
    if CONFIG_EXTS.contains(&ext) {
        return Category::Config;
    }
    Category::Other
}

const MAX_SYMBOLS: usize = 200;

/// Column-0 declaration scanners per language. Line = 1-based declaration line.
pub fn extract_symbols(rel: &str, ext: &str, text: &str) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    let mut push = |name: String, kind: &str, line: usize| {
        if out.len() >= MAX_SYMBOLS || name.is_empty() || name.len() > 64 {
            return;
        }
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '-'))
        {
            return;
        }
        out.push(Symbol { name, kind: kind.to_owned(), line: line as u32 });
    };
    match ext {
        "rs" => {
            for (i, line) in text.lines().enumerate() {
                let t = line.trim_start();
                if let Some(rest) = t.strip_prefix("pub fn ") {
                    push(ident(rest), "fn", i + 1);
                } else if let Some(rest) = t.strip_prefix("pub struct ") {
                    push(ident(rest), "class", i + 1);
                } else if let Some(rest) = t.strip_prefix("pub enum ") {
                    push(ident(rest), "type", i + 1);
                } else if let Some(rest) = t.strip_prefix("pub trait ") {
                    push(ident(rest), "type", i + 1);
                } else if line.starts_with("mod ") || line.starts_with("pub mod ") {
                    let rest = t.trim_start_matches("pub ").trim_start_matches("mod ");
                    push(ident(rest), "mod", i + 1);
                }
            }
        }
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" | "vue" | "svelte" => {
            for (i, line) in text.lines().enumerate() {
                // column-0 only: this is an outline, not a parser.
                let t = line;
                let Some(rest) = t.strip_prefix("export ") else { continue };
                let rest = rest
                    .strip_prefix("default ")
                    .unwrap_or(rest);
                for (kw, kind) in
                    [("function ", "fn"), ("class ", "class"), ("const ", "const"), ("let ", "const"), ("interface ", "type"), ("type ", "type"), ("enum ", "type")]
                {
                    if let Some(r) = rest.strip_prefix(kw) {
                        push(ident(r), kind, i + 1);
                    }
                }
                // module-level function/class without export
                if let Some(r) = t.strip_prefix("function ") {
                    push(ident(r), "fn", i + 1);
                } else if let Some(r) = t.strip_prefix("class ") {
                    push(ident(r), "class", i + 1);
                }
            }
        }
        "py" => {
            for (i, line) in text.lines().enumerate() {
                if let Some(r) = line.strip_prefix("def ") {
                    push(ident(r.trim_start_matches('_')), "fn", i + 1);
                } else if let Some(r) = line.strip_prefix("class ") {
                    push(ident(r), "class", i + 1);
                }
            }
        }
        "go" => {
            for (i, line) in text.lines().enumerate() {
                if let Some(rest) = line.strip_prefix("func ") {
                    let name = if rest.starts_with('(') {
                        rest.rsplit_once(')').map(|(_, r)| ident(r)).unwrap_or_default()
                    } else {
                        ident(rest)
                    };
                    push(name, "fn", i + 1);
                } else if let Some(rest) = line.strip_prefix("type ") {
                    push(ident(rest), "type", i + 1);
                }
            }
        }
        _ => {}
    }
    // routes ride the symbol table (pushed directly: names like "GET /x"
    // would fail the identifier charset guard above, by design).
    out.extend(extract_routes(ext, text));
    let _ = rel;
    out
}

fn ident(rest: &str) -> String {
    rest.trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '$'))
        .collect()
}

/// Markdown heading skeleton with inclusive 1-based line ranges.
pub fn extract_sections(text: &str) -> Vec<Section> {
    let mut marks: Vec<(String, u32)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim_start();
        if !t.starts_with('#') {
            continue;
        }
        let hashes = t.bytes().take_while(|b| *b == b'#').count();
        if !(1..=6).contains(&hashes) {
            continue;
        }
        let title = t[hashes..].trim().to_owned();
        if title.is_empty() {
            continue;
        }
        if marks.len() >= 300 {
            break;
        }
        marks.push((title, i as u32 + 1));
    }
    let total = text.lines().count() as u32;
    let mut out = Vec::new();
    for (idx, (title, start)) in marks.iter().enumerate() {
        let end = marks.get(idx + 1).map(|(_, l)| l - 1).unwrap_or(total);
        out.push(Section { title: title.clone(), start: *start, end: end.max(*start) });
    }
    out
}

// --------------------------------------------------------------- relations

const MAX_IMPORT_SPECS: usize = 180;
const MAX_IMPORT_EDGES: usize = 60;
const MAX_ROUTES: usize = 60;

/// Import specifiers found in a file, each tagged with a language marker so
/// resolution knows how to interpret it. Specs we can never resolve on
/// purpose (Java/C#/… package names) are tagged `xlang:` and land in the
/// explicit unresolved count — a recorded gap, not a silent drop.
fn import_specs(ext: &str, text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && s.len() <= 300 && !s.contains('\n') && out.len() < MAX_IMPORT_SPECS {
            out.push(s);
        }
    };
    match ext {
        "rs" => {
            for line in text.lines() {
                let t = line.trim_start();
                let t = t.strip_prefix("pub ").unwrap_or(t);
                if let Some(r) = t.strip_prefix("mod ") {
                    if !r.trim_start().starts_with('{') {
                        let n = ident(r);
                        if !n.is_empty() && n != "tests" {
                            push(format!("mod:{n}"));
                        }
                    }
                } else if let Some(r) = t.strip_prefix("use ") {
                    let p = r.split(';').next().unwrap_or("").trim();
                    let p = p.split(" as ").next().unwrap_or(p);
                    let p = p.split("::{").next().unwrap_or(p);
                    for (marker, prefix) in
                        [("crate:", "crate::"), ("self:", "self::"), ("super:", "super::")]
                    {
                        if let Some(rest) = p.strip_prefix(prefix) {
                            let mut segs: Vec<&str> =
                                rest.split("::").filter(|s| !s.is_empty()).collect();
                            // a::b::Type — the type itself is not a file; the
                            // module path is what we can resolve.
                            if segs.len() > 1
                                && segs
                                    .last()
                                    .map(|s| s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false))
                                    .unwrap_or(false)
                            {
                                segs.pop();
                            }
                            if !segs.is_empty() {
                                push(format!("{marker}{}", segs.join("/")));
                            }
                        }
                    }
                }
            }
        }
        "go" => {
            let mut in_block = false;
            for line in text.lines() {
                let t = line.trim();
                if in_block {
                    if t.starts_with(')') {
                        in_block = false;
                        continue;
                    }
                    if let Some(s) = between_quotes(t) {
                        push(format!("go:{s}"));
                    }
                } else if let Some(r) = t.strip_prefix("import") {
                    if r.trim_start().starts_with('(') {
                        in_block = true;
                    } else if let Some(s) = between_quotes(r) {
                        push(format!("go:{s}"));
                    }
                }
            }
        }
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" | "vue" | "svelte" => {
            for line in text.lines() {
                let mut from = 0usize;
                while from < line.len() {
                    let rest = &line[from..];
                    let (marker, skip) = ["from ", "from \"", "from '", "require(", "import("]
                        .iter()
                        .find_map(|m| rest.find(m).map(|i| (*m, i + m.len())))
                        .unwrap_or(("", usize::MAX));
                    if marker.is_empty() {
                        break;
                    }
                    if let Some(s) = between_quotes(&rest[skip.min(rest.len())..]) {
                        push(format!("js:{s}"));
                    }
                    from += skip + 1;
                }
            }
        }
        "py" => {
            for line in text.lines() {
                let t = line.trim();
                if let Some(r) = t.strip_prefix("from ") {
                    let path = r.split(" import ").next().unwrap_or(r).trim();
                    let dots = path.chars().take_while(|c| *c == '.').count();
                    let rest = &path[dots..].replace('.', "/");
                    push(format!("py:{}{}", ".".repeat(dots), rest));
                } else if let Some(r) = t.strip_prefix("import ") {
                    for part in r.split(',') {
                        let p = part.trim().split(" as ").next().unwrap_or("").trim();
                        if !p.is_empty() {
                            push(format!("py:{}", p.replace('.', "/")));
                        }
                    }
                }
            }
        }
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" => {
            for line in text.lines() {
                let t = line.trim_start();
                if let Some(r) = t.strip_prefix("#include") {
                    if let Some(s) = between_quotes(r) {
                        push(format!("rel:{s}"));
                    }
                }
            }
        }
        "java" | "kt" | "kts" | "scala" | "cs" => {
            for line in text.lines() {
                let t = line.trim_start();
                for kw in ["import ", "using "] {
                    if let Some(r) = t.strip_prefix(kw) {
                        let p = r.trim().trim_end_matches(';');
                        if !p.is_empty() {
                            push(format!("xlang:{}", p.replace('.', "/")));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn between_quotes(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|b| *b == b'"' || *b == b'\'' || *b == b'`')?;
    let q = bytes[start];
    let rest = &s[start + 1..];
    let end = rest.find(q as char)?;
    let inner = &rest[..end];
    if inner.is_empty() || inner.len() > 256 {
        return None;
    }
    Some(inner.to_owned())
}

/// Deterministically resolve tagged specs to real repo-relative files.
/// Only files present in `all` (every discovered candidate rel) become edges;
/// anything else is counted, not guessed. Returns (edges, unresolved count).
fn resolve_import_specs(rel: &str, specs: &[String], all: &BTreeSet<String>) -> (Vec<String>, u32) {
    let dir = rel.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let stem = rel
        .rsplit('/')
        .next()
        .and_then(|n| n.rsplit_once('.'))
        .map(|(s, _)| s.to_string())
        .unwrap_or_default();
    let mut edges: BTreeSet<String> = BTreeSet::new();
    let mut unresolved = 0u32;
    for spec in specs {
        let cands: Vec<String> = if let Some(s) = spec.strip_prefix("mod:") {
            rust_mod_candidates(&dir, &stem, s, all)
        } else if let Some(s) = spec.strip_prefix("self:") {
            rust_mod_candidates(&dir, &stem, s, all)
        } else if let Some(s) = spec.strip_prefix("super:") {
            let parent = trim_last(&dir);
            rust_file_candidates(&parent, s, all)
        } else if let Some(s) = spec.strip_prefix("crate:") {
            let mut c = vec![format!("src/{s}.rs"), format!("src/{s}/mod.rs")];
            if s == "lib" || s == "main" {
                c.push(format!("{s}.rs"));
            }
            c
        } else if let Some(s) = spec.strip_prefix("go:") {
            go_candidates(s, all)
        } else if let Some(s) = spec.strip_prefix("js:") {
            js_candidates(&dir, s, all)
        } else if let Some(s) = spec.strip_prefix("py:") {
            py_candidates(&dir, s, all)
        } else if let Some(s) = spec.strip_prefix("rel:") {
            vec![
                join_rel(&dir, s),
                format!("include/{s}"),
                format!("src/{s}"),
                s.to_string(),
            ]
        } else {
            unresolved += 1;
            continue;
        };
        let mut found = 0usize;
        for c in cands {
            if all.contains(&c) && c != rel {
                if edges.len() < MAX_IMPORT_EDGES {
                    edges.insert(c);
                    found += 1;
                }
            }
        }
        if found == 0 {
            unresolved += 1;
        }
    }
    (edges.into_iter().collect(), unresolved)
}

fn rust_file_candidates(dir: &str, name: &str, _all: &BTreeSet<String>) -> Vec<String> {
    vec![join_rel(dir, &format!("{name}.rs")), join_rel(dir, &format!("{name}/mod.rs"))]
}

fn rust_mod_candidates(
    dir: &str,
    stem: &str,
    name: &str,
    all: &BTreeSet<String>,
) -> Vec<String> {
    let mut c = rust_file_candidates(dir, name, all);
    // `mod x;` inside src/foo.rs (not mod.rs) means src/foo/x.rs.
    if !stem.is_empty() && stem != "mod" {
        let sub = join_rel(dir, &format!("{stem}/{name}.rs"));
        c.push(sub);
    }
    c
}

fn go_candidates(spec: &str, all: &BTreeSet<String>) -> Vec<String> {
    let last = spec.rsplit('/').next().unwrap_or(spec);
    let mut out = Vec::new();
    for r in all.iter() {
        let d = trim_last(r);
        if d == spec
            || d.ends_with(&format!("/{spec}"))
            || d.ends_with(&format!("/{last}"))
            || d == last
        {
            out.push(r.clone());
            if out.len() >= 8 {
                break;
            }
        }
    }
    out
}

const JS_EXTS: &[&str] = &[
    ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".vue", ".svelte", ".json",
];

fn js_candidates(dir: &str, spec: &str, _all: &BTreeSet<String>) -> Vec<String> {
    let base = if spec.starts_with("./") || spec.starts_with("../") {
        join_rel(dir, spec)
    } else if let Some(rest) = spec.strip_prefix("@/").or(spec.strip_prefix("~/")) {
        format!("src/{rest}")
    } else {
        return Vec::new(); // bare package specifier — external
    };
    let mut c = vec![base.clone()];
    for e in JS_EXTS {
        c.push(format!("{base}{e}"));
    }
    for e in JS_EXTS {
        c.push(format!("{base}/index{e}"));
    }
    c
}

fn py_candidates(dir: &str, spec: &str, _all: &BTreeSet<String>) -> Vec<String> {
    let dots = spec.chars().take_while(|c| *c == '.').count();
    let rest = &spec[dots.min(spec.len())..].trim_matches('/');
    let base = if dots == 0 {
        String::new()
    } else {
        let mut d = dir.to_string();
        for _ in 1..dots {
            d = trim_last(&d);
        }
        d
    };
    if rest.is_empty() {
        return vec![join_rel(&base, "__init__.py")];
    }
    let mut c = vec![
        join_rel(&base, &format!("{rest}.py")),
        join_rel(&base, &format!("{rest}/__init__.py")),
    ];
    if dots == 0 && !base.is_empty() {
        c.push(format!("{base}.py"));
        c.push(format!("{base}/__init__.py"));
    }
    c
}

/// Join a relative import path onto a directory, resolving `.` / `..`
/// manually (workspace-relative, never escapes the tree).
fn join_rel(dir: &str, spec: &str) -> String {
    let mut segs: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for s in spec.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            other => segs.push(other),
        }
    }
    segs.join("/")
}

/// HTTP routes as pseudo-symbols (`GET /api/x`), scanned column-agnostically
/// from the handful of unambiguous framework shapes we trust. These give
/// "which module does a request pass through" questions a real anchor.
fn extract_routes(ext: &str, text: &str) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    let mut route = |method: &str, path: String, line: usize| {
        if out.len() >= MAX_ROUTES || path.is_empty() || path.len() > 120 {
            return;
        }
        let name = format!("{method} {path}");
        out.push(Symbol { name, kind: "route".into(), line: line as u32 });
    };
    match ext {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            for (i, line) in text.lines().enumerate() {
                let lower = line.to_lowercase();
                for (method, pat) in [
                    ("GET", ".get("),
                    ("POST", ".post("),
                    ("PUT", ".put("),
                    ("DELETE", ".delete("),
                    ("PATCH", ".patch("),
                    ("ALL", ".all("),
                ] {
                    let Some(idx) = lower.find(pat) else { continue };
                    // receiver must plausibly be a router/app object
                    let recv = lower
                        .get(..idx)
                        .unwrap_or("")
                        .rsplit(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '.'))
                        .next_back()
                        .unwrap_or("");
                    let recv_leaf = recv.rsplit('.').next().unwrap_or(recv);
                    if !["app", "router", "route", "routes", "server", "api"]
                        .iter()
                        .any(|k| recv_leaf.contains(k))
                    {
                        continue;
                    }
                    if let Some(after) = line.get(idx + pat.len()..) {
                        if let Some(p) = between_quotes(after) {
                            if p.starts_with('/') {
                                route(method, p, i + 1);
                            }
                        }
                    }
                }
            }
        }
        "py" => {
            for (i, line) in text.lines().enumerate() {
                let t = line.trim_start();
                let Some(r) = t.strip_prefix('@') else { continue };
                let lower = r.to_lowercase();
                let method = if lower.contains(".get(") {
                    "GET"
                } else if lower.contains(".post(") {
                    "POST"
                } else if lower.contains(".put(") {
                    "PUT"
                } else if lower.contains(".delete(") {
                    "DELETE"
                } else if lower.contains(".route(") || lower.contains(".api_route(") {
                    "ROUTE"
                } else {
                    continue;
                };
                if let Some(p) = between_quotes(r) {
                    if p.starts_with('/') {
                        route(method, p, i + 1);
                    }
                }
            }
        }
        _ => {}
    }
    out
}

// ------------------------------------------------------------------ lookup

#[derive(Clone, Debug)]
pub struct Query {
    pub query: String,
    pub max_hits: usize,
}

/// Keyword + path + symbol + section search over the evidence index, with a
/// bounded content pass over selected files so original-file excerpts are
/// available for verification. Content hits are LINE-RANGE WINDOWS around
/// clustered matches (an implementation question needs the code, not one
/// declaration line), and files that import a hit are pulled in as bounded
/// `related` hits. Files are read live from the workspace; a changed or
/// deleted file degrades to metadata-only hits instead of failing.
pub fn lookup(ws_root: &Path, index: &EvidenceIndex, question: &str, max_hits: usize) -> Vec<EvidenceHit> {
    let terms = wiki::search_terms(question);
    if terms.is_empty() {
        return Vec::new();
    }
    // Prose files (docs, manifests, config) get the developer-domain alias
    // expansion — a Chinese question reaches `npm run test:local` in
    // CONTRIBUTING.md only through 测试→test. Code content and symbols stay
    // un-aliased: in code every `fn run`/`const run` reads as an alias hit
    // and drowns the real matches (observed on the real REA corpus).
    let mut aliased = terms.clone();
    wiki::expand_aliases(&mut aliased);
    let mut hits: Vec<EvidenceHit> = Vec::new();
    let mut hit_rels: BTreeSet<String> = BTreeSet::new();
    for f in &index.files {
        let rel_lower = f.rel.to_lowercase();
        let mut path_score = 0u32;
        for term in &terms {
            if rel_lower.contains(term.as_str()) {
                path_score += 4;
            }
        }
        if path_score > 0 {
            hit_rels.insert(f.rel.clone());
            hits.push(EvidenceHit {
                rel: f.rel.clone(),
                start: 1,
                end: 1,
                excerpt: f.rel.clone(),
                via: "path".into(),
                score: path_score,
            });
        }
        for (si, sym) in f.symbols.iter().enumerate() {
            let name = sym.name.to_lowercase();
            let mut s = 0u32;
            for term in &terms {
                if name == *term {
                    s += 8;
                } else if name.contains(term.as_str()) {
                    s += 5;
                }
            }
            if s > 0 {
                // span the declaration up to the next one — the symbol hit
                // is a real code range, not a single line.
                let end = f
                    .symbols
                    .get(si + 1)
                    .map(|n| n.line.saturating_sub(1).max(sym.line))
                    .unwrap_or(f.lines.max(sym.line))
                    .min(sym.line + 400);
                hit_rels.insert(f.rel.clone());
                hits.push(EvidenceHit {
                    rel: f.rel.clone(),
                    start: sym.line,
                    end,
                    excerpt: format!("{} {} ({}:{})", sym.kind, sym.name, f.rel, sym.line),
                    via: "symbol".into(),
                    score: s,
                });
            }
        }
        let prose = matches!(f.category, Category::Doc | Category::Manifest | Category::Config);
        for sec in &f.sections {
            let title = sec.title.to_lowercase();
            let mut s = 0u32;
            for term in if prose { &aliased } else { &terms } {
                if title.contains(term.as_str()) {
                    s += 3;
                }
            }
            if s > 0 {
                hit_rels.insert(f.rel.clone());
                hits.push(EvidenceHit {
                    rel: f.rel.clone(),
                    start: sec.start,
                    end: sec.end,
                    excerpt: format!("{} ({}:{})", sec.title, f.rel, sec.start),
                    via: "section".into(),
                    score: s,
                });
            }
        }
    }

    // Bounded content pass: original-file lines beat wiki prose for
    // verification, so it runs on every query. Selection is prioritised —
    // files already hit by metadata first (their windows are the payload),
    // then entrypoints/manifests, then the rest — under a hard file cap so a
    // metadata tier of thousands of files never turns a query into a sweep.
    let mut order: Vec<&EvidenceFile> = Vec::new();
    let mut done: BTreeSet<&str> = BTreeSet::new();
    for f in &index.files {
        if hit_rels.contains(&f.rel) && done.insert(f.rel.as_str()) {
            order.push(f);
        }
    }
    for f in &index.files {
        if matches!(f.category, Category::Entry | Category::Manifest) && done.insert(f.rel.as_str()) {
            order.push(f);
        }
    }
    for f in &index.files {
        if done.insert(f.rel.as_str()) {
            order.push(f);
        }
    }
    for f in order.into_iter().take(400) {
        let Ok(text) = fs::read_to_string(ws_root.join(&f.rel)) else { continue };
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len() as u32;
        // Prose files match against the alias-expanded terms; code files
        // only against the question's own terms (see note above `hits`).
        let prose = matches!(f.category, Category::Doc | Category::Manifest | Category::Config);
        let line_terms: &[String] = if prose { &aliased } else { &terms };
        let mut scored: Vec<(u32, u32)> = Vec::new(); // (line, score)
        for (i, line) in lines.iter().enumerate() {
            let lower = line.to_lowercase();
            let mut s = line_terms.iter().filter(|t| lower.contains(t.as_str())).count() as u32;
            if s > 0 && !prose && line_terms.iter().any(|t| lower.contains(&format!("{t}("))) {
                s += 2; // callsite/definition shape: `term(` outranks a mention
            }
            if s > 0 {
                scored.push((i as u32 + 1, s));
            }
        }
        if scored.is_empty() {
            continue;
        }
        // Cluster nearby matches (≤8 lines apart) into windows, take the two
        // strongest clusters, pad ±5 and cap each window at 40 lines.
        scored.sort_by_key(|(l, _)| *l);
        let mut clusters: Vec<(u32, u32, u32)> = Vec::new(); // (first, last, score)
        for (line, s) in scored {
            match clusters.last_mut() {
                Some((_f0, l1, sc)) if line <= *l1 + 8 => {
                    *l1 = line;
                    *sc += s;
                }
                _ => clusters.push((line, line, s)),
            }
        }
        clusters.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        for (first, last, cscore) in clusters.into_iter().take(2) {
            // content hits are the dominant class for "who uses X": they
            // feed the one-hop importer expansion below just like symbols.
            hit_rels.insert(f.rel.clone());
            let mut start = first.saturating_sub(5).max(1);
            let mut end = (last + 5).min(total).max(first);
            if end - start + 1 > 40 {
                let mid = (first + last) / 2;
                start = mid.saturating_sub(19).max(1);
                end = (start + 39).min(total).max(first);
            }
            let mut excerpt = String::new();
            let mut used = 0usize;
            for l in lines[start as usize - 1..end as usize].iter() {
                let t = l.trim_end();
                let n = t.chars().count().min(400);
                if used + n > 1200 {
                    excerpt.push_str("…");
                    break;
                }
                if !excerpt.is_empty() {
                    excerpt.push('\n');
                }
                excerpt.push_str(&t.chars().take(400).collect::<String>());
                used += n + 1;
            }
            hits.push(EvidenceHit {
                rel: f.rel.clone(),
                start,
                end,
                excerpt,
                via: "content".into(),
                score: cscore,
            });
        }
    }

    // Relation expansion (one hop, read-only over the recorded import
    // edges): who imports a hit file is the natural next place to look for
    // "where is it used / what does it feed". Bounded and low-scored — it
    // suggests, never outranks a real match.
    if !hit_rels.is_empty() {
        let mut importers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for f in &index.files {
            for imp in &f.imports {
                if hit_rels.contains(imp.as_str()) {
                    importers.entry(imp.as_str()).or_default().push(f.rel.as_str());
                }
            }
        }
        let rel_cap = (max_hits / 3).max(2);
        let mut related: Vec<EvidenceHit> = Vec::new();
        let mut seen_rel: BTreeSet<(String, String)> = BTreeSet::new();
        for h in hits.iter() {
            if related.len() >= rel_cap {
                break;
            }
            let Some(importers) = importers.get(h.rel.as_str()) else { continue };
            for imp in importers.iter().take(5) {
                if related.len() >= rel_cap {
                    break;
                }
                if !seen_rel.insert(((*imp).to_string(), h.rel.clone())) {
                    continue;
                }
                related.push(EvidenceHit {
                    rel: (*imp).to_string(),
                    start: 1,
                    end: 1,
                    excerpt: format!(
                        "{} imports {}",
                        imp.rsplit('/').next().unwrap_or(imp),
                        h.rel
                    ),
                    via: "related".into(),
                    score: 5,
                });
            }
        }
        hits.extend(related);
    }

    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.rel.cmp(&b.rel)).then_with(|| a.start.cmp(&b.start)));
    let mut seen: BTreeSet<(String, u32)> = BTreeSet::new();
    hits.retain(|h| seen.insert((h.rel.clone(), h.start)));
    hits.truncate(max_hits);
    hits
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| format!("{secs}"))
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(ws: &Path, rel: &str, body: &str) {
        let p = ws.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn priority_beats_walk_order_under_a_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "package.json", "{\"name\":\"x\",\"main\":\"src/entry.ts\"}");
        touch(&ws, "README.md", "# X\n");
        touch(&ws, "docs/guide.md", "# Guide\n");
        touch(&ws, "src/entry.ts", "export function start() {}\n");
        for i in 0..50 {
            touch(&ws, &format!("src/deep/mod{i}/thing.ts"), "export const a = 1;\n");
        }
        touch(&ws, "src/deep/mod0/thing.test.ts", "export const t = 1;\n");
        let idx = build_index(&tmp.path().join("data"), &ws, 5, 65_536).unwrap();
        let rels: Vec<&str> = idx.files.iter().map(|f| f.rel.as_str()).collect();
        // budget of 5 must keep the manifest, readme, doc and entry first.
        assert!(rels.contains(&"package.json"), "{rels:?}");
        assert!(rels.contains(&"README.md"), "{rels:?}");
        assert!(idx.coverage.budget_hit);
        assert!(idx.coverage.over_budget > 0);
        assert_eq!(idx.coverage.root_manifest, true);
        let scored = build_index(&tmp.path().join("data2"), &ws, 200, 65_536).unwrap();
        let e = scored.files.iter().find(|f| f.rel == "src/entry.ts").expect("entry indexed");
        let deep = scored.files.iter().find(|f| f.rel == "src/deep/mod7/thing.ts").unwrap();
        assert!(e.priority > deep.priority, "{} vs {}", e.priority, deep.priority);
        let test = scored
            .files
            .iter()
            .find(|f| f.rel == "src/deep/mod0/thing.test.ts")
            .unwrap();
        assert!(deep.priority > test.priority);
    }

    #[test]
    fn ignored_dirs_gitignore_and_size_limits_are_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "package.json", "{}");
        touch(&ws, "node_modules/left-pad/index.js", "junk\n");
        touch(&ws, ".gitignore", "secret*.ts\n");
        touch(&ws, "secret-keys.ts", "export const k = 1;\n");
        touch(&ws, "big.ts", &"x".repeat(5000));
        let idx = build_index(&tmp.path().join("d"), &ws, 100, 1024).unwrap();
        assert!(idx.files.iter().all(|f| !f.rel.contains("node_modules")));
        assert!(idx.coverage.ignored_dirs.contains(&"node_modules".to_string()));
        assert!(idx.files.iter().all(|f| f.rel != "secret-keys.ts"));
        assert!(idx.files.iter().all(|f| f.rel != "big.ts"));
        assert_eq!(idx.coverage.too_large, 1);
        assert!(idx.coverage.too_large_examples.contains(&"big.ts".to_string()));
    }

    #[test]
    fn multi_repo_parent_is_detected_and_suggested() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("parent");
        touch(&ws, "app-a/package.json", "{}");
        touch(&ws, "app-b/Cargo.toml", "[package]\nname=\"b\"\n");
        let idx = build_index(&tmp.path().join("d"), &ws, 100, 65_536).unwrap();
        assert!(!idx.coverage.root_manifest);
        assert_eq!(idx.coverage.child_repos.len(), 2);
        assert!(idx.coverage.root_suggestion.is_none());

        let tmp2 = tempfile::tempdir().unwrap();
        let ws2 = tmp2.path().join("parent2");
        touch(&ws2, "only-app/package.json", "{}");
        let idx2 = build_index(&tmp2.path().join("d"), &ws2, 100, 65_536).unwrap();
        assert_eq!(idx2.coverage.root_suggestion.as_deref(), Some("only-app"));
    }

    #[test]
    fn symbols_sections_and_hash_are_captured() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(
            &ws,
            "src/lib.rs",
            "pub fn alpha() {}\npub struct Beta;\nmod gamma;\nfn private() {}\n",
        );
        touch(
            &ws,
            "src/run.ts",
            "export function delta() {}\nexport const eps = 1;\nclass NotExported {}\n",
        );
        touch(&ws, "docs/read-me.md", "# Top\n\nintro\n\n## Sub\n\nmore\n");
        let idx = build_index(&tmp.path().join("d"), &ws, 100, 65_536).unwrap();
        let rs = idx.files.iter().find(|f| f.rel == "src/lib.rs").unwrap();
        assert!(rs.symbols.iter().any(|s| s.name == "alpha" && s.kind == "fn" && s.line == 1));
        assert!(rs.symbols.iter().any(|s| s.name == "Beta"));
        assert!(rs.symbols.iter().any(|s| s.name == "gamma" && s.kind == "mod"));
        assert!(!rs.symbols.iter().any(|s| s.name == "private"));
        let ts = idx.files.iter().find(|f| f.rel == "src/run.ts").unwrap();
        assert!(ts.symbols.iter().any(|s| s.name == "delta" && s.line == 1));
        assert!(ts.symbols.iter().any(|s| s.name == "eps"));
        let md = idx.files.iter().find(|f| f.rel == "docs/read-me.md").unwrap();
        assert_eq!(md.sections.len(), 2);
        assert_eq!(md.sections[0].title, "Top");
        assert!(md.sections[1].start <= md.sections[1].end);
        assert_eq!(rs.hash.len(), 64);
    }

    #[test]
    fn lookup_ranks_symbols_sections_and_content() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "README.md", "# Tool\n");
        touch(
            &ws,
            "src/server.ts",
            "export function startServer() {}\n// the capture pipeline lives here\n",
        );
        touch(&ws, "docs/capture.md", "# Capture\n\nhow capture works\n");
        let data = tmp.path().join("d");
        let idx = build_index(&data, &ws, 100, 65_536).unwrap();
        let hits = lookup(&ws, &idx, "capture pipeline", 10);
        assert!(!hits.is_empty());
        // doc section on capture must rank in
        assert!(hits.iter().any(|h| h.rel == "docs/capture.md"));
        // content line inside server.ts found
        let content = hits.iter().find(|h| h.rel == "src/server.ts" && h.via == "content");
        assert!(content.is_some(), "{hits:?}");
        let hits2 = lookup(&ws, &idx, "startServer", 10);
        assert!(hits2.iter().any(|h| h.via == "symbol" && h.excerpt.contains("startServer")));
    }

    #[test]
    fn index_roundtrips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "Cargo.toml", "[package]\nname=\"x\"\n");
        let data = tmp.path().join("d");
        let idx = build_index(&data, &ws, 10, 65_536).unwrap();
        let loaded = load_index(&data).expect("saved");
        assert_eq!(loaded.files.len(), idx.files.len());
        assert_eq!(loaded.files[0].hash, idx.files[0].hash);
    }

    // ---- tiered budgets, relations, windows (deep-build index) ----------

    #[test]
    fn translated_readmes_lose_to_the_canonical_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "package.json", "{}");
        touch(&ws, "README.md", "# Tool\nreal intro\n");
        touch(&ws, "README_zh.md", "# 工具\n中文简介\n");
        touch(&ws, "README.ja.md", "# ツール\n日本語\n");
        touch(&ws, "docs/guide-fr.md", "# Guide\n");
        touch(&ws, "src/main.ts", "console.log(1);\n");
        let idx = build_index(&tmp.path().join("d"), &ws, 100, 65_536).unwrap();
        let pri = |rel: &str| {
            idx.files
                .iter()
                .find(|f| f.rel == rel)
                .unwrap_or_else(|| panic!("{rel} indexed"))
                .priority
        };
        // translated copies must not outrank (or crowd out) the canonical doc.
        assert!(pri("README.md") > pri("README_zh.md"), "{} vs {}", pri("README.md"), pri("README_zh.md"));
        assert!(pri("README.md") > pri("README.ja.md"));
        assert!(pri("README_zh.md") > pri("docs/guide-fr.md"));
    }

    #[test]
    fn tiered_mode_indexes_oversized_files_as_metadata_only() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "README.md", "# x\n");
        // 5 KB: fits the metadata read cap, exceeds the body file cap.
        let mut big = String::from("pub fn bigsymbol() {\n");
        while big.len() < 5_000 {
            big.push_str("    let pad = 1;\n");
        }
        big.push_str("}\n");
        touch(&ws, "src/big.rs", &big);
        touch(&ws, "src/small.rs", "pub fn tiny() {}\n");
        let budget = IndexBudget::tiered(100, 64_000, 10, 1_000, 40);
        let idx = build_index_with_budget(&tmp.path().join("d"), &ws, &budget).unwrap();
        let big = idx.files.iter().find(|f| f.rel == "src/big.rs").expect("metadata-indexed");
        // metadata survives: an oversized file is windowed, never skipped whole.
        assert!(big.symbols.iter().any(|s| s.name == "bigsymbol"));
        assert!(!big.body_budget, "oversized file must not hold a body slot");
        let small = idx.files.iter().find(|f| f.rel == "src/small.rs").unwrap();
        assert!(small.body_budget);
        assert_eq!(idx.coverage.meta_indexed_files, 3);
        assert_eq!(idx.coverage.body_budget_files, 2);
        assert_eq!(idx.coverage.body_over, 1);
        assert!(idx.coverage.budget_hit);
        assert_eq!(idx.coverage.body_quota_dropped, 0);
    }

    #[test]
    fn doc_quota_demotes_excess_docs_but_keeps_their_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "README.md", "# x\n");
        touch(&ws, "README_zh.md", "# x 中文\n");
        touch(&ws, "README_ja.md", "# x 日本語\n");
        touch(&ws, "docs/guide1.md", "# g1\n");
        touch(&ws, "docs/guide2.md", "# g2\n");
        touch(&ws, "docs/guide3.md", "# g3\n");
        touch(&ws, "docs/guide4.md", "# g4\n");
        for i in 0..3 {
            touch(&ws, &format!("src/mod{i}.ts"), "export const a = 1;\n");
        }
        // body tier = 8 slots, docs get 40% => 3 doc slots; 5 docs exist at
        // root+docs tree plus 2 translated readmes — over quota for sure.
        let budget = IndexBudget::tiered(100, 64_000, 8, 64_000, 40);
        let idx = build_index_with_budget(&tmp.path().join("d"), &ws, &budget).unwrap();
        assert!(idx.coverage.body_quota_dropped >= 2, "quota must demote docs");
        assert!(idx.coverage.body_over >= idx.coverage.body_quota_dropped);
        // every doc still metadata-indexed (lookup sees them), just body-dropped.
        for rel in ["docs/guide4.md", "README_ja.md"] {
            let f = idx.files.iter().find(|x| x.rel == rel).unwrap_or_else(|| panic!("{rel} indexed"));
            assert!(!f.body_budget, "{rel} beyond its category quota keeps metadata only");
        }
        let readme = idx.files.iter().find(|f| f.rel == "README.md").unwrap();
        assert!(readme.body_budget, "the canonical readme wins its slot");
        // core sources were never quota candidates: all three hold body slots.
        for i in 0..3 {
            let f = idx.files.iter().find(|x| x.rel == format!("src/mod{i}.ts")).unwrap();
            assert!(f.body_budget);
        }
        assert!(idx.coverage.body_budget_files <= 8);
        assert!(idx.coverage.body_budget_files >= idx.coverage.meta_indexed_files - idx.coverage.body_over);
    }

    #[test]
    fn legacy_budget_leaves_every_indexed_file_body_eligible() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "README.md", "# x\n");
        touch(&ws, "README_zh.md", "# x\n");
        touch(&ws, "docs/guide.md", "# g\n");
        touch(&ws, "src/main.ts", "console.log(1);\n");
        touch(&ws, "src/main.test.ts", "test('t', () => {});\n");
        let idx = build_index(&tmp.path().join("d"), &ws, 100, 65_536).unwrap();
        // pre-tiering behaviour: one budget, no quota, nothing demoted.
        assert!(idx.files.iter().all(|f| f.body_budget), "{:?}", idx.files.iter().map(|f| (&f.rel, f.body_budget)).collect::<Vec<_>>());
        assert_eq!(idx.coverage.body_over, 0);
        assert_eq!(idx.coverage.body_quota_dropped, 0);
        let n = idx.files.len() as u32;
        assert_eq!(idx.coverage.meta_indexed_files, n);
        assert_eq!(idx.coverage.body_budget_files, n);
    }

    #[test]
    fn import_edges_resolve_across_languages() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "package.json", "{}");
        touch(&ws, "src/lib.rs", "mod alpha;\npub use crate::beta::Thing;\n");
        touch(&ws, "src/alpha.rs", "pub fn a() {}\n");
        touch(&ws, "src/beta.rs", "pub struct Thing;\n");
        touch(&ws, "src/app.ts", "import { x } from \"./util\";\nconsole.log(x);\n");
        touch(&ws, "src/util.ts", "export const x = 1;\n");
        touch(&ws, "pkg/main.py", "from .helper import f\nf()\n");
        touch(&ws, "pkg/helper.py", "def f():\n    pass\n");
        touch(&ws, "cmd/main.go", "package main\nimport \"demo/pkg\"\nfunc main() { pkg.Run() }\n");
        touch(&ws, "pkg/run.go", "package pkg\nfunc Run() {}\n");
        touch(&ws, "src/App.java", "package demo;\nimport com.example.Foo;\npublic class App {}\n");
        let idx = build_index(&tmp.path().join("d"), &ws, 200, 65_536).unwrap();
        let imports = |rel: &str| -> Vec<String> {
            idx.files
                .iter()
                .find(|f| f.rel == rel)
                .unwrap_or_else(|| panic!("{rel} indexed"))
                .imports
                .clone()
        };
        let unresolved = |rel: &str| -> u32 {
            idx.files.iter().find(|f| f.rel == rel).unwrap().unresolved_imports
        };
        let rs = imports("src/lib.rs");
        assert!(rs.contains(&"src/alpha.rs".to_string()), "{rs:?}");
        assert!(rs.contains(&"src/beta.rs".to_string()), "{rs:?}");
        assert_eq!(unresolved("src/lib.rs"), 0);
        assert_eq!(imports("src/app.ts"), vec!["src/util.ts".to_string()]);
        assert_eq!(unresolved("src/app.ts"), 0);
        assert_eq!(imports("pkg/main.py"), vec!["pkg/helper.py".to_string()]);
        // loose dir match may also pick other files under `pkg/`; the point
        // is the real target is found and every edge exists on disk.
        let go_edges = imports("cmd/main.go");
        assert!(go_edges.contains(&"pkg/run.go".to_string()), "{go_edges:?}");
        assert!(go_edges.iter().all(|e| idx.files.iter().any(|f| &f.rel == e)));
        // xlang imports are never guessed: honest unresolved accounting only.
        assert!(imports("src/App.java").is_empty());
        assert!(unresolved("src/App.java") >= 1);
        assert!(idx.coverage.unresolved_imports_total >= 1);
    }

    #[test]
    fn unresolved_import_edges_never_point_at_missing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "package.json", "{}");
        // ./ghost does not exist in the tree: must land in unresolved,
        // never as an edge to a phantom file.
        touch(&ws, "src/app.ts", "import { g } from \"./ghost\";\nimport lodash from \"lodash\";\n");
        let idx = build_index(&tmp.path().join("d"), &ws, 100, 65_536).unwrap();
        let app = idx.files.iter().find(|f| f.rel == "src/app.ts").unwrap();
        assert!(app.imports.iter().all(|e| idx.files.iter().any(|f| &f.rel == e)), "{:?}", app.imports);
        assert_eq!(app.unresolved_imports, 2);
    }

    #[test]
    fn routes_become_pseudo_symbols() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(
            &ws,
            "src/server.ts",
            "app.get(\"/health\", h);\nrouter.post(\"/v1/run\", run);\nfoo.get(\"/not-a-route\", x);\n",
        );
        touch(&ws, "api/main.py", "@app.route(\"/status\")\ndef status():\n    pass\n");
        let idx = build_index(&tmp.path().join("d"), &ws, 100, 65_536).unwrap();
        let srv = idx.files.iter().find(|f| f.rel == "src/server.ts").unwrap();
        let routes: Vec<&str> = srv
            .symbols
            .iter()
            .filter(|s| s.kind == "route")
            .map(|s| s.name.as_str())
            .collect();
        assert!(routes.contains(&"GET /health"), "{routes:?}");
        assert!(routes.contains(&"POST /v1/run"), "{routes:?}");
        assert!(!routes.contains(&"GET /not-a-route"), "non-router receiver is not a route");
        let py = idx.files.iter().find(|f| f.rel == "api/main.py").unwrap();
        assert!(
            py.symbols.iter().any(|s| s.kind == "route" && s.name == "ROUTE /status"),
            "{:?}",
            py.symbols
        );
        // routes are queryable like any symbol.
        let hits = lookup(&ws, &idx, "GET /health", 10);
        assert!(hits.iter().any(|h| h.rel == "src/server.ts" && h.via == "symbol"), "{hits:?}");
    }

    #[test]
    fn symbol_hit_spans_to_the_next_symbol() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "README.md", "# x\n");
        touch(
            &ws,
            "src/lib.rs",
            "pub fn one() {\n    let a = 1;\n    a\n}\npub fn two() {}\n",
        );
        let idx = build_index(&tmp.path().join("d"), &ws, 100, 65_536).unwrap();
        let hits = lookup(&ws, &idx, "one", 10);
        let sym = hits
            .iter()
            .find(|h| h.rel == "src/lib.rs" && h.via == "symbol" && h.excerpt.contains("one"))
            .unwrap_or_else(|| panic!("{hits:?}"));
        assert_eq!(sym.start, 1);
        // body of `one` is lines 1..=4; the next symbol starts at 5.
        assert_eq!(sym.end, 4, "symbol window must reach the whole body, stop at next symbol");
    }

    #[test]
    fn content_hits_return_line_windows_around_the_match() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "README.md", "# x\n");
        let mut body = String::new();
        for i in 1..=80 {
            if i == 30 {
                body.push_str("// flurbz allocates the widget pool\n");
            } else if i == 60 {
                body.push_str("// flurbz teardown drops every widget\n");
            } else {
                body.push_str(&format!("const filler{i} = {i};\n"));
            }
        }
        touch(&ws, "src/pool.ts", &body);
        let idx = build_index(&tmp.path().join("d"), &ws, 100, 65_536).unwrap();
        let hits = lookup(&ws, &idx, "flurbz", 10);
        let c = hits
            .iter()
            .find(|h| h.rel == "src/pool.ts" && h.via == "content")
            .unwrap_or_else(|| panic!("{hits:?}"));
        assert!(c.start <= 30 && c.end >= 30, "window {}..{} must cover the match", c.start, c.end);
        assert!(c.end - c.start < 40);
        assert!(c.excerpt.contains("flurbz"));
        assert!(c.excerpt.len() <= 1400, "excerpt bounded: {}", c.excerpt.len());
    }

    #[test]
    fn importers_of_a_hit_appear_as_related_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        touch(&ws, "package.json", "{}");
        touch(&ws, "src/a.ts", "export function helper() {\n    // flurbz lives here\n    return 1;\n}\n");
        touch(&ws, "src/b.ts", "import { helper } from \"./a\";\nhelper();\n");
        let idx = build_index(&tmp.path().join("d"), &ws, 100, 65_536).unwrap();
        let hits = lookup(&ws, &idx, "flurbz", 10);
        assert!(hits.iter().any(|h| h.rel == "src/a.ts" && h.via == "content"), "{hits:?}");
        let rel = hits.iter().find(|h| h.via == "related");
        let rel = rel.unwrap_or_else(|| panic!("expected a related importer hit: {hits:?}"));
        assert_eq!(rel.rel, "src/b.ts");
        assert!(rel.excerpt.contains("src/a.ts"), "{}", rel.excerpt);
    }
}
