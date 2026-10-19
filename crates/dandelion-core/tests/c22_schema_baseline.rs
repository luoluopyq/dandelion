//! Phase-1 gate: the knowledge-space schema baseline.
//!
//! Locks three promises:
//! 1. a legacy sessions.sqlite upgrades in place, preserving every row;
//! 2. reopening a current DB is a true no-op (zero side effects);
//! 3. the space lifecycle API (rename / archive / goal / workspace registry)
//!    works on stable ids — names are presentation, never identity.

use dandelion_core::store::sqlite::{KnowledgeSpace, Store};
use rusqlite::params;

/// Spaces are listed ordered by name; tests must resolve by stable id,
/// never by list position — that is the point of this gate.
fn find<'a>(spaces: &'a [KnowledgeSpace], id: &str) -> &'a KnowledgeSpace {
    spaces
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("space {id} missing from listing"))
}

fn legacy_db(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    let conn = rusqlite::Connection::open(dir.join("sessions.sqlite")).unwrap();
    // Deliberately the *old* column sets (no goal/kind/archived_at, no
    // parse_report, no role, no meta/registry pages).
    conn.execute_batch(
        "
        CREATE TABLE sessions (
          id TEXT PRIMARY KEY, title TEXT NOT NULL, workspace TEXT NOT NULL DEFAULT '',
          created_at TEXT NOT NULL, updated_at TEXT NOT NULL
        );
        CREATE TABLE knowledge_spaces (
          id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, customer_id TEXT NOT NULL, name TEXT NOT NULL,
          external_policy TEXT NOT NULL DEFAULT 'local_only', created_at TEXT NOT NULL,
          UNIQUE(tenant_id, customer_id, name)
        );
        CREATE TABLE knowledge_documents (
          id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, customer_id TEXT NOT NULL, space_id TEXT NOT NULL,
          source_kind TEXT NOT NULL, source_id TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'draft',
          created_at TEXT NOT NULL, deleted_at TEXT
        );
        CREATE TABLE task_bindings (
          session_id TEXT NOT NULL, kind TEXT NOT NULL, resource_id TEXT NOT NULL,
          enabled INTEGER NOT NULL DEFAULT 1, PRIMARY KEY (session_id, kind, resource_id)
        );
        INSERT INTO sessions VALUES ('s1','旧会话','/old/ws','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
        INSERT INTO knowledge_spaces VALUES ('space_legacy','local','local','旧资料','local_only','2026-01-01T00:00:00Z');
        INSERT INTO knowledge_documents VALUES ('doc1','local','local','space_legacy','file','a.md','published','2026-01-01T00:00:00Z',NULL);
        INSERT INTO task_bindings VALUES ('s1','corpus','c1',1);
        ",
    )
    .unwrap();
    drop(conn);
}

fn snapshot(dir: &std::path::Path) -> String {
    let conn = rusqlite::Connection::open(dir.join("sessions.sqlite")).unwrap();
    let mut out = String::new();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    out.push_str(&format!("v={version}\n"));
    let mut stmt = conn
        .prepare("SELECT key,value FROM dandelion_meta ORDER BY key")
        .unwrap();
    for row in stmt
        .query_map([], |r| Ok(format!("{}={}", r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
    {
        out.push_str(&format!("meta:{}\n", row.unwrap()));
    }
    let mut stmt = conn
        .prepare("SELECT id,name,external_policy,goal,kind,archived_at FROM knowledge_spaces ORDER BY id")
        .unwrap();
    for row in stmt
        .query_map([], |r| {
            Ok(format!(
                "space:{}|{}|{}|{:?}|{}|{:?}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })
        .unwrap()
    {
        out.push_str(&format!("{}\n", row.unwrap()));
    }
    let docs: i64 = conn
        .query_row("SELECT COUNT(*) FROM knowledge_documents", [], |r| r.get(0))
        .unwrap();
    out.push_str(&format!("docs={docs}\n"));
    let role: String = conn
        .query_row("SELECT role FROM task_bindings WHERE session_id='s1'", [], |r| r.get(0))
        .unwrap();
    out.push_str(&format!("bind_role={role}\n"));
    out
}

#[test]
fn legacy_db_upgrades_in_place_and_reopen_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    legacy_db(&data);

    {
        let store = Store::open(&data, 5000).unwrap();
        // The legacy row survived the additive upgrade with DB defaults.
        let spaces = store.knowledge_local_spaces().unwrap();
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].id, "space_legacy");
        assert_eq!(spaces[0].name, "旧资料");
        assert_eq!(spaces[0].kind, "docs");
        assert!(spaces[0].goal.is_none());
        assert!(spaces[0].archived_at.is_none());
        assert_eq!(store.meta_value("schema_baseline").unwrap().as_deref(), Some("1"));
    }
    let first = snapshot(&data);

    // Second open: byte-identical observable state.
    {
        let store = Store::open(&data, 5000).unwrap();
        assert_eq!(store.meta_value("schema_baseline").unwrap().as_deref(), Some("1"));
    }
    let second = snapshot(&data);
    assert_eq!(first, second, "second open must have zero side effects");
    assert!(first.contains("v=1"), "user_version stamp missing: {first}");
    assert!(first.contains("bind_role=read"), "role default missing: {first}");
}

#[test]
fn baseline_tables_and_vec_exist_on_fresh_open() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let store = Store::open(&data, 5000).unwrap();
    let conn = rusqlite::Connection::open(data.join("sessions.sqlite")).unwrap();
    for table in [
        "dandelion_meta",
        "knowledge_space_workspaces",
        "knowledge_evidence_spans",
        "knowledge_pages",
        "knowledge_build_jobs",
        "knowledge_proposals",
    ] {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "baseline table {table} missing");
    }
    // The corpus landing table must be a live vec0 table, not a stub.
    drop(store);
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM knowledge_chunk_vec", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    // vec0 wants a native-endian float32 blob (same encoding as
    // corpus.rs `blob_f32`), not JSON.
    let blob: Vec<u8> = vec![0.5f32; 64]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    conn.execute(
        "INSERT INTO knowledge_chunk_vec (chunk_id, embedding, space_id, document_id, ordinal)
         VALUES ('chunk_a', ?1, 'space_x', 'doc_x', 0)",
        params![blob],
    )
    .unwrap();
    let hit: String = conn
        .query_row(
            "SELECT chunk_id FROM knowledge_chunk_vec WHERE space_id='space_x'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hit, "chunk_a");
}

#[test]
fn space_lifecycle_keeps_the_stable_id() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let store = Store::open(&data, 5000).unwrap();
    let space = store
        .knowledge_create_local_space("u1", "旧名", "local_only")
        .unwrap();
    assert_eq!(space.kind, "docs");

    // Rename changes presentation only; the id (identity) never moves.
    store.knowledge_rename_space(&space.id, " 新名 ").unwrap();
    let spaces = store.knowledge_local_spaces().unwrap();
    assert_eq!(spaces.len(), 1);
    assert_eq!(spaces[0].id, space.id);
    assert_eq!(spaces[0].name, "新名");
    // Duplicate names in the same scope are refused; empty names too.
    let other = store
        .knowledge_create_local_space("u1", "占用名", "local_only")
        .unwrap();
    assert!(store.knowledge_rename_space(&space.id, "占用名").is_err());
    assert!(store.knowledge_rename_space(&space.id, "  ").is_err());
    assert!(store.knowledge_rename_space("space_missing", "x").is_err());

    // Goal: trim, blank clears, unknown space errors.
    store.knowledge_set_space_goal(&space.id, Some(" 团队上手 ")).unwrap();
    assert_eq!(
        find(&store.knowledge_local_spaces().unwrap(), &space.id)
            .goal
            .as_deref(),
        Some("团队上手")
    );
    store.knowledge_set_space_goal(&space.id, Some("   ")).unwrap();
    assert!(find(&store.knowledge_local_spaces().unwrap(), &space.id)
        .goal
        .is_none());
    assert!(store.knowledge_set_space_goal("space_missing", Some("x")).is_err());

    // Archive is reversible and touches nothing else.
    store.knowledge_archive_space(&space.id, true).unwrap();
    assert!(find(&store.knowledge_local_spaces().unwrap(), &space.id)
        .archived_at
        .is_some());
    // Archiving one space leaves its sibling readable by its owner.
    assert!(store
        .knowledge_documents("local", "local", &other.id, "u1")
        .is_ok());
    store.knowledge_archive_space(&space.id, false).unwrap();
    assert!(find(&store.knowledge_local_spaces().unwrap(), &space.id)
        .archived_at
        .is_none());
    assert!(store.knowledge_archive_space("space_missing", true).is_err());

    // Workspace registry: idempotent upsert, dual lookup, path refresh.
    store
        .knowledge_register_workspace(&space.id, "hash_a", "C:/ws/a")
        .unwrap();
    store
        .knowledge_register_workspace(&space.id, "hash_a", "D:/moved/a")
        .unwrap();
    assert_eq!(
        store.knowledge_workspace_for_space(&space.id).unwrap().unwrap(),
        ("hash_a".into(), "D:/moved/a".into())
    );
    assert_eq!(
        store.knowledge_space_for_workspace("hash_a").unwrap().as_deref(),
        Some(space.id.as_str())
    );
    assert!(store.knowledge_space_for_workspace("hash_none").unwrap().is_none());
}
