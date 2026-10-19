//! Project context file discovery (C4b). True source: `docs/context-files.md`.

use crate::config::Settings;
use crate::sandbox::WorkspaceFs;
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const DEV_MARKERS: &[&str] = &[
    "not a runtime skill injected into dln",
    "禁止当成 dln 运行时",
    "this file is for developers of dandelion",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextKind {
    Dandelion,
    Agents,
    Claude,
    CursorRules,
}

#[derive(Clone, Debug)]
pub struct ContextFile {
    pub rel: String,
    pub header_name: String,
    pub body: String,
}

#[derive(Clone, Debug)]
pub struct ProjectBundle {
    pub kind: Option<ContextKind>,
    pub files: Vec<ContextFile>,
    pub used_bytes: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextStatusFile {
    pub rel: String,
    pub header_name: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextStatus {
    pub injected: bool,
    pub kind: Option<String>,
    pub files: Vec<ContextStatusFile>,
}

impl ContextKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dandelion => "dandelion",
            Self::Agents => "agents",
            Self::Claude => "claude",
            Self::CursorRules => "cursorrules",
        }
    }
}

pub fn context_status(fs: &WorkspaceFs, settings: &Settings) -> ContextStatus {
    let bundle = discover_session_start(fs, settings);
    ContextStatus {
        injected: !bundle.files.is_empty(),
        kind: bundle.kind.map(ContextKind::as_str).map(str::to_string),
        files: bundle
            .files
            .iter()
            .map(|f| ContextStatusFile {
                rel: f.rel.clone(),
                header_name: f.header_name.clone(),
            })
            .collect(),
    }
}

pub fn discover_session_start(fs: &WorkspaceFs, settings: &Settings) -> ProjectBundle {
    if !settings.context_enabled {
        return ProjectBundle {
            kind: None,
            files: Vec::new(),
            used_bytes: 0,
        };
    }
    let workspace = fs.root();
    let git_root = find_git_root(workspace);
    let dirs = search_dirs(workspace, git_root.as_deref());
    let kind = select_kind(&dirs);
    let Some(kind) = kind else {
        return ProjectBundle {
            kind: None,
            files: Vec::new(),
            used_bytes: 0,
        };
    };
    let mut collected = Vec::new();
    for dir in &dirs {
        if let Some(path) = file_for_kind(dir, kind) {
            if let Some(file) = load_file(workspace, git_root.as_deref(), &path, settings) {
                collected.push(file);
            }
        }
    }
    dedup_adjacent(&mut collected);
    let files = fit_budget(collected, settings.context_max_inject_bytes);
    let used_bytes = files.iter().map(file_bytes).sum();
    ProjectBundle {
        kind: Some(kind),
        files,
        used_bytes,
    }
}

pub fn discover_from_dir(
    fs: &WorkspaceFs,
    kind: ContextKind,
    dir: &Path,
    already: &HashSet<String>,
    remaining: usize,
    settings: &Settings,
) -> Vec<ContextFile> {
    if remaining == 0 || !settings.context_enabled {
        return Vec::new();
    }
    let workspace = fs.root();
    let mut walk = Vec::new();
    let mut cur = Some(dir);
    while let Some(p) = cur {
        walk.push(p.to_path_buf());
        if p == workspace {
            break;
        }
        if !p.starts_with(workspace) {
            break;
        }
        cur = p.parent();
    }
    let mut found = Vec::new();
    for d in walk {
        if let Some(path) = file_for_kind(&d, kind) {
            if let Some(file) = load_file(workspace, Some(workspace), &path, settings) {
                if !already.contains(&file.rel) {
                    found.push(file);
                }
            }
        }
    }
    fit_budget(found, remaining)
}

pub fn render_project_segment(files: &[ContextFile]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut out = String::from("项目须知：\n");
    for f in files {
        out.push_str(&format!(
            "## {}\n\n{}\n\n",
            f.header_name,
            f.body.trim_end()
        ));
    }
    out
}

fn search_dirs(workspace: &Path, git_root: Option<&Path>) -> Vec<PathBuf> {
    let Some(git_root) = git_root else {
        return vec![workspace.to_path_buf()];
    };
    let mut dirs = Vec::new();
    let mut cur = Some(workspace);
    while let Some(p) = cur {
        dirs.push(p.to_path_buf());
        if p == git_root {
            break;
        }
        cur = p.parent();
    }
    dirs.reverse();
    dirs
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(p) = cur {
        if p.join(".git").exists() {
            return dunce::canonicalize(p)
                .ok()
                .or_else(|| Some(p.to_path_buf()));
        }
        cur = p.parent();
    }
    None
}

fn select_kind(dirs: &[PathBuf]) -> Option<ContextKind> {
    if dirs
        .iter()
        .any(|d| file_for_kind(d, ContextKind::Dandelion).is_some())
    {
        return Some(ContextKind::Dandelion);
    }
    if dirs
        .iter()
        .any(|d| file_for_kind(d, ContextKind::Agents).is_some())
    {
        return Some(ContextKind::Agents);
    }
    if dirs
        .iter()
        .any(|d| file_for_kind(d, ContextKind::Claude).is_some())
    {
        return Some(ContextKind::Claude);
    }
    if dirs
        .iter()
        .any(|d| file_for_kind(d, ContextKind::CursorRules).is_some())
    {
        return Some(ContextKind::CursorRules);
    }
    None
}

fn file_for_kind(dir: &Path, kind: ContextKind) -> Option<PathBuf> {
    match kind {
        ContextKind::Dandelion => {
            for name in [
                "DANDELION.override.md",
                ".dandelion.override.md",
                ".dandelion.md",
                "DANDELION.md",
                "蒲公英.md",
            ] {
                let p = dir.join(name);
                if p.is_file() {
                    return Some(p);
                }
            }
            None
        }
        ContextKind::Agents => {
            let over = dir.join("AGENTS.override.md");
            if over.is_file() {
                return Some(over);
            }
            let p = dir.join("AGENTS.md");
            if p.is_file() && !is_developer_agents(&p) {
                Some(p)
            } else {
                None
            }
        }
        ContextKind::Claude => {
            let p = dir.join("CLAUDE.md");
            p.is_file().then_some(p)
        }
        ContextKind::CursorRules => {
            let p = dir.join(".cursorrules");
            p.is_file().then_some(p)
        }
    }
}

fn is_developer_agents(path: &Path) -> bool {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = vec![0u8; 8192];
    let n = f.read(&mut buf).unwrap_or(0);
    let head = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
    DEV_MARKERS.iter().any(|m| head.contains(m))
}

fn load_file(
    workspace: &Path,
    allowed_root: Option<&Path>,
    path: &Path,
    settings: &Settings,
) -> Option<ContextFile> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() as usize > settings.context_max_file_bytes {
        tracing::warn!(path = %path.display(), "context file skipped: over maxFileBytes");
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.contains(&0) {
        tracing::warn!(path = %path.display(), "context file skipped: binary");
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if text.len() > settings.context_max_file_bytes {
        tracing::warn!(path = %path.display(), "context file skipped: over maxFileBytes");
        return None;
    }
    if looks_like_secret(&text) {
        tracing::warn!(path = %path.display(), "context file skipped: secret-like content");
        return None;
    }
    let resolved = dunce::canonicalize(path).ok()?;
    if crate::sandbox::is_system_path(&resolved) {
        tracing::warn!(path = %path.display(), "context file skipped: system path");
        return None;
    }
    let fence = allowed_root.unwrap_or(workspace);
    if !crate::sandbox::path_is_inside(fence, &resolved)
        && !crate::sandbox::path_is_inside(workspace, &resolved)
    {
        tracing::warn!(path = %path.display(), "context file skipped: outside git/workspace");
        return None;
    }
    let rel = resolved
        .strip_prefix(workspace)
        .or_else(|_| resolved.strip_prefix(fence))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| resolved.to_string_lossy().replace('\\', "/"));
    let header_name = path
        .file_name()
        .map(|n| {
            let parent = path.parent().and_then(|p| {
                p.strip_prefix(workspace)
                    .or_else(|_| p.strip_prefix(fence))
                    .ok()
                    .filter(|p| !p.as_os_str().is_empty())
            });
            match parent {
                Some(p) => format!(
                    "{}/{}",
                    p.to_string_lossy().replace('\\', "/"),
                    n.to_string_lossy()
                ),
                None => n.to_string_lossy().into_owned(),
            }
        })
        .unwrap_or_else(|| rel.clone());
    Some(ContextFile {
        rel,
        header_name,
        body: text,
    })
}

fn looks_like_secret(text: &str) -> bool {
    if text.contains("DANDELION_API_KEY") {
        return true;
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == b's' && bytes[i + 1] == b'k' && bytes[i + 2] == b'-' {
            let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            if boundary {
                let mut n = 0usize;
                let mut j = i + 3;
                while j < bytes.len()
                    && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'-')
                {
                    n += 1;
                    j += 1;
                }
                if n >= 32 {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

fn dedup_adjacent(files: &mut Vec<ContextFile>) {
    files.dedup_by(|a, b| a.body == b.body);
}

fn file_bytes(f: &ContextFile) -> usize {
    format!("## {}\n\n{}\n\n", f.header_name, f.body.trim_end()).len()
}

/// Prefer files closer to the workspace (end of git-root→workspace list).
fn fit_budget(files: Vec<ContextFile>, cap: usize) -> Vec<ContextFile> {
    if cap == 0 {
        return Vec::new();
    }
    let mut keep = vec![false; files.len()];
    let mut used = 0usize;
    for (i, f) in files.iter().enumerate().rev() {
        let n = file_bytes(f);
        if used.saturating_add(n) > cap {
            tracing::warn!(file = %f.header_name, cap, "context file skipped: over maxInjectBytes");
            continue;
        }
        keep[i] = true;
        used += n;
    }
    files
        .into_iter()
        .zip(keep)
        .filter_map(|(f, k)| k.then_some(f))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use std::fs;
    use std::process::Command;

    fn settings() -> Settings {
        let mut s = Settings::defaults();
        s.context_enabled = true;
        s.context_max_inject_bytes = 32_768;
        s.context_max_file_bytes = 65_536;
        s
    }

    #[test]
    fn dandelion_type_beats_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let _ = Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output();
        fs::write(
            root.join(".dandelion.md"),
            "DANDELION_UNIQUE_SENTENCE_AAA\n",
        )
        .unwrap();
        let nested = root.join("pkg");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("AGENTS.md"), "should-not-win\n").unwrap();
        let fs = WorkspaceFs::new(nested).unwrap();
        let b = discover_session_start(&fs, &settings());
        assert_eq!(b.kind, Some(ContextKind::Dandelion));
        let text = render_project_segment(&b.files);
        assert!(text.contains("DANDELION_UNIQUE_SENTENCE_AAA"));
        assert!(!text.contains("should-not-win"));
    }

    #[test]
    fn developer_agents_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("AGENTS.md"),
            "This file is for developers of Dandelion, not a runtime skill injected into dln.\nsecret-dev-marker\n",
        )
        .unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let b = discover_session_start(&fs, &settings());
        assert!(b.files.is_empty());
        assert!(b.kind.is_none());
    }

    #[test]
    fn outside_git_does_not_walk_parents() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".dandelion.md"), "PARENT_ONLY\n").unwrap();
        let child = tmp.path().join("ws");
        fs::create_dir(&child).unwrap();
        let fs = WorkspaceFs::new(child).unwrap();
        let b = discover_session_start(&fs, &settings());
        assert!(b.files.is_empty());
    }

    #[test]
    fn secret_like_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join(".dandelion.md"),
            "export DANDELION_API_KEY=foo\n",
        )
        .unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let b = discover_session_start(&fs, &settings());
        assert!(b.files.is_empty());
    }

    #[test]
    fn override_replaces_product_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("DANDELION.md"), "BASE_SHOULD_HIDE\n").unwrap();
        fs::write(
            tmp.path().join("DANDELION.override.md"),
            "OVERRIDE_UNIQUE_SENTENCE\n",
        )
        .unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let b = discover_session_start(&fs, &settings());
        let text = render_project_segment(&b.files);
        assert!(text.contains("OVERRIDE_UNIQUE_SENTENCE"));
        assert!(!text.contains("BASE_SHOULD_HIDE"));
    }

    #[test]
    fn dotfile_beats_dandelion_md() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".dandelion.md"), "DOTFILE_WINS\n").unwrap();
        fs::write(tmp.path().join("DANDELION.md"), "UPPERCASE_LOSES\n").unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let text = render_project_segment(&discover_session_start(&fs, &settings()).files);
        assert!(text.contains("DOTFILE_WINS"));
        assert!(!text.contains("UPPERCASE_LOSES"));
    }

    #[test]
    fn claude_fallback_when_no_dandelion() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("CLAUDE.md"), "CLAUDE_ONLY_NOTE\n").unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let b = discover_session_start(&fs, &settings());
        assert_eq!(b.kind, Some(ContextKind::Claude));
        assert!(render_project_segment(&b.files).contains("CLAUDE_ONLY_NOTE"));
    }

    #[test]
    fn user_agents_md_without_marker_is_used() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "USER_AGENTS_CONTRACT\n").unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let b = discover_session_start(&fs, &settings());
        assert_eq!(b.kind, Some(ContextKind::Agents));
        assert!(render_project_segment(&b.files).contains("USER_AGENTS_CONTRACT"));
    }

    #[test]
    fn disabled_context_discovers_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".dandelion.md"), "SHOULD_NOT_SEE\n").unwrap();
        let mut s = settings();
        s.context_enabled = false;
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let b = discover_session_start(&fs, &s);
        assert!(b.kind.is_none());
        assert!(b.files.is_empty());
    }

    #[test]
    fn max_file_bytes_skips_whole_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".dandelion.md"), "x".repeat(2000)).unwrap();
        let mut s = settings();
        s.context_max_file_bytes = 1024;
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let b = discover_session_start(&fs, &s);
        assert!(b.files.is_empty());
    }

    #[test]
    fn closer_file_kept_when_budget_tight() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let _ = Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output();
        fs::write(
            root.join(".dandelion.md"),
            format!("ROOT{}\n", "R".repeat(800)),
        )
        .unwrap();
        let nested = root.join("pkg");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join(".dandelion.md"), "NEAR_UNIQUE\n").unwrap();
        let mut s = settings();
        s.context_max_inject_bytes = 120;
        let fs = WorkspaceFs::new(nested).unwrap();
        let text = render_project_segment(&discover_session_start(&fs, &s).files);
        assert!(text.contains("NEAR_UNIQUE"), "{text}");
        assert!(!text.contains("ROOT"), "{text}");
    }

    #[test]
    fn chinese_product_filename() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("蒲公英.md"), "ZH_PRODUCT_NOTE\n").unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let text = render_project_segment(&discover_session_start(&fs, &settings()).files);
        assert!(text.contains("ZH_PRODUCT_NOTE"));
    }

    #[test]
    fn long_openai_key_skipped_short_sk_in_url_kept() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join(".dandelion.md"),
            "see https://example.com/sk-short and docs\n",
        )
        .unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let text = render_project_segment(&discover_session_start(&fs, &settings()).files);
        assert!(text.contains("sk-short"));

        fs::write(
            tmp.path().join(".dandelion.md"),
            format!("key=sk-{}\n", "a".repeat(40)),
        )
        .unwrap();
        let b = discover_session_start(&fs, &settings());
        assert!(b.files.is_empty());
    }

    #[test]
    fn git_root_parent_file_is_loaded() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let _ = Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output();
        fs::write(root.join(".dandelion.md"), "GIT_ROOT_UNIQUE\n").unwrap();
        let nested = root.join("pkg");
        fs::create_dir(&nested).unwrap();
        let fs = WorkspaceFs::new(nested).unwrap();
        let text = render_project_segment(&discover_session_start(&fs, &settings()).files);
        assert!(text.contains("GIT_ROOT_UNIQUE"), "{text}");
    }

    #[test]
    fn hermes_brand_file_is_not_a_product_type() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".hermes.md"), "HERMES_ONLY_NOTE\n").unwrap();
        fs::write(tmp.path().join("HERMES.md"), "HERMES_UPPER\n").unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let b = discover_session_start(&fs, &settings());
        assert!(b.kind.is_none());
        assert!(b.files.is_empty());
    }

    #[test]
    fn cursorrules_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".cursorrules"), "CURSORRULES_NOTE\n").unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let b = discover_session_start(&fs, &settings());
        assert_eq!(b.kind, Some(ContextKind::CursorRules));
        assert!(render_project_segment(&b.files).contains("CURSORRULES_NOTE"));
    }

    #[test]
    fn cursor_mdc_rules_never_injected() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".cursor/rules");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("team.mdc"), "MDC_MUST_NOT_APPEAR\n").unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let b = discover_session_start(&fs, &settings());
        assert!(b.files.is_empty());
        assert!(render_project_segment(&b.files).is_empty());
    }

    #[test]
    fn binary_file_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".dandelion.md"), [b'a', 0, b'b']).unwrap();
        let fs = WorkspaceFs::new(tmp.path().to_path_buf()).unwrap();
        let b = discover_session_start(&fs, &settings());
        assert!(b.files.is_empty());
    }
}
