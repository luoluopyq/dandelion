//! WikiSkill (C8 guard + C17 compile loop).
//!
//! Path lock: `{data_dir}/wiki/` (not workspace `.dandelion/wiki/`).
//! Inference must never read wiki bodies or write `{data_dir}`.
//! Humans browse via `Runtime::wiki_*` / `dln wiki`; model gets search snippets only when asked.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const COMPILED_ARCH: &str = "ARCHITECTURE.md";
pub const COMPILED_MODULES: &str = "MODULES.md";
pub const COMPILED_FEATURES: &str = "FEATURES.md";
pub const COMPILED_APIS: &str = "APIS.md";
pub const META_FILE: &str = "meta.json";
pub const PAGES_FILE: &str = "pages.json";
pub const AI_OVERVIEW: &str = "OVERVIEW.md";

/// Reading-list semantics a renderer can branch on. Titles and groups are
/// display strings owned by the profile; `role` is the stable contract the
/// UI selects pages with (default page, provenance hints).
pub const ROLE_OVERVIEW: &str = "overview";
pub const ROLE_SCAN: &str = "scan";
pub const ROLE_DISTILLED: &str = "distilled";
/// Deep-build article written by the model against the evidence index
/// (`wiki_build.rs`). Carries input-file hashes so staleness is derivable.
pub const ROLE_MODEL: &str = "model";

/// Display groups for the reading table of contents, decided by the profile
/// and the compile — the UI must not invent groups from file names.
pub const GROUP_OVERVIEW: &str = "概览";
pub const GROUP_SCAN: &str = "扫描索引";
pub const GROUP_DISTILLED: &str = "对话沉淀";
pub const GROUP_MODEL: &str = "项目文档";

/// One page of a project wiki: the file under `compiled/` and its display title.
/// The registry (`{wiki_dir}/pages.json`) is the only source of page identity;
/// renderers and the UI must not hardcode page names or labels.
/// `group`/`role` are optional so registries written before they existed keep
/// loading unchanged; `effective_pages` backfills them for known files.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiPageEntry {
    pub file: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Derived at READ time (`status`): a model article whose evidence
    /// hashes no longer match the current index, or whose body vanished.
    /// Never trusted from disk — `effective_pages` always resets it, so a
    /// stale value persisted by an older/newer writer can never lie.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
}

/// Per-project page set. Written once per domain profile (W2+) and preserved
/// across compiles, so renames/reordering survive re-creation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiPageRegistry {
    pub pages: Vec<WikiPageEntry>,
}

/// Display title for the deterministic pages. Names describe what the scan
/// actually produces (a package list is not an architecture), so a quick
/// scan never over-promise what a model build would. Unknown files fall
/// back to their stem so a new profile can add pages without a UI change.
pub fn default_page_title(file: &str) -> String {
    match file {
        AI_OVERVIEW => "项目概览",
        COMPILED_ARCH => "仓库构成",
        COMPILED_MODULES => "源码结构",
        COMPILED_FEATURES => "文档要点",
        COMPILED_APIS => "公开函数",
        OUTLINE_PAGE => "资料大纲",
        PAGE_FLOWS => "业务流程",
        PAGE_DECISIONS => "决策与经验",
        PAGE_TERMS => "术语表",
        _ => return file.trim_end_matches(".md").to_string(),
    }
    .to_string()
}

/// Reading-group default for known pages, so a registry written before
/// groups existed still gets a correct table of contents.
pub fn default_page_group(file: &str) -> Option<String> {
    let group = match file {
        AI_OVERVIEW => GROUP_OVERVIEW,
        COMPILED_ARCH | COMPILED_MODULES | COMPILED_FEATURES | COMPILED_APIS | OUTLINE_PAGE => {
            GROUP_SCAN
        }
        PAGE_FLOWS | PAGE_DECISIONS | PAGE_TERMS => GROUP_DISTILLED,
        _ => return None,
    };
    Some(group.to_string())
}

/// Page-role default for known pages (see `ROLE_*`).
pub fn default_page_role(file: &str) -> Option<String> {
    let role = match file {
        AI_OVERVIEW => ROLE_OVERVIEW,
        COMPILED_ARCH | COMPILED_MODULES | COMPILED_FEATURES | COMPILED_APIS | OUTLINE_PAGE => {
            ROLE_SCAN
        }
        PAGE_FLOWS | PAGE_DECISIONS | PAGE_TERMS => ROLE_DISTILLED,
        _ => return None,
    };
    Some(role.to_string())
}

/// Default tab order for the code profile; unknown files sort after known ones.
fn default_page_order(file: &str) -> usize {
    match file {
        AI_OVERVIEW => 0,
        COMPILED_ARCH => 1,
        COMPILED_MODULES => 2,
        COMPILED_FEATURES => 3,
        COMPILED_APIS => 4,
        OUTLINE_PAGE => 2,
        PAGE_FLOWS => 6,
        PAGE_DECISIONS => 7,
        PAGE_TERMS => 8,
        _ => 5,
    }
}

pub fn registry_path(data_dir: &Path) -> PathBuf {
    wiki_dir(data_dir).join(PAGES_FILE)
}

/// Load the page registry; a missing or corrupt file means "no registry yet",
/// not an error — `effective_pages` derives a working set from disk in that case.
pub fn load_registry(data_dir: &Path) -> WikiPageRegistry {
    fs::read_to_string(registry_path(data_dir))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save_registry(data_dir: &Path, registry: &WikiPageRegistry) -> crate::Result<()> {
    fs::create_dir_all(wiki_dir(data_dir))?;
    fs::write(
        registry_path(data_dir),
        serde_json::to_string_pretty(registry)?,
    )?;
    Ok(())
}

/// Registry entries that exist on disk, in registry order, plus any compiled
/// files the registry has not seen yet (default title, then default order).
pub fn effective_pages(data_dir: &Path) -> Vec<WikiPageEntry> {
    let existing = list_compiled_names(data_dir);
    let registry = load_registry(data_dir);
    let mut out: Vec<WikiPageEntry> = Vec::new();
    for entry in &registry.pages {
        if existing.iter().any(|f| *f == entry.file) && !out.iter().any(|o| o.file == entry.file) {
            let mut entry = entry.clone();
            entry.stale = false; // derived, never trusted from disk
            // Registries written before group/role existed backfill from the
            // known-page defaults; unknown pages stay ungrouped.
            if entry.group.is_none() {
                entry.group = default_page_group(&entry.file);
            }
            if entry.role.is_none() {
                entry.role = default_page_role(&entry.file);
            }
            out.push(entry);
        }
    }
    let mut extra: Vec<WikiPageEntry> = existing
        .into_iter()
        .filter(|f| !out.iter().any(|o| &o.file == f))
        .map(|file| {
            let title = default_page_title(&file);
            let group = default_page_group(&file);
            let role = default_page_role(&file);
            WikiPageEntry { file, title, group, role, stale: false }
        })
        .collect();
    extra.sort_by(|a, b| {
        default_page_order(&a.file)
            .cmp(&default_page_order(&b.file))
            .then_with(|| a.file.cmp(&b.file))
    });
    out.extend(extra);
    out
}

/// Upsert registry entries. `force_meta` re-titles and re-tags existing
/// entries (used for profile-owned pages, whose title/group/role the profile
/// decides — this is how title corrections from a code change reach old
/// registries on the next compile); otherwise an existing entry keeps its
/// metadata so custom pages survive re-compiles.
pub(crate) fn sync_registry(data_dir: &Path, entries: &[WikiPageEntry], force_meta: bool) -> crate::Result<()> {
    let mut registry = load_registry(data_dir);
    let mut changed = false;
    for entry in entries {
        match registry.pages.iter_mut().find(|p| p.file == entry.file) {
            Some(existing) if force_meta => {
                if existing.title != entry.title {
                    existing.title = entry.title.clone();
                    changed = true;
                }
                if existing.group != entry.group {
                    existing.group = entry.group.clone();
                    changed = true;
                }
                if existing.role != entry.role {
                    existing.role = entry.role.clone();
                    changed = true;
                }
            }
            Some(_) => {}
            None => {
                registry.pages.push(entry.clone());
                changed = true;
            }
        }
    }
    if changed || !registry_path(data_dir).is_file() {
        save_registry(data_dir, &registry)?;
    }
    Ok(())
}

/// Drop pages from the TOC registry — the counterpart of [`sync_registry`]
/// the upsert-only sync never had. Used when a deep build retires an
/// article (topic renamed away): the page must vanish from every reader's
/// table of contents, not just from disk.
pub(crate) fn remove_pages(data_dir: &Path, files: &[String]) -> crate::Result<()> {
    let mut registry = load_registry(data_dir);
    let before = registry.pages.len();
    registry.pages.retain(|p| !files.iter().any(|f| *f == p.file));
    if registry.pages.len() != before {
        save_registry(data_dir, &registry)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- domain profile

pub const PROFILE_FILE: &str = "profile.json";
pub const OUTLINE_PAGE: &str = "OUTLINE.md";
pub const PAGE_FLOWS: &str = "FLOWS.md";
pub const PAGE_DECISIONS: &str = "DECISIONS.md";
pub const PAGE_TERMS: &str = "TERMS.md";

/// What kind of work a project is, decided by heuristic, model hint or user.
/// It picks which pages a compile produces; the page set itself lives in the
/// registry so every layer below the profile stays domain-agnostic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiProfile {
    /// "code" | "generic"
    pub domain: String,
    /// Display name for the domain, e.g. 软件项目 / 资料与业务.
    pub label: String,
}

impl WikiProfile {
    pub const DOMAIN_CODE: &'static str = "code";
    pub const DOMAIN_GENERIC: &'static str = "generic";

    pub fn code() -> Self {
        Self { domain: Self::DOMAIN_CODE.into(), label: "软件项目".into() }
    }
    pub fn generic() -> Self {
        Self { domain: Self::DOMAIN_GENERIC.into(), label: "资料与业务".into() }
    }
    pub fn is_code(&self) -> bool {
        self.domain == Self::DOMAIN_CODE
    }
}

pub fn profile_path(data_dir: &Path) -> PathBuf {
    wiki_dir(data_dir).join(PROFILE_FILE)
}

pub fn load_profile(data_dir: &Path) -> Option<WikiProfile> {
    fs::read_to_string(profile_path(data_dir))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

pub fn save_profile(data_dir: &Path, profile: &WikiProfile) -> crate::Result<()> {
    fs::create_dir_all(wiki_dir(data_dir))?;
    fs::write(profile_path(data_dir), serde_json::to_string_pretty(profile)?)?;
    Ok(())
}

/// Deterministic domain heuristic: build manifests or a `src` directory at the
/// workspace root mean code; everything else is a docs/business workspace.
pub fn heuristic_profile(ws: &Path) -> WikiProfile {
    const MARKERS: [&str; 9] = [
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "CMakeLists.txt",
        "Makefile",
        "src",
    ];
    if MARKERS.iter().any(|m| ws.join(m).exists()) {
        WikiProfile::code()
    } else {
        WikiProfile::generic()
    }
}

/// Pages a deterministic compile owns for this domain, in reading order.
/// Titles here win over `default_page_title` (e.g. FEATURES reads 文档要点
/// for code and 要点 for docs work); the compile re-titles and re-tags these
/// entries on every run, so renames reach old registries without a migration.
/// Scan-page titles name what the deterministic scan really produces —
/// a README excerpt and package list is 仓库构成, not 架构.
pub fn profile_pages(profile: &WikiProfile) -> Vec<WikiPageEntry> {
    let pairs: &[(&str, &str)] = if profile.is_code() {
        &[
            (AI_OVERVIEW, "项目概览"),
            (COMPILED_ARCH, "仓库构成"),
            (COMPILED_MODULES, "源码结构"),
            (COMPILED_FEATURES, "文档要点"),
            (COMPILED_APIS, "公开函数"),
        ]
    } else {
        &[
            (AI_OVERVIEW, "项目概览"),
            (OUTLINE_PAGE, "资料大纲"),
            (COMPILED_FEATURES, "要点"),
        ]
    };
    pairs
        .iter()
        .map(|(file, title)| WikiPageEntry {
            file: (*file).to_string(),
            title: (*title).to_string(),
            group: default_page_group(file),
            role: default_page_role(file),
            stale: false,
        })
        .collect()
}

// ---------------------------------------------------------------- distilled entries

pub const DISTILLED_FILE: &str = "distilled.jsonl";

/// Ledger statuses. `pending` entries are agent proposals awaiting user
/// review; only `accepted` entries ever compile onto a page. `rejected`
/// entries stay in the ledger so the same claim is not re-proposed as if new
/// (dedup treats every status as "seen").
pub const ENTRY_STATUS_PENDING: &str = "pending";
pub const ENTRY_STATUS_ACCEPTED: &str = "accepted";
pub const ENTRY_STATUS_REJECTED: &str = "rejected";

fn default_status() -> String {
    ENTRY_STATUS_ACCEPTED.to_string()
}

/// One piece of knowledge extracted from a project conversation. Provenance is
/// mandatory: readers must always be able to trace a claim back to its session.
/// `status`/`updated_at`/`revision_of`/`turn` all default so ledgers written
/// before in-conversation wiki writes keep loading unchanged (as accepted).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DistilledEntry {
    pub id: String,
    /// flow | decision | pitfall | term
    pub kind: String,
    pub content: String,
    pub session_id: String,
    pub created_at: String,
    /// pending | accepted | rejected
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// id of the entry this one revised in place (provenance of corrections).
    #[serde(default)]
    pub revision_of: Option<String>,
    /// 1-based user-message (turn) index in the source session, when known.
    #[serde(default)]
    pub turn: Option<u32>,
    /// Caller-supplied idempotency key (external bridges, retried submits).
    /// A second submission with the same key resolves to the existing entry
    /// instead of adding or re-pending anything, so retries after a crash or
    /// an app restart can never duplicate a fact.
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

impl DistilledEntry {
    pub fn is_accepted(&self) -> bool {
        self.status == ENTRY_STATUS_ACCEPTED
    }
}

pub const DISTILL_KINDS: [&str; 4] = ["flow", "decision", "pitfall", "term"];

pub fn distilled_path(data_dir: &Path) -> PathBuf {
    wiki_dir(data_dir).join(DISTILLED_FILE)
}

pub fn load_distilled(data_dir: &Path) -> Vec<DistilledEntry> {
    fs::read_to_string(distilled_path(data_dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Rewrite the whole ledger atomically (temp file + rename). Entry mutation
/// paths are low-frequency (review, revise), so a full rewrite is simpler and
/// crash-safer than in-place edits of an append-only file.
fn write_entries(data_dir: &Path, all: &[DistilledEntry]) -> crate::Result<()> {
    fs::create_dir_all(wiki_dir(data_dir))?;
    let path = distilled_path(data_dir);
    let tmp = path.with_extension("jsonl.tmp");
    let mut raw = String::new();
    for e in all {
        let line = serde_json::to_string(e)?;
        raw.push_str(&line);
        raw.push('\n');
    }
    fs::write(&tmp, raw)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Input for one knowledge write. The caller (agent tool or manual distill)
/// supplies intent; the ledger decides identity, dedup and storage.
#[derive(Clone, Debug)]
pub struct ProposedEntry {
    pub kind: String,
    pub content: String,
    pub session_id: String,
    /// 1-based turn index for provenance, when the caller can locate it.
    pub turn: Option<u32>,
    /// Revise this existing entry in place instead of adding a new one.
    pub revises: Option<String>,
    /// Optional host-supplied idempotency key (see `DistilledEntry`).
    pub idempotency_key: Option<String>,
}

/// What happened to each submitted draft, so the tool can report the truth:
/// nothing may claim "written" for a duplicate or a still-pending proposal.
#[derive(Clone, Debug, Default)]
pub struct IngestReport {
    pub added: Vec<DistilledEntry>,
    pub revised: Vec<DistilledEntry>,
    /// draft index → the existing entry it duplicates.
    pub duplicates: Vec<(usize, DistilledEntry)>,
    /// draft index → validation / conflict reason.
    pub rejected: Vec<(usize, String)>,
}

/// Content validation shared by every write path (tool, distill, edit).
pub fn validate_entry(kind: &str, content: &str, max_bytes: usize) -> Result<(), String> {
    if !DISTILL_KINDS.contains(&kind) {
        return Err(format!(
            "无效类型 {kind}（只允许 {}）",
            DISTILL_KINDS.join(" / ")
        ));
    }
    if content.trim().is_empty() {
        return Err("内容为空".into());
    }
    let len = content.trim().len();
    if len > max_bytes {
        return Err(format!("内容超长：{len} 字节（上限 {max_bytes}）"));
    }
    Ok(())
}

/// Dedup key: drop whitespace and common CN/EN punctuation, lowercase. Exact
/// matches and containment (≥6 normalized chars) count as the same fact, so
/// retries, re-distills and lightly reworded re-proposals do not pile up.
fn normalize_key(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    ',' | '.' | ';' | ':' | '!' | '?' | '(' | ')' | '-' | '_' | '~' | '"' | '\''
                        | '，' | '。' | '、' | '；' | '：' | '！' | '？' | '（' | '）' | '「' | '」'
                        | '『' | '』' | '—' | '～' | '·'
                )
        })
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn is_near_duplicate(a: &str, b: &str) -> bool {
    let (ka, kb) = (normalize_key(a), normalize_key(b));
    if ka.is_empty() || kb.is_empty() {
        return false;
    }
    if ka == kb {
        return true;
    }
    let (short, long) = if ka.len() <= kb.len() { (&ka, &kb) } else { (&kb, &ka) };
    short.len() >= 6 && long.contains(short.as_str())
}

/// Validate, dedup and append/revise entries in one ledger pass. This is the
/// only sanctioned write path: `wiki::compile` renders accepted entries, the
/// panel reviews the rest, and no caller edits the jsonl by hand.
/// Retrying the same submission is a no-op (all duplicates), which makes tool
/// retries and repeated distills of one session idempotent.
pub fn ingest_entries(
    data_dir: &Path,
    drafts: &[ProposedEntry],
    status: &str,
    max_bytes: usize,
) -> crate::Result<IngestReport> {
    let mut report = IngestReport::default();
    if drafts.is_empty() {
        return Ok(report);
    }
    // Read-modify-write of the whole ledger: two agents (or a review and a
    // proposal) committing to the SAME project must serialize here, while
    // other projects stay unblocked. Same lock the compile path uses.
    let lock = compile_lock(data_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut all = load_distilled(data_dir);
    let now = now_rfc3339();
    let mut changed = false;
    for (i, draft) in drafts.iter().enumerate() {
        let content = draft.content.trim();
        if let Err(reason) = validate_entry(&draft.kind, content, max_bytes) {
            report.rejected.push((i, reason));
            continue;
        }
        // Idempotent replay: the same key already landed → report it as the
        // duplicate it is, never a second entry or a status change.
        if let Some(key) = &draft.idempotency_key {
            if let Some(hit) = all.iter().find(|e| e.idempotency_key.as_deref() == Some(key)) {
                report.duplicates.push((i, hit.clone()));
                continue;
            }
        }
        if let Some(target_id) = &draft.revises {
            if let Some(conflict) = all
                .iter()
                .find(|e| &e.id != target_id && is_near_duplicate(&e.content, content))
            {
                let cid = conflict.id.clone();
                report.rejected.push((i, format!("修订后的内容与另一条目重复：{cid}")));
                continue;
            }
            let Some(target) = all.iter_mut().find(|e| &e.id == target_id) else {
                report.rejected.push((i, format!("要修订的条目不存在：{target_id}")));
                continue;
            };
            target.kind = draft.kind.clone();
            target.content = content.to_string();
            target.updated_at = Some(now.clone());
            target.revision_of = Some(target.id.clone());
            report.revised.push(target.clone());
            changed = true;
            continue;
        }
        if let Some(hit) = all.iter().find(|e| is_near_duplicate(&e.content, content)) {
            report.duplicates.push((i, hit.clone()));
            continue;
        }
        let entry = DistilledEntry {
            id: uuid::Uuid::new_v4().to_string(),
            kind: draft.kind.clone(),
            content: content.to_string(),
            session_id: draft.session_id.clone(),
            created_at: now.clone(),
            status: status.to_string(),
            updated_at: None,
            revision_of: None,
            turn: draft.turn,
            idempotency_key: draft.idempotency_key.clone(),
        };
        report.added.push(entry.clone());
        all.push(entry);
        changed = true;
    }
    if changed {
        write_entries(data_dir, &all)?;
    }
    Ok(report)
}

/// Review transition (pending → accepted / rejected) from the Wiki panel.
pub fn set_entry_status(data_dir: &Path, id: &str, status: &str) -> crate::Result<bool> {
    if !matches!(status, ENTRY_STATUS_PENDING | ENTRY_STATUS_ACCEPTED | ENTRY_STATUS_REJECTED) {
        return Err(crate::Error::Invalid(format!("unknown entry status: {status}")));
    }
    let lock = compile_lock(data_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut all = load_distilled(data_dir);
    let Some(entry) = all.iter_mut().find(|e| e.id == id) else {
        return Ok(false);
    };
    if entry.status != status {
        entry.status = status.to_string();
        entry.updated_at = Some(now_rfc3339());
        write_entries(data_dir, &all)?;
    }
    Ok(true)
}

/// Panel-side edit of one entry (content and/or kind). Dedup runs against the
/// other entries so an edit cannot create a second copy of an existing fact.
pub fn edit_entry(
    data_dir: &Path,
    id: &str,
    content: Option<&str>,
    kind: Option<&str>,
    max_bytes: usize,
) -> crate::Result<bool> {
    let lock = compile_lock(data_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut all = load_distilled(data_dir);
    let Some(index) = all.iter().position(|e| e.id == id) else {
        return Ok(false);
    };
    let next_content = content.map(str::trim).unwrap_or(all[index].content.as_str()).to_string();
    let next_kind = kind.unwrap_or(all[index].kind.as_str()).to_string();
    validate_entry(&next_kind, &next_content, max_bytes)
        .map_err(crate::Error::Invalid)?;
    if let Some(hit) = all
        .iter()
        .enumerate()
        .find(|(i, e)| *i != index && is_near_duplicate(&e.content, &next_content))
    {
        return Err(crate::Error::Invalid(format!(
            "内容与另一条目重复：{}",
            hit.1.id
        )));
    }
    all[index].content = next_content;
    all[index].kind = next_kind;
    all[index].updated_at = Some(now_rfc3339());
    write_entries(data_dir, &all)?;
    Ok(true)
}

pub fn remove_distilled(data_dir: &Path, id: &str) -> crate::Result<bool> {
    let lock = compile_lock(data_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let all = load_distilled(data_dir);
    let kept: Vec<DistilledEntry> = all.iter().filter(|e| e.id != id).cloned().collect();
    if kept.len() == all.len() {
        return Ok(false);
    }
    write_entries(data_dir, &kept)?;
    Ok(true)
}

// ------------------------------------------------------- unified submit service
//
// ONE entry point for every wiki write: the native `wiki_propose` tool, the
// manual distill, and the external-engine bridge all arrive here. The request
// carries only what a model may legitimately choose (kind/content/mode/
// revises/idempotencyKey); session id, project identity and turn are injected
// by the host — model-supplied workspace/path/session fields simply do not
// exist in this API. Results are structured so both the agent's wording and
// the UI's state machine follow the TRUTH of what happened.

/// A submission as the host presents it. `workspace: None` means the session
/// has NO explicitly bound project — wiki writes must never fall back to the
/// process cwd or the global settings workspace, so such a submission becomes
/// a persisted draft awaiting the project onboarding flow instead.
#[derive(Clone, Debug)]
pub struct WikiSubmission {
    pub kind: String,
    pub content: String,
    /// "propose" (default) | "write"
    pub mode: String,
    pub revises: Option<String>,
    pub idempotency_key: Option<String>,
    pub session_id: String,
    pub turn: Option<u32>,
    pub workspace: Option<PathBuf>,
    /// Knowledge-space attribution (`space_` id): the entry lands in that
    /// space's ledger (`wiki/spaces/<id>/`) and NO project directory is
    /// required at all. When both are set the space wins — space identity is
    /// the stable one (phase 2), a workspace binding is a presentation that
    /// can move. `submit_wiki` only enforces the id SHAPE here (this module
    /// is store-free pure file IO); whether the space EXISTS is the host's
    /// (Store-backed) check before a submission is ever constructed.
    pub space_id: Option<String>,
}

/// Structured outcome of `submit_wiki`. `cancelled` is produced by the draft
/// flow (the user dismissed onboarding), not by the ledger itself.
#[derive(Clone, Debug)]
pub enum WikiSubmitResult {
    /// No project bound: the draft was persisted for the onboarding flow.
    ProjectRequired { draft: WikiDraft },
    /// Landed as a PENDING candidate — not on any page until the user approves.
    Pending { entry: DistilledEntry },
    /// Landed ACCEPTED (explicit user ask), page recompile is queued.
    Accepted { entry: DistilledEntry },
    /// Nothing was written: the same (or near-identical, or same-key) entry
    /// already exists.
    Duplicate { existing: DistilledEntry },
    /// The ledger refused a revision (target missing / revision conflicts).
    Conflict { reason: String },
    /// Validation or storage failure. Nothing was written.
    Failed { reason: String },
}

impl WikiSubmitResult {
    pub fn status(&self) -> &'static str {
        match self {
            Self::ProjectRequired { draft } => if draft.knowledge_document_id.is_some() { "saved_knowledge" } else { "project_required" },
            Self::Pending { .. } => "pending",
            Self::Accepted { .. } => "accepted",
            Self::Duplicate { .. } => "duplicate",
            Self::Conflict { .. } => "conflict",
            Self::Failed { .. } => "failed",
        }
    }

    pub fn entry_id(&self) -> Option<&str> {
        match self {
            Self::Pending { entry } | Self::Accepted { entry } => Some(&entry.id),
            Self::Duplicate { existing } => Some(&existing.id),
            _ => None,
        }
    }
}

/// The page a kind compiles onto (UI + agent wording share this).
pub fn entry_page(kind: &str) -> &'static str {
    match kind {
        "flow" => "业务流程",
        "term" => "术语表",
        _ => "决策与经验",
    }
}

/// Validate + ingest one submission into `workspace`'s ledger. Queueing the
/// recompile stays with the caller (the runtime owns the compile notifier),
/// keeping this function pure file IO so every host can reuse it.
pub fn submit_wiki(
    data_dir: &Path,
    submission: &WikiSubmission,
    max_bytes: usize,
) -> crate::Result<WikiSubmitResult> {
    let content = submission.content.trim().to_string();
    // Attribution precedence: space > workspace > draft. A space-targeted
    // submission works with no project on disk at all.
    let project_data = if let Some(space_id) = &submission.space_id {
        if !is_safe_space_id(space_id) {
            validate_entry(&submission.kind, &content, max_bytes)
                .map_err(crate::Error::Invalid)?;
            return Ok(WikiSubmitResult::Failed {
                reason: "空间 id 形状非法，提交被拒绝。".into(),
            });
        }
        space_data_dir(data_dir, space_id)
    } else {
        let Some(workspace) = &submission.workspace else {
            validate_entry(&submission.kind, &content, max_bytes)
                .map_err(crate::Error::Invalid)?;
            let draft = save_draft(data_dir, submission)?;
            return Ok(WikiSubmitResult::ProjectRequired { draft });
        };
        resolve_project_data_dir(data_dir, workspace)
    };
    let status = if submission.mode == "write" { ENTRY_STATUS_ACCEPTED } else { ENTRY_STATUS_PENDING };
    let draft_input = ProposedEntry {
        kind: submission.kind.clone(),
        content: content.clone(),
        session_id: submission.session_id.clone(),
        turn: submission.turn,
        revises: submission.revises.clone(),
        idempotency_key: submission.idempotency_key.clone(),
    };
    let report = ingest_entries(&project_data, std::slice::from_ref(&draft_input), status, max_bytes)?;
    if let Some((_, reason)) = report.rejected.first() {
        return Ok(WikiSubmitResult::Failed { reason: reason.clone() });
    }
    if let Some((_, existing)) = report.duplicates.first() {
        return Ok(WikiSubmitResult::Duplicate { existing: existing.clone() });
    }
    let entry = if let Some(entry) = report.added.first() {
        entry.clone()
    } else if let Some(entry) = report.revised.first() {
        entry.clone()
    } else {
        return Ok(WikiSubmitResult::Failed { reason: "提交未产生任何变化".into() });
    };
    // The stored status is the truth: a new propose lands pending, a new
    // write lands accepted, and a revise keeps its target's status (revising
    // a pending entry must not silently promote it, or the UI would claim
    // "已写入" while the entry still waits for review).
    Ok(if entry.is_accepted() {
        WikiSubmitResult::Accepted { entry }
    } else {
        WikiSubmitResult::Pending { entry }
    })
}

// ------------------------------------------------------------ draft storage
//
// A wiki draft from a project-less conversation is persisted per session
// (`{data_dir}/wiki/drafts/`) so an app restart or a cancelled dialog never
// loses the user's pending knowledge. Resolving a draft MOVES the file to
// `drafts/resolved/` together with the entry id it produced — a second
// confirmation of the same draft reads that record and returns the same
// result instead of writing twice.

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiDraft {
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub knowledge_document_id: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub project_name: Option<String>,
    pub draft_id: String,
    pub session_id: String,
    pub kind: String,
    pub content: String,
    pub mode: String,
    pub revises: Option<String>,
    pub idempotency_key: Option<String>,
    pub created_at: String,
}

/// A draft after the user completed onboarding: same identity, plus what it
/// became. Kept on disk so re-confirmation is idempotent across restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedDraft {
    #[serde(flatten)]
    pub draft: WikiDraft,
    pub resolved_at: String,
    pub workspace: String,
    pub entry_id: String,
    pub entry_status: String,
    /// Set when the draft was attributed to a knowledge space instead of a
    /// project (`workspace` is empty then). `serde(default)` keeps every
    /// pre-space resolved record on disk readable.
    #[serde(default)]
    pub space_id: Option<String>,
}

/// Shape check for space ids arriving through file-IO paths: the ids we mint
/// are `space_{uuid-simple}` (alnum + `_`); anything path-shaped is refused
/// before it can name a directory. Existence is a Store question, checked by
/// the host — this module never opens the database.
pub fn is_safe_space_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn drafts_dir(data_dir: &Path) -> PathBuf {
    wiki_dir(data_dir).join("drafts")
}

fn draft_path(data_dir: &Path, draft_id: &str) -> PathBuf {
    // draft_id is always a uuid we minted; still refuse anything path-shaped.
    let safe = draft_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
        .then_some(draft_id)
        .filter(|s| !s.is_empty());
    safe.map(|id| drafts_dir(data_dir).join(format!("{id}.json")))
        .unwrap_or_else(|| drafts_dir(data_dir).join("invalid.json"))
}

fn resolved_path(data_dir: &Path, draft_id: &str) -> PathBuf {
    let safe = draft_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
        .then_some(draft_id)
        .filter(|s| !s.is_empty());
    safe.map(|id| drafts_dir(data_dir).join("resolved").join(format!("{id}.json")))
        .unwrap_or_else(|| drafts_dir(data_dir).join("resolved").join("invalid.json"))
}

fn save_draft(data_dir: &Path, submission: &WikiSubmission) -> crate::Result<WikiDraft> {
    let dir = drafts_dir(data_dir);
    let lock = compile_lock(&dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // A retried tool call must reuse the same unresolved draft. The project
    // ledger has its own idempotency check after onboarding.
    if let Some(key) = submission.idempotency_key.as_deref() {
        if let Some(existing) = list_drafts(data_dir, &submission.session_id)
            .into_iter()
            .find(|draft| draft.idempotency_key.as_deref() == Some(key))
        {
            return Ok(existing);
        }
    }
    let draft = WikiDraft {
        knowledge_document_id: None,
        project_name: None,
        draft_id: uuid::Uuid::new_v4().to_string(),
        session_id: submission.session_id.clone(),
        kind: submission.kind.clone(),
        content: submission.content.trim().to_string(),
        mode: submission.mode.clone(),
        revises: submission.revises.clone(),
        idempotency_key: submission.idempotency_key.clone(),
        created_at: now_rfc3339(),
    };
    fs::create_dir_all(&dir)?;
    fs::write(draft_path(data_dir, &draft.draft_id), serde_json::to_vec_pretty(&draft)?)?;
    Ok(draft)
}

/// Open drafts for one session, oldest first.
pub fn list_drafts(data_dir: &Path, session_id: &str) -> Vec<WikiDraft> {
    let mut drafts = Vec::new();
    let Ok(entries) = fs::read_dir(drafts_dir(data_dir)) else { return drafts };
    let mut files: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    files.sort();
    for file in files {
        if file.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&file) else { continue };
        if let Ok(draft) = serde_json::from_str::<WikiDraft>(&raw) {
            if draft.session_id == session_id {
                drafts.push(draft);
            }
        }
    }
    drafts.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    drafts
}

pub fn load_draft(data_dir: &Path, draft_id: &str) -> Option<WikiDraft> {
    fs::read_to_string(draft_path(data_dir, draft_id))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

pub fn load_resolved_draft(data_dir: &Path, draft_id: &str) -> Option<ResolvedDraft> {
    fs::read_to_string(resolved_path(data_dir, draft_id))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// User dismissed the onboarding dialog: the draft disappears, nothing else
/// happened (no directory, no project, no ledger entry — there never was one).
pub fn cancel_draft(data_dir: &Path, draft_id: &str) -> crate::Result<bool> {
    match fs::remove_file(draft_path(data_dir, draft_id)) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// Mark a draft resolved AFTER its entry landed. The move is the commit
/// point for idempotency: while the open file exists, onboarding can be
/// re-offered; once moved, re-confirmation returns the recorded outcome.
pub fn mark_draft_resolved(
    data_dir: &Path,
    draft: &WikiDraft,
    workspace: &Path,
    entry: &DistilledEntry,
    space_id: Option<&str>,
) -> crate::Result<()> {
    let resolved = ResolvedDraft {
        draft: draft.clone(),
        resolved_at: now_rfc3339(),
        workspace: workspace.to_string_lossy().into_owned(),
        entry_id: entry.id.clone(),
        entry_status: entry.status.clone(),
        space_id: space_id.map(str::to_string),
    };
    let target = resolved_path(data_dir, &draft.draft_id);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, serde_json::to_vec_pretty(&resolved)?)?;
    let _ = fs::remove_file(draft_path(data_dir, &draft.draft_id));
    Ok(())
}

/// How the user chose to give the draft a project (desktop onboarding dialog).
/// All three land on an absolute directory the HOST validated — the model
/// never participates in this choice.
#[derive(Clone, Debug)]
pub enum WikiDraftTarget {
    /// Pick an already registered project.
    ExistingProject { project_id: String },
    /// Pick an existing directory and register it as a project.
    RegisterDirectory { path: PathBuf },
    /// Create `parent/name` and register it. `wiki::resolve` shows the target
    /// path to the user BEFORE creating and refuses the process-cwd /
    /// program-dir locations outright.
    NewProject { parent: PathBuf, name: String },
    /// Attribute the draft to an existing knowledge space (phase 7): the
    /// entry lands in the space ledger, no directory is created and no
    /// project registered. The host validates the id against the Store —
    /// names and paths never participate; the conversation's project binding
    /// stays untouched (a space is not a project).
    Space { space_id: String },
}

/// Result of completing onboarding — what the UI confirms to the user.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiDraftOutcome {
    pub workspace: String,
    pub project_id: String,
    pub entry_id: String,
    /// The entry's real status after submit ("pending" for proposals).
    pub status: String,
    /// Set when the draft was attributed to a space (`workspace`/`project_id`
    /// are empty then).
    #[serde(default)]
    pub space_id: Option<String>,
}

fn render_distilled_page(title: &str, entries: &[&DistilledEntry]) -> String {
    let mut s = format!("# {title}\n\n");
    s.push_str("以下条目由对话自动提炼,保留出处;如有出入请在对话中纠正后重新提炼。\n\n");
    for e in entries {
        let mark = if e.kind == "pitfall" { "⚠ " } else { "" };
        let sid: String = e.session_id.chars().take(8).collect();
        let day: String = e.created_at.chars().take(10).collect();
        let turn = e
            .turn
            .map(|t| format!(" · 第 {t} 轮"))
            .unwrap_or_default();
        s.push_str(&format!(
            "- {mark}{}\n  — 来自对话 `{sid}`{turn} · {day}\n\n",
            e.content.trim()
        ));
    }
    s
}

/// Stable fingerprint of a workspace directory (canonical path sha256).
/// It identifies the *directory*, not the space: the space's identity is
/// its `space_` id, assigned once by the migration and kept in the
/// `knowledge_space_workspaces` registry plus a self-describing
/// `origin.json` inside the space directory.
pub fn workspace_hash(workspace: &Path) -> String {
    let canonical = dunce::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    format!("{:x}", Sha256::digest(canonical.to_string_lossy().as_bytes()))
}

/// Legacy (pre-migration) per-project wiki location: keyed by the raw
/// workspace hash. Kept pure and stable — the migrator and existing tests
/// derive it, and an unmigrated workspace still lives here.
pub fn project_data_dir(data_dir: &Path, workspace: &Path) -> PathBuf {
    data_dir.join("wiki").join("projects").join(workspace_hash(workspace))
}

/// Post-migration home for every space wiki: `wiki/spaces/<spaceId>/`.
pub fn spaces_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("wiki").join("spaces")
}

pub fn space_data_dir(data_dir: &Path, space_id: &str) -> PathBuf {
    spaces_dir(data_dir).join(space_id)
}

/// Self-description dropped into a migrated space dir, so the directory can
/// rebuild the SQLite registry (and never needs the reverse hash).
pub const SPACE_ORIGIN_FILE: &str = "origin.json";

// ---- workspace → space resolution cache ----------------------------------
//
// The deep wiki call sites (`submit_wiki`, the wiki tool) only carry
// `data_dir`, not a `Store` handle, so the (data root, workspace hash) →
// space id mapping is mirrored process-wide. SQLite + `origin.json` are
// the durable truth; this cache is reloaded at `Store::open` and updated
// by the migrator, and `resolve_project_data_dir` below always agrees
// with it within one process.
static SPACE_DIRS: std::sync::Mutex<
    std::collections::BTreeMap<(PathBuf, String), String>,
> = std::sync::Mutex::new(std::collections::BTreeMap::new());

fn cache_root(data_dir: &Path) -> PathBuf {
    dunce::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf())
}

/// Merge `(workspace_hash, space_id)` rows read from SQLite into the cache.
pub fn load_space_dir_cache(data_dir: &Path, rows: &[(String, String)]) {
    let root = cache_root(data_dir);
    if let Ok(mut map) = SPACE_DIRS.lock() {
        for (hash, space_id) in rows {
            map.insert((root.clone(), hash.clone()), space_id.clone());
        }
    }
}

pub fn cache_space_dir(data_dir: &Path, workspace_hash: &str, space_id: &str) {
    if let Ok(mut map) = SPACE_DIRS.lock() {
        map.insert((cache_root(data_dir), workspace_hash.to_string()), space_id.to_string());
    }
}

pub fn cached_space_id(data_dir: &Path, workspace_hash: &str) -> Option<String> {
    SPACE_DIRS
        .lock()
        .ok()?
        .get(&(cache_root(data_dir), workspace_hash.to_string()))
        .cloned()
}

/// Resolve the wiki data dir for `workspace`: the space-owned directory
/// once the workspace is registered, else the legacy hash directory
/// (unchanged behavior for everything the migration has not seen).
pub fn resolve_project_data_dir(data_dir: &Path, workspace: &Path) -> PathBuf {
    if let Some(id) = cached_space_id(data_dir, &workspace_hash(workspace)) {
        return space_data_dir(data_dir, &id);
    }
    project_data_dir(data_dir, workspace)
}

pub fn wiki_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("wiki")
}

pub fn raw_dir(data_dir: &Path) -> PathBuf {
    wiki_dir(data_dir).join("raw")
}

// ------------------------------------------------------- compile coordination
//
// Compilation is deterministic but scans files and rewrites whole pages, so
// two compiles of the SAME project must not interleave, while compiles of
// DIFFERENT projects must not wait on each other. Per project (keyed by the
// project's data dir, which already hashes the workspace):
//   * `compile_lock` — a std Mutex guarding the actual file writes. Sync
//     callers take it directly; async callers hold it inside spawn_blocking.
//   * `schedule_compile` — background path used by agent proposals and the
//     idle hook: returns immediately, and schedules requested while a
//     compile runs coalesce into exactly one follow-up compile (dirty flag),
//     so a burst of proposals never queues a burst of compiles.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Default)]
struct CompileProject {
    running: bool,
    dirty: bool,
}

fn compile_states() -> &'static Mutex<HashMap<PathBuf, CompileProject>> {
    static STATES: OnceLock<Mutex<HashMap<PathBuf, CompileProject>>> = OnceLock::new();
    STATES.get_or_init(Default::default)
}

fn compile_locks() -> &'static Mutex<HashMap<PathBuf, Arc<Mutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(Default::default)
}

/// Serialize file-level work for one project's wiki. Poisoned locks keep
/// working: the worst case is a torn page that the next compile overwrites.
pub fn compile_lock(project_data_dir: &Path) -> Arc<Mutex<()>> {
    let mut map = compile_locks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.entry(project_data_dir.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Notifier for observable compile progress: (status, version, error).
/// The runtime wires this to `SessionUpdate::wiki_update`.
pub type WikiNotifier = Arc<dyn Fn(&str, Option<u32>, Option<String>) + Send + Sync + 'static>;

/// Run one deterministic compile for `workspace` in the background,
/// coalescing repeat requests. Panics if called outside a Tokio runtime.
/// `budget` optionally selects the tiered evidence-index budgets (see
/// `wiki_index::IndexBudget`); `None` keeps the legacy single-cap behavior.
pub fn schedule_compile(
    project_data_dir: PathBuf,
    workspace: PathBuf,
    max_scan_files: u32,
    max_file_bytes: usize,
    budget: Option<crate::wiki_index::IndexBudget>,
    notify: WikiNotifier,
) {
    let key = project_data_dir.clone();
    {
        let mut states = compile_states()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = states.entry(key.clone()).or_default();
        if state.running {
            state.dirty = true;
            return;
        }
        state.running = true;
    }
    tokio::spawn(async move {
        loop {
            let lock = compile_lock(&project_data_dir);
            let data = project_data_dir.clone();
            let ws = workspace.clone();
            let started = notify.clone();
            let budget = budget.clone();
            let result = tokio::task::spawn_blocking(move || {
                let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
                started("started", None, None);
                crate::wiki::compile_with_budget(&data, &ws, max_scan_files, max_file_bytes, budget.as_ref())
                    .map(|meta| meta.version)
                    .map_err(|err| err.to_string())
            })
            .await;
            match result {
                Ok(Ok(version)) => notify("finished", Some(version), None),
                Ok(Err(message)) => notify("error", None, Some(message)),
                Err(_) => notify("error", None, Some("wiki compile task failed".into())),
            }
            let mut states = compile_states()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let state = states.entry(key.clone()).or_default();
            if state.dirty {
                state.dirty = false;
                continue;
            }
            state.running = false;
            break;
        }
    });
}

// ---- Immutable version layout ---------------------------------------------
// Pages live in `wiki/versions/<n>/`, where `<n>` is a monotonically
// increasing SNAPSHOT number — deliberately independent of the deep-build
// `build` number in articles.json (scans and prunes also publish versions and
// must not move build numbers). `wiki/current.json` names the one snapshot
// that is published; switching it is a single rename, so a reader always sees
// a COMPLETE version, never a half-written page directory. The legacy flat
// `wiki/compiled/` layout reads as "version 0": the fallback while no pointer
// exists, and never written again once snapshot compiles begin.

pub const POINTER_FILE: &str = "current.json";
pub const VERSIONS_DIR: &str = "versions";
pub const VERSION_MANIFEST: &str = "version.json";

/// `current.json` (and each snapshot's `version.json` forensics manifest).
/// `kind` records which flow published it ("scan" / "build" / "prune" /
/// "overview"); `build` is the articles.json build number at that moment.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionPointer {
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub build: u32,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub updated_at: String,
}

/// How many versions to keep after publishing. The knob itself is
/// `dandelion.wiki.versionsKeep` in settings.json (AGENTS.md: tunables live
/// in Settings); publish/scan only see the PROJECT data dir while Settings
/// reads the GLOBAL one, so Runtime::assemble applies the value here once per
/// process instead of loading global Settings inside a publish.
static VERSIONS_KEEP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(3);

pub fn set_versions_keep(n: usize) {
    if (1..=16).contains(&n) {
        VERSIONS_KEEP.store(n, std::sync::atomic::Ordering::Relaxed);
    }
}

pub fn versions_keep() -> usize {
    VERSIONS_KEEP.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn versions_root(data_dir: &Path) -> PathBuf {
    wiki_dir(data_dir).join(VERSIONS_DIR)
}

fn pointer_path(data_dir: &Path) -> PathBuf {
    wiki_dir(data_dir).join(POINTER_FILE)
}

/// The pre-versioning flat layout, readable as version 0; never written after
/// the first snapshot-based compile migrates the project forward.
pub fn legacy_compiled_dir(data_dir: &Path) -> PathBuf {
    wiki_dir(data_dir).join("compiled")
}

/// Current pointer, lenient: absent or unreadable reads as "no version yet"
/// so readers fall back to the legacy layout instead of erroring.
pub fn current_pointer(data_dir: &Path) -> Option<VersionPointer> {
    let raw = fs::read_to_string(pointer_path(data_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Replace `path` with `bytes` atomically: write a sibling `.tmp`, then
/// rename. `std::fs::rename` on Windows is `MoveFileExW` with
/// MOVEFILE_REPLACE_EXISTING, so it CAN replace an existing file — the g9
/// suite proves the property on the real platform, not just in a comment.
pub(crate) fn atomic_replace(path: &Path, bytes: &[u8]) -> crate::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = {
        let mut s = path.as_os_str().to_os_string();
        s.push(".tmp");
        PathBuf::from(s)
    };
    fs::write(&tmp, bytes)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

/// Name snapshot `n` as current. This is the COMMIT point of every write
/// flow: once the rename lands, all readers see the complete new version and
/// none sees a partial one. The `version.json` stamp inside the snapshot is
/// best-effort forensics; the pointer is what readers follow.
pub fn switch_version(data_dir: &Path, version: u64, build: u32, kind: &str) -> crate::Result<()> {
    let pointer = VersionPointer {
        version,
        build,
        kind: kind.to_string(),
        updated_at: now_rfc3339(),
    };
    let raw = serde_json::to_string_pretty(&pointer)?;
    let dir = versions_root(data_dir).join(version.to_string());
    if dir.is_dir() {
        let _ = fs::write(dir.join(VERSION_MANIFEST), &raw);
    }
    atomic_replace(&pointer_path(data_dir), raw.as_bytes())
}

/// Next snapshot number: strictly above the pointer AND above any orphaned
/// (crashed, never-published) snapshot dirs, so numbers are never reused.
pub fn next_snapshot_version(data_dir: &Path) -> u64 {
    let mut top = current_pointer(data_dir).map(|p| p.version).unwrap_or(0);
    if let Ok(rd) = fs::read_dir(versions_root(data_dir)) {
        for ent in rd.flatten() {
            if let Ok(n) = ent.file_name().to_string_lossy().parse::<u64>() {
                if ent.path().is_dir() {
                    top = top.max(n);
                }
            }
        }
    }
    top.saturating_add(1)
}

/// Open a fresh, invisible snapshot dir `versions/<n>/` (not current until
/// [`switch_version`]). A stale orphan of the same number (should be
/// impossible — numbers are not reused — but a crashed run must never poison
/// its retry) is cleared first.
pub fn begin_snapshot(data_dir: &Path) -> crate::Result<(u64, PathBuf)> {
    let n = next_snapshot_version(data_dir);
    let dir = versions_root(data_dir).join(n.to_string());
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    Ok((n, dir))
}

/// Copy every `*.md` page visible from the current version into a fresh
/// snapshot BEFORE the flow writes the pages it owns. This is what carries
/// user edits and distilled knowledge on pages the flow does not own into
/// the new version byte-for-byte (spec: rebuilds must never lose them).
pub fn carry_forward(from: &Path, to: &Path) -> crate::Result<()> {
    if from == to {
        return Ok(());
    }
    let Ok(rd) = fs::read_dir(from) else {
        return Ok(());
    };
    for ent in rd.flatten() {
        let name = ent.file_name();
        if !name.to_string_lossy().ends_with(".md") {
            continue;
        }
        if ent.path().is_file() {
            fs::copy(ent.path(), to.join(&name))?;
        }
    }
    Ok(())
}

/// Drop old snapshots after a successful switch: the `keep` highest numbers
/// survive (plus the current one, always — a pointer must never dangle).
/// Best-effort; GC failure never fails a publish.
pub fn gc_versions(data_dir: &Path, keep: usize) {
    let keep = keep.max(1);
    let current = current_pointer(data_dir).map(|p| p.version);
    let mut nums: Vec<u64> = Vec::new();
    if let Ok(rd) = fs::read_dir(versions_root(data_dir)) {
        for ent in rd.flatten() {
            if ent.path().is_dir() {
                if let Ok(n) = ent.file_name().to_string_lossy().parse::<u64>() {
                    nums.push(n);
                }
            }
        }
    }
    nums.sort_unstable();
    let kept: std::collections::BTreeSet<u64> = nums.iter().rev().take(keep).copied().collect();
    for n in &nums {
        if kept.contains(n) || Some(*n) == current {
            continue;
        }
        let _ = fs::remove_dir_all(versions_root(data_dir).join(n.to_string()));
    }
}

/// Crash-safety test hook (g9). When `DANDELION_WIKI_TEST_PARK` equals
/// `stage`, the process writes the file named by
/// `DANDELION_WIKI_TEST_PARK_MARKER` and then stops forever, so the test —
/// which re-spawns THIS executable as a real child — can kill it inside one
/// exact commit window and assert recovery. Hand-written journals can only
/// prove the recovery code; only a real kill proves the WRITE ordering.
/// Inert in production: the env vars are never set outside the test.
pub(crate) fn test_park(stage: &str) {
    let Ok(want) = std::env::var("DANDELION_WIKI_TEST_PARK") else {
        return;
    };
    if want != stage {
        return;
    }
    if let Ok(marker) = std::env::var("DANDELION_WIKI_TEST_PARK_MARKER") {
        if let Some(p) = Path::new(&marker).parent() {
            let _ = fs::create_dir_all(p);
        }
        let _ = fs::write(&marker, format!("parked@{stage}"));
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// Page names (`*.md`) inside a page directory — one resolved dir, no
/// pointer chase. [`list_compiled_names`] is this over the current version.
pub(crate) fn names_in(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return names;
    };
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name.ends_with(".md") {
            names.push(name);
        }
    }
    names.sort();
    names
}

/// The page directory READERS use: the current snapshot if the pointer names
/// one that exists, else the legacy `compiled/` (version 0). Every read path
/// (show/status/evidence/pages/ask) goes through here, so pinning happens per
/// resolution — a reader listing pages and a reader opening one page each see
/// a consistent dir; two separate calls may straddle a publish, which is
/// exactly what an immutable-version scheme allows (never a torn page).
pub fn compiled_dir(data_dir: &Path) -> PathBuf {
    if let Some(p) = current_pointer(data_dir) {
        let dir = versions_root(data_dir).join(p.version.to_string());
        if dir.is_dir() {
            return dir;
        }
    }
    legacy_compiled_dir(data_dir)
}

pub fn is_under(root: &Path, candidate: &Path) -> bool {
    let Ok(root) = dunce::canonicalize(root) else {
        return lexical_under(root, candidate);
    };
    let cand = dunce::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    cand == root || cand.starts_with(&root)
}

fn lexical_under(root: &Path, candidate: &Path) -> bool {
    candidate == root || candidate.starts_with(root)
}

pub fn is_wiki_path(data_dir: &Path, path: &Path) -> bool {
    is_under(&wiki_dir(data_dir), path)
}

pub fn is_data_dir_path(data_dir: &Path, path: &Path) -> bool {
    is_under(data_dir, path)
}

/// A window into an ORIGINAL project file — the sanctioned way for any host
/// to open a cited `rel:Lx-Ly`. The path is joined onto the CANONICALIZED
/// workspace and re-checked with [`is_under`] before reading, so `..`,
/// absolute paths, and symlink escapes all resolve back inside the bound
/// project or are refused; a sibling project's file can never be read
/// through this door.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceWindow {
    pub rel: String,
    /// 1-based inclusive range actually returned (may be clamped).
    pub start: u32,
    pub end: u32,
    pub total_lines: u32,
    pub lines: Vec<String>,
    /// True when the window was capped by `max_lines`.
    pub truncated: bool,
}

pub fn read_source_window(
    ws: &Path,
    rel: &str,
    start: u32,
    end: u32,
    max_lines: u32,
) -> crate::Result<SourceWindow> {
    if rel.is_empty() || rel.contains('\0') || Path::new(rel).is_absolute() {
        return Err(crate::Error::Invalid(format!("非法源码引用路径：{rel:?}")));
    }
    let ws_canon = dunce::canonicalize(ws)
        .map_err(|_| crate::Error::Invalid(format!("工作区不存在：{}", ws.display())))?;
    // Resolve `.`/`..` lexically BEFORE the containment check: a raw
    // `ws\..\secret.txt` is component-prefixed by `ws`, so `starts_with`
    // would call it "inside the project" until the path is normalised.
    // Normalisation happens on strings, never on disk — nothing is created.
    let mut target = ws_canon.clone();
    for comp in Path::new(rel).components() {
        match comp {
            std::path::Component::Normal(seg) => target.push(seg),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !target.pop() {
                    return Err(crate::Error::Invalid(format!("引用路径不在当前项目内：{rel}")));
                }
            }
            // Prefix/RootDir — an absolute path under another spelling.
            _ => return Err(crate::Error::Invalid(format!("非法源码引用路径：{rel:?}"))),
        }
    }
    if !is_under(&ws_canon, &target) {
        return Err(crate::Error::Invalid(format!("引用路径不在当前项目内：{rel}")));
    }
    let raw = fs::read_to_string(&target)
        .map_err(|e| crate::Error::Invalid(format!("无法读取 {rel}：{e}")))?;
    let all: Vec<&str> = raw.lines().collect();
    let total = all.len() as u32;
    if total == 0 {
        return Ok(SourceWindow { rel: rel.to_string(), start: 1, end: 0, total_lines: 0, lines: Vec::new(), truncated: false });
    }
    let start = start.clamp(1, total);
    let end = end.max(start).min(total);
    let cap = max_lines.max(1);
    // shown_end keeps the window within `cap` lines and within the file.
    let shown_end = start + (end - start).min(cap - 1).min(total - start);
    let lines: Vec<String> = all[start as usize - 1..=shown_end as usize - 1].iter().map(|s| s.to_string()).collect();
    Ok(SourceWindow {
        rel: rel.to_string(),
        start,
        end: shown_end,
        total_lines: total,
        lines,
        truncated: shown_end < end,
    })
}

/// Block Inference from reading wiki or writing `{data_dir}`.
pub fn inference_guard(
    name: &str,
    args: &serde_json::Value,
    fs: &crate::sandbox::WorkspaceFs,
    data_dir: &Path,
) -> Option<String> {
    let write = matches!(name, "write_file" | "patch");
    let raw = match name {
        "read_file" | "write_file" | "patch" => args.get("path")?.as_str()?,
        "search_files" => args
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("."),
        _ => return None,
    };
    let resolved = fs.resolve(raw).ok()?;
    inference_fs_block(data_dir, &resolved, write)
}

pub fn inference_fs_block(data_dir: &Path, path: &Path, write: bool) -> Option<String> {
    if write && is_data_dir_path(data_dir, path) {
        return Some("Error: cannot write data_dir from inference".into());
    }
    if !write && is_wiki_path(data_dir, path) {
        return Some("Error: wiki is not readable by inference".into());
    }
    None
}

/// Why the last deep build (model knowledge build) stopped before publishing.
/// Retained across quick compiles so the UI can keep showing the failure
/// until the next build attempt succeeds or replaces it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildError {
    /// coarse stage name: "plan" | "evidence" | "article" | "verify" | "publish"
    pub stage: String,
    pub reason: String,
    pub at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiMeta {
    pub version: u32,
    pub workspace: String,
    pub compiled_at: String,
    pub pages: Vec<String>,
    pub source_files: u32,
    /// Set by a failed/cancelled deep build; cleared by the next successful
    /// one. `serde(default)` keeps older meta.json files loading unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_build_error: Option<BuildError>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiStatus {
    pub path: String,
    pub has_compiled: bool,
    pub version: u32,
    /// Last published deep-build number (articles registry version). Distinct
    /// from `version`, which counts wiki compiles; readers ask "which build am
    /// I reading?", and that is this one. 0 = no deep build published yet.
    pub build_version: u32,
    pub workspace: Option<String>,
    pub compiled_at: Option<String>,
    pub pages: Vec<WikiPageEntry>,
    pub source_files: Option<u32>,
    /// Last deep-build failure (see `BuildError`), if any.
    pub last_build_error: Option<BuildError>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiPage {
    pub name: String,
    pub path: String,
    pub body: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiHit {
    pub page: String,
    pub excerpt: String,
    /// "raw" — a line range in an ORIGINAL project file; "wiki" — a Wiki
    /// page. Absent only on hits produced before the evidence index existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Raw hits carry their 1-based inclusive line range so any reader can
    /// open the exact place in the original file (`rel:Lx-Ly`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u32>,
    /// A Wiki-article hit whose article is *stale* — its recorded evidence
    /// no longer matches the current source index, so it is a reading lead,
    /// never a current fact (spec: 陈旧文章不作为当前事实). Raw hits never
    /// carry it; with no index to compare against nothing is judgeable.
    #[serde(default)]
    pub stale: bool,
}

/// Canonical one-line provenance for a hit, shared by every reader (native
/// QA prompt, bridge replies, UI) so the same hit always reads identically.
pub fn hit_label(hit: &WikiHit) -> String {
    match (hit.kind.as_deref(), hit.start, hit.end) {
        (Some("raw"), Some(a), Some(b)) if a == b => {
            format!("{}:L{a}（原始文件，优先采信）", hit.page)
        }
        (Some("raw"), Some(a), Some(b)) => {
            format!("{}:L{a}-L{b}（原始文件，优先采信）", hit.page)
        }
        // A stale article must announce itself at every reader — the label
        // is the one line all of them share, so the warning travels.
        _ if hit.stale => {
            format!("{}（Wiki 文章：依据文件已变更，可能过期，只能作线索、不作当前事实）", hit.page)
        }
        _ => format!("{}（Wiki 文章）", hit.page),
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiAnswer {
    pub answer: String,
    pub sources: Vec<WikiHit>,
}

/// Compact, project-local evidence for a question. Page names are selected by code,
/// rather than invented by a model. Chinese character pairs and ASCII words both match.
/// Section-level heading matching applies to EVERY page — overview and model
/// articles alike. An earlier overview-only gate made deep-build articles
/// unreachable from QA: their best per-line scores lose to the overview's
/// +4-weighted sections, so a question answered verbatim by a published
/// article could still come back "不知道".
/// First `n` BYTES of `s`, never splitting a UTF-8 character.
fn take_bytes(s: &str, n: usize) -> &str {
    let mut end = n.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// True for character pairs containing ANY interrogative/function character
/// (如何、什么、这个、个项…). Two reasons, both from the real REA
/// acceptance: (1) deep-build templates name sections after the question
/// shapes they answer (「X 是什么」「如何…」), so a function-word pair earns
/// the heading bonus on EVERY article — 快速上手 beat 开发与验证 on an
/// alphabetical tiebreak of 「是什么/如何」 ties; (2) bigrams are blind to
/// word boundaries, and an IDF weight then AMPLIFIES that blind spot —
/// the noise pair 个项 (这|个|项目 boundary, nowhere in any article but the
/// question-shaped overview heading) scored rare-and-heavy, inflating the
/// overview over the article that actually held the test commands. Only
/// pairs made of two topic-bearing characters may earn content weight.
fn generic_cjk_pair(term: &str) -> bool {
    let is_fn = |c: char| matches!(c,
        '如' | '何' | '为' | '什' | '么' | '哪' | '些' | '怎' | '这' | '那' | '个'
        | '是' | '的' | '了' | '和' | '与' | '或' | '请' | '能' | '可' | '要' | '没' | '有'
        | '吗' | '呢' | '吧' | '啊' | '以' | '从' | '就' | '把' | '被' | '让' | '给'
        | '于' | '而' | '但' | '也' | '还');
    let mut chars = term.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(a), Some(b), None) => is_fn(a) || is_fn(b),
        _ => false, // ASCII tokens and mixed pairs carry topic.
    }
}

pub fn evidence(data_dir: &Path, question: &str, max_hits: usize) -> Vec<WikiHit> {
    evidence_ranked(data_dir, question, max_hits).into_iter().map(|(_, hit)| hit).collect()
}

/// `evidence` with each hit's rank score kept, for tooling and debugging —
/// the scoring rules (caps, heading vs window weights, threshold) are easy
/// to misread from the final pack alone.
pub fn evidence_ranked(data_dir: &Path, question: &str, max_hits: usize) -> Vec<(usize, WikiHit)> {
    let mut terms = search_terms(question);
    // Curated articles answer Chinese questions in Chinese prose around
    // English artifacts, so this pass (and only this pass) expands domain
    // vocabulary into the identifiers the articles actually carry.
    expand_aliases(&mut terms);
    if terms.is_empty() { return Vec::new(); }
    let mut ranked = Vec::new();
    // The section pass is corpus-wide: a term's usefulness is a property of
    // the whole Wiki (how many sections even mention it), so sections are
    // collected across pages first and scored afterwards with inverse
    // document frequency. Counting bare matches made every occurrence of
    // 运行/项目 weigh what a rare topical word like 测试 should weigh —
    // real REA Q5 lost that way: the article dense in test commands tied
    // with pages that merely mentioned 运行 once, and alphabetical
    // tie-break shipped the wrong half of the Wiki (real REA Q5: the
    // overview heading scored 8 and the sections holding the test commands
    // scored 5).
    // (page, title, section_line, body) with lowercase title/body kept for
    // matching.
    let mut sections: Vec<(String, String, usize, String, String, String)> = Vec::new();
    let pages = show(data_dir, None).unwrap_or_default();
    for page in &pages {
        {
            let mut section_title = String::new();
            let mut section_line = 0usize;
            let mut section_body = String::new();
            let mut collected = Vec::new();
            for (line_no, line) in page.body.lines().enumerate() {
                if line.starts_with("## ") {
                    if !section_title.is_empty() {
                        collected.push((section_title.clone(), section_line, section_body.clone()));
                    }
                    section_title = line.trim_start_matches('#').trim().to_lowercase();
                    section_line = line_no + 1;
                    section_body.clear();
                } else if !section_title.is_empty() && section_body.len() < 4_800 {
                    section_body.push_str(line);
                    section_body.push('\n');
                }
            }
            if !section_title.is_empty() {
                collected.push((section_title, section_line, section_body));
            }
            for (title, line, body) in collected {
                let lower_body = body.to_lowercase();
                // title was lowercased at capture; it doubles as ltitle.
                sections.push((page.name.clone(), title.clone(), line, body, title, lower_body));
            }
        }
    }
    let n_sections = sections.len().max(1);
    // w(term) = 1 + ln(sections / sections mentioning it), clamped to
    // 1..=3: ubiquitous pairs keep weight 1, corpus-rare ones carry up to
    // triple influence, and the clamp keeps total scores in a small range.
    let weights: Vec<usize> = terms.iter().map(|t| {
        let df = sections
            .iter()
            .filter(|(_, ltitle, _, _, _, lbody)| ltitle.contains(t.as_str()) || lbody.contains(t.as_str()))
            .count()
            .max(1);
        1 + ((n_sections as f64 / df as f64).ln()).clamp(0.0, 2.0).round() as usize
    }).collect();
    // Top-two term contribution preserves the old anti-inflation cap:
    // overlapping CJK pairs inside one word (项目 = 个项 + 项目) no longer
    // stack, and a section cannot win on term volume alone. Question-shaped
    // overview headings lose the sweep they used to win (real REA Q5: the
    // overview heading scored 8 and erased the sections holding the test
    // commands scored 5) both via generic_cjk_pair and via weight 1 on
    // ubiquitous pairs.
    let top2 = |hits: &[usize]| -> usize {
        let mut w: Vec<usize> = hits.iter().map(|i| weights[*i]).collect();
        w.sort_unstable_by(|a, b| b.cmp(a));
        w.truncate(2);
        w.iter().sum()
    };
    // Term occurrences inside the EXCERPT that actually ships. Scores only
    // ask whether the question vocabulary appears in the section; in a
    // deep-build corpus one template voice writes every article, so 测试/
    // 命令 dot most sections and several hits tie at the same score (real
    // REA Q5: five OVERVIEW/TOPIC-1 windows and the TOPIC-7 windows that
    // hold the commands all scored equal, and the page-name tiebreak sent
    // the wrong half of the Wiki). Density settles those ties toward the
    // passage that talks about the topic repeatedly rather than once.
    let density = |excerpt: &str, hits: &[usize]| -> usize {
        hits.iter()
            .map(|i| weights[*i] * excerpt.matches(terms[*i].as_str()).count())
            .sum()
    };
    // Bare-line floor: a single matching line still counts when no section
    // heading or window context earns more, but is capped below every
    // window (4+) so it can never outrank context. Runs after the weights
    // pass so its density shares the same term weights.
    for page in &pages {
        for (line_no, line) in page.body.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("Deterministic compile") { continue; }
            let lower = line.to_lowercase();
            let hits: Vec<usize> = terms
                .iter()
                .enumerate()
                .filter(|(_, t)| !generic_cjk_pair(t) && lower.contains(t.as_str()))
                .map(|(i, _)| i)
                .collect();
            if !hits.is_empty() {
                ranked.push((
                    hits.len().min(3),
                    density(&lower, &hits).min(3),
                    page.name.clone(),
                    line_no + 1,
                    line.chars().take(320).collect::<String>(),
                ));
            }
        }
    }
    for (page, _title, line, body, ltitle, lbody) in &sections {
        // Heading match (+4, above any body match): a section whose TITLE
        // answers the question ships whole.
        let heading: Vec<usize> = terms
            .iter()
            .enumerate()
            .filter(|(_, t)| !generic_cjk_pair(t) && ltitle.contains(t.as_str()))
            .map(|(i, _)| i)
            .collect();
        if !heading.is_empty() && !body.trim().is_empty() {
            let score = 4 + top2(&heading);
            let excerpt = take_bytes(body, 1_600);
            let dens = density(excerpt, &heading).max(1);
            ranked.push((score, dens, page.clone(), *line, excerpt.to_string()));
            continue;
        }
        // Body match (+2, below any heading match): the answer to a
        // question often sits mid-section several bullets from its
        // heading, where the per-line pass scores it as one bare
        // line without the surrounding facts (REA acceptance: the
        // test commands were bullets away from any 测试-matching
        // line). The window is centred on the first matching line
        // (see below) and the page:line provenance points at the
        // window start.
        let matched: Vec<usize> = terms
            .iter()
            .enumerate()
            .filter(|(_, t)| !generic_cjk_pair(t) && lbody.contains(t.as_str()))
            .map(|(i, _)| i)
            .collect();
        if matched.is_empty() { continue; }
        // Window: whole lines from at most 1600 bytes before the
        // first matching line, past it to the end of its paragraph
        // (at most 800 bytes on). Answers commonly sit one bullet
        // from the line that carries the query term in either
        // direction (real REA TOPIC-2: the 测试-adjacent heading
        // sits below `npm run check:fast`, and `npm test` sits one
        // bullet under the line naming 运行测试 — a one-sided
        // window kept cutting off whichever command came after the
        // first match). Whole-line bounds keep UTF-8 safe.
        let body_lines: Vec<&str> = body.split('\n').collect();
        let mi = body_lines
            .iter()
            .position(|l| matched.iter().any(|i| l.contains(terms[*i].as_str())))
            .unwrap_or(0);
        let mut start_i = mi;
        let mut acc = 0usize;
        loop {
            acc += body_lines[start_i].len() + 1;
            if start_i == 0 || acc > 1_600 { break; }
            start_i -= 1;
        }
        let mut end_i = mi;
        let mut acc = 0usize;
        while end_i + 1 < body_lines.len() {
            let nxt = body_lines[end_i + 1];
            if nxt.trim().is_empty() { break; }
            acc += nxt.len() + 1;
            if acc > 800 { break; }
            end_i += 1;
        }
        let window = body_lines[start_i..=end_i].join("\n");
        let score = 2 + top2(&matched);
        let excerpt = take_bytes(&window, 1_600).to_lowercase();
        let dens = density(&excerpt, &matched).max(1);
        ranked.push((
            score,
            dens,
            page.clone(),
            line + 1 + start_i,
            take_bytes(&window, 1_600).to_string(),
        ));
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)).then_with(|| a.2.cmp(&b.2)).then_with(|| a.3.cmp(&b.3)));
    // Honest-evidence floor: keep hits worth at least two thirds of
    // the best. On the 1..=3 weight scale a single strong-term body
    // window scores 5, and command answers often live in exactly one
    // such window; the older max−max/3 cut (6 at max 8)
    // systematically erased them. Pack size (slots) still bounds how
    // much noise ships, the floor still tracks the maximum so a weak
    // pack cuts itself, and emptiness still means honest "not enough
    // evidence".
    let threshold = ranked.first().map(|row| (row.0 * 2 / 3).max(1)).unwrap_or(1);
    let mut hits: Vec<(usize, WikiHit)> = ranked
        .into_iter()
        .filter(|row| row.0 >= threshold)
        .take(max_hits)
        .map(|(score, _dens, page, line, excerpt)| (score, WikiHit {
            page: format!("{page}:{line}"), excerpt, kind: Some("wiki".into()), start: None, end: None, stale: false,
        }))
        .collect();
    // Stale articles are reading leads, never current facts (spec 四:
    // 陈旧文章不作为当前事实). When an article's recorded evidence moved
    // after publication, every reader — QA prompt, bridge reply, UI — must
    // see that on the hit itself. With no index to compare against nothing
    // is judgeable, the same rule `status` applies.
    if let Some(index) = crate::wiki_index::load_index(data_dir) {
        let stale = crate::wiki_build::stale_article_files(data_dir, &index);
        if !stale.is_empty() {
            for (_, hit) in hits.iter_mut() {
                // Wiki hit pages are `FILE:line`; the file names in the
                // compiled directory carry no colon, so the split is safe.
                if let Some((file, _line)) = hit.page.rsplit_once(':') {
                    hit.stale = stale.iter().any(|f| f == file);
                }
            }
        }
    }
    hits
}

/// THE unified project query — one code path for every reader: the native
/// QA turn (`Runtime::wiki_ask_for`) and external engines through the wiki
/// bridge. Raw evidence ranks ahead of Wiki prose, because the original
/// files are the fact source and Wiki articles are derived from them; when
/// both sides hit, at least one Wiki hit keeps its slot so derived reading
/// order still shows through. An empty result is the honest
/// "not enough evidence" signal — callers must say so, never improvise.
pub fn gather(
    data_dir: &Path,
    workspace: &Path,
    question: &str,
    max_hits: usize,
    max_scan_files: u32,
    max_file_bytes: usize,
) -> Vec<WikiHit> {
    if max_hits == 0 {
        return Vec::new();
    }
    // Quick compiles and deep builds refresh the index; build one now only
    // if this project was never indexed, so ordinary questions cost no scan.
    let index = crate::wiki_index::load_index(data_dir).or_else(|| {
        crate::wiki_index::build_index(data_dir, workspace, max_scan_files as usize, max_file_bytes).ok()
    });
    let raw: Vec<WikiHit> = index
        .as_ref()
        .map(|index| {
            crate::wiki_index::lookup(workspace, index, question, max_hits)
                .into_iter()
                .map(|h| WikiHit {
                    page: h.rel,
                    excerpt: h.excerpt,
                    kind: Some("raw".into()),
                    start: Some(h.start),
                    end: Some(h.end),
                    stale: false,
                })
                .collect()
        })
        .unwrap_or_default();
    let pages = evidence(data_dir, question, max_hits);
    let mut hits = Vec::with_capacity(max_hits);
    // Raw first; reserve the last slots for Wiki hits when both sides hit.
    let raw_take = match (raw.is_empty(), pages.is_empty()) {
        (true, _) => 0,
        (_, true) => max_hits,
        // Single slot goes to raw (it is the preferred source). Smaller
        // reservations proved too few in the real REA acceptance: a wall of
        // weak raw matches (near-duplicate multilingual READMEs all scoring
        // one) occupied every other slot and starved the published articles
        // holding the answer — the QA pack shipped six README near-hits while
        // the sections answering 「如何测试」 sat outside it. Raw still owns
        // the first half of any real pack; the back half belongs to Wiki
        // prose, which carries the reading order the raw index cannot.
        (false, false) if max_hits == 1 => 1,
        (false, false) if max_hits == 2 => 1,
        (false, false) if max_hits == 3 => 1,
        (false, false) => (max_hits / 2).min(raw.len()),
    };
    hits.extend(raw.into_iter().take(raw_take));
    hits.extend(pages.into_iter().take(max_hits - hits.len()));
    hits
}

/// Developer-domain query expansion for prose: a Chinese question names a
/// concept in Chinese, but the text carries the concrete artifact under its
/// English identifier — 「验证流程」is the line holding `npm run check:fast`,
/// 「测试」the section discussing `vitest`. With no shared token the strict
/// scorer cannot see the match at all (real REA Q5: every test command sits
/// on a line with zero overlap with 测试/命令/运行). The map is
/// developer-domain vocabulary, not one repo's script names. It applies to
/// curated Wiki articles and to prose files in the raw index (docs,
/// manifests, config), never to code content or symbols: there every
/// `fn run`/`const run` becomes an alias hit (real REA Q5 after blanket
/// expansion: `run`/`test` flooded the raw ranking and pushed the
/// CONTRIBUTING.md region out). Aliases price through the same IDF path as
/// any other term, so a ubiquitous expansion earns its low weight rather
/// than a free pass.
const QUERY_ALIASES: &[(&str, &[&str])] = &[
    ("测试", &["test", "check", "verify"]),
    ("命令", &["command", "cli"]),
    ("运行", &["run"]),
    ("构建", &["build"]),
    ("安装", &["install", "setup"]),
    ("部署", &["deploy"]),
    ("启动", &["start", "launch"]),
    ("配置", &["config"]),
    ("文档", &["docs"]),
    ("发布", &["release"]),
    ("架构", &["architect"]),
    ("性能", &["benchmark"]),
    ("错误", &["error", "failure"]),
];

pub(crate) fn search_terms(question: &str) -> Vec<String> {
    let lower = question.to_lowercase();
    let mut terms = Vec::new();
    for token in lower.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
        if token.len() >= 3 { terms.push(token.to_owned()); }
    }
    let cjk: Vec<char> = lower.chars().filter(|c| matches!(*c as u32, 0x3400..=0x9fff)).collect();
    for pair in cjk.windows(2) { terms.push(pair.iter().collect()); }
    terms.sort(); terms.dedup(); terms
}

/// Apply `QUERY_ALIASES` to a term list. Used by the Wiki-article scoring
/// pass and, in the raw index, only for prose files (see the map's doc
/// comment and `wiki_index::lookup`).
pub(crate) fn expand_aliases(terms: &mut Vec<String>) {
    let mut extra: Vec<String> = Vec::new();
    for term in terms.iter() {
        if let Some((_, aliases)) = QUERY_ALIASES.iter().find(|(k, _)| *k == term.as_str()) {
            extra.extend(aliases.iter().map(|a| (*a).to_owned()));
        }
    }
    terms.extend(extra);
    terms.sort(); terms.dedup();
}

pub fn status(data_dir: &Path) -> WikiStatus {
    // A publish killed mid-write leaves its journal behind; the on-disk
    // pages are then a mixed half-version no reader should be shown.
    // Rewind before reading anything (a single stat when no journal
    // exists), and never over a live build — its journal is in-flight.
    // A recovery that itself fails stays visible via last_build_error.
    if crate::wiki_build::journal_path(data_dir).is_file()
        && !crate::wiki_build::is_building(data_dir)
    {
        if let Err(e) = crate::wiki_build::recover_publish(data_dir) {
            record_build_error(data_dir, "recover", &e.to_string());
        }
    }
    let root = wiki_dir(data_dir);
    let meta = read_meta(data_dir).ok();
    let mut pages = effective_pages(data_dir);
    // Derived staleness: a model article is stale when its recorded evidence
    // hashes no longer match the current index (or an input vanished). With no
    // index to compare against we cannot judge, so nothing is flagged.
    if let Some(index) = crate::wiki_index::load_index(data_dir) {
        let stale: Vec<String> = crate::wiki_build::stale_article_files(data_dir, &index);
        if !stale.is_empty() {
            for page in pages.iter_mut() {
                if stale.iter().any(|f| *f == page.file) {
                    page.stale = true;
                }
            }
        }
    }
    WikiStatus {
        path: root.to_string_lossy().into_owned(),
        has_compiled: !pages.is_empty(),
        version: meta.as_ref().map(|m| m.version).unwrap_or(0),
        build_version: crate::wiki_build::load_articles(data_dir).version,
        workspace: meta.as_ref().map(|m| m.workspace.clone()),
        compiled_at: meta.as_ref().map(|m| m.compiled_at.clone()),
        pages,
        source_files: meta.as_ref().map(|m| m.source_files),
        last_build_error: meta.as_ref().and_then(|m| m.last_build_error.clone()),
    }
}

pub fn show(data_dir: &Path, page: Option<&str>) -> crate::Result<Vec<WikiPage>> {
    let compiled = compiled_dir(data_dir);
    if !compiled.is_dir() {
        return Err(crate::Error::Invalid(
            "wiki not compiled yet; run dln wiki compile".into(),
        ));
    }
    let names = match page {
        Some(p) => {
            let name = normalize_page_name(p);
            if !compiled.join(&name).is_file() {
                return Err(crate::Error::Invalid(format!(
                    "wiki page not found: {name}"
                )));
            }
            vec![name]
        }
        None => list_compiled_names(data_dir),
    };
    let mut out = Vec::new();
    for name in names {
        let path = compiled.join(&name);
        let body = fs::read_to_string(&path)?;
        out.push(WikiPage {
            name,
            path: path.to_string_lossy().into_owned(),
            body,
        });
    }
    Ok(out)
}

pub fn search(data_dir: &Path, query: &str, max_hits: usize) -> Vec<WikiHit> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for page in show(data_dir, None).unwrap_or_default() {
        for line in page.body.lines() {
            if line.to_ascii_lowercase().contains(&q) {
                hits.push(WikiHit {
                    page: page.name.clone(),
                    excerpt: line.trim().chars().take(240).collect(),
                    kind: Some("wiki".into()),
                    start: None,
                    end: None,
                    stale: false,
                });
                if hits.len() >= max_hits {
                    return hits;
                }
            }
        }
    }
    hits
}

/// Deterministic compile from workspace → `{data_dir}/wiki/{raw,compiled}`.
/// Does not call a model. Does not inject into agent system prompt.
/// Legacy single-cap index budgets; see [`compile_with_budget`] for tiers.
pub fn compile(
    data_dir: &Path,
    workspace: &Path,
    max_scan_files: u32,
    max_file_bytes: usize,
) -> crate::Result<WikiMeta> {
    compile_with_budget(data_dir, workspace, max_scan_files, max_file_bytes, None)
}

/// [`compile`] with optional tiered evidence-index budgets. `budget` only
/// shapes the traceable index (metadata tier vs model-body tier); the page
/// compile itself and every other output are identical either way.
pub fn compile_with_budget(
    data_dir: &Path,
    workspace: &Path,
    max_scan_files: u32,
    max_file_bytes: usize,
    budget: Option<&crate::wiki_index::IndexBudget>,
) -> crate::Result<WikiMeta> {
    let ws = dunce::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    if !ws.is_dir() {
        return Err(crate::Error::Invalid(format!(
            "wiki compile: workspace missing: {}",
            ws.display()
        )));
    }
    let raw = raw_dir(data_dir);
    fs::create_dir_all(&raw)?;
    // Build the next version INVISIBLE at `versions/<n>/`: every page write
    // below lands in the snapshot, readers keep seeing the current version,
    // and the pointer flips once at the end (or never — a crashed scan leaves
    // only an orphan dir no reader can reach). Carry first, so pages this
    // scan does not own (deep-build articles, user edits) ride along.
    let (snapshot_n, compiled) = begin_snapshot(data_dir)?;
    carry_forward(&compiled_dir(data_dir), &compiled)?;

    let scanned = scan_workspace(&ws, max_scan_files as usize, max_file_bytes);
    // Freeze a few entrypoints into raw/ (never for inference).
    for (rel, text) in &scanned.raw_copies {
        let dest = raw.join(rel.replace('/', "_").replace('\\', "_"));
        let _ = fs::write(dest, text);
    }
    // Refresh the traceable evidence index on every quick scan, so QA and
    // external queries see current files without paying a rescan per
    // question. Auxiliary: page compilation must not fail over an index
    // hiccup (`gather` degrades to Wiki prose, and lazy-builds once).
    let _ = match budget {
        Some(b) => crate::wiki_index::build_index_with_budget(data_dir, &ws, b),
        None => crate::wiki_index::build_index(data_dir, &ws, max_scan_files as usize, max_file_bytes),
    };

    // Domain profile decides which pages exist; absent profile.json means the
    // heuristic runs once and is persisted, so later runs are stable.
    let profile = match load_profile(data_dir) {
        Some(p) => p,
        None => {
            let p = heuristic_profile(&ws);
            let _ = save_profile(data_dir, &p);
            p
        }
    };

    let mut stale_pages: Vec<&str> = Vec::new();
    if profile.is_code() {
        fs::write(compiled.join(COMPILED_ARCH), render_architecture(&ws, &scanned))?;
        fs::write(compiled.join(COMPILED_MODULES), render_modules(&scanned))?;
        fs::write(compiled.join(COMPILED_FEATURES), render_features(&ws, &scanned, "文档要点"))?;
        fs::write(compiled.join(COMPILED_APIS), render_apis(&scanned))?;
        stale_pages.push(OUTLINE_PAGE);
    } else {
        fs::write(compiled.join(OUTLINE_PAGE), render_outline(&scanned))?;
        fs::write(compiled.join(COMPILED_FEATURES), render_features(&ws, &scanned, "要点"))?;
        for stale in [COMPILED_ARCH, COMPILED_MODULES, COMPILED_APIS] {
            stale_pages.push(stale);
        }
    }
    for stale in stale_pages {
        let _ = fs::remove_file(compiled.join(stale));
    }
    // The quick scan owns the placeholder overview, but a deep-build
    // overview/article set is versioned knowledge: a scan must never delete
    // it (spec D). Registered in `articles.json` ⇒ owned by wiki_build.
    if !crate::wiki_build::is_model_article(data_dir, AI_OVERVIEW) {
        let _ = fs::remove_file(compiled.join(AI_OVERVIEW));
    }

    // Conversation-distilled knowledge renders onto its own pages, with the
    // source session on every line. Pending proposals and rejected claims
    // never reach a page — review is the authorization.
    let distilled: Vec<DistilledEntry> = load_distilled(data_dir)
        .into_iter()
        .filter(|e| e.is_accepted())
        .collect();
    // Each kind page is written or removed on every compile, including when
    // the ledger is empty — undo/reject must take the stale page down with it.
    let mut distilled_pages: Vec<WikiPageEntry> = Vec::new();
    let flows: Vec<&DistilledEntry> = distilled.iter().filter(|e| e.kind == "flow").collect();
    if flows.is_empty() {
        let _ = fs::remove_file(compiled.join(PAGE_FLOWS));
    } else {
        fs::write(compiled.join(PAGE_FLOWS), render_distilled_page("业务流程", &flows))?;
        distilled_pages.push(WikiPageEntry {
            file: PAGE_FLOWS.into(),
            title: "业务流程".into(),
            group: default_page_group(PAGE_FLOWS),
            role: default_page_role(PAGE_FLOWS),
            stale: false,
        });
    }
    let choices: Vec<&DistilledEntry> = distilled
        .iter()
        .filter(|e| e.kind == "decision" || e.kind == "pitfall")
        .collect();
    if choices.is_empty() {
        let _ = fs::remove_file(compiled.join(PAGE_DECISIONS));
    } else {
        fs::write(compiled.join(PAGE_DECISIONS), render_distilled_page("决策与经验", &choices))?;
        distilled_pages.push(WikiPageEntry {
            file: PAGE_DECISIONS.into(),
            title: "决策与经验".into(),
            group: default_page_group(PAGE_DECISIONS),
            role: default_page_role(PAGE_DECISIONS),
            stale: false,
        });
    }
    let terms: Vec<&DistilledEntry> = distilled.iter().filter(|e| e.kind == "term").collect();
    if terms.is_empty() {
        let _ = fs::remove_file(compiled.join(PAGE_TERMS));
    } else {
        fs::write(compiled.join(PAGE_TERMS), render_distilled_page("术语表", &terms))?;
        distilled_pages.push(WikiPageEntry {
            file: PAGE_TERMS.into(),
            title: "术语表".into(),
            group: default_page_group(PAGE_TERMS),
            role: default_page_role(PAGE_TERMS),
            stale: false,
        });
    }

    // Profile pages re-title on every compile (profile owns their names);
    // distilled pages keep whatever title the registry already has.
    sync_registry(data_dir, &profile_pages(&profile), true)?;
    sync_registry(data_dir, &distilled_pages, false)?;

    let prev = read_meta(data_dir).ok();
    let version = prev.as_ref().map(|m| m.version.saturating_add(1)).unwrap_or(1);
    // A deep-build failure stays visible until the next build replaces it —
    // quick compiles must not silently clear the user's only clue.
    let last_build_error = prev.and_then(|m| m.last_build_error);
    let meta = WikiMeta {
        version,
        workspace: ws.to_string_lossy().into_owned(),
        compiled_at: now_rfc3339(),
        // The SNAPSHOT's page set — the pointer still names the old version,
        // so listing via compiled_dir would describe the previous compile.
        pages: names_in(&compiled),
        source_files: scanned.files.len() as u32,
        last_build_error,
    };
    write_meta(data_dir, &meta)?;
    // COMMIT: one rename makes the whole snapshot visible, then sweep.
    crate::wiki::test_park("scan-before-switch");
    let build = crate::wiki_build::load_articles(data_dir).version;
    switch_version(data_dir, snapshot_n, build, "scan")?;
    gc_versions(data_dir, versions_keep());
    Ok(meta)
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

fn read_meta(data_dir: &Path) -> crate::Result<WikiMeta> {
    let raw = fs::read_to_string(wiki_dir(data_dir).join(META_FILE))?;
    Ok(serde_json::from_str(&raw)?)
}

fn write_meta(data_dir: &Path, meta: &WikiMeta) -> crate::Result<()> {
    atomic_replace(
        &wiki_dir(data_dir).join(META_FILE),
        serde_json::to_string_pretty(meta)?.as_bytes(),
    )
}

pub(crate) fn write_project_meta(data_dir: &Path, meta: &WikiMeta) -> crate::Result<()> {
    write_meta(data_dir, meta)
}

/// Record why the last deep build failed or was cancelled, so `status()`
/// keeps showing it. Only applies once a meta exists (a wiki that was never
/// compiled has nothing to keep readable, so nothing to warn about).
pub fn record_build_error(data_dir: &Path, stage: &str, reason: &str) {
    if let Ok(mut meta) = read_meta(data_dir) {
        meta.last_build_error = Some(BuildError {
            stage: stage.to_string(),
            reason: reason.to_string(),
            at: now_rfc3339(),
        });
        let _ = write_meta(data_dir, &meta);
    }
}

/// Clear the recorded deep-build failure (called on a successful publish).
pub fn clear_build_error(data_dir: &Path) {
    if let Ok(mut meta) = read_meta(data_dir) {
        if meta.last_build_error.take().is_some() {
            let _ = write_meta(data_dir, &meta);
        }
    }
}

pub(crate) fn list_compiled_names(data_dir: &Path) -> Vec<String> {
    names_in(&compiled_dir(data_dir))
}

/// What [`normalize_page_name`] returns instead of a name when the input is
/// empty or tries to traverse. Callers (page save, page delete) check this
/// constant so the rejection rule lives in exactly one place.
pub const INVALID_PAGE_NAME: &str = "__invalid_page__";

pub fn normalize_page_name(raw: &str) -> String {
    let t = raw.trim();
    if t.contains('/') || t.contains('\\') || t.contains("..") || t.is_empty() {
        return INVALID_PAGE_NAME.into();
    }
    if t.ends_with(".md") {
        t.to_string()
    } else {
        format!("{t}.md")
    }
}

struct ScanResult {
    files: Vec<ScannedFile>,
    raw_copies: Vec<(String, String)>,
}

struct ScannedFile {
    rel: String,
    kind: FileKind,
    text: String,
}

#[derive(Clone, Copy)]
enum FileKind {
    Rust,
    Toml,
    Md,
    Code,
    #[allow(dead_code)]
    Other,
}

fn scan_workspace(ws: &Path, max_files: usize, max_bytes: usize) -> ScanResult {
    let mut files = Vec::new();
    let mut raw_copies = Vec::new();
    let mut stack = vec![ws.to_path_buf()];
    let skip = ["target", "node_modules", ".git", "dist", "wiki"];
    while let Some(dir) = stack.pop() {
        if files.len() >= max_files {
            break;
        }
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for ent in rd.flatten() {
            if files.len() >= max_files {
                break;
            }
            let path = ent.path();
            let name = ent.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && name != ".dandelion.md" {
                continue;
            }
            if path.is_dir() {
                if skip.iter().any(|s| name.eq_ignore_ascii_case(s)) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let kind = match ext.as_str() {
                "rs" => FileKind::Rust,
                "toml" => FileKind::Toml,
                // txt joins Md: for a docs workspace the outline treats any
                // plain-text document like markdown (headings if present).
                "md" | "txt" => FileKind::Md,
                "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "vue" => FileKind::Code,
                _ => continue,
            };
            let Ok(meta) = ent.metadata() else {
                continue;
            };
            if meta.len() as usize > max_bytes {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(ws)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if matches!(
                rel.as_str(),
                "README.md" | "Cargo.toml" | "AGENTS.md" | ".dandelion.md" | "DANDELION.md"
            ) || rel.ends_with("/Cargo.toml")
            {
                raw_copies.push((rel.clone(), text.clone()));
            }
            files.push(ScannedFile { rel, kind, text });
        }
    }
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    ScanResult { files, raw_copies }
}

fn render_architecture(ws: &Path, scan: &ScanResult) -> String {
    let mut s = String::from("# 仓库构成\n\n");
    s.push_str("本页由快速扫描生成：README 摘录与包清单，不含模型推断的架构结论。\n\n");
    s.push_str(&format!("Workspace: `{}`\n\n", ws.display()));
    s.push_str(
        "Deterministic compile from workspace files (C17). Not injected into agent system.\n\n",
    );
    if let Some(readme) = scan.files.iter().find(|f| {
        f.rel.eq_ignore_ascii_case("README.md") || f.rel.eq_ignore_ascii_case("readme.md")
    }) {
        s.push_str("## README excerpt\n\n");
        for line in readme.text.lines().take(40) {
            s.push_str(line);
            s.push('\n');
        }
        s.push('\n');
    }
    let crates: Vec<_> = scan
        .files
        .iter()
        .filter(|f| f.rel.ends_with("Cargo.toml"))
        .collect();
    if !crates.is_empty() {
        s.push_str("## Cargo packages\n\n");
        for c in crates {
            let name = extract_toml_name(&c.text).unwrap_or_else(|| c.rel.clone());
            s.push_str(&format!("- `{}` ({})\n", name, c.rel));
        }
        s.push('\n');
    }
    s
}

/// Generic-domain page: one section per scanned document with its heading
/// skeleton (or first lines when the document has no headings).
fn render_outline(scan: &ScanResult) -> String {
    let mut s = String::from("# 资料大纲\n\n");
    let docs: Vec<_> = scan.files.iter().filter(|f| matches!(f.kind, FileKind::Md)).collect();
    if docs.is_empty() {
        s.push_str("_No documents scanned._\n");
        return s;
    }
    for f in docs {
        s.push_str(&format!("## `{}`\n\n", f.rel));
        let mut shown = 0usize;
        for line in f.text.lines() {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            let bullet = if t.starts_with('#') {
                let depth = t.chars().take_while(|c| *c == '#').count();
                format!("{}- {}", "  ".repeat(depth.saturating_sub(1)), t.trim_start_matches('#').trim())
            } else {
                let mut end = t.len().min(120);
                while !t.is_char_boundary(end) {
                    end -= 1;
                }
                format!("- {}", &t[..end])
            };
            s.push_str(&bullet);
            s.push('\n');
            shown += 1;
            if shown >= 12 {
                break;
            }
        }
        if shown == 0 {
            s.push_str("_（空文档）_\n");
        }
        s.push('\n');
    }
    s
}

fn render_modules(scan: &ScanResult) -> String {
    let mut s = String::from("# 源码结构\n\n");
    let code: Vec<_> = scan
        .files
        .iter()
        .filter(|f| matches!(f.kind, FileKind::Rust | FileKind::Code))
        .collect();
    if code.is_empty() {
        s.push_str("_No source files scanned._\n");
        return s;
    }
    for f in code {
        s.push_str(&format!("## `{}`\n\n", f.rel));
        if matches!(f.kind, FileKind::Code) {
            for line in f.text.lines().map(str::trim).filter(|line| {
                line.starts_with("export ") || line.starts_with("class ") ||
                line.starts_with("def ") || line.starts_with("async def ") ||
                line.starts_with("func ") || line.starts_with("function ")
            }).take(12) {
                s.push_str(&format!("- {}\n", line.chars().take(180).collect::<String>()));
            }
            s.push('\n');
            continue;
        }
        let mods = extract_mod_decls(&f.text);
        if !mods.is_empty() {
            s.push_str("Modules: ");
            s.push_str(&mods.join(", "));
            s.push_str("\n\n");
        }
        let structs = extract_pub_items(&f.text, "struct");
        let enums = extract_pub_items(&f.text, "enum");
        let traits = extract_pub_items(&f.text, "trait");
        if !structs.is_empty() {
            s.push_str(&format!("Structs: {}\n\n", structs.join(", ")));
        }
        if !enums.is_empty() {
            s.push_str(&format!("Enums: {}\n\n", enums.join(", ")));
        }
        if !traits.is_empty() {
            s.push_str(&format!("Traits: {}\n\n", traits.join(", ")));
        }
    }
    s
}

fn render_features(ws: &Path, scan: &ScanResult, title: &str) -> String {
    let mut s = format!("# {title}\n\n");
    s.push_str("本页由快速扫描汇总项目说明文档的标题与列表而来。\n\n");
    for name in [
        ".dandelion.md",
        "DANDELION.md",
        "蒲公英.md",
    ] {
        let path = ws.join(name);
        if let Ok(text) = fs::read_to_string(&path) {
            s.push_str(&format!("## From `{name}`\n\n"));
            for line in text.lines().take(60) {
                if line.starts_with('#') || line.starts_with('-') || line.starts_with('*') {
                    s.push_str(line);
                    s.push('\n');
                }
            }
            s.push('\n');
        }
    }
    for f in scan.files.iter().filter(|f| matches!(f.kind, FileKind::Md)) {
        if f.rel.contains("README") {
            continue;
        }
        let headings: Vec<_> = f
            .text
            .lines()
            .filter(|l| l.starts_with("## "))
            .take(12)
            .collect();
        if headings.is_empty() {
            continue;
        }
        s.push_str(&format!("## `{}`\n\n", f.rel));
        for h in headings {
            s.push_str("- ");
            s.push_str(h.trim_start_matches('#').trim());
            s.push('\n');
        }
        s.push('\n');
    }
    if s.lines().count() < 4 {
        s.push_str("_No feature headings discovered._\n");
    }
    s
}

fn render_apis(scan: &ScanResult) -> String {
    let mut s = String::from("# 公开函数索引\n\n");
    let re = regex::Regex::new(
        r"(?m)^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(([^)]*)\)",
    )
    .expect("regex");
    let mut any = false;
    for f in scan
        .files
        .iter()
        .filter(|f| matches!(f.kind, FileKind::Rust))
    {
        let mut block = String::new();
        for cap in re.captures_iter(&f.text) {
            let name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let args = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            // Prefer pub items; still list others lightly for small crates
            let line_start = &f.text[..cap.get(0).unwrap().start()];
            let line = line_start.lines().last().unwrap_or("");
            if !line.contains("pub") && !f.rel.contains("lib.rs") && !f.rel.contains("mod.rs") {
                continue;
            }
            block.push_str(&format!("- `fn {name}({args})`\n"));
        }
        if !block.is_empty() {
            any = true;
            s.push_str(&format!("## `{}`\n\n{block}\n", f.rel));
        }
    }
    if !any {
        s.push_str("_No public functions found in scan cap._\n");
    }
    s
}

fn extract_toml_name(text: &str) -> Option<String> {
    let mut in_pkg = false;
    for line in text.lines() {
        let t = line.trim();
        if t == "[package]" {
            in_pkg = true;
            continue;
        }
        if t.starts_with('[') {
            in_pkg = false;
        }
        if in_pkg {
            if let Some(rest) = t.strip_prefix("name") {
                let rest = rest.trim().trim_start_matches('=').trim();
                let name = rest.trim_matches('"').trim_matches('\'');
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

fn extract_mod_decls(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"(?m)^(?:pub\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;").unwrap();
    re.captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

fn extract_pub_items(text: &str, kind: &str) -> Vec<String> {
    let pat = format!(r"(?m)^pub\s+(?:\([^)]*\)\s+)?{kind}\s+([A-Za-z_][A-Za-z0-9_]*)");
    let re = regex::Regex::new(&pat).unwrap();
    re.captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .take(40)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_under_data_dir_detected() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = wiki_dir(dir.path());
        std::fs::create_dir_all(&wiki).unwrap();
        let page = wiki.join("ARCH.md");
        std::fs::write(&page, "secret-wiki-sentence").unwrap();
        assert!(is_wiki_path(dir.path(), &page));
        assert!(inference_fs_block(dir.path(), &page, false).is_some());
        assert!(inference_fs_block(dir.path(), dir.path(), true).is_some());
        assert!(inference_fs_block(dir.path(), &dir.path().join("other.txt"), false).is_none());
    }

    #[test]
    fn inference_guard_blocks_wiki_read_and_data_dir_write() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let wiki = data.join("wiki");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::write(wiki.join("ARCH.md"), "secret-wiki-sentence").unwrap();
        let fs = crate::sandbox::WorkspaceFs::new(dir.path().to_path_buf()).unwrap();
        let read = inference_guard(
            "read_file",
            &serde_json::json!({"path": "data/wiki/ARCH.md"}),
            &fs,
            &data,
        );
        assert!(read.as_deref().unwrap_or("").contains("wiki"), "{read:?}");
        let write = inference_guard(
            "write_file",
            &serde_json::json!({"path": "data/DANDELION.soul.md", "content": "x"}),
            &fs,
            &data,
        );
        assert!(
            write.as_deref().unwrap_or("").contains("data_dir"),
            "{write:?}"
        );
        let ok = inference_guard(
            "read_file",
            &serde_json::json!({"path": "README.md"}),
            &fs,
            &data,
        );
        assert!(ok.is_none());
        let search = inference_guard(
            "search_files",
            &serde_json::json!({"path": "data/wiki", "pattern": "secret"}),
            &fs,
            &data,
        );
        assert!(
            search.as_deref().unwrap_or("").contains("wiki"),
            "{search:?}"
        );
    }

    #[test]
    fn compile_writes_four_pages_and_search_hits() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(
            ws.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(ws.join("README.md"), "# Demo\n\nFeature: greeter\n").unwrap();
        std::fs::write(
            ws.join("src/lib.rs"),
            "pub mod greeter;\npub fn hello(name: &str) -> String { name.into() }\n",
        )
        .unwrap();
        std::fs::write(
            ws.join("src/greeter.rs"),
            "pub struct Greeter;\nimpl Greeter { pub fn wave(&self) {}\n}\n",
        )
        .unwrap();
        let meta = compile(&data, &ws, 50, 65_536).unwrap();
        assert_eq!(meta.version, 1);
        assert!(compiled_dir(&data).join(COMPILED_APIS).is_file());
        let pages = show(&data, Some("APIS")).unwrap();
        assert!(pages[0].body.contains("hello"), "{}", pages[0].body);
        let hits = search(&data, "hello", 5);
        assert!(!hits.is_empty(), "{hits:?}");
        // Inference still blocked
        assert!(is_wiki_path(
            &data,
            &compiled_dir(&data).join(COMPILED_APIS)
        ));
    }

    #[test]
    fn registry_orders_titles_and_survives_recompile() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("README.md"), "# Demo\n").unwrap();
        std::fs::write(ws.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        compile(&data, &ws, 50, 65_536).unwrap();

        // First compile bootstraps the registry, including the not-yet-written overview.
        let reg = load_registry(&data);
        assert_eq!(
            reg.pages
                .iter()
                .map(|p| p.file.as_str())
                .collect::<Vec<_>>(),
            vec![AI_OVERVIEW, COMPILED_ARCH, COMPILED_MODULES, COMPILED_FEATURES, COMPILED_APIS]
        );

        // Effective pages drop entries without a file and carry default titles.
        let eff = effective_pages(&data);
        assert!(!eff.iter().any(|p| p.file == AI_OVERVIEW));
        assert_eq!(eff[0].file, COMPILED_ARCH);
        // Scan pages carry honest titles and reading metadata (role/group)
        // straight from the registry sync.
        assert_eq!(eff[0].title, "仓库构成");
        assert_eq!(eff[0].role.as_deref(), Some(ROLE_SCAN));
        assert_eq!(eff[0].group.as_deref(), Some(GROUP_SCAN));

        // A distilled kind page is compile-owned (a hand-written FLOWS.md
        // without ledger entries would be removed) and keeps a custom
        // registry title across recompiles (force_meta stays off).
        let rep = ingest_entries(
            &data,
            &[draft("flow", "先出样品再报价")],
            ENTRY_STATUS_ACCEPTED,
            2_000,
        )
        .unwrap();
        assert_eq!(rep.added.len(), 1);
        compile(&data, &ws, 50, 65_536).unwrap();
        let mut reg = load_registry(&data);
        reg.pages
            .iter_mut()
            .find(|p| p.file == PAGE_FLOWS)
            .expect("flows page registered")
            .title = "流程沉淀".into();
        save_registry(&data, &reg).unwrap();
        compile(&data, &ws, 50, 65_536).unwrap();
        let eff = effective_pages(&data);
        assert_eq!(
            eff.iter()
                .find(|p| p.file == PAGE_FLOWS)
                .map(|p| p.title.as_str()),
            Some("流程沉淀")
        );

        // Files the registry never saw still surface with a stem-fallback title.
        std::fs::write(compiled_dir(&data).join("EXTRA.md"), "x\n").unwrap();
        let eff = effective_pages(&data);
        assert!(eff
            .iter()
            .any(|p| p.file == "EXTRA.md" && p.title == "EXTRA"));
    }

    #[test]
    fn generic_domain_replaces_code_pages_and_distills_onto_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("notes.md"), "# 市场计划\n## 渠道\n线上投放\n").unwrap();
        compile(&data, &ws, 50, 65_536).unwrap();

        // No build markers → generic profile: outline page, no code pages.
        let profile = load_profile(&data).expect("profile persisted");
        assert_eq!(profile.domain, WikiProfile::DOMAIN_GENERIC);
        assert!(compiled_dir(&data).join(OUTLINE_PAGE).is_file());
        assert!(!compiled_dir(&data).join(COMPILED_MODULES).is_file());
        let eff = effective_pages(&data);
        assert!(eff.iter().any(|p| p.file == OUTLINE_PAGE && p.title == "资料大纲"));

        // Distilled entries land on kind pages with provenance, dedup on re-add.
        let draft = |kind: &str, content: &str| ProposedEntry {
            kind: kind.into(),
            content: content.into(),
            session_id: "sess-123456789".into(),
            turn: Some(2),
            revises: None,
            idempotency_key: None,
        };
        let report = ingest_entries(
            &data,
            &[draft("flow", "先出样品再报价"), draft("term", "SOP=标准作业程序")],
            ENTRY_STATUS_ACCEPTED,
            2_000,
        )
        .unwrap();
        assert_eq!(report.added.len(), 2);
        let retry =
            ingest_entries(&data, &[draft("flow", "先出样品再报价")], ENTRY_STATUS_ACCEPTED, 2_000)
                .unwrap();
        assert!(
            retry.added.is_empty() && retry.duplicates.len() == 1,
            "same content must not be stored twice"
        );
        compile(&data, &ws, 50, 65_536).unwrap();
        let flows = show(&data, Some("FLOWS")).unwrap();
        assert!(flows[0].body.contains("先出样品再报价"));
        assert!(flows[0].body.contains("sess-123"));
        let eff = effective_pages(&data);
        assert!(eff.iter().any(|p| p.file == PAGE_FLOWS && p.title == "业务流程"));

        // Removing the only flow entry drops the flows page on next compile.
        let flow_id = load_distilled(&data)
            .into_iter()
            .find(|e| e.kind == "flow")
            .unwrap()
            .id;
        assert!(remove_distilled(&data, &flow_id).unwrap());
        assert!(!remove_distilled(&data, &flow_id).unwrap());
        compile(&data, &ws, 50, 65_536).unwrap();
        let files: Vec<String> = effective_pages(&data).into_iter().map(|p| p.file).collect();
        assert!(!files.contains(&PAGE_FLOWS.to_string()));
        assert!(files.contains(&PAGE_TERMS.to_string()));
    }

    fn draft(kind: &str, content: &str) -> ProposedEntry {
        ProposedEntry {
            kind: kind.into(),
            content: content.into(),
            session_id: "sess-abcdef123".into(),
            turn: Some(3),
            revises: None,
            idempotency_key: None,
        }
    }

    fn doc_workspace(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let ws = tmp.path().join("ws");
        let data = tmp.path().join("data");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("notes.md"), "# 计划\n").unwrap();
        (ws, data)
    }

    #[test]
    fn legacy_ledger_without_status_loads_as_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, data) = doc_workspace(&tmp);
        fs::create_dir_all(wiki_dir(&data)).unwrap();
        // Exactly the pre-proposal line format — no status/updatedAt fields.
        fs::write(
            distilled_path(&data),
            "{\"id\":\"old-1\",\"kind\":\"term\",\"content\":\"SOP=标准作业程序\",\"sessionId\":\"s1\",\"createdAt\":\"2026-10-01T00:00:00Z\"}\n",
        )
        .unwrap();
        let all = load_distilled(&data);
        assert_eq!(all.len(), 1);
        assert!(all[0].is_accepted(), "old entries were formal knowledge before");
        // And a proposal with the same content still dedups against it.
        let report =
            ingest_entries(&data, &[draft("term", "SOP=标准作业程序")], ENTRY_STATUS_PENDING, 2_000)
                .unwrap();
        assert!(report.added.is_empty());
        assert_eq!(report.duplicates[0].1.id, "old-1");
    }

    #[test]
    fn pending_stays_off_pages_until_approved() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, data) = doc_workspace(&tmp);
        let report = ingest_entries(
            &data,
            &[draft("decision", "报价统一走线上审批流")],
            ENTRY_STATUS_PENDING,
            2_000,
        )
        .unwrap();
        let id = report.added[0].id.clone();
        compile(&data, &ws, 50, 65_536).unwrap();
        // Pending: no decisions page at all — review is the authorization.
        let files: Vec<String> = effective_pages(&data).into_iter().map(|p| p.file).collect();
        assert!(!files.contains(&PAGE_DECISIONS.to_string()), "{files:?}");

        assert!(set_entry_status(&data, &id, ENTRY_STATUS_ACCEPTED).unwrap());
        compile(&data, &ws, 50, 65_536).unwrap();
        let page = show(&data, Some("DECISIONS")).unwrap();
        assert!(page[0].body.contains("报价统一走线上审批流"));
        assert!(page[0].body.contains("sess-abc"), "provenance must survive review");

        // Reject after the fact: entry leaves the ledger view on recompile.
        assert!(set_entry_status(&data, &id, ENTRY_STATUS_REJECTED).unwrap());
        compile(&data, &ws, 50, 65_536).unwrap();
        let files: Vec<String> = effective_pages(&data).into_iter().map(|p| p.file).collect();
        assert!(!files.contains(&PAGE_DECISIONS.to_string()));
        // A rejected fact is still "seen": re-proposing is a duplicate, not a new pending.
        let again =
            ingest_entries(&data, &[draft("decision", "报价统一走线上审批流")], ENTRY_STATUS_PENDING, 2_000)
                .unwrap();
        assert!(again.added.is_empty() && again.duplicates.len() == 1);
    }

    #[test]
    fn near_duplicates_and_revisions_collapse_one_fact() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, data) = doc_workspace(&tmp);
        let first = ingest_entries(
            &data,
            &[draft("flow", "先出样品、再报价。")],
            ENTRY_STATUS_ACCEPTED,
            2_000,
        )
        .unwrap();
        let id = first.added[0].id.clone();
        // Reworded only by punctuation/spacing → same fact.
        let reword = ingest_entries(
            &data,
            &[draft("flow", "先出样品 再报价")],
            ENTRY_STATUS_ACCEPTED,
            2_000,
        )
        .unwrap();
        assert!(reword.added.is_empty() && reword.duplicates.len() == 1);

        // Explicit revision updates the entry in place; page shows the fix.
        let mut revise = draft("flow", "先出样品，报价前必须先核对成本");
        revise.revises = Some(id.clone());
        let done =
            ingest_entries(&data, &[revise], ENTRY_STATUS_ACCEPTED, 2_000).unwrap();
        assert_eq!(done.revised.len(), 1);
        assert_eq!(done.revised[0].revision_of.as_deref(), Some(id.as_str()));
        let all = load_distilled(&data);
        assert_eq!(all.len(), 1, "revision must not append a second entry");
        assert!(all[0].content.contains("核对成本"));
        assert!(all[0].updated_at.is_some());

        // Revising to content another entry already holds is a conflict.
        let other = ingest_entries(
            &data,
            &[draft("flow", "月底统一对账")],
            ENTRY_STATUS_ACCEPTED,
            2_000,
        )
        .unwrap();
        let mut collide = draft("flow", "月底统一对账");
        collide.revises = Some(id);
        let bad = ingest_entries(&data, &[collide], ENTRY_STATUS_ACCEPTED, 2_000).unwrap();
        assert_eq!(bad.rejected.len(), 1, "{bad:?}");
        assert!(bad.rejected[0].1.contains(&other.added[0].id));
    }

    #[test]
    fn ingest_validates_kind_content_and_length() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, data) = doc_workspace(&tmp);
        let bad = ingest_entries(
            &data,
            &[
                draft("opinion", "我觉得还行"),
                draft("flow", "   "),
                draft("term", &"超".repeat(500)),
            ],
            ENTRY_STATUS_PENDING,
            200,
        )
        .unwrap();
        assert_eq!(bad.rejected.len(), 3, "{bad:?}");
        assert!(bad.rejected[0].1.contains("类型"));
        assert!(bad.rejected[1].1.contains("空"));
        assert!(bad.rejected[2].1.contains("超长"));
        assert!(load_distilled(&data).is_empty(), "nothing invalid may land");

        let mut ghost = draft("flow", "修订幽灵条目");
        ghost.revises = Some("missing-id".into());
        let gone = ingest_entries(&data, &[ghost], ENTRY_STATUS_ACCEPTED, 200).unwrap();
        assert!(gone.rejected[0].1.contains("不存在"));
    }

    #[test]
    fn edit_and_undo_reflect_on_recompiled_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let (ws, data) = doc_workspace(&tmp);
        let report = ingest_entries(
            &data,
            &[draft("decision", "默认分支用 master"), draft("decision", "另一个流程条目")],
            ENTRY_STATUS_ACCEPTED,
            2_000,
        )
        .unwrap();
        let id = report.added[0].id.clone();
        compile(&data, &ws, 50, 65_536).unwrap();
        assert!(show(&data, Some("DECISIONS")).unwrap()[0].body.contains("master"));

        assert!(edit_entry(&data, &id, Some("默认分支用 develop"), None, 2_000).unwrap());
        compile(&data, &ws, 50, 65_536).unwrap();
        let page = show(&data, Some("DECISIONS")).unwrap()[0].body.clone();
        assert!(page.contains("develop") && !page.contains("master"));

        // Editing into another entry's content is refused.
        let dup = edit_entry(&data, &id, Some("另一个流程条目"), None, 2_000);
        assert!(dup.err().is_some_and(|e| e.to_string().contains("重复")));

        // Undo (user revokes the fact): removed from the ledger and the page.
        assert!(remove_distilled(&data, &id).unwrap());
        compile(&data, &ws, 50, 65_536).unwrap();
        assert!(!show(&data, Some("DECISIONS")).unwrap()[0].body.contains("develop"));
    }
}

/// Preserve an explicitly requested knowledge reply when no project has been
/// chosen. This only creates drafts; it never selects a project or publishes.
pub fn draft_requested_reply(data_dir: &Path, session_id: &str, source_id: &str, request: &str, answer: &str, max_bytes: usize) -> crate::Result<Vec<WikiDraft>> {
    let lower=request.to_lowercase();
    if ["不要保存","不用保存","无需保存","别保存","不要写入","不写入","不要记录"].iter().any(|phrase|lower.contains(phrase)){return Ok(Vec::new());}
    let target=lower.contains("wiki") || lower.contains("知识库") || lower.contains("知识中");
    let save=["保存","写入","存入","记录","整理到","学习"].iter().any(|verb|lower.contains(verb));
    if !target || !save || answer.trim().is_empty() || max_bytes<4 {return Ok(Vec::new());}
    let mut chunks=Vec::new();let mut chunk=String::new();
    for c in answer.trim().chars(){
        if chunk.len()+c.len_utf8()>max_bytes {chunks.push(std::mem::take(&mut chunk));}
        chunk.push(c);
    }
    if !chunk.trim().is_empty(){chunks.push(chunk);}
    let mut drafts=Vec::new();
    for (index,content) in chunks.into_iter().enumerate(){
        let result=submit_wiki(data_dir,&WikiSubmission{kind:"term".into(),content,mode:"propose".into(),revises:None,idempotency_key:Some(format!("reply:{source_id}:{index}")),session_id:session_id.into(),turn:None,workspace:None,space_id:None},max_bytes)?;
        if let WikiSubmitResult::ProjectRequired{draft}=result {drafts.push(draft);}
    }
    Ok(drafts)
}

/// A model may suggest one folder component, never a location.
pub fn validate_project_folder_name(raw: &str) -> crate::Result<String> {
    let name=raw.trim();
    let stem=name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved=["CON","PRN","AUX","NUL","COM1","COM2","COM3","COM4","COM5","COM6","COM7","COM8","COM9","LPT1","LPT2","LPT3","LPT4","LPT5","LPT6","LPT7","LPT8","LPT9"];
    if name.is_empty() || name=="." || name==".." || name.ends_with('.') || name.len()>120 || reserved.contains(&stem.as_str()) || name.chars().any(|c|c.is_control() || matches!(c,'/'|'\\'|':'|'*'|'?'|'"'|'<'|'>'|'|')) {
        return Err(crate::Error::Invalid("项目名必须是单个有效文件夹名，不能包含路径或系统保留名称。".into()));
    }
    Ok(name.into())
}

pub fn remember_draft_project_name(data_dir:&Path,draft_id:&str,name:&str)->crate::Result<()> {
    let name=validate_project_folder_name(name)?;
    let lock=compile_lock(&drafts_dir(data_dir));let _guard=lock.lock().unwrap_or_else(|p|p.into_inner());
    let mut draft=load_draft(data_dir,draft_id).ok_or_else(||crate::Error::Invalid("草稿已不存在".into()))?;
    draft.project_name=Some(name);
    fs::write(draft_path(data_dir,draft_id),serde_json::to_vec_pretty(&draft)?)?;
    Ok(())
}

pub fn project_name_from_model_reply(reply:&str)->crate::Result<String>{
    let raw=reply.trim();
    let raw=raw.strip_prefix("```json").or_else(||raw.strip_prefix("```")).map(|s|s.trim().strip_suffix("```").unwrap_or(s).trim()).unwrap_or(raw);
    let value:serde_json::Value=serde_json::from_str(raw).map_err(|_|crate::Error::Invalid("模型未返回有效的项目名，请重新生成或手动填写。".into()))?;
    validate_project_folder_name(value["name"].as_str().unwrap_or(""))
}

/// Attach a saved personal document to the legacy draft notification. This
/// keeps ACP v2 and older clients compatible without inventing another event.
pub fn mark_draft_knowledge(data:&Path,draft:&mut WikiDraft,document:&str)->crate::Result<()> {
    draft.knowledge_document_id=Some(document.into());
    fs::write(draft_path(data,&draft.draft_id),serde_json::to_vec_pretty(draft)?)?;Ok(())
}

pub fn save_unbound_knowledge(store:&crate::store::Store,data:&Path,result:&mut WikiSubmitResult)->crate::Result<()> {
    if let WikiSubmitResult::ProjectRequired{draft}=result {
        if draft.mode=="write" && draft.knowledge_document_id.is_none() {
            let item=save_conversation_knowledge(store,&draft.session_id,&format!("draft:{}",draft.draft_id),&draft.content)?;
            mark_draft_knowledge(data,draft,&item.id)?;
        }
    }
    Ok(())
}

fn save_conversation_knowledge(store:&crate::store::Store,session:&str,source:&str,answer:&str)->crate::Result<crate::store::sqlite::KnowledgeItem>{
    store.knowledge_save_item("local-user",&crate::store::sqlite::KnowledgeItemInput{
        title:answer.lines().find(|l|!l.trim().is_empty()).unwrap_or("对话整理").trim_start_matches('#').trim().chars().take(60).collect(),
        kind:"article".into(),body:answer.into(),notes:format!("来源对话：{session}；模型整理"),document_id:None,expected_version_id:None,source_id:Some(format!("conversation:{source}"))
    })
}

/// Called only after an explicitly requested save, on a successful completed
/// turn. Store the full answer once; split legacy notifications all point to
/// the same document instead of splitting one article into multiple projects.
pub fn capture_requested_reply(store:&crate::store::Store,data:&Path,session:&str,source:&str,request:&str,answer:&str,max_bytes:usize)->crate::Result<Vec<WikiDraft>> {
    let mut drafts=draft_requested_reply(data,session,source,request,answer,max_bytes)?;
    if !drafts.is_empty() {
        let item=save_conversation_knowledge(store,session,&format!("reply:{source}"),answer)?;
        for draft in &mut drafts {mark_draft_knowledge(data,draft,&item.id)?;}
        // The complete answer is durable as ONE document. Keep one legacy
        // notification instead of flooding the conversation with one card per
        // byte-budget fragment. The plain draft-only API stays unchanged.
        for redundant in drafts.iter().skip(1) {fs::remove_file(draft_path(data,&redundant.draft_id))?;}
        drafts.truncate(1);
    }
    Ok(drafts)
}


/// An active working directory is context, not a mandatory knowledge owner.
/// Explicit project/Wiki wording still selects the existing project path.
pub fn request_targets_personal_knowledge(request:&str)->bool {
    let text=request.to_lowercase();
    (text.contains("知识库")||text.contains("知识中")||text.contains("本地知识"))
      && !text.contains("wiki") && !text.contains("项目文档") && !text.contains("项目知识")
}
