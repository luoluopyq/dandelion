use crate::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

const EXCLUDED: &[&str] = &[".git", "node_modules", "target", ".venv"];
const FRAMEWORK_PREFIX: &str = "_framework";

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillListItem {
    pub id: String,
    pub description: String,
    pub path: PathBuf,
    pub framework: bool,
    pub enabled: bool,
    pub size_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct SkillDoc {
    pub id: String,
    pub description: String,
    pub body: String,
    pub raw: String,
    pub path: PathBuf,
    pub framework: bool,
}

pub fn scan_roots(data_dir: &Path, workspace: Option<&Path>) -> Vec<SkillDoc> {
    let mut by_id = std::collections::BTreeMap::<String, SkillDoc>::new();
    scan_root(&data_dir.join("skills"), false, &mut by_id);
    if let Some(ws) = workspace {
        scan_root(&ws.join(".dandelion").join("skills"), false, &mut by_id);
    }
    by_id.into_values().collect()
}

fn scan_root(root: &Path, _nested: bool, out: &mut std::collections::BTreeMap<String, SkillDoc>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if EXCLUDED.iter().any(|e| *e == name) {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let framework =
            name == FRAMEWORK_PREFIX || name.starts_with(&format!("{FRAMEWORK_PREFIX}/"));
        if name == FRAMEWORK_PREFIX {
            scan_framework(&path, out);
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if let Ok(doc) = load_skill_md(&skill_md, framework) {
            out.insert(doc.id.clone(), doc);
        }
    }
}

fn scan_framework(root: &Path, out: &mut std::collections::BTreeMap<String, SkillDoc>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if let Ok(doc) = load_skill_md(&skill_md, true) {
            out.insert(doc.id.clone(), doc);
        }
    }
}

pub fn load_skill_md(path: &Path, framework: bool) -> Result<SkillDoc> {
    let text = fs::read_to_string(path)?;
    let (id, description, body) = parse_frontmatter(&text)?;
    Ok(SkillDoc {
        id,
        description,
        body,
        raw: text,
        path: path.to_path_buf(),
        framework,
    })
}

/// Resolve `-s` which may be an id or a path to SKILL.md / skill dir.
pub fn resolve_skill_id(raw: &str, data_dir: &Path, workspace: Option<&Path>) -> Result<String> {
    let trimmed = raw.trim();
    if is_skill_id(trimmed) {
        return Ok(trimmed.to_string());
    }
    let path = PathBuf::from(trimmed);
    let canon = dunce::canonicalize(&path)
        .map_err(|err| Error::Invalid(format!("skill path not found: {trimmed}: {err}")))?;
    let file = if canon.is_dir() {
        canon.join("SKILL.md")
    } else {
        canon
    };
    let mut allowed = vec![data_dir.join("skills")];
    if let Some(ws) = workspace {
        allowed.push(ws.join(".dandelion").join("skills"));
    }
    let allowed: Vec<PathBuf> = allowed
        .into_iter()
        .filter_map(|p| dunce::canonicalize(p).ok())
        .collect();
    let under = allowed.iter().any(|root| file.starts_with(root));
    if !under {
        return Err(Error::Invalid(
            "skill path must be under data_dir/skills or workspace/.dandelion/skills".into(),
        ));
    }
    let doc = load_skill_md(&file, false)?;
    Ok(doc.id)
}

fn is_skill_id(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn parse_frontmatter(text: &str) -> Result<(String, String, String)> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text
        .strip_prefix("---")
        .ok_or_else(|| Error::Invalid("SKILL.md must start with YAML frontmatter".into()))?;
    let rest = rest.trim_start_matches(['\r', '\n']);
    let end = rest
        .find("\n---")
        .ok_or_else(|| Error::Invalid("SKILL.md frontmatter not closed".into()))?;
    let yaml = &rest[..end];
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']).to_string();
    let mut name = None;
    let mut description = None;
    for line in yaml.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(unquote(v.trim()));
        } else if let Some(v) = line.strip_prefix("description:") {
            description = Some(unquote(v.trim()));
        }
    }
    let name = name.ok_or_else(|| Error::Invalid("SKILL.md missing name".into()))?;
    if !is_skill_id(&name) {
        return Err(Error::Invalid(format!(
            "SKILL.md name must match [a-z0-9-]{{1,64}}: {name}"
        )));
    }
    let description =
        description.ok_or_else(|| Error::Invalid("SKILL.md missing description".into()))?;
    Ok((name, description, body))
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

pub fn list_items(docs: &[SkillDoc], disabled_ids: &[String]) -> Vec<SkillListItem> {
    docs.iter()
        .map(|d| SkillListItem {
            id: d.id.clone(),
            description: d.description.clone(),
            path: d.path.clone(),
            framework: d.framework,
            enabled: !disabled_ids.iter().any(|id| id == &d.id),
            size_bytes: d.raw.trim().len(),
        })
        .collect()
}

/// Copy a SKILL.md file or skill directory into `{data_dir}/skills/<id>/`.
pub fn import_into_data_dir(data_dir: &Path, src: &Path) -> Result<SkillListItem> {
    let canon = dunce::canonicalize(src)
        .map_err(|err| Error::Invalid(format!("skill path not found: {}: {err}", src.display())))?;
    let skill_md = if canon.is_dir() {
        canon.join("SKILL.md")
    } else {
        canon.clone()
    };
    let doc = load_skill_md(&skill_md, false)?;
    if doc.framework {
        return Err(Error::Invalid("cannot import a _framework skill".into()));
    }
    let dest_dir = data_dir.join("skills").join(&doc.id);
    if dest_dir.exists() {
        fs::remove_dir_all(&dest_dir)?;
    }
    fs::create_dir_all(&dest_dir)?;
    if canon.is_dir() {
        copy_dir(&canon, &dest_dir)?;
    } else {
        fs::copy(&skill_md, dest_dir.join("SKILL.md"))?;
    }
    let dest_md = dest_dir.join("SKILL.md");
    let imported = load_skill_md(&dest_md, false)?;
    Ok(SkillListItem {
        id: imported.id,
        description: imported.description,
        path: dest_md,
        framework: false,
        enabled: true,
        size_bytes: imported.raw.trim().len(),
    })
}

pub fn export_skill_dir(docs: &[SkillDoc], id: &str, dest_parent: &Path) -> Result<PathBuf> {
    let doc = docs
        .iter()
        .find(|d| d.id == id)
        .ok_or_else(|| Error::Invalid(format!("unknown skill: {id}")))?;
    let src_dir = doc
        .path
        .parent()
        .ok_or_else(|| Error::Invalid("skill path has no parent".into()))?;
    let dest = dest_parent.join(&doc.id);
    if dest.exists() {
        return Err(Error::Invalid(format!(
            "export destination already exists: {}",
            dest.display()
        )));
    }
    copy_dir(src_dir, &dest)?;
    Ok(dest)
}

/// Delete `{data_dir}/skills/<id>/`. A project skill under
/// `{workspace}/.dandelion/skills` lives in the user's own repository, so it is
/// refused here rather than deleted behind their back.
pub fn remove_skill_dir(data_dir: &Path, docs: &[SkillDoc], id: &str) -> Result<PathBuf> {
    let doc = docs
        .iter()
        .find(|d| d.id == id)
        .ok_or_else(|| Error::Invalid(format!("unknown skill: {id}")))?;
    if doc.framework {
        return Err(Error::Invalid(format!(
            "cannot remove a _framework skill: {id}"
        )));
    }
    let dir = doc
        .path
        .parent()
        .ok_or_else(|| Error::Invalid("skill path has no parent".into()))?;
    let global_root = data_dir.join("skills");
    let canon_dir = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let canon_root =
        dunce::canonicalize(&global_root).unwrap_or_else(|_| global_root.to_path_buf());
    if !canon_dir.starts_with(&canon_root) {
        return Err(Error::Invalid(format!(
            "{id} is a project skill at {}; delete it in your repository instead",
            canon_dir.display()
        )));
    }
    fs::remove_dir_all(&canon_dir)?;
    Ok(canon_dir)
}

pub fn skill_md_path<'a>(docs: &'a [SkillDoc], id: &str) -> Result<&'a Path> {
    docs.iter()
        .find(|d| d.id == id)
        .map(|d| d.path.as_path())
        .ok_or_else(|| Error::Invalid(format!("unknown skill: {id}")))
}

pub fn open_path_in_os(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        // 打开资源管理器是本函数唯一的可见效果；承载 `start` 的 cmd 本身
        // 必须无窗运行，否则 GUI 宿主下每次都会闪一个黑色 cmd 窗口。
        let mut open = std::process::Command::new("cmd");
        open.args(["/c", "start", "", &path.to_string_lossy()]);
        crate::agent_plugins::hide_console_windows_std(&mut open);
        open.spawn()
            .map_err(|err| Error::Invalid(format!("open {}: {err}", path.display())))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|err| Error::Invalid(format!("open {}: {err}", path.display())))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|err| Error::Invalid(format!("open {}: {err}", path.display())))?;
        Ok(())
    }
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if EXCLUDED.iter().any(|e| *e == name.to_string_lossy()) {
            continue;
        }
        let to = dst.join(&name);
        let ty = entry.file_type()?;
        if ty.is_symlink() {
            continue;
        }
        if ty.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_minimal_skill() {
        let text = "---\nname: demo-skill\ndescription: hello\n---\nbody here\n";
        let (id, desc, body) = parse_frontmatter(text).unwrap();
        assert_eq!(id, "demo-skill");
        assert_eq!(desc, "hello");
        assert!(body.contains("body here"));
    }

    #[test]
    fn scan_workspace_overrides_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let ws = tmp.path().join("ws");
        fs::create_dir_all(data.join("skills/demo-skill")).unwrap();
        fs::write(
            data.join("skills/demo-skill/SKILL.md"),
            "---\nname: demo-skill\ndescription: global\n---\nglobal\n",
        )
        .unwrap();
        fs::create_dir_all(ws.join(".dandelion/skills/demo-skill")).unwrap();
        fs::write(
            ws.join(".dandelion/skills/demo-skill/SKILL.md"),
            "---\nname: demo-skill\ndescription: local\n---\nlocal\n",
        )
        .unwrap();
        let docs = scan_roots(&data, Some(&ws));
        assert_eq!(docs.len(), 1);
        assert!(docs[0].body.contains("local"));
    }

    #[test]
    fn parse_quoted_and_bom() {
        let text = "\u{feff}---\nname: \"quoted-id\"\ndescription: 'hi there'\nallowed-tools: [terminal]\n---\nbody\n";
        let (id, desc, body) = parse_frontmatter(text).unwrap();
        assert_eq!(id, "quoted-id");
        assert_eq!(desc, "hi there");
        assert_eq!(body.trim(), "body");
    }

    #[test]
    fn parse_rejects_uppercase_name() {
        let err = parse_frontmatter("---\nname: Demo\ndescription: x\n---\n").unwrap_err();
        assert!(err.to_string().contains("name"), "{err}");
    }

    #[test]
    fn parse_rejects_missing_frontmatter() {
        assert!(parse_frontmatter("# just markdown\n").is_err());
        assert!(parse_frontmatter("---\nname: a\n").is_err());
        assert!(parse_frontmatter("---\nname: ok\n---\n").is_err());
    }

    #[test]
    fn parse_rejects_underscore_and_too_long() {
        assert!(parse_frontmatter("---\nname: bad_id\ndescription: x\n---\n").is_err());
        let long = "a".repeat(65);
        assert!(parse_frontmatter(&format!("---\nname: {long}\ndescription: x\n---\n")).is_err());
    }

    #[test]
    fn scan_skips_excluded_and_marks_framework() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        fs::create_dir_all(data.join("skills/node_modules/fake/SKILL.md")).unwrap_or(());
        fs::create_dir_all(data.join("skills/node_modules/fake")).unwrap();
        fs::write(
            data.join("skills/node_modules/fake/SKILL.md"),
            "---\nname: fake\ndescription: no\n---\n",
        )
        .ok();
        fs::create_dir_all(data.join("skills/_framework/wiki")).unwrap();
        fs::write(
            data.join("skills/_framework/wiki/SKILL.md"),
            "---\nname: wiki\ndescription: fw\n---\nwiki body\n",
        )
        .unwrap();
        fs::create_dir_all(data.join("skills/ok-skill")).unwrap();
        fs::write(
            data.join("skills/ok-skill/SKILL.md"),
            "---\nname: ok-skill\ndescription: yes\n---\nuser\n",
        )
        .unwrap();
        for noise in ["target", ".venv", ".git"] {
            let dir = data.join("skills").join(noise).join("fake");
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                "---\nname: fake\ndescription: no\n---\n",
            )
            .unwrap();
        }
        let docs = scan_roots(&data, None);
        let ids: Vec<_> = docs.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"ok-skill"));
        assert!(ids.contains(&"wiki"));
        assert!(!ids.contains(&"fake"));
        let wiki = docs.iter().find(|d| d.id == "wiki").unwrap();
        assert!(wiki.framework);
        let items = list_items(&docs, &[]);
        assert!(items.iter().any(|i| i.id == "wiki" && i.framework));
    }

    #[test]
    fn resolve_skill_id_accepts_id_and_path() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let dir = data.join("skills/path-skill");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: path-skill\ndescription: p\n---\n",
        )
        .unwrap();
        assert_eq!(
            resolve_skill_id("path-skill", &data, None).unwrap(),
            "path-skill"
        );
        let via_file =
            resolve_skill_id(dir.join("SKILL.md").to_str().unwrap(), &data, None).unwrap();
        assert_eq!(via_file, "path-skill");
        let via_dir = resolve_skill_id(dir.to_str().unwrap(), &data, None).unwrap();
        assert_eq!(via_dir, "path-skill");
        let outside = tmp.path().join("other/SKILL.md");
        fs::create_dir_all(outside.parent().unwrap()).unwrap();
        fs::write(&outside, "---\nname: other\ndescription: x\n---\n").unwrap();
        assert!(resolve_skill_id(outside.to_str().unwrap(), &data, None).is_err());
    }

    #[test]
    fn missing_skills_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(scan_roots(tmp.path(), None).is_empty());
    }

    #[test]
    fn import_copies_into_data_dir_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("incoming/hello-skill");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("SKILL.md"),
            "---\nname: hello-skill\ndescription: hi\n---\nbody\n",
        )
        .unwrap();
        let data = tmp.path().join("data");
        let item = import_into_data_dir(&data, &src).unwrap();
        assert_eq!(item.id, "hello-skill");
        assert!(item.path.ends_with("SKILL.md"));
        assert!(data.join("skills/hello-skill/SKILL.md").exists());
        let dest = tmp.path().join("out");
        fs::create_dir_all(&dest).unwrap();
        let docs = scan_roots(&data, None);
        let exported = export_skill_dir(&docs, "hello-skill", &dest).unwrap();
        assert!(exported.join("SKILL.md").exists());
    }
}
