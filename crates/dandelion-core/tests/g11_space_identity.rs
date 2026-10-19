//! G11: Wiki knowledge-space identity (unification phase 2).
//!
//! Identity is the `knowledge_spaces` id — never a name, never a path. The
//! directory `wiki/spaces/<id>/` is presentation, kept durable by two
//! anchors: the `knowledge_space_workspaces` registry row (queryable) and
//! the self-describing `origin.json` inside the space dir (survives a
//! swapped or restored disk). These tests pin the work-order migration
//! requirements: idempotent re-runs, rename-proof identity, an interrupted
//! move finished by the next pass, mismatch reporting that deletes and
//! renames nothing, `wiki_auto_migrate = false` meaning exactly "no space,
//! no move", registry rebuild from the origin anchors at Runtime
//! construction — and the cumulative report as the audit trail.

use dandelion_core::host::CliHost;
use dandelion_core::store::sqlite::Store;
use dandelion_core::wiki::{self, SPACE_ORIGIN_FILE};
use dandelion_core::wiki_space_migrate::{self as mig, MigrationReport};
use dandelion_core::{Runtime, ScriptedProvider, Settings};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The cumulative audit report `write_report` maintains next to the spaces.
const REPORT_FILE: &str = "space-migration-report.json";

fn open_store(data: &Path) -> Store {
    fs::create_dir_all(data).unwrap();
    Store::open(data, 5_000).unwrap()
}

fn ensure(data: &Path, ws: &Path, migrate: bool) -> PathBuf {
    let store = open_store(data);
    mig::ensure_space_dir(&store, data, ws, migrate).expect("ensure_space_dir")
}

fn touch(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn db(data: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(data.join("sessions.sqlite")).unwrap()
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn report(data: &Path) -> MigrationReport {
    let raw = fs::read(wiki::wiki_dir(data).join(REPORT_FILE)).expect("report written");
    serde_json::from_slice(&raw).unwrap()
}

fn space_id_of(dir: &Path) -> String {
    dir.file_name().unwrap().to_string_lossy().to_string()
}

/// A never-seen workspace on first resolve gets a real knowledge space:
/// home under `wiki/spaces/<id>/`, registry row, `origin.json`, and a
/// "created" entry in the verification report. No legacy tree is left
/// behind, and no legacy tree is ever written first.
#[test]
fn fresh_workspace_gets_a_space_home_with_origin_and_report() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    fs::create_dir_all(&ws).unwrap();

    let dir = ensure(&data, &ws, true);

    assert!(dir.starts_with(wiki::spaces_dir(&data)), "home is under wiki/spaces: {dir:?}");
    assert!(!wiki::project_data_dir(&data, &ws).exists(), "legacy tree must not be used");

    let origin = mig::read_origin(&dir).expect("origin.json");
    assert_eq!(origin.schema, mig::ORIGIN_SCHEMA);
    assert_eq!(origin.space_id, space_id_of(&dir), "dir name is the space id");
    assert_eq!(origin.workspace_hash, wiki::workspace_hash(&ws));

    let store = open_store(&data);
    assert_eq!(
        store.knowledge_space_for_workspace(&wiki::workspace_hash(&ws)).unwrap().as_deref(),
        Some(origin.space_id.as_str()),
        "registry row binds the workspace hash to the space"
    );
    assert_eq!(count(&db(&data), "SELECT COUNT(*) FROM knowledge_spaces"), 1);

    let rep = report(&data);
    assert_eq!(rep.entries.len(), 1);
    assert_eq!(rep.entries[0].action, "created", "a fresh space is not reported as migrated");
    assert_eq!(rep.entries[0].files, 0);
    assert_eq!(rep.entries[0].space_id, origin.space_id);
}

/// Re-running `ensure` is a no-op — same dir, one space, one registry row,
/// no second report entry. Renaming the space (identity is the id, not the
/// name) keeps resolution on the same home.
#[test]
fn ensure_is_idempotent_and_identity_survives_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    fs::create_dir_all(&ws).unwrap();

    let dir1 = ensure(&data, &ws, true);
    let id = space_id_of(&dir1);

    let store = open_store(&data);
    store.knowledge_rename_space(&id, "改名后的空间").expect("rename");
    drop(store);

    let dir2 = ensure(&data, &ws, true);
    assert_eq!(dir1, dir2, "name is never identity");
    assert_eq!(count(&db(&data), "SELECT COUNT(*) FROM knowledge_spaces"), 1, "no duplicate space");
    assert_eq!(count(&db(&data), "SELECT COUNT(*) FROM knowledge_space_workspaces"), 1);
    assert_eq!(report(&data).entries.len(), 1, "a no-op pass writes no report entry");
}

/// A legacy quick-wiki tree is moved on first resolve: every file verified
/// byte-identical at the destination (sha256 inside `copy_verify_delete`),
/// the legacy tree gone afterwards, and the report saying "migrated" with
/// the honest file count.
#[test]
fn legacy_tree_moves_byte_verified_and_disappears() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    fs::create_dir_all(&ws).unwrap();
    let legacy = wiki::project_data_dir(&data, &ws);
    touch(&legacy, "pages.json", "{\"pages\":[]}");
    touch(&legacy, "compiled/OVERVIEW.md", "# 概览\n内容。\n");
    touch(&legacy, "versions/v1/deep/nested.md", "嵌套文件\n");
    let before = fs::read(legacy.join("compiled/OVERVIEW.md")).unwrap();

    let dir = ensure(&data, &ws, true);

    assert!(!legacy.exists(), "source is deleted only after verification");
    assert_eq!(fs::read(dir.join("compiled/OVERVIEW.md")).unwrap(), before, "bytes identical");
    assert!(dir.join("pages.json").is_file());
    assert_eq!(fs::read_to_string(dir.join("versions/v1/deep/nested.md")).unwrap(), "嵌套文件\n");

    let rep = report(&data);
    assert_eq!(rep.entries.len(), 1);
    assert_eq!(rep.entries[0].action, "migrated");
    assert_eq!(rep.entries[0].files, 3);
}

/// An interrupted move (copy done, delete never ran — both trees stand) is
/// finished by the next pass, reported as "repaired".
#[test]
fn interrupted_move_finishes_as_repair() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    fs::create_dir_all(&ws).unwrap();
    let dir = ensure(&data, &ws, true);

    // Simulate the crash window: the registry row exists and the legacy
    // tree is still standing next to the space home.
    let legacy = wiki::project_data_dir(&data, &ws);
    touch(&legacy, "compiled/OVERVIEW.md", "# 旧版残留\n");

    let dir2 = ensure(&data, &ws, true);
    assert_eq!(dir, dir2, "repair resolves through the registry, not the path");
    assert!(!legacy.exists(), "leftover legacy tree is finished off");
    assert_eq!(fs::read_to_string(dir2.join("compiled/OVERVIEW.md")).unwrap(), "# 旧版残留\n");

    let rep = report(&data);
    let last = rep.entries.last().unwrap();
    assert_eq!(last.action, "repaired");
    assert_eq!(last.files, 1);
}

/// Origin/registry disagreement is evidence, not something to "fix" by
/// destroying data: `rebuild_registry` reports the anomaly and moves,
/// renames and deletes nothing.
#[test]
fn mismatch_is_reported_and_nothing_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    fs::create_dir_all(&ws).unwrap();
    let dir_a = ensure(&data, &ws, true);
    let id_a = space_id_of(&dir_a);

    // Hand-repoint the workspace hash at a different (real) space.
    let store = open_store(&data);
    let b = store
        .knowledge_create_local_space(mig::LOCAL_SUBJECT, "接管空间", "local_only")
        .unwrap();
    db(&data)
        .execute(
            "UPDATE knowledge_space_workspaces SET space_id = ?1",
            rusqlite::params![b.id.clone()],
        )
        .unwrap();
    drop(store);

    let rep = mig::rebuild_registry(&open_store(&data), &data);
    assert!(
        rep.anomalies.iter().any(|a| a.contains("is registered to") && a.contains(&id_a)),
        "anomaly names the contested binding: {:?}",
        rep.anomalies
    );
    assert!(rep.registry_restored.is_empty());
    // Nothing was destroyed or rewritten: space A's home and anchors stand.
    assert!(dir_a.join(SPACE_ORIGIN_FILE).is_file());
    assert_eq!(fs::read(dir_a.join(SPACE_ORIGIN_FILE)).unwrap().len() > 0, true);
    assert_eq!(count(&db(&data), "SELECT COUNT(*) FROM knowledge_space_workspaces"), 1);
    assert_eq!(
        open_store(&data).knowledge_space_for_workspace(&wiki::workspace_hash(&ws)).unwrap().as_deref(),
        Some(b.id.as_str()),
        "the registry is left as found — reporting does not overwrite bindings"
    );
}

/// Registry rows lost while the `knowledge_spaces` rows survive (fresh
/// registry, partially restored backup): `rebuild_registry` restores the
/// binding from the `origin.json` anchor alone.
#[test]
fn registry_loss_restores_from_origin_anchor() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    fs::create_dir_all(&ws).unwrap();
    let dir = ensure(&data, &ws, true);
    let id = space_id_of(&dir);

    db(&data).execute("DELETE FROM knowledge_space_workspaces", []).unwrap();

    let store = open_store(&data);
    let rep = mig::rebuild_registry(&store, &data);
    assert_eq!(rep.registry_restored, vec![id.clone()]);
    assert!(rep.anomalies.is_empty(), "a recoverable loss is not an anomaly: {:?}", rep.anomalies);
    assert_eq!(
        store.knowledge_space_for_workspace(&wiki::workspace_hash(&ws)).unwrap().as_deref(),
        Some(id.as_str())
    );
    assert_eq!(ensure(&data, &ws, true), dir, "resolution is back on the space home");
}

/// Total database loss (both anchors' DB side gone): restoration is
/// impossible — the spaces row the registry row must reference is gone —
/// and the module says so honestly instead of inventing identity. The space
/// directory and its `origin.json` survive untouched.
#[test]
fn fresh_database_reports_anomaly_and_deletes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    fs::create_dir_all(&ws).unwrap();
    let dir = ensure(&data, &ws, true);

    let conn = db(&data);
    conn.execute("DELETE FROM knowledge_space_workspaces", []).unwrap();
    conn.execute("DELETE FROM knowledge_spaces", []).unwrap();
    drop(conn);

    let rep = mig::rebuild_registry(&open_store(&data), &data);
    assert!(rep.registry_restored.is_empty(), "nothing can be restored against a missing space row");
    assert!(
        rep.anomalies.iter().any(|a| a.contains("restore failed")),
        "the honest failure is reported: {:?}",
        rep.anomalies
    );
    assert!(dir.join(SPACE_ORIGIN_FILE).is_file(), "the anchor on disk is never destroyed");
}

/// `wiki_auto_migrate = false` means the old behavior exactly: the legacy
/// path is returned untouched, and no space, registry row, or report is
/// created.
#[test]
fn migration_disabled_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    fs::create_dir_all(&ws).unwrap();

    let dir = ensure(&data, &ws, false);

    assert_eq!(dir, wiki::project_data_dir(&data, &ws), "legacy path, unchanged");
    assert!(!wiki::spaces_dir(&data).exists(), "no space tree is created");
    let conn = db(&data);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM knowledge_spaces"), 0);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM knowledge_space_workspaces"), 0);
    assert!(!wiki::wiki_dir(&data).join(REPORT_FILE).exists(), "nothing to report");
}

/// End-to-end through the Runtime: with the registry wiped, constructing a
/// Runtime rebuilds the binding from the on-disk anchors, every later
/// resolution lands on the space home, and the startup pass persists the
/// repair into the cumulative report.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_startup_hook_restores_registry_before_any_resolve() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    fs::create_dir_all(&ws).unwrap();
    let dir = ensure(&data, &ws, true);
    let id = space_id_of(&dir);

    db(&data).execute("DELETE FROM knowledge_space_workspaces", []).unwrap();

    let settings = Settings::defaults_configured(); // wiki_auto_migrate default: true
    let _rt = Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(data.clone())),
        settings,
        Arc::new(ScriptedProvider::new(vec![])),
    )
    .expect("runtime with a wiped registry must still construct");

    assert_eq!(rt_wiki_dir(&_rt, &ws), dir, "resolution lands on the space home");
    assert_eq!(count(&db(&data), "SELECT COUNT(*) FROM knowledge_space_workspaces"), 1, "hook rebuilt the row");
    let rep = report(&data);
    assert!(rep.registry_restored.contains(&id), "repair is in the audit trail: {:?}", rep.registry_restored);
}

/// The audit trail is cumulative but NOT a log tail: startup repair re-runs
/// every launch, so a standing anomaly repeated verbatim must collapse to one
/// line (the real report showed one missing-origin anomaly 20 times after 20
/// boots). Distinct findings — and moves that actually did different work —
/// still stack.
#[test]
fn the_audit_trail_never_stacks_identical_findings() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path();
    let same = MigrationReport {
        at: "2026-01-01T00:00:00+00:00".into(),
        entries: vec![mig::MigrationEntry {
            space_id: "space_x".into(),
            workspace_hash: "hash_x".into(),
            from: "legacy/a".into(),
            to: "spaces/space_x".into(),
            files: 3,
            action: "created".into(),
        }],
        registry_restored: vec!["space_y".into()],
        anomalies: vec!["space_z: missing or unreadable origin.json".into()],
    };
    mig::write_report(data, &same);
    mig::write_report(data, &same);
    mig::write_report(data, &same);
    let mut different = same.clone();
    different.anomalies.push("space_w: registry lookup failed".into());
    different.entries[0].files = 7; // a repair that moved MORE files is new work
    mig::write_report(data, &different);

    let rep = report(data);
    assert_eq!(
        rep.anomalies.iter().filter(|a| a.contains("space_z")).count(),
        1,
        "an identical standing anomaly appears exactly once: {:?}",
        rep.anomalies
    );
    assert!(
        rep.anomalies.iter().any(|a| a.contains("space_w")),
        "a DISTINCT anomaly still lands: {:?}",
        rep.anomalies
    );
    assert_eq!(
        rep.registry_restored.iter().filter(|s| *s == "space_y").count(),
        1,
        "restores dedupe too: {:?}",
        rep.registry_restored
    );
    let moved: Vec<usize> = rep
        .entries
        .iter()
        .filter(|e| e.space_id == "space_x")
        .map(|e| e.files)
        .collect();
    assert_eq!(moved, vec![3, 7], "identical moves collapse, real new work stacks: {moved:?}");
}

/// A report written by the pre-dedupe code carries per-boot duplicates.
/// Any later write must NORMALIZE that history — collapse the duplicates to
/// one — while keeping every distinct finding. The trail converges clean;
/// nothing is lost, only verbatim repeats collapse.
#[test]
fn polluted_history_self_heals_on_the_next_write() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path();
    let polluted = MigrationReport {
        at: "2025-01-01T00:00:00+00:00".into(),
        entries: vec![
            mig::MigrationEntry {
                space_id: "space_p".into(),
                workspace_hash: "hash_p".into(),
                from: "legacy/p".into(),
                to: "spaces/space_p".into(),
                files: 2,
                action: "migrated".into(),
            };
            9 // nine identical boots of the same move, as the old code wrote
        ],
        registry_restored: vec!["space_r".into(); 5],
        anomalies: vec!["space_q: missing or unreadable origin.json".into(); 20],
    };
    let path = wiki::wiki_dir(data).join(REPORT_FILE);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(&polluted).unwrap()).unwrap();

    let fresh = MigrationReport {
        at: "2026-01-01T00:00:00+00:00".into(),
        anomalies: vec!["space_q: missing or unreadable origin.json".into(), "space_n: registry lookup failed".into()],
        ..Default::default()
    };
    mig::write_report(data, &fresh);

    let rep = report(data);
    assert_eq!(
        rep.anomalies.iter().filter(|a| a.contains("space_q")).count(),
        1,
        "20 identical history rows collapse to one: {:?}",
        rep.anomalies
    );
    assert!(rep.anomalies.iter().any(|a| a.contains("space_n")), "new distinct finding lands");
    assert_eq!(rep.registry_restored.iter().filter(|s| *s == "space_r").count(), 1);
    assert_eq!(rep.entries.iter().filter(|e| e.space_id == "space_p").count(), 1);
}

fn rt_wiki_dir(rt: &Runtime, ws: &Path) -> PathBuf {
    rt.wiki_dir_for(Some(ws))
}
