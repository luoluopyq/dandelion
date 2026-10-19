//! User-document corpus (Pixso 04), stored in the knowledge space since
//! unification phase 5. There is **no separate corpus database**: rows live
//! in `sessions.sqlite`'s `knowledge_*` tables under one `kind='corpus'`
//! space of the local tenant, documents as
//! `source_kind='corpus-file', source_id=<original path>`. The public
//! surface (`list/import/delete/search/format_segment`) is a frozen
//! compatibility wrapper — Runtime, the Tauri commands and ipc.ts call it
//! unchanged (pinned by `c24_corpus_compat_ipc`).
//!
//! A legacy `corpus.sqlite` from before the merge is migrated once at
//! `open`: chunk text and vector blobs move **byte-identically** (no
//! recompute), each document commits as one transaction with a count check
//! before commit, the run is written to `corpus-migration-report.json`, and
//! the old file is only renamed to `corpus.sqlite.migrated` after success.
//! Any failure leaves the old database untouched; nothing is silently lost.
//!
//! Retrieval is a short **user** segment. It must not enter the system prompt.

use crate::store::Store;
use crate::wiki;
use crate::wiki_space_migrate::LOCAL_SUBJECT;
use crate::{Error, Result};
use chrono::Utc;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};
use uuid::Uuid;

/// The single knowledge-store database (Store's own file).
const MAIN_DB_FILE: &str = "sessions.sqlite";
/// Pre-merge corpus database, migrated once and then sealed.
const LEGACY_DB_FILE: &str = "corpus.sqlite";
const SEALED_SUFFIX: &str = ".migrated";
const MIGRATION_REPORT: &str = "corpus-migration-report.json";
const FILES_DIR: &str = "corpus";
const DIM: usize = 64;
const BUSY_MS: u64 = 5_000;
/// The corpus space lives in the flat local scope, same as every other
/// desktop-local knowledge space.
const CORPUS_SCOPE: &str = "local";
const CORPUS_SPACE_NAME: &str = "本地资料库";
/// Fallback space name if a *docs* space already owns the default name
/// (`knowledge_spaces` enforces `UNIQUE(tenant, customer, name)`).
const CORPUS_SPACE_NAME_ALT: &str = "本地资料库·语料";
pub const SEGMENT_MARK: &str = "[知识库检索]";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorpusDoc {
    pub id: String,
    pub name: String,
    pub source: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub chunks: u32,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CorpusHit {
    pub doc_id: String,
    pub doc_name: String,
    pub ordinal: u32,
    pub text: String,
}

/// One KNN hit carrying its space, for the phase-8 unified search
/// (`Runtime::space_search`), which spans ANY resolved set of spaces rather
/// than only the corpus space. Authorization happened at the Runtime layer
/// before this query ran — this method is a dumb scoped query surface, like
/// `search`.
#[derive(Clone, Debug)]
pub struct SpaceVectorHit {
    pub space_id: String,
    pub doc_id: String,
    pub doc_name: String,
    pub ordinal: u32,
    pub text: String,
    pub chunk_id: String,
    pub version_no: i64,
}

/// One migrated document as recorded in the report (`sha256` covers the
/// file text; chunk ids follow the deterministic `chunk_m_{doc_id}_{ordinal}`
/// convention pinned by `g14_corpus_migration`).
#[derive(Clone, Debug, Serialize)]
pub struct MigratedCorpusDoc {
    pub doc_id: String,
    pub name: String,
    pub chunks: usize,
    pub vec_rows: usize,
    pub sha256: String,
}

/// Written to `corpus-migration-report.json` after every migration attempt,
/// success or failure. `anomalies` lists every skipped/missing item with a
/// reason — the report is the proof that nothing vanished silently.
#[derive(Clone, Debug, Serialize)]
pub struct CorpusMigrationReport {
    pub ok: bool,
    pub space_id: Option<String>,
    pub migrated: Vec<MigratedCorpusDoc>,
    pub already_present: Vec<String>,
    pub anomalies: Vec<String>,
    pub sealed: Option<String>,
}

impl CorpusMigrationReport {
    fn write(&self, data_dir: &Path) {
        let path = data_dir.join(MIGRATION_REPORT);
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!(%e, path = %path.display(), "corpus: cannot write migration report");
                }
            }
            Err(e) => tracing::warn!(%e, "corpus: cannot serialize migration report"),
        }
    }
}

pub struct CorpusIndex {
    data_dir: PathBuf,
    /// Provisioning path: creating the corpus space and stamping its kind
    /// goes through the same Store the rest of the product uses.
    store: Store,
    /// Query path: one more connection to `sessions.sqlite` (WAL readers do
    /// not block; writers serialize on `busy_timeout`).
    conn: Mutex<Connection>,
}

impl CorpusIndex {
    pub fn open(data_dir: &Path) -> Result<Self> {
        // Startup phase timing (work-order §二): this second `Store::open`
        // re-runs the full migration+repair path per launch; the breakdown
        // here is the evidence the dedup batch (Batch 1) will be graded on.
        let t_all = std::time::Instant::now();
        register_sqlite_vec();
        std::fs::create_dir_all(data_dir.join(FILES_DIR))?;
        let t = std::time::Instant::now();
        let store = Store::open(data_dir, BUSY_MS)?;
        tracing::debug!(elapsed_ms = t.elapsed().as_millis() as u64, "startup: corpus store open");
        let t = std::time::Instant::now();
        let conn = Self::open_conn(data_dir)?;
        tracing::debug!(elapsed_ms = t.elapsed().as_millis() as u64, "startup: corpus extra conn");
        let idx = Self {
            data_dir: data_dir.to_path_buf(),
            store,
            conn: Mutex::new(conn),
        };
        let t = std::time::Instant::now();
        idx.migrate_if_needed();
        tracing::debug!(
            elapsed_ms = t.elapsed().as_millis() as u64,
            total_ms = t_all.elapsed().as_millis() as u64,
            "startup: corpus migrate"
        );
        Ok(idx)
    }

    fn open_conn(data_dir: &Path) -> Result<Connection> {
        // register_sqlite_vec already armed the auto-extension; vec0 queries
        // on this connection would fail without it.
        let conn = Connection::open(data_dir.join(MAIN_DB_FILE))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             PRAGMA foreign_keys=ON;",
        )?;
        Ok(conn)
    }

    // ---- corpus space ----------------------------------------------------

    pub fn corpus_space_id(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let id = conn
            .query_row(
                "SELECT id FROM knowledge_spaces
                 WHERE tenant_id = ?1 AND customer_id = ?2 AND kind = 'corpus'
                 ORDER BY created_at LIMIT 1",
                params![CORPUS_SCOPE, CORPUS_SCOPE],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id)
    }

    /// Create the corpus space on first use (lazy: a fresh install with no
    /// corpus data never creates it). `knowledge_create_local_space`
    /// self-provisions the local tenant/customer rows; membership
    /// `('local-user', Owner)` is what makes the whole knowledge
    /// authorize/scope/external-policy chain apply to corpus content.
    fn ensure_space(&self) -> Result<String> {
        if let Some(id) = self.corpus_space_id()? {
            return Ok(id);
        }
        let mut last_err: Option<Error> = None;
        for name in [CORPUS_SPACE_NAME, CORPUS_SPACE_NAME_ALT] {
            match self
                .store
                .knowledge_create_local_space(LOCAL_SUBJECT, name, "local_only")
            {
                Ok(space) => {
                    self.store.knowledge_set_space_kind(&space.id, "corpus")?;
                    return Ok(space.id);
                }
                Err(e) => {
                    // A parallel creator may have won the race; re-check
                    // before treating the UNIQUE(name) collision as fatal.
                    if let Some(id) = self.corpus_space_id()? {
                        return Ok(id);
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| Error::Store("cannot create corpus space".into())))
    }

    // ---- public surface (frozen) ------------------------------------------

    pub fn list(&self) -> Result<Vec<CorpusDoc>> {
        let Some(space) = self.corpus_space_id()? else {
            return Ok(Vec::new());
        };
        let rows = {
            let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            let mut stmt = conn.prepare(
                "SELECT d.id, d.source_id,
                        (SELECT COUNT(*) FROM knowledge_document_versions v
                           JOIN knowledge_chunks c ON c.version_id = v.id
                          WHERE v.document_id = d.id)
                 FROM knowledge_documents d
                 WHERE d.space_id = ?1 AND d.deleted_at IS NULL
                 ORDER BY d.source_id",
            )?;
            let rows = stmt
                .query_map(params![space], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)? as u32,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        // CorpusDoc is reconstructed from the knowledge rows + file copies:
        // name = basename(source_id), bytes from the stored copy, status is
        // always 'ready' (the legacy 'indexing' state never lands here).
        let mut out = Vec::with_capacity(rows.len());
        for (id, source, chunks) in rows {
            let path = self.copy_path(&id);
            out.push(CorpusDoc {
                name: base_name(&source),
                bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                id,
                source,
                path,
                chunks,
                status: "ready".into(),
                error: None,
            });
        }
        Ok(out)
    }

    pub fn import(
        &self,
        src: &Path,
        chunk_chars: usize,
        min_disk_free_mb: u64,
        max_file_bytes: usize,
    ) -> Result<CorpusDoc> {
        refuse_source(&self.data_dir, src)?;
        let meta = std::fs::metadata(src)?;
        if meta.len() as usize > max_file_bytes {
            return Err(Error::Corpus(format!(
                "corpus refuse: file too large ({} > {max_file_bytes} bytes)",
                meta.len()
            )));
        }
        if let Some(free) = crate::sidecar::disk_free_bytes(&self.data_dir) {
            let need = meta.len().saturating_add(64 * 1024);
            let min_bytes = min_disk_free_mb.saturating_mul(1024 * 1024);
            if free < min_bytes || free < need {
                return Err(Error::Corpus(format!(
                    "corpus disk: need {} MiB free, have {} MiB",
                    min_disk_free_mb.max(1),
                    free / (1024 * 1024)
                )));
            }
        }
        let text = read_utf8(src)?;
        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("doc.txt")
            .to_string();
        let id = doc_id(&name);
        let dest = self
            .data_dir
            .join(FILES_DIR)
            .join(format!("{id}{}", ext_of(src)));
        std::fs::copy(src, &dest)?;
        let chunks = chunk_text(&text, chunk_chars.max(256));
        let space = self.ensure_space()?;
        let source = src.display().to_string();
        let now = Utc::now().to_rfc3339();
        let sha256 = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
        let version_id = format!("docver_{id}");
        {
            let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO knowledge_documents
                    (id, tenant_id, customer_id, space_id, source_kind, source_id, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'corpus-file', ?5, 'published', ?6)",
                params![id, CORPUS_SCOPE, CORPUS_SCOPE, space, source, now],
            )?;
            tx.execute(
                "INSERT INTO knowledge_document_versions
                    (id, document_id, version_no, sha256, content, created_at, published_at, parser_version)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, ?5, ?6)",
                params![version_id, id, sha256, text, now, crate::wiki_adapters::PARSER_VERSION],
            )?;
            // Membership lives only in knowledge_space_links; a document
            // without a link row is invisible to the space that owns it.
            tx.execute(
                "INSERT OR IGNORE INTO knowledge_space_links
                    (space_id, document_id, binding, pinned_version_no, added_by, created_at)
                 VALUES (?1, ?2, 'follow', NULL, 'migration', ?3)",
                params![space, id, now],
            )?;
            for (i, chunk) in chunks.iter().enumerate() {
                let chunk_id = format!("chunk_{id}_{i}");
                tx.execute(
                    "INSERT INTO knowledge_chunks (id, version_id, ordinal, content)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![chunk_id, version_id, i as i64, chunk],
                )?;
                tx.execute(
                    "INSERT INTO knowledge_chunk_vec
                        (chunk_id, embedding, space_id, document_id, ordinal)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![chunk_id, blob_f32(&embed(chunk)), space, id, i as i64],
                )?;
            }
            tx.commit()?;
        }
        Ok(CorpusDoc {
            id,
            name,
            source,
            path: dest,
            bytes: meta.len(),
            chunks: chunks.len() as u32,
            status: "ready".into(),
            error: None,
        })
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let Some(space) = self.corpus_space_id()? else {
            return Ok(());
        };
        let copy = self.copy_path(id);
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let tx = conn.unchecked_transaction()?;
        // Deletion is scoped to the corpus space: an id from any other
        // space must stay untouched here.
        let version_ids = {
            let mut stmt = tx.prepare(
                "SELECT v.id FROM knowledge_document_versions v
                   JOIN knowledge_documents d ON d.id = v.document_id
                  WHERE d.id = ?1 AND d.space_id = ?2",
            )?;
            let rows = stmt.query_map(params![id, space], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        if version_ids.is_empty() {
            return Ok(());
        }
        let mut cascade_chunks = 0i64;
        for vid in &version_ids {
            let chunk_ids = {
                let mut stmt = tx.prepare("SELECT id FROM knowledge_chunks WHERE version_id = ?1")?;
                let rows = stmt.query_map(params![vid], |r| r.get::<_, String>(0))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            for cid in chunk_ids {
                tx.execute("DELETE FROM knowledge_chunk_vec WHERE chunk_id = ?1", params![cid])?;
                cascade_chunks += 1;
            }
            tx.execute("DELETE FROM knowledge_chunks WHERE version_id = ?1", params![vid])?;
        }
        tx.execute(
            "DELETE FROM knowledge_document_versions WHERE document_id = ?1",
            params![id],
        )?;
        let gone = tx.execute(
            "DELETE FROM knowledge_documents WHERE id = ?1 AND space_id = ?2",
            params![id, space],
        )?;
        if gone > 0 {
            // Same audit discipline as `knowledge_delete_document`: every
            // deletion leaves a cascade-counted row.
            tx.execute(
                "INSERT INTO knowledge_deletion_audits
                    (id, tenant_id, customer_id, space_id, document_id, deleted_at, cascade_versions, cascade_chunks)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    format!("delete_{}", Uuid::new_v4().simple()),
                    CORPUS_SCOPE,
                    CORPUS_SCOPE,
                    space,
                    id,
                    Utc::now().to_rfc3339(),
                    version_ids.len() as i64,
                    cascade_chunks
                ],
            )?;
        }
        tx.commit()?;
        drop(conn);
        let _ = std::fs::remove_file(copy);
        Ok(())
    }

    pub fn search(
        &self,
        query: &str,
        doc_ids: &[String],
        max_hits: u32,
        max_bytes: usize,
    ) -> Result<Vec<CorpusHit>> {
        if max_hits == 0 || doc_ids.is_empty() || query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let Some(space) = self.corpus_space_id()? else {
            return Ok(Vec::new());
        };
        let q = embed(query);
        let k = max_hits.max(8).min(64);
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        // sqlite-vec 0.1.9 rejects any WHERE constraint on a vec0 auxiliary
        // column inside a KNN query ("illegal WHERE constraint … auxiliary
        // column"), so the space and document scope live on the *joined*
        // knowledge tables, outside the KNN core — exactly where the legacy
        // `corpus.sqlite` search applied its doc-id filter. Published and
        // non-revoked versions only.
        let placeholders = (0..doc_ids.len())
            .map(|n| format!("?{}", n + 3))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT d.id, d.source_id, c.ordinal, c.content, v.distance
             FROM knowledge_chunk_vec v
             JOIN knowledge_chunks c ON c.id = v.chunk_id
             JOIN knowledge_document_versions w ON w.id = c.version_id
             JOIN knowledge_documents d ON d.id = w.document_id
             WHERE v.embedding MATCH ?1 AND k = {k}
               AND d.space_id = ?2 AND d.id IN ({placeholders})
               AND d.deleted_at IS NULL AND w.published_at IS NOT NULL AND w.revoked_at IS NULL
             ORDER BY v.distance"
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(%err, "sqlite-vec search prepare failed");
                return Ok(Vec::new());
            }
        };
        let mut query_values = vec![Value::Blob(blob_f32(&q)), Value::Text(space)];
        query_values.extend(doc_ids.iter().cloned().map(Value::Text));
        let rows = stmt.query_map(params_from_iter(query_values), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? as u32,
                r.get::<_, String>(3)?,
            ))
        });
        let rows = match rows {
            Ok(r) => r,
            Err(err) => {
                tracing::warn!(%err, "sqlite-vec search failed");
                return Ok(Vec::new());
            }
        };
        let mut hits = Vec::new();
        let mut used = 0usize;
        for row in rows {
            let (doc_id, source_id, ordinal, text) = row?;
            let doc_name = base_name(&source_id);
            let add = text.len() + doc_name.len() + 16;
            if used + add > max_bytes {
                break;
            }
            used += add;
            hits.push(CorpusHit {
                doc_id,
                doc_name,
                ordinal,
                text,
            });
            if hits.len() as u32 >= max_hits {
                break;
            }
        }
        Ok(hits)
    }

    /// KNN over `knowledge_chunk_vec` scoped to a RESOLVED set of space ids
    /// (phase-8 unified search). The caller — `Runtime::space_search` — has
    /// already passed every space through authorize + scope + policy; the
    /// same sqlite-vec 0.1.9 constraint applies, so all filtering lives on
    /// the JOINed knowledge tables, never on vec0 auxiliary columns. As in
    /// `search`, a vec-layer failure degrades to "no vector hits" with a
    /// warning: the lexical and article surfaces still answer.
    pub fn search_spaces(
        &self,
        space_ids: &[String],
        query: &str,
        max_hits: u32,
        max_bytes: usize,
    ) -> Result<Vec<SpaceVectorHit>> {
        if max_hits == 0 || space_ids.is_empty() || query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let q = embed(query);
        let k = max_hits.max(8).min(64);
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let placeholders = (0..space_ids.len())
            .map(|n| format!("?{}", n + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT l.space_id, d.id, d.source_id, c.ordinal, c.content, c.id, w.version_no
             FROM knowledge_chunk_vec v
             JOIN knowledge_chunks c ON c.id = v.chunk_id
             JOIN knowledge_document_versions w ON w.id = c.version_id
             JOIN knowledge_documents d ON d.id = w.document_id
             JOIN knowledge_space_links l ON l.document_id=d.id
             WHERE v.embedding MATCH ?1 AND k = {k}
               AND l.space_id IN ({placeholders})
               AND d.deleted_at IS NULL AND w.published_at IS NOT NULL AND w.revoked_at IS NULL
               AND w.version_no = CASE WHEN l.binding='pinned' AND l.pinned_version_no IS NOT NULL
                   THEN l.pinned_version_no ELSE (SELECT MAX(w2.version_no) FROM knowledge_document_versions w2
                       WHERE w2.document_id=d.id AND w2.published_at IS NOT NULL
                         AND w2.revoked_at IS NULL AND w2.superseded_at IS NULL) END
             ORDER BY v.distance"
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(%err, "sqlite-vec space search prepare failed");
                return Ok(Vec::new());
            }
        };
        let mut query_values = vec![Value::Blob(blob_f32(&q))];
        query_values.extend(space_ids.iter().cloned().map(Value::Text));
        let rows = stmt.query_map(params_from_iter(query_values), |r| {
            Ok(SpaceVectorHit {
                space_id: r.get(0)?,
                doc_id: r.get(1)?,
                doc_name: base_name(&r.get::<_, String>(2)?),
                ordinal: r.get::<_, i64>(3)? as u32,
                text: r.get(4)?,
                chunk_id: r.get(5)?,
                version_no: r.get(6)?,
            })
        });
        let rows = match rows {
            Ok(r) => r,
            Err(err) => {
                tracing::warn!(%err, "sqlite-vec space search failed");
                return Ok(Vec::new());
            }
        };
        let mut hits = Vec::new();
        let mut used = 0usize;
        for row in rows {
            let hit = row?;
            let add = hit.text.len() + hit.doc_name.len() + 16;
            if used + add > max_bytes {
                break;
            }
            used += add;
            hits.push(hit);
            if hits.len() as u32 >= max_hits {
                break;
            }
        }
        Ok(hits)
    }

    // ---- one-time migration from corpus.sqlite ----------------------------

    fn migrate_if_needed(&self) {
        let legacy = self.data_dir.join(LEGACY_DB_FILE);
        let sealed = self.data_dir.join(format!("{LEGACY_DB_FILE}{SEALED_SUFFIX}"));
        if !legacy.exists() || sealed.exists() {
            return;
        }
        match self.migrate_legacy(&legacy, &sealed) {
            Ok(report) => {
                tracing::info!(
                    migrated = report.migrated.len(),
                    already_present = report.already_present.len(),
                    anomalies = report.anomalies.len(),
                    sealed = ?report.sealed,
                    "corpus: migrated corpus.sqlite into the knowledge space"
                );
                report.write(&self.data_dir);
            }
            Err(err) => {
                // Honest failure: the legacy file is kept exactly as it was
                // and the corpus simply starts empty over the new store.
                tracing::warn!(%err, "corpus: migration failed; corpus.sqlite kept untouched");
                CorpusMigrationReport {
                    ok: false,
                    space_id: self.corpus_space_id().ok().flatten(),
                    migrated: Vec::new(),
                    already_present: Vec::new(),
                    anomalies: vec![format!("migration failed: {err}")],
                    sealed: None,
                }
                .write(&self.data_dir);
            }
        }
    }

    fn migrate_legacy(&self, legacy: &Path, sealed: &Path) -> Result<CorpusMigrationReport> {
        let old = Connection::open(legacy)?;
        let docs = {
            let mut stmt = old.prepare(
                "SELECT id, name, source, path, chunks, status, error
                 FROM documents ORDER BY rowid",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut report = CorpusMigrationReport {
            ok: true,
            space_id: None,
            migrated: Vec::new(),
            already_present: Vec::new(),
            anomalies: Vec::new(),
            sealed: None,
        };
        if docs.is_empty() {
            // The seal renames the file; SQLite must not be holding it.
            drop(old);
            self.seal_legacy(legacy, sealed, &mut report);
            return Ok(report);
        }
        let space = self.ensure_space()?;
        report.space_id = Some(space.clone());
        let now = Utc::now().to_rfc3339();
        for (id, name, source, path, _declared, status, error) in docs {
            let present: Option<String> = {
                let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
                conn.query_row(
                    "SELECT id FROM knowledge_documents WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?
            };
            if present.is_some() {
                // Half-migrated rerun: this document already committed.
                report.already_present.push(id);
                continue;
            }
            if status != "ready" {
                report.anomalies.push(format!(
                    "doc {id} ({name}): legacy status '{status}'{}",
                    error.map(|e| format!(" — {e}")).unwrap_or_default()
                ));
                continue;
            }
            let file = PathBuf::from(&path);
            if !file.exists() {
                report.anomalies.push(format!("doc {id} ({name}): file copy missing ({}); text not migrated — it still exists inside the sealed legacy database's chunks table", file.display()));
                continue;
            }
            let text = match read_utf8(&file) {
                Ok(t) => t,
                Err(e) => {
                    report.anomalies.push(format!("doc {id} ({name}): {e}"));
                    continue;
                }
            };
            let old_chunks = {
                let mut stmt = old.prepare(
                    "SELECT rowid, ordinal, text FROM chunks WHERE doc_id = ?1 ORDER BY ordinal",
                )?;
                let rows = stmt.query_map(params![id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            if old_chunks.is_empty() {
                report.anomalies.push(format!("doc {id} ({name}): no chunk rows to migrate"));
                continue;
            }
            let sha256 = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
            let version_id = format!("docver_m_{id}");
            let mut vec_rows = 0usize;
            let mut missing_vec = 0usize;
            {
                let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
                let tx = conn.unchecked_transaction()?;
                tx.execute(
                    "INSERT INTO knowledge_documents
                        (id, tenant_id, customer_id, space_id, source_kind, source_id, status, created_at)
                     VALUES (?1, ?2, ?3, ?4, 'corpus-file', ?5, 'published', ?6)",
                    params![id, CORPUS_SCOPE, CORPUS_SCOPE, space, source, now],
                )?;
                tx.execute(
                    "INSERT INTO knowledge_document_versions
                        (id, document_id, version_no, sha256, content, created_at, published_at, parser_version)
                     VALUES (?1, ?2, 1, ?3, ?4, ?5, ?5, ?6)",
                    params![version_id, id, sha256, text, now, crate::wiki_adapters::PARSER_VERSION],
                )?;
                // Membership lives only in knowledge_space_links; a document
                // without a link row is invisible to the space that owns it.
                tx.execute(
                    "INSERT OR IGNORE INTO knowledge_space_links
                        (space_id, document_id, binding, pinned_version_no, added_by, created_at)
                     VALUES (?1, ?2, 'follow', NULL, 'migration', ?3)",
                    params![space, id, now],
                )?;
                for (rowid, ordinal, text) in &old_chunks {
                    let chunk_id = format!("chunk_m_{id}_{ordinal}");
                    tx.execute(
                        "INSERT INTO knowledge_chunks (id, version_id, ordinal, content)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![chunk_id, version_id, ordinal, text],
                    )?;
                    // Vectors move byte-identically; nothing is recomputed.
                    let blob: Option<Vec<u8>> = old
                        .query_row(
                            "SELECT embedding FROM vec_chunks WHERE rowid = ?1",
                            params![rowid],
                            |r| r.get(0),
                        )
                        .optional()?;
                    match blob {
                        Some(b) => {
                            tx.execute(
                                "INSERT INTO knowledge_chunk_vec
                                    (chunk_id, embedding, space_id, document_id, ordinal)
                                 VALUES (?1, ?2, ?3, ?4, ?5)",
                                params![chunk_id, b, space, id, ordinal],
                            )?;
                            vec_rows += 1;
                        }
                        None => missing_vec += 1,
                    }
                }
                // Verify inside the transaction before committing it.
                let n: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM knowledge_chunks WHERE version_id = ?1",
                    params![version_id],
                    |r| r.get(0),
                )?;
                if n as usize != old_chunks.len() {
                    report.anomalies.push(format!(
                        "doc {id} ({name}): rollback — wrote {n} of {} chunks",
                        old_chunks.len()
                    ));
                    drop(tx); // transaction discarded, nothing committed
                    continue;
                }
                tx.commit()?;
            }
            if missing_vec > 0 {
                report.anomalies.push(format!(
                    "doc {id} ({name}): {missing_vec} of {} chunks had no vector row — text migrated, vectors missing for those chunks",
                    old_chunks.len()
                ));
            }
            report.migrated.push(MigratedCorpusDoc {
                doc_id: id,
                name,
                chunks: old_chunks.len(),
                vec_rows,
                sha256,
            });
        }
        drop(old);
        self.seal_legacy(legacy, sealed, &mut report);
        Ok(report)
    }

    /// Rename `corpus.sqlite` (+WAL sidecars) to `.migrated`. The file is
    /// never deleted, so even anomalous rows stay recoverable. A rename
    /// failure only means the rerun will pass through again (idempotent).
    fn seal_legacy(&self, legacy: &Path, sealed: &Path, report: &mut CorpusMigrationReport) {
        let mut ok = true;
        for suffix in ["", "-wal", "-shm"] {
            let from = PathBuf::from(format!("{}{suffix}", legacy.display()));
            if from.exists() {
                let to = format!("{}{SEALED_SUFFIX}{suffix}", legacy.display());
                if let Err(e) = std::fs::rename(&from, &to) {
                    report.anomalies.push(format!(
                        "could not seal {} to {to}: {e} (data already migrated; rerun will retry sealing)",
                        from.display()
                    ));
                    ok = false;
                }
            }
        }
        let _ = sealed;
        if ok {
            report.sealed = Some(sealed.display().to_string());
        }
    }

    /// File copies live at `corpus/{id}{ext}`; enumerate instead of guessing
    /// the extension (ids contain no '.', so the prefix match is exact).
    fn copy_path(&self, id: &str) -> PathBuf {
        let dir = self.data_dir.join(FILES_DIR);
        let prefix = format!("{id}.");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    return entry.path();
                }
            }
        }
        dir.join(format!("{id}.txt"))
    }
}

pub fn format_segment(hits: &[CorpusHit]) -> String {
    if hits.is_empty() {
        return String::new();
    }
    let mut out = String::from(SEGMENT_MARK);
    out.push('\n');
    for h in hits {
        out.push_str(&format!(
            "{} #{}\n{}\n\n",
            h.doc_name,
            h.ordinal + 1,
            h.text
        ));
    }
    out
}

/// Arm the process-global sqlite-vec auto-extension. Safe to call from
/// anywhere (idempotent `Once`); integration fixtures (`g14`) need it before
/// opening a fixture database that declares a `vec0` table.
pub fn register_sqlite_vec() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        type ExtInit = unsafe extern "C" fn(
            db: *mut rusqlite::ffi::sqlite3,
            pz_err_msg: *mut *mut std::os::raw::c_char,
            p_api: *const rusqlite::ffi::sqlite3_api_routines,
        ) -> std::os::raw::c_int;
        let init_ptr: *const () = sqlite_vec::sqlite3_vec_init as *const ();
        let init: ExtInit = std::mem::transmute(init_ptr);
        rusqlite::ffi::sqlite3_auto_extension(Some(init));
    });
}

fn base_name(text: &str) -> String {
    text.rsplit(['/', '\\']).find(|part| !part.is_empty()).unwrap_or(text).to_string()
}

fn refuse_source(data_dir: &Path, src: &Path) -> Result<()> {
    let name = src
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if wiki::is_wiki_path(data_dir, src) {
        return Err(Error::Corpus("corpus refuse: wiki".into()));
    }
    if name == "sessions.sqlite" || name == "corpus.sqlite" {
        return Err(Error::Corpus("corpus refuse: session log".into()));
    }
    if name.starts_with("dandelion.") && name.ends_with(".md") {
        return Err(Error::Corpus("corpus refuse: memory file".into()));
    }
    if wiki::is_under(&data_dir.join("memories"), src) {
        return Err(Error::Corpus("corpus refuse: memory file".into()));
    }
    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "txt" | "md" | "markdown" | "rst") {
        return Err(Error::Corpus(
            "corpus refuse: only txt/md (upload a text document)".into(),
        ));
    }
    Ok(())
}

fn read_utf8(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    String::from_utf8(bytes).map_err(|_| Error::Corpus("corpus refuse: not utf-8 text".into()))
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_else(|| ".txt".into())
}

fn doc_id(name: &str) -> String {
    let stem: String = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("doc")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let stem = stem.trim_matches('-');
    let stem = if stem.is_empty() { "doc" } else { stem };
    let short = Uuid::new_v4().simple().to_string();
    format!("{stem}-{}", &short[..8])
}

fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut buf = String::new();
    for para in text.split("\n\n") {
        let p = para.trim();
        if p.is_empty() {
            continue;
        }
        if buf.is_empty() {
            buf.push_str(p);
        } else if buf.chars().count() + 2 + p.chars().count() <= max_chars {
            buf.push_str("\n\n");
            buf.push_str(p);
        } else {
            chunks.push(std::mem::take(&mut buf));
            buf.push_str(p);
        }
        while buf.chars().count() > max_chars {
            let take: String = buf.chars().take(max_chars).collect();
            chunks.push(take);
            buf = buf.chars().skip(max_chars).collect();
        }
    }
    if !buf.trim().is_empty() {
        chunks.push(buf);
    }
    if chunks.is_empty() && !text.trim().is_empty() {
        chunks.push(text.trim().to_string());
    }
    chunks
}

fn embed(text: &str) -> Vec<f32> {
    let mut v = vec![0f32; DIM];
    let lower = text.to_lowercase();
    for w in lower.split(|c: char| !c.is_alphanumeric()) {
        if w.chars().count() >= 2 {
            add_token(&mut v, w);
        }
    }
    let chars: Vec<char> = lower.chars().filter(|c| !c.is_whitespace()).collect();
    for w in chars.windows(2) {
        let gram: String = w.iter().collect();
        add_token(&mut v, &gram);
    }
    let mut norm = 0f32;
    for x in &v {
        norm += *x * *x;
    }
    let norm = norm.sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn add_token(v: &mut [f32], token: &str) {
    let mut h = DefaultHasher::new();
    token.hash(&mut h);
    let n = h.finish();
    let idx = (n as usize) % DIM;
    let sign = if n & 1 == 0 { 1.0 } else { -1.0 };
    v[idx] += sign;
}

fn blob_f32(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed_embed_is_stable_and_normalized() {
        let a = embed("hello dandelion corpus");
        let b = embed("hello dandelion corpus");
        assert_eq!(a, b);
        assert_eq!(a.len(), DIM);
        let n: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-5, "{n}");
    }

    #[test]
    fn chunk_packs_paragraphs() {
        let t = "aaa\n\nbbb\n\nccc";
        let c = chunk_text(t, 10);
        assert!(c.len() >= 2, "{c:?}");
    }

    #[test]
    fn sqlite_vec_roundtrip_search() {
        let dir = tempfile::tempdir().unwrap();
        let idx = CorpusIndex::open(dir.path()).expect("open");
        let src = dir.path().join("note.txt");
        std::fs::write(
            &src,
            "UNIQUE_CORPUS_TOKEN_alpha lives here.\n\nOther paragraph.",
        )
        .unwrap();
        let doc = idx.import(&src, 800, 1, 1_048_576).expect("import");
        assert_eq!(doc.status, "ready");
        assert!(doc.chunks >= 1);
        let hits = idx
            .search("UNIQUE_CORPUS_TOKEN_alpha", &[doc.id.clone()], 4, 16_384)
            .expect("search");
        assert!(!hits.is_empty(), "{hits:?}");
        assert!(hits
            .iter()
            .any(|h| h.text.contains("UNIQUE_CORPUS_TOKEN_alpha")));
        let seg = format_segment(&hits);
        assert!(seg.starts_with(SEGMENT_MARK));
        assert!(!seg.is_empty());
    }

    #[test]
    fn search_scopes_before_knn_limit() {
        let dir = tempfile::tempdir().unwrap();
        let idx = CorpusIndex::open(dir.path()).expect("open");
        let first = dir.path().join("first.txt");
        let selected = dir.path().join("selected.txt");
        std::fs::write(&first, "same scope token\n\nsame scope token").unwrap();
        std::fs::write(&selected, "same scope token selected-only").unwrap();
        let first_doc = idx.import(&first, 10, 1, 1_048_576).expect("import first");
        let selected_doc = idx
            .import(&selected, 800, 1, 1_048_576)
            .expect("import selected");
        let hits = idx
            .search("same scope token", &[selected_doc.id.clone()], 1, 16_384)
            .expect("scoped search");
        assert!(!hits.is_empty(), "{hits:?}");
        assert!(hits.iter().all(|hit| hit.doc_id == selected_doc.id));
        assert!(hits.iter().all(|hit| hit.doc_id != first_doc.id));
    }

    #[test]
    fn refuses_wiki_and_session_db() {
        let dir = tempfile::tempdir().unwrap();
        let idx = CorpusIndex::open(dir.path()).expect("open");
        let wiki = dir.path().join("wiki");
        std::fs::create_dir_all(&wiki).unwrap();
        let page = wiki.join("ARCH.md");
        std::fs::write(&page, "secret wiki").unwrap();
        let err = idx.import(&page, 800, 1, 1_048_576).unwrap_err();
        assert!(err.to_string().contains("wiki"), "{err}");
        let db = dir.path().join("sessions.sqlite");
        std::fs::write(&db, "not a db").unwrap();
        let err = idx.import(&db, 800, 1, 1_048_576).unwrap_err();
        assert!(
            err.to_string().contains("session log") || err.to_string().contains("only txt"),
            "{err}"
        );
    }

    #[test]
    fn corpus_space_is_lazy_and_kind_corpus() {
        // A fresh install with no corpus data must not create the space;
        // the first import creates it exactly once, typed 'corpus'.
        let dir = tempfile::tempdir().unwrap();
        let idx = CorpusIndex::open(dir.path()).expect("open");
        assert!(idx.corpus_space_id().unwrap().is_none());
        let src = dir.path().join("a.txt");
        std::fs::write(&src, "lazy space").unwrap();
        idx.import(&src, 800, 1, 1_048_576).expect("import");
        let space = idx
            .corpus_space_id()
            .expect("query")
            .expect("space after import");
        let conn = Connection::open(dir.path().join("sessions.sqlite")).unwrap();
        let kind: String = conn
            .query_row(
                "SELECT kind FROM knowledge_spaces WHERE id = ?1",
                params![space],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "corpus");
        // Stable across further imports.
        let src2 = dir.path().join("b.txt");
        std::fs::write(&src2, "second doc").unwrap();
        idx.import(&src2, 800, 1, 1_048_576).expect("import 2");
        assert_eq!(idx.corpus_space_id().unwrap().unwrap(), space);
    }
}
