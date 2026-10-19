//! Install a Skill from a `SKILL.md` URL or a git repository.
//!
//! Both paths land through `skills::import_into_data_dir`, so frontmatter
//! validation, id rules, and the destination layout stay in one place.
//!
//! A remotely fetched Skill is installed **disabled**. Skill bodies are
//! injected verbatim into the system prompt (`skills/inject.rs`), so unreviewed
//! remote text would become model instruction on first use. The local folder
//! import stays enable-on-import: those bytes are already on the user's disk.

use super::{get_text, require_allowed_host};
use crate::config::Settings;
use crate::skills::{import_into_data_dir, SkillListItem};
use crate::{Error, Result};
use std::path::{Path, PathBuf};

const EXCLUDED: &[&str] = &[".git", "node_modules", "target", ".venv"];
/// Depth cap for the `SKILL.md` sweep of a cloned repository.
const MAX_SCAN_DEPTH: usize = 6;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInstallReport {
    pub items: Vec<SkillListItem>,
    /// Always true for this path: review the body, then enable.
    pub installed_disabled: bool,
    pub notes: Vec<String>,
}

pub async fn install_from_url(
    data_dir: &Path,
    url: &str,
    settings: &Settings,
) -> Result<SkillInstallReport> {
    require_allowed_host(url, settings)?;
    let body = get_text(url, settings).await?;
    if body.trim().is_empty() {
        return Err(Error::Invalid(format!("empty response from {url}")));
    }
    let staging = TempDir::new(data_dir, "skill-url")?;
    let file = staging.path().join("SKILL.md");
    std::fs::write(&file, body)?;
    let item = import_into_data_dir(data_dir, &file)?;
    Ok(SkillInstallReport {
        items: vec![item],
        installed_disabled: true,
        notes: vec![format!("fetched from {url}")],
    })
}

pub async fn install_from_git(
    data_dir: &Path,
    url: &str,
    subdir: Option<&str>,
    settings: &Settings,
) -> Result<SkillInstallReport> {
    require_allowed_host(url, settings)?;
    let staging = TempDir::new(data_dir, "skill-git")?;
    let clone_dir = staging.path().join("repo");
    let out = std::process::Command::new("git")
        .args(["clone", "--depth", "1", "--", url])
        .arg(&clone_dir)
        .output()
        .map_err(|err| Error::Invalid(format!("git clone failed (is git on PATH?): {err}")))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Invalid(format!(
            "git clone {url} failed: {}",
            detail.trim()
        )));
    }

    let search_root = match subdir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(sub) => {
            let joined = clone_dir.join(sub);
            // A `--subdir` of `../..` must not read outside the clone.
            let canon_clone = dunce::canonicalize(&clone_dir).unwrap_or_else(|_| clone_dir.clone());
            let canon = dunce::canonicalize(&joined).map_err(|err| {
                Error::Invalid(format!("subdir not found in repository: {sub}: {err}"))
            })?;
            if !canon.starts_with(&canon_clone) {
                return Err(Error::Invalid(format!(
                    "subdir must stay inside the repository: {sub}"
                )));
            }
            canon
        }
        None => clone_dir.clone(),
    };

    let mut found = Vec::new();
    collect_skill_files(&search_root, 0, &mut found);
    if found.is_empty() {
        return Err(Error::Invalid(format!(
            "no SKILL.md found in {url}{}",
            subdir.map(|s| format!(" under {s}")).unwrap_or_default()
        )));
    }
    found.sort();

    let mut items = Vec::new();
    let mut notes = vec![format!("cloned from {url}")];
    for file in &found {
        match import_skill_directory(data_dir, file) {
            Ok(item) => items.push(item),
            // One malformed SKILL.md in a multi-skill repo must not sink the rest.
            Err(err) => notes.push(format!("skipped {}: {err}", file.display())),
        }
    }
    if items.is_empty() {
        return Err(Error::Invalid(format!(
            "no valid SKILL.md in {url}: {}",
            notes.join("; ")
        )));
    }
    Ok(SkillInstallReport {
        items,
        installed_disabled: true,
        notes,
    })
}

fn import_skill_directory(data_dir: &Path, skill_md: &Path) -> Result<SkillListItem> {
    let folder = skill_md.parent().ok_or_else(|| Error::Invalid("SKILL.md has no parent directory".into()))?;
    import_into_data_dir(data_dir, folder)
}

fn collect_skill_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let direct = dir.join("SKILL.md");
    if direct.is_file() {
        out.push(direct);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if EXCLUDED.iter().any(|e| *e == name) {
            continue;
        }
        collect_skill_files(&path, depth + 1, out);
    }
}

/// Staging directory under `data_dir`, removed on drop. Kept inside `data_dir`
/// so a clone never lands in the user's workspace.
struct TempDir(PathBuf);

impl TempDir {
    fn new(data_dir: &Path, tag: &str) -> Result<Self> {
        let dir = data_dir
            .join("tmp")
            .join(format!("{tag}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn settings() -> Settings {
        Settings::defaults()
    }

    #[tokio::test]
    async fn url_must_be_https_and_allowlisted() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let s = settings();
        let err = install_from_url(&data, "http://github.com/a/SKILL.md", &s)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("https"), "{err}");
        let err = install_from_url(&data, "https://evil.example/SKILL.md", &s)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("skillAllowedHosts"), "{err}");
    }

    #[tokio::test]
    async fn empty_allowlist_blocks_git_too() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = settings();
        s.registry_skill_allowed_hosts.clear();
        let err = install_from_git(&tmp.path().join("data"), "https://github.com/a/b", None, &s)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("disabled"), "{err}");
    }

    #[test]
    fn collect_finds_nested_and_skips_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("skills/one")).unwrap();
        fs::write(root.join("skills/one/SKILL.md"), "x").unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("node_modules/pkg/SKILL.md"), "x").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/SKILL.md"), "x").unwrap();
        let mut found = Vec::new();
        collect_skill_files(root, 0, &mut found);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].ends_with("skills/one/SKILL.md")
                || found[0].ends_with("skills\\one\\SKILL.md")
        );
    }

    #[test]
    fn temp_dir_removed_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let path = {
            let staging = TempDir::new(&data, "t").unwrap();
            let p = staging.path().to_path_buf();
            assert!(p.is_dir());
            p
        };
        assert!(!path.exists(), "staging must not outlive the install");
    }

    #[test]
    fn git_skill_import_keeps_supporting_files() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("skill-creator");
        fs::create_dir_all(source.join("scripts")).unwrap();
        fs::write(source.join("SKILL.md"), "---\nname: skill-creator\ndescription: create skills\n---\nUse scripts/init.py\n").unwrap();
        fs::write(source.join("scripts/init.py"), "print('ok')\n").unwrap();
        let data = tmp.path().join("data");
        import_skill_directory(&data, &source.join("SKILL.md")).unwrap();
        assert!(data.join("skills/skill-creator/scripts/init.py").is_file());
    }
}
