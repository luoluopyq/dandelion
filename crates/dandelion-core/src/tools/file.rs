use super::{Tool, ToolCtx};
use crate::acp::ToolKind;
use crate::config::Settings;
use crate::provider::types::ToolSpec;
use crate::sandbox::sandbox_error_for_model;
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

async fn fs_blocking(work: impl FnOnce() -> String + Send + 'static) -> String {
    match tokio::task::spawn_blocking(work).await {
        Ok(s) => s,
        Err(err) => format!("Error: filesystem task: {err}"),
    }
}

pub struct ReadFileTool {
    default_limit: u32,
    max_limit: u32,
    max_bytes: u64,
}

impl ReadFileTool {
    pub fn new(settings: &Settings) -> Self {
        Self {
            default_limit: settings.file_read_default_limit,
            max_limit: settings.file_read_max_limit,
            max_bytes: settings.file_read_max_bytes,
        }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a UTF-8 text file with line numbers. Path relative to workspace or absolute inside it. Use offset/limit for large files. Do not use terminal to cat files.".into(),
            source: crate::provider::types::ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 1, "description": "1-indexed start line, default 1" },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": self.max_limit,
                        "description": format!("default {}", self.default_limit)
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: invalid arguments: missing path".into(),
        };
        let resolved = match ctx.workspace.resolve(path) {
            Ok(p) => p,
            Err(err) => return sandbox_error_for_model(&err),
        };
        let max_bytes = self.max_bytes;
        let default_limit = self.default_limit;
        let max_limit = self.max_limit;
        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            .max(1) as usize;
        let mut limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(default_limit as u64) as u32;
        if limit == 0 {
            limit = default_limit;
        }
        limit = limit.min(max_limit);
        // Debug-only pause so g3_harness §10.12 can observe spawn overlapping a still-running read.
        let overlap_pause = cfg!(debug_assertions)
            && resolved
                .file_name()
                .is_some_and(|n| n == "__dandelion_g3_overlap_read");
        fs_blocking(move || {
            if overlap_pause {
                std::thread::sleep(Duration::from_millis(300));
            }
            read_file_sync(&resolved, max_bytes, offset, limit)
        })
        .await
    }
}

pub struct WriteFileTool {
    max_bytes: u64,
}

impl WriteFileTool {
    pub fn new(settings: &Settings) -> Self {
        Self {
            max_bytes: settings.file_write_max_bytes,
        }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "Create or overwrite a UTF-8 text file inside the workspace.".into(),
            source: crate::provider::types::ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: invalid arguments: missing path".into(),
        };
        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return "Error: invalid arguments: missing content".into(),
        };
        if content.len() as u64 > self.max_bytes {
            return "Error: content exceeds write max bytes".into();
        }
        let resolved = match ctx.workspace.resolve(path) {
            Ok(p) => p,
            Err(err) => return sandbox_error_for_model(&err),
        };
        let display = ctx.workspace.display_path(&resolved);
        let content = content.to_string();
        fs_blocking(move || {
            if let Some(parent) = resolved.parent() {
                if let Err(err) = fs::create_dir_all(parent) {
                    return format!("Error: {err}");
                }
            }
            match fs::write(&resolved, content.as_bytes()) {
                Ok(()) => format!("Wrote {} bytes to {display}", content.len()),
                Err(err) => format!("Error: {err}"),
            }
        })
        .await
    }
}

pub struct PatchTool {
    max_bytes: u64,
}

impl PatchTool {
    pub fn new(settings: &Settings) -> Self {
        Self {
            max_bytes: settings.file_write_max_bytes,
        }
    }
}

#[async_trait]
impl Tool for PatchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "patch".into(),
            description:
                "Replace exact text in a workspace file. Use replace_all for every occurrence."
                    .into(),
            source: crate::provider::types::ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean", "default": false }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: invalid arguments: missing path".into(),
        };
        let old = match args.get("old_string").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return "Error: invalid arguments: missing old_string".into(),
        };
        let new = match args.get("new_string").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return "Error: invalid arguments: missing new_string".into(),
        };
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let resolved = match ctx.workspace.resolve(path) {
            Ok(p) => p,
            Err(err) => return sandbox_error_for_model(&err),
        };
        let display = ctx.workspace.display_path(&resolved);
        let old = old.to_string();
        let new = new.to_string();
        let max_bytes = self.max_bytes;
        fs_blocking(move || {
            let original = match fs::read_to_string(&resolved) {
                Ok(t) => t,
                Err(err) => return format!("Error: {err}"),
            };
            match apply_patch(&original, &old, &new, replace_all) {
                Ok((patched, k)) => {
                    if patched.len() as u64 > max_bytes {
                        return "Error: patched content exceeds write max bytes".into();
                    }
                    if let Err(err) = fs::write(&resolved, patched.as_bytes()) {
                        return format!("Error: {err}");
                    }
                    format!("Patched {display} ({k} replacements)")
                }
                Err(msg) => msg,
            }
        })
        .await
    }
}

pub fn apply_patch(
    original: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> std::result::Result<(String, usize), String> {
    if old.is_empty() {
        return Err("Error: old_string must not be empty".into());
    }
    if old == new {
        return Err("Error: old_string and new_string are identical".into());
    }
    let matches = original.matches(old).count();
    if matches == 0 {
        return Err("Error: old_string not found".into());
    }
    if matches > 1 && !replace_all {
        return Err(format!(
            "Error: old_string matched {matches} times; pass replace_all=true or more context"
        ));
    }
    let patched = if replace_all {
        original.replace(old, new)
    } else {
        original.replacen(old, new, 1)
    };
    Ok((patched, matches))
}

pub struct SearchFilesTool {
    max_results: u32,
    max_file_bytes: u64,
}

impl SearchFilesTool {
    pub fn new(settings: &Settings) -> Self {
        Self {
            max_results: settings.search_max_results,
            max_file_bytes: settings.search_max_file_bytes,
        }
    }
}

#[async_trait]
impl Tool for SearchFilesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_files".into(),
            description:
                "Search file names (glob) or UTF-8 file contents (Rust regex) inside the workspace."
                    .into(),
            source: crate::provider::types::ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "target": { "type": "string", "enum": ["content", "files"], "default": "content" },
                    "path": { "type": "string", "default": "." },
                    "file_glob": { "type": "string" },
                    "limit": {
                        "type": "integer",
                        "default": self.max_results,
                        "maximum": self.max_results
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let pattern = match args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: invalid arguments: missing pattern".into(),
        };
        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("content");
        let rel = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let root = match ctx.workspace.resolve(rel) {
            Ok(p) => p,
            Err(err) => return sandbox_error_for_model(&err),
        };
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.max_results as u64)
            .min(self.max_results as u64) as usize;
        let file_glob = args.get("file_glob").and_then(|v| v.as_str());
        let glob_pat = match file_glob {
            Some(g) => match glob::Pattern::new(g) {
                Ok(p) => Some(p),
                Err(err) => return format!("Error: invalid file_glob: {err}"),
            },
            None => None,
        };
        let ws_root = ctx.workspace.root().to_path_buf();
        let max_file_bytes = self.max_file_bytes;
        let pattern = pattern.to_string();
        let target = target.to_string();
        fs_blocking(move || {
            search_files_sync(
                &root,
                &ws_root,
                &pattern,
                &target,
                glob_pat,
                limit,
                max_file_bytes,
            )
        })
        .await
    }
}

fn read_file_sync(resolved: &Path, max_bytes: u64, offset: usize, limit: u32) -> String {
    if !resolved.is_file() {
        return "Error: file not found".into();
    }
    let meta = match fs::metadata(resolved) {
        Ok(m) => m,
        Err(err) => return format!("Error: {err}"),
    };
    if meta.len() > max_bytes {
        return "Error: file too large".into();
    }
    let bytes = match fs::read(resolved) {
        Ok(b) => b,
        Err(err) => return format!("Error: {err}"),
    };
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(_) => return "Error: not utf-8 text".into(),
    };
    let all: Vec<&str> = text.lines().collect();
    if offset > all.len() && !all.is_empty() {
        return format!(
            "Error: offset {offset} past end of file ({} lines)",
            all.len()
        );
    }
    let start = offset.saturating_sub(1);
    let end = (start + limit as usize).min(all.len());
    let mut out = String::new();
    for (i, line) in all[start..end].iter().enumerate() {
        let n = start + i + 1;
        out.push_str(&format!("{n:>6}|{line}\n"));
    }
    if out.is_empty() {
        "(empty file)".into()
    } else {
        out
    }
}

fn search_files_sync(
    root: &Path,
    ws_root: &Path,
    pattern: &str,
    target: &str,
    glob_pat: Option<glob::Pattern>,
    limit: usize,
    max_file_bytes: u64,
) -> String {
    if target == "files" {
        let mut hits = Vec::new();
        walk_files(root, ws_root, &mut |rel_path, _abs| {
            if hits.len() >= limit {
                return false;
            }
            let name = rel_path.replace('\\', "/");
            let ok = match &glob_pat {
                Some(g) => {
                    g.matches(&name)
                        || g.matches(
                            Path::new(rel_path)
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or(""),
                        )
                }
                None => {
                    name.contains(pattern)
                        || Path::new(rel_path)
                            .file_name()
                            .and_then(|s| s.to_str())
                            .map(|n| n.contains(pattern))
                            .unwrap_or(false)
                }
            };
            if ok {
                hits.push(rel_path.to_string());
            }
            hits.len() < limit
        });
        if hits.is_empty() {
            "(no matches)".into()
        } else {
            hits.join("\n")
        }
    } else {
        let re = match Regex::new(pattern) {
            Ok(r) => r,
            Err(err) => return format!("Error: invalid regex: {err}"),
        };
        let mut hits = Vec::new();
        walk_files(root, ws_root, &mut |rel_path, abs| {
            if hits.len() >= limit {
                return false;
            }
            if let Some(g) = &glob_pat {
                let name = rel_path.replace('\\', "/");
                if !g.matches(&name) {
                    return true;
                }
            }
            let meta = match fs::metadata(abs) {
                Ok(m) => m,
                Err(_) => return true,
            };
            if meta.len() > max_file_bytes {
                return true;
            }
            let mut f = match fs::File::open(abs) {
                Ok(f) => f,
                Err(_) => return true,
            };
            let mut buf = Vec::new();
            if f.read_to_end(&mut buf).is_err() {
                return true;
            }
            if buf.contains(&0) {
                return true;
            }
            let Ok(text) = String::from_utf8(buf) else {
                return true;
            };
            for (i, line) in text.lines().enumerate() {
                if hits.len() >= limit {
                    break;
                }
                if re.is_match(line) {
                    hits.push(format!("{}:{}:{line}", rel_path.replace('\\', "/"), i + 1));
                }
            }
            hits.len() < limit
        });
        if hits.is_empty() {
            "(no matches)".into()
        } else {
            hits.join("\n")
        }
    }
}

fn walk_files(dir: &Path, workspace_root: &Path, visit: &mut dyn FnMut(&str, &Path) -> bool) {
    let Ok(ws_canon) = dunce::canonicalize(workspace_root) else {
        return;
    };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(cur_canon) = dunce::canonicalize(&cur) else {
            continue;
        };
        if !crate::sandbox::path_is_inside(&ws_canon, &cur_canon)
            || crate::sandbox::is_system_path(&cur_canon)
        {
            continue;
        }
        let entries = match fs::read_dir(&cur_canon) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name == ".git" || name == "target" || name == "node_modules" {
                continue;
            }
            let Ok(canon) = dunce::canonicalize(&path) else {
                continue;
            };
            if !crate::sandbox::path_is_inside(&ws_canon, &canon)
                || crate::sandbox::is_system_path(&canon)
            {
                continue;
            }
            if canon.is_dir() {
                stack.push(canon);
                continue;
            }
            let rel = canon
                .strip_prefix(&ws_canon)
                .unwrap_or(&canon)
                .to_string_lossy()
                .into_owned();
            if !visit(&rel, &canon) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::apply_patch;
    use super::walk_files;

    #[test]
    fn patch_zero_one_many() {
        assert!(apply_patch("abc", "x", "y", false).is_err());
        let (s, k) = apply_patch("abc", "b", "B", false).unwrap();
        assert_eq!(s, "aBc");
        assert_eq!(k, 1);
        let err = apply_patch("bab", "b", "B", false).unwrap_err();
        assert!(err.contains("matched 2 times"));
        let (s, k) = apply_patch("bab", "b", "B", true).unwrap();
        assert_eq!(s, "BaB");
        assert_eq!(k, 2);
    }

    #[test]
    fn walk_skips_paths_outside_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("inside.txt"), "ok").unwrap();
        let mut found = Vec::new();
        walk_files(dir.path(), dir.path(), &mut |rel, _| {
            found.push(rel.replace('\\', "/"));
            true
        });
        assert!(found.iter().any(|p| p.ends_with("inside.txt")), "{found:?}");

        #[cfg(windows)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("secret.txt"), "nope").unwrap();
            let link = dir.path().join("escape.txt");
            if std::os::windows::fs::symlink_file(outside.path().join("secret.txt"), &link).is_ok()
            {
                let mut found = Vec::new();
                walk_files(dir.path(), dir.path(), &mut |rel, _| {
                    found.push(rel.replace('\\', "/"));
                    true
                });
                assert!(
                    !found
                        .iter()
                        .any(|p| p.contains("secret") || p.contains("escape")),
                    "{found:?}"
                );
            }
        }
    }
}
