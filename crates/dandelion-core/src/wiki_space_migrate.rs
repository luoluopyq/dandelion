//! Legacy wiki directory → knowledge-space migration (phase 2).
//!
//! Identity model: a space's identity is its `space_` id in SQLite; the
//! directory `wiki/spaces/<id>/` is presentation. Two anchors keep the map
//! durable — the `knowledge_space_workspaces` registry (queryable) and an
//! `origin.json` inside each space dir (self-describing, so a swapped or
//! restored disk can rebuild the registry). Names and paths never carry
//! identity: renaming a space or moving the workspace path resolves
//! through the workspace hash, not either string.
//!
//! Migration is copy → per-file sha256 verify → delete-source: an
//! interrupted run leaves both trees, and re-running finishes the job.
//! Nothing is ever deleted that has not been byte-verified at the
//! destination. Failures never destroy the legacy tree.

use crate::wiki::{self, SPACE_ORIGIN_FILE};
use crate::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// Frontend default subject on a single-user desktop (see KnowledgeHubPanel
/// and the Tauri IPC layer): migrated spaces must be readable there.
pub const LOCAL_SUBJECT: &str = "local-user";

/// Current on-disk origin format version.
pub const ORIGIN_SCHEMA: u32 = 1;

/// Self-description of a space directory (the reverse anchor).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpaceOrigin {
    pub schema: u32,
    pub space_id: String,
    pub workspace_hash: String,
    pub workspace_path: String,
    pub migrated_at: String,
}

pub fn read_origin(space_dir: &Path) -> Option<SpaceOrigin> {
    let raw = fs::read(space_dir.join(SPACE_ORIGIN_FILE)).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn write_origin(space_dir: &Path, origin: &SpaceOrigin) -> Result<()> {
    fs::create_dir_all(space_dir).map_err(|e| Error::Store(e.to_string()))?;
    fs::write(
        space_dir.join(SPACE_ORIGIN_FILE),
        serde_json::to_vec_pretty(origin).map_err(|e| Error::Store(e.to_string()))?,
    )
    .map_err(|e| Error::Store(e.to_string()))
}

/// One action recorded by a migration pass (for the verification report).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationEntry {
    pub space_id: String,
    pub workspace_hash: String,
    pub from: String,
    pub to: String,
    pub files: usize,
    pub action: String, // "migrated" | "repaired" | "created"
}

/// Verification report for the whole pass — what moved, what was rebuilt,
/// and what was found inconsistent but deliberately left alone.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    pub at: String,
    pub entries: Vec<MigrationEntry>,
    pub registry_restored: Vec<String>,
    pub anomalies: Vec<String>,
}

const REPORT_FILE: &str = "space-migration-report.json";

/// Best-effort merge-and-rewrite of `wiki/space-migration-report.json`
/// (cumulative across runs — the report is the audit trail, not a log
/// tail). Reporting must never fail a migration.
pub fn write_report(data_dir: &Path, report: &MigrationReport) {
    let path = wiki::wiki_dir(data_dir).join(REPORT_FILE);
    let mut merged = fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<MigrationReport>(&raw).ok())
        .unwrap_or_default();
    merged.at = report.at.clone();
    // Dedupe on merge: startup repair re-runs EVERY launch, and a standing
    // anomaly (e.g. a space dir whose origin.json is gone) must appear ONCE
    // in the audit trail, not once per boot. Distinct messages still stack.
    // The EXISTING file is normalized too: reports written before the
    // merge-dedupe existed hold per-boot duplicates, and every write since
    // self-heals that history (the trail converges clean; nothing is ever
    // lost — duplicates collapse, distinct findings all survive).
    fn dedupe_history(v: &mut Vec<String>) {
        let mut seen: Vec<String> = Vec::with_capacity(v.len());
        v.retain(|item| {
            if seen.iter().any(|s| s == item) {
                false
            } else {
                seen.push(item.clone());
                true
            }
        });
    }
    dedupe_history(&mut merged.anomalies);
    dedupe_history(&mut merged.registry_restored);
    let same_move = |a: &MigrationEntry, b: &MigrationEntry| {
        a.space_id == b.space_id
            && a.workspace_hash == b.workspace_hash
            && a.from == b.from
            && a.to == b.to
            && a.action == b.action
            && a.files == b.files
    };
    let mut kept: Vec<MigrationEntry> = Vec::with_capacity(merged.entries.len());
    for entry in std::mem::take(&mut merged.entries) {
        if !kept.iter().any(|k| same_move(k, &entry)) {
            kept.push(entry);
        }
    }
    merged.entries = kept;
    fn extend_dedup(dst: &mut Vec<String>, src: &[String]) {
        for item in src {
            if !dst.contains(item) {
                dst.push(item.clone());
            }
        }
    }
    extend_dedup(&mut merged.anomalies, &report.anomalies);
    extend_dedup(&mut merged.registry_restored, &report.registry_restored);
    for entry in &report.entries {
        if !merged.entries.iter().any(|e| same_move(e, entry)) {
            merged.entries.push(entry.clone());
        }
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_vec_pretty(&merged) {
        let _ = fs::write(path, raw);
    }
}

// ---- content-honest move ---------------------------------------------------

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|e| Error::Store(format!("read {}: {e}", path.display())))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn rel_files(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| Error::Store(e.to_string()))? {
        let entry = entry.map_err(|e| Error::Store(e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            rel_files(&path, base, out)?;
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push(rel.to_path_buf());
        }
    }
    Ok(())
}

/// Copy `src` into `dst`, verify every file's sha256 at the destination,
/// and only then delete `src`. Idempotent: re-running after an interrupted
/// copy re-copies and re-verifies every file. Returns the file count.
fn copy_verify_delete(src: &Path, dst: &Path) -> Result<usize> {
    let mut rels = Vec::new();
    rel_files(src, src, &mut rels)?;
    fs::create_dir_all(dst).map_err(|e| Error::Store(e.to_string()))?;
    for rel in &rels {
        let from = src.join(rel);
        let to = dst.join(rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::Store(e.to_string()))?;
        }
        let bytes =
            fs::read(&from).map_err(|e| Error::Store(format!("read {}: {e}", from.display())))?;
        // Write to a sibling temp file, verify, then rename — a crash
        // mid-write never leaves a truncated file under the final name.
        let tmp = to.with_extension("copying");
        fs::write(&tmp, &bytes).map_err(|e| Error::Store(format!("write {}: {e}", to.display())))?;
        let want = format!("{:x}", Sha256::digest(&bytes));
        let got = sha256_file(&tmp)?;
        if got != want {
            let _ = fs::remove_file(&tmp);
            return Err(Error::Store(format!(
                "sha256 mismatch copying {}: {want} != {got}",
                from.display()
            )));
        }
        // Retry the rename once: Windows may briefly hold the temp handle.
        if fs::rename(&tmp, &to).is_err() {
            fs::copy(&tmp, &to).map_err(|e| Error::Store(format!("copy {}: {e}", to.display())))?;
        }
        let _ = fs::remove_file(&tmp);
        // Final check on the settled destination name.
        verify_hash(&to, &want).map_err(|_| {
            Error::Store(format!("sha256 mismatch after move: {}", to.display()))
        })?;
    }
    fs::remove_dir_all(src).map_err(|e| Error::Store(format!("remove src: {e}")))?;
    Ok(rels.len())
}

fn verify_hash(path: &Path, want: &str) -> Result<()> {
    if sha256_file(path)? == want {
        Ok(())
    } else {
        Err(Error::Store("hash check failed".into()))
    }
}

// ---- ensure ----------------------------------------------------------------

/// Resolve the wiki data dir for `workspace`, migrating lazily when asked.
///
/// * Registry hit → `wiki/spaces/<id>/` (repairing a half-finished move if
///   the space dir is gone but the legacy tree still stands).
/// * Registry miss + legacy dir exists + `migrate` → create the space,
///   content-honestly move the tree, register, cache.
/// * Registry miss + no legacy dir + `migrate` → create the space eagerly
///   so a fresh wiki never lands in the legacy tree and then moves.
/// * Registry miss + no legacy dir + `!migrate` → legacy path unchanged
///   (behavior identical to before this module existed).
pub fn ensure_space_dir(
    store: &crate::store::sqlite::Store,
    data_dir: &Path,
    workspace: &Path,
    migrate: bool,
) -> Result<PathBuf> {
    // First-time resolution can race the idle compiler or another writer.
    // Serialize with the legacy compiler before copying and deleting its
    // tree; otherwise a successful ledger write can be read from a half-move.
    let legacy_lock = wiki::compile_lock(&wiki::project_data_dir(data_dir, workspace));
    let _migration = legacy_lock.lock().unwrap_or_else(|e| e.into_inner());
    let hash = wiki::workspace_hash(workspace);
    if let Some(space_id) = store.knowledge_space_for_workspace(&hash)? {
        let dir = wiki::space_data_dir(data_dir, &space_id);
        let legacy = wiki::project_data_dir(data_dir, workspace);
        let mut files = 0usize;
        let mut action = String::new();
        if legacy.exists() {
            // A half-finished move (copy done, delete never ran, or the
            // reverse): finish it content-honestly. Idempotent.
            files = copy_verify_delete(&legacy, &dir)?;
            action = "repaired".into();
        } else {
            fs::create_dir_all(&dir).map_err(|e| Error::Store(e.to_string()))?;
        }
        let origin = SpaceOrigin {
            schema: ORIGIN_SCHEMA,
            space_id: space_id.clone(),
            workspace_hash: hash.clone(),
            workspace_path: canonical_path(workspace),
            migrated_at: Utc::now().to_rfc3339(),
        };
        if read_origin(&dir).is_none() {
            write_origin(&dir, &origin)?;
        }
        wiki::cache_space_dir(data_dir, &hash, &space_id);
        if !action.is_empty() {
            write_report(
                data_dir,
                &MigrationReport {
                    at: origin.migrated_at,
                    entries: vec![MigrationEntry {
                        space_id,
                        workspace_hash: hash,
                        from: legacy.to_string_lossy().to_string(),
                        to: dir.to_string_lossy().to_string(),
                        files,
                        action,
                    }],
                    ..Default::default()
                },
            );
        }
        return Ok(dir);
    }
    if !migrate {
        return Ok(wiki::project_data_dir(data_dir, workspace));
    }
    let legacy = wiki::project_data_dir(data_dir, workspace);
    let name = workspace
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "wiki".into());
    let space = match store.knowledge_create_local_space(LOCAL_SUBJECT, &name, "local_only") {
        Ok(space) => space,
        // Same-scope name taken: keep the old name recognizable, disambiguate
        // by the workspace fingerprint (identity is the id either way).
        Err(_) => store.knowledge_create_local_space(
            LOCAL_SUBJECT,
            &format!("{name}-{}", &hash[..8]),
            "local_only",
        )?,
    };
    store.knowledge_set_space_kind(&space.id, "wiki")?;
    let dir = wiki::space_data_dir(data_dir, &space.id);
    // "migrated" only when a legacy tree actually moved; a fresh space is
    // "created" — the report must tell the two apart.
    let (files, action) = if legacy.exists() {
        (copy_verify_delete(&legacy, &dir)?, "migrated")
    } else {
        fs::create_dir_all(&dir).map_err(|e| Error::Store(e.to_string()))?;
        (0, "created")
    };
    let origin = SpaceOrigin {
        schema: ORIGIN_SCHEMA,
        space_id: space.id.clone(),
        workspace_hash: hash.clone(),
        workspace_path: canonical_path(workspace),
        migrated_at: Utc::now().to_rfc3339(),
    };
    write_origin(&dir, &origin)?;
    store.knowledge_register_workspace(&space.id, &hash, &origin.workspace_path)?;
    wiki::cache_space_dir(data_dir, &hash, &space.id);
    write_report(
        data_dir,
        &MigrationReport {
            at: origin.migrated_at.clone(),
            entries: vec![MigrationEntry {
                space_id: space.id,
                workspace_hash: hash,
                from: legacy.to_string_lossy().to_string(),
                to: dir.to_string_lossy().to_string(),
                files,
                action: action.to_string(),
            }],
            ..Default::default()
        },
    );
    Ok(dir)
}

fn canonical_path(path: &Path) -> String {
    dunce::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

// ---- registry rebuild --------------------------------------------------------

/// Rebuild missing registry rows from the `origin.json` files on disk.
/// Reports every inconsistency; deletes and renames nothing — a mismatch
/// is evidence, not something to "fix" by destroying data.
pub fn rebuild_registry(
    store: &crate::store::sqlite::Store,
    data_dir: &Path,
) -> MigrationReport {
    let mut report = MigrationReport {
        at: Utc::now().to_rfc3339(),
        ..Default::default()
    };
    let root = wiki::spaces_dir(data_dir);
    let Ok(entries) = fs::read_dir(&root) else {
        return report; // no spaces yet: nothing to rebuild, nothing anomalous
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let Some(origin) = read_origin(&dir) else {
            report
                .anomalies
                .push(format!("{dir_name}: missing or unreadable origin.json"));
            continue;
        };
        if origin.space_id != dir_name {
            report.anomalies.push(format!(
                "{dir_name}: origin.json space_id {} disagrees with directory name",
                origin.space_id
            ));
            continue;
        }
        match store.knowledge_space_for_workspace(&origin.workspace_hash) {
            Ok(Some(id)) if id == origin.space_id => {}
            Ok(Some(id)) => report.anomalies.push(format!(
                "{}: workspace hash {} is registered to {id}, origin claims {}",
                dir_name, origin.workspace_hash, origin.space_id
            )),
            Ok(None) => {
                // Row lost (fresh DB, moved disk): restore from the anchor.
                if let Err(e) = store.knowledge_register_workspace(
                    &origin.space_id,
                    &origin.workspace_hash,
                    &origin.workspace_path,
                ) {
                    report
                        .anomalies
                        .push(format!("{}: restore failed: {e}", dir_name));
                } else {
                    report.registry_restored.push(origin.space_id.clone());
                }
            }
            Err(e) => report
                .anomalies
                .push(format!("{}: registry lookup failed: {e}", dir_name)),
        }
        wiki::cache_space_dir(data_dir, &origin.workspace_hash, &origin.space_id);
    }
    report
}
