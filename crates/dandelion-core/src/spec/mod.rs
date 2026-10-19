//! Spec pipeline (C16). Same `agent_loop`; write pin is a second directory needle.

pub mod schema;

pub use schema::{
    diff_specs, new_spec_id, parse_plan, parse_spec, render_spec, PlanDoc, SpecDiff, SpecDoc,
    SpecMode, SpecStatus, DIFF_FILE, INVALID_PREFIX, META_FILE, PIN_MSG, PLAN_FILE, SPEC_FILE,
    UNCHANGED,
};

use crate::sandbox::WorkspaceFs;
use crate::wiki;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecMeta {
    pub id: String,
    pub mode: SpecMode,
    pub status: SpecStatus,
    pub version: u32,
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecHandle {
    pub session_id: crate::ids::SessionId,
    pub spec_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecSnapshot {
    pub spec_id: String,
    pub session_id: String,
    pub status: SpecStatus,
    pub version: u32,
    pub mode: SpecMode,
    pub path: String,
    pub spec: Option<SpecDoc>,
    pub plan: Option<PlanDoc>,
    pub diff_markdown: Option<String>,
}

pub fn spec_root(workspace: &Path) -> PathBuf {
    workspace.join(".dandelion").join("specs")
}

pub fn spec_dir(workspace: &Path, spec_id: &str) -> PathBuf {
    spec_root(workspace).join(spec_id)
}

pub fn version_path(dir: &Path, version: u32) -> PathBuf {
    dir.join("versions").join(format!("v{version:04}.md"))
}

pub fn generator_segment(spec_id: &str, spec_dir: &Path) -> String {
    format!(
        "规格计划（Generator）：只解释并写出 SPEC.md。front matter 必须含 id/version/status/goal/acceptance（acceptance 至少一条）。id 用 {spec_id}。先读工作区再写 Spec。只允许把文件写在 {} 下（write_file/patch）。不要改业务代码，不要用 terminal/process/MCP。",
        spec_dir.display()
    )
}

pub fn adapter_segment(spec_id: &str, spec_dir: &Path) -> String {
    format!(
        "规格计划（Adapter）：把当前 SPEC.md 编译成 PLAN.md。front matter 必须含 spec_id（{spec_id}）、spec_version、steps（至少一步，id 唯一、title 非空）。步骤必须是工作区可验证的活，不要把「调用子代理/MCP」当唯一内容。只写 {} 。",
        spec_dir.display()
    )
}

pub fn plan_inject_segment(plan_text: &str) -> String {
    format!("可执行计划（plan 段）：\n{plan_text}")
}

/// Second directory needle after `WorkspaceFs::resolve`. Only write_file/patch.
pub fn write_guard(
    name: &str,
    args: &serde_json::Value,
    fs: &WorkspaceFs,
    spec_write_root: &Path,
) -> Option<String> {
    if !matches!(name, "write_file" | "patch") {
        return None;
    }
    let raw = args.get("path")?.as_str()?;
    let resolved = fs.resolve(raw).ok()?;
    if wiki::is_under(spec_write_root, &resolved) {
        None
    } else {
        Some(PIN_MSG.into())
    }
}

pub fn validate_written(
    name: &str,
    args: &serde_json::Value,
    fs: &WorkspaceFs,
    spec_write_root: &Path,
    max_bytes: usize,
) -> Option<String> {
    if !matches!(name, "write_file" | "patch") {
        return None;
    }
    let raw = args.get("path")?.as_str()?;
    let resolved = fs.resolve(raw).ok()?;
    if !wiki::is_under(spec_write_root, &resolved) {
        return Some(PIN_MSG.into());
    }
    let name = resolved.file_name()?.to_string_lossy();
    let text = std::fs::read_to_string(&resolved).ok()?;
    let err = if name == SPEC_FILE {
        parse_spec(&text, max_bytes).err()
    } else if name == PLAN_FILE {
        parse_plan(&text, max_bytes).err()
    } else {
        None
    };
    err.map(|e| format!("{INVALID_PREFIX}{e}"))
}

pub fn read_meta(dir: &Path) -> crate::Result<SpecMeta> {
    let p = dir.join(META_FILE);
    let raw = std::fs::read_to_string(&p)?;
    Ok(serde_json::from_str(&raw)?)
}

pub fn write_meta(dir: &Path, meta: &SpecMeta) -> crate::Result<()> {
    std::fs::create_dir_all(dir)?;
    let raw = serde_json::to_string_pretty(meta)?;
    std::fs::write(dir.join(META_FILE), raw)?;
    Ok(())
}

pub fn snapshot_spec(dir: &Path, version: u32) -> crate::Result<()> {
    let src = dir.join(SPEC_FILE);
    if !src.is_file() {
        return Err(crate::Error::Invalid("SPEC.md is missing".into()));
    }
    let dest = version_path(dir, version);
    if dest.is_file() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dest)?;
    Ok(())
}

pub fn load_spec_file(dir: &Path, max_bytes: usize) -> Result<SpecDoc, String> {
    let text = std::fs::read_to_string(dir.join(SPEC_FILE)).map_err(|e| e.to_string())?;
    parse_spec(&text, max_bytes)
}

pub fn load_plan_file(dir: &Path, max_bytes: usize) -> Result<PlanDoc, String> {
    let text = std::fs::read_to_string(dir.join(PLAN_FILE)).map_err(|e| e.to_string())?;
    parse_plan(&text, max_bytes)
}

pub fn write_diff_file(dir: &Path, diff: &SpecDiff) -> crate::Result<()> {
    std::fs::write(dir.join(DIFF_FILE), diff.render_markdown())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::WorkspaceFs;

    #[test]
    fn write_guard_blocks_outside_spec_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        std::fs::create_dir_all(ws.join("crates")).unwrap();
        std::fs::write(ws.join("crates/foo.rs"), "x").unwrap();
        let spec = spec_dir(ws, "spc_test");
        std::fs::create_dir_all(&spec).unwrap();
        let fs = WorkspaceFs::new(ws.to_path_buf()).unwrap();
        let hit = write_guard(
            "write_file",
            &serde_json::json!({"path": "crates/foo.rs"}),
            &fs,
            &spec,
        );
        assert_eq!(hit.as_deref(), Some(PIN_MSG));
        let ok = write_guard(
            "write_file",
            &serde_json::json!({"path": ".dandelion/specs/spc_test/SPEC.md"}),
            &fs,
            &spec,
        );
        assert!(ok.is_none(), "{ok:?}");
        assert!(write_guard(
            "read_file",
            &serde_json::json!({"path": "crates/foo.rs"}),
            &fs,
            &spec,
        )
        .is_none());
    }
}
