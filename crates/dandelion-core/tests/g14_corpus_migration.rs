//! G14: corpus.sqlite → knowledge space migration (unification phase 5).
//!
//! Pinned contract: legacy document ids survive (session bindings keyed by
//! `kind="corpus"` ids keep resolving); chunk vectors move **byte-identical**
//! (no recompute); chunk ids follow `chunk_m_{doc_id}_{ordinal}`; every
//! skipped row appears in `corpus-migration-report.json` as a named anomaly
//! and the old file is sealed (`.migrated`, never deleted) only after a
//! successful run; a garbage legacy database is kept untouched with an
//! `ok:false` report; reruns converge through `already_present`.
//! Migrated documents answer `knowledge_search` through the normal
//! authorize/scope chain — the corpus is ordinary knowledge content now.

use dandelion_core::corpus::{self, CorpusIndex};
use dandelion_core::store::Store;
use dandelion_core::wiki_space_migrate::LOCAL_SUBJECT;
use rusqlite::{params, Connection};
use std::path::Path;

/// Legacy DDL exactly as shipped before the merge (`git show
/// HEAD~n:crates/dandelion-core/src/corpus.rs`), so the fixture is the real
/// thing, not an approximation.
fn legacy_db(dir: &Path) -> Connection {
    corpus::register_sqlite_vec();
    std::fs::create_dir_all(dir.join("corpus")).unwrap();
    let conn = Connection::open(dir.join("corpus.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE documents (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            source TEXT NOT NULL,
            path TEXT NOT NULL,
            bytes INTEGER NOT NULL,
            chunks INTEGER NOT NULL,
            status TEXT NOT NULL,
            error TEXT
         );
         CREATE TABLE chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            doc_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            text TEXT NOT NULL
         );
         CREATE VIRTUAL TABLE vec_chunks USING vec0(embedding float[64]);",
    )
    .unwrap();
    conn
}

/// Deterministic 64-dim f32 blob (256 bytes) so byte-identity is checkable.
fn blob(seed: u8) -> Vec<u8> {
    (0..64u8)
        .flat_map(|i| (seed.wrapping_mul(7).wrapping_add(i) as f32).to_le_bytes())
        .collect()
}

struct FixtureDoc {
    id: String,
    name: String,
    /// (chunk rowid, embedding blob) as inserted into legacy vec_chunks.
    vec_rows: Vec<(i64, Vec<u8>)>,
}

fn add_doc(
    conn: &Connection,
    dir: &Path,
    id: &str,
    name: &str,
    status: &str,
    chunks: &[&str],
    drop_vec_for: Option<usize>,
    file_bytes: Option<&[u8]>,
    missing_file: bool,
) -> FixtureDoc {
    let path = dir.join("corpus").join(format!("{id}.txt"));
    if !missing_file {
        let joined = chunks.join("\n\n");
        let body: &[u8] = file_bytes.unwrap_or(joined.as_bytes());
        std::fs::write(&path, body).unwrap();
    }
    let bytes = if missing_file { 0 } else { chunks.join("\n\n").len() as i64 };
    conn.execute(
        "INSERT INTO documents VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
        params![id, name, format!(r"C:\orig\{name}"), path.display().to_string(), bytes, chunks.len() as i64, status],
    )
    .unwrap();
    let mut vec_rows = Vec::new();
    for (ord, text) in chunks.iter().enumerate() {
        conn.execute(
            "INSERT INTO chunks (doc_id, ordinal, text) VALUES (?1, ?2, ?3)",
            params![id, ord as i64, text],
        )
        .unwrap();
        if Some(ord) != drop_vec_for {
            let rowid = conn.last_insert_rowid();
            let b = blob((ord as u8) + 3);
            conn.execute(
                "INSERT INTO vec_chunks (rowid, embedding) VALUES (?1, ?2)",
                params![rowid, b],
            )
            .unwrap();
            vec_rows.push((rowid, b));
        }
    }
    FixtureDoc {
        id: id.to_string(),
        name: name.to_string(),
        vec_rows,
    }
}

fn read_report(dir: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(dir.join("corpus-migration-report.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn migration_moves_ids_text_and_vectors_then_seals() {
    let dir = tempfile::tempdir().unwrap();
    let doc_a = {
        let old = legacy_db(dir.path());
        let a = add_doc(
            &old,
            dir.path(),
            "指南-a1b2c3d4",
            "指南.md",
            "ready",
            &["蒲公英 深度 构建 ALPHA_TOKEN 内容", "第二段 beta 内容"],
            None,
            None,
            false,
        );
        add_doc(
            &old,
            dir.path(),
            "gamma-1111aaaa",
            "gamma.txt",
            "ready",
            &["gamma searchtoken gamma", "gamma second", "gamma third"],
            Some(1), // vec row deliberately missing for ordinal 1
            None,
            false,
        );
        add_doc(&old, dir.path(), "stuck-2222bbbb", "stuck.txt", "indexing", &["never finished"], None, None, false);
        add_doc(&old, dir.path(), "gone-3333cccc", "gone.txt", "ready", &["file vanished"], None, None, true);
        add_doc(&old, dir.path(), "binary-4444dddd", "binary.txt", "ready", &["not utf8 at all"], None, Some(&[0xFFu8, 0xFE, 0x00, 0x01]), false);
        drop(old);
        a
    };

    let idx = CorpusIndex::open(dir.path()).expect("open runs the migration");

    // Seal: the legacy file is renamed, never deleted.
    assert!(!dir.path().join("corpus.sqlite").exists());
    assert!(dir.path().join("corpus.sqlite.migrated").exists());

    // Report: successes, anomalies and the seal are all accounted for.
    let report = read_report(dir.path());
    assert_eq!(report["ok"], serde_json::json!(true), "{report:#}");
    assert!(report["space_id"].is_string());
    let space = report["space_id"].as_str().unwrap().to_string();
    let migrated: Vec<&str> = report["migrated"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["doc_id"].as_str().unwrap())
        .collect();
    assert_eq!(migrated, vec!["指南-a1b2c3d4", "gamma-1111aaaa"], "{report:#}");
    assert_eq!(report["migrated"][0]["chunks"], serde_json::json!(2));
    assert_eq!(report["migrated"][0]["vec_rows"], serde_json::json!(2));
    assert_eq!(report["migrated"][1]["vec_rows"], serde_json::json!(2));
    assert!(report["migrated"][0]["sha256"].as_str().unwrap().starts_with("sha256:"));
    let anomalies = report["anomalies"].as_array().unwrap();
    let anomaly_text = anomalies
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(anomaly_text.contains("stuck-2222bbbb") && anomaly_text.contains("indexing"), "{anomaly_text}");
    assert!(anomaly_text.contains("gone-3333cccc") && anomaly_text.contains("missing"), "{anomaly_text}");
    assert!(anomaly_text.contains("binary-4444dddd") && anomaly_text.contains("utf-8"), "{anomaly_text}");
    assert!(anomaly_text.contains("gamma-1111aaaa") && anomaly_text.contains("no vector"), "{anomaly_text}");
    assert_eq!(anomalies.len(), 4, "{report:#}");
    assert!(report["sealed"].is_string());

    // Frozen surface: list reconstructs CorpusDocs under the OLD ids.
    let docs = idx.list().unwrap();
    let ids: Vec<&str> = docs.iter().map(|d| d.id.as_str()).collect();
    assert!(ids.contains(&"指南-a1b2c3d4") && ids.contains(&"gamma-1111aaaa"), "{ids:?}");
    let a = docs.iter().find(|d| d.id == doc_a.id).unwrap();
    assert_eq!(a.name, doc_a.name);
    assert_eq!(a.status, "ready");
    assert_eq!(a.chunks, 2);
    assert!(a.bytes > 0);

    // Deterministic chunk-id convention and byte-identical vectors.
    let conn = Connection::open(dir.path().join("sessions.sqlite")).unwrap();
    for (ord, (_, expected)) in doc_a.vec_rows.iter().enumerate() {
        let chunk_id = format!("chunk_m_{}_{ord}", doc_a.id);
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT embedding FROM knowledge_chunk_vec WHERE chunk_id = ?1",
                params![chunk_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(&stored, expected, "vector for {chunk_id} must be byte-identical");
    }
    // Text moved verbatim, published and searchable through the space.
    let content: String = conn
        .query_row(
            "SELECT c.content FROM knowledge_chunks c WHERE c.id = ?1",
            params![format!("chunk_m_{}_0", doc_a.id)],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(content, "蒲公英 深度 构建 ALPHA_TOKEN 内容");
    let kind: String = conn
        .query_row("SELECT kind FROM knowledge_spaces WHERE id = ?1", params![space], |r| r.get(0))
        .unwrap();
    assert_eq!(kind, "corpus");

    // Semantic search still answers, scoped to the selected documents.
    let hits = idx
        .search(
            "gamma searchtoken gamma",
            &["指南-a1b2c3d4".to_string(), "gamma-1111aaaa".to_string()],
            4,
            16_384,
        )
        .unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].doc_id, "gamma-1111aaaa");
    assert_eq!(hits[0].doc_name, "gamma.txt");

    // The knowledge authorization chain covers the migrated rows: a
    // stranger is refused, the local Owner reads them.
    let store = Store::open(dir.path(), 5000).unwrap();
    assert!(store
        .knowledge_search("local", "local", &space, "stranger", "ALPHA_TOKEN", 5)
        .is_err());
    let hits = store
        .knowledge_search("local", "local", &space, LOCAL_SUBJECT, "ALPHA_TOKEN", 5)
        .unwrap();
    assert!(hits.iter().any(|h| h.document_id == doc_a.id), "{hits:?}");

    // Deletion: cascade through vec rows + audit row, copy removed.
    idx.delete(&doc_a.id).unwrap();
    assert!(idx.list().unwrap().iter().all(|d| d.id != doc_a.id));
    let left: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM knowledge_chunk_vec WHERE chunk_id LIKE ?1",
            params![format!("chunk_m_{}_%", doc_a.id)],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
    let audits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM knowledge_deletion_audits WHERE document_id = ?1",
            params![doc_a.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audits, 1);
    assert!(!dir.path().join("corpus").join(format!("{}.txt", doc_a.id)).exists());

    // Reopen: sealed means no-op.
    drop(idx);
    let idx = CorpusIndex::open(dir.path()).expect("reopen");
    assert_eq!(idx.list().unwrap().len(), 1);

    // Crash-before-seal replay: unsealing the file and rerunning must
    // converge via already_present without duplicating anything.
    std::fs::rename(
        dir.path().join("corpus.sqlite.migrated"),
        dir.path().join("corpus.sqlite"),
    )
    .unwrap();
    drop(idx);
    let idx = CorpusIndex::open(dir.path()).expect("rerun");
    let report = read_report(dir.path());
    let already: Vec<&str> = report["already_present"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(already.contains(&"gamma-1111aaaa"), "{report:#}");
    assert!(report["migrated"].as_array().unwrap().is_empty(), "{report:#}");
    assert_eq!(idx.list().unwrap().len(), 1, "no duplicates after rerun");
    // gamma survived deletion-free and still has exactly 2 vector rows.
    let gammas: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM knowledge_chunk_vec WHERE chunk_id LIKE 'chunk_m_gamma-1111aaaa_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(gammas, 2);
}

#[test]
fn garbage_legacy_database_is_kept_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let junk = b"this is not a sqlite database at all".to_vec();
    std::fs::write(dir.path().join("corpus.sqlite"), &junk).unwrap();

    // Opening must succeed: the corpus starts empty over the new store…
    let idx = CorpusIndex::open(dir.path()).expect("open never bricks");
    assert_eq!(idx.list().unwrap().len(), 0);

    // …the junk file is byte-untouched and unsealed…
    assert!(dir.path().join("corpus.sqlite").exists());
    assert!(!dir.path().join("corpus.sqlite.migrated").exists());
    assert_eq!(std::fs::read(dir.path().join("corpus.sqlite")).unwrap(), junk);

    // …and the report says so honestly.
    let report = read_report(dir.path());
    assert_eq!(report["ok"], serde_json::json!(false), "{report:#}");
    assert!(report["anomalies"][0].as_str().unwrap().contains("migration failed"));
    assert!(report["sealed"].is_null());

    // The new corpus works despite the broken legacy file.
    let src = dir.path().join("fresh.txt");
    std::fs::write(&src, "fresh start after broken legacy").unwrap();
    let doc = idx.import(&src, 800, 1, 1_048_576).unwrap();
    assert_eq!(idx.list().unwrap().len(), 1);
    let hits = idx.search("fresh start", &[doc.id], 4, 16_384).unwrap();
    assert!(!hits.is_empty());
}
