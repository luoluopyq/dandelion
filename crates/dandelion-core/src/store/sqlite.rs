use crate::context_os::{
    Authority, ContextBuildRequest, ContextCacheStatus, ContextItem, ContextPack,
    ContextRankSignals, EntityGraphEdge, EntityGraphNode, EntityGraphProjection, MemoryDomain,
    MemoryEvent, MemoryEventDraft, MemoryEventKind, MemoryScope, TemporalProjectionItem,
};
use crate::ids::{MessageId, SessionId};
use crate::provider::types::{ChatMessage, ToolCall};
use crate::trace::TraceEvent;
use crate::{Error, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use std::time::Duration;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
  id          TEXT PRIMARY KEY,
  title       TEXT NOT NULL DEFAULT '',
  workspace   TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  parent_id   TEXT,
  kind        TEXT NOT NULL DEFAULT 'user',
  depth       INTEGER NOT NULL DEFAULT 0,
  folder      TEXT NOT NULL DEFAULT '',
  prefs       TEXT NOT NULL DEFAULT '{}',
  pinned      INTEGER NOT NULL DEFAULT 0,
  archived    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS projects (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  workspace   TEXT NOT NULL UNIQUE,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id  TEXT NOT NULL,
  role        TEXT NOT NULL,
  content     TEXT NOT NULL,
  tool_calls  TEXT,
  tool_call_id TEXT,
  acp_message_id TEXT,
  created_at  TEXT NOT NULL,
  FOREIGN KEY(session_id) REFERENCES sessions(id)
);

CREATE TABLE IF NOT EXISTS message_agent_engines (
  session_id TEXT NOT NULL,
  message_id TEXT NOT NULL,
  engine_id TEXT NOT NULL,
  PRIMARY KEY (session_id, message_id),
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

-- One row per context compaction. `summary_message_id` / `todo_message_id` are
-- the `acp_message_id`s of the rows the compaction injected, so later passes
-- can tell derived rows from real conversation.
CREATE TABLE IF NOT EXISTS compaction_checkpoints (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  parent_id TEXT,
  trigger_kind TEXT NOT NULL,
  summary TEXT NOT NULL,
  todo TEXT,
  summary_message_id TEXT NOT NULL,
  todo_message_id TEXT,
  covered_count INTEGER NOT NULL,
  tail_count INTEGER NOT NULL,
  source_bytes INTEGER NOT NULL,
  source_truncated INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  UNIQUE (session_id, ordinal),
  FOREIGN KEY(session_id) REFERENCES sessions(id)
);

-- Original rows removed from `messages` by a compaction, kept verbatim.
-- `is_derived = 1` marks rows an earlier compaction injected (summary / todo).
CREATE TABLE IF NOT EXISTS message_archive (
  archive_id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  checkpoint_id TEXT NOT NULL,
  orig_id INTEGER NOT NULL,
  role TEXT NOT NULL,
  content TEXT NOT NULL,
  tool_calls TEXT,
  tool_call_id TEXT,
  acp_message_id TEXT,
  attachments TEXT,
  created_at TEXT NOT NULL,
  is_derived INTEGER NOT NULL DEFAULT 0,
  FOREIGN KEY(session_id) REFERENCES sessions(id),
  FOREIGN KEY(checkpoint_id) REFERENCES compaction_checkpoints(id)
);
CREATE INDEX IF NOT EXISTS message_archive_session_idx ON message_archive(session_id, archive_id);
CREATE INDEX IF NOT EXISTS compaction_checkpoints_session_idx ON compaction_checkpoints(session_id, ordinal);

CREATE TABLE IF NOT EXISTS task_bindings (
  session_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (session_id, kind, resource_id),
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS cron_jobs (
  id TEXT PRIMARY KEY,
  expr TEXT NOT NULL,
  prompt TEXT NOT NULL,
  workspace TEXT NOT NULL,
  model_ref TEXT,
  enabled INTEGER NOT NULL DEFAULT 1,
  last_run TEXT,
  next_run TEXT NOT NULL,
  missed_policy TEXT NOT NULL DEFAULT 'skip',
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS session_todos (
  session_id TEXT NOT NULL,
  id TEXT NOT NULL,
  content TEXT NOT NULL,
  status TEXT NOT NULL,
  parent TEXT,
  position INTEGER NOT NULL,
  PRIMARY KEY (session_id, id),
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS session_todo_meta (
  session_id TEXT PRIMARY KEY,
  revision INTEGER NOT NULL,
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS specs (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  status TEXT NOT NULL,
  version INTEGER NOT NULL,
  path TEXT NOT NULL,
  mode TEXT NOT NULL DEFAULT 'ask'
);

CREATE TABLE IF NOT EXISTS memory_events (
  id TEXT PRIMARY KEY,
  domain TEXT NOT NULL,
  scope_json TEXT NOT NULL,
  kind TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  source_kind TEXT NOT NULL,
  source_id TEXT NOT NULL,
  source_version TEXT,
  authority TEXT NOT NULL,
  observed_at TEXT NOT NULL,
  valid_from TEXT,
  valid_to TEXT,
  confidence REAL NOT NULL,
  sensitivity TEXT NOT NULL,
  revision_of TEXT
);
CREATE INDEX IF NOT EXISTS memory_events_scope_idx ON memory_events(domain, scope_json, observed_at DESC);
CREATE INDEX IF NOT EXISTS memory_events_revision_idx ON memory_events(revision_of);

CREATE TABLE IF NOT EXISTS context_pack_audits (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  pack_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS context_pack_session_idx ON context_pack_audits(session_id, created_at DESC);

CREATE TABLE IF NOT EXISTS preflight_runs (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL, created_at TEXT NOT NULL,
  outcome TEXT NOT NULL, latency_ms INTEGER NOT NULL, decision_json TEXT, detail TEXT
);
CREATE INDEX IF NOT EXISTS preflight_runs_session_idx ON preflight_runs(session_id, created_at DESC);

CREATE TABLE IF NOT EXISTS trace_events (
  run_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  kind TEXT NOT NULL,
  detail_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (run_id, sequence)
);
CREATE INDEX IF NOT EXISTS trace_events_session_idx ON trace_events(session_id, created_at DESC);

CREATE TABLE IF NOT EXISTS agent_runs (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  status TEXT NOT NULL,
  stop_reason TEXT,
  error_message TEXT,
  duration_ms INTEGER
);
CREATE INDEX IF NOT EXISTS agent_runs_session_idx ON agent_runs(session_id, started_at DESC);

CREATE TABLE IF NOT EXISTS knowledge_tenants (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS knowledge_customers (
  id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, name TEXT NOT NULL, created_at TEXT NOT NULL,
  UNIQUE(tenant_id, name), FOREIGN KEY(tenant_id) REFERENCES knowledge_tenants(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS knowledge_spaces (
  id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, customer_id TEXT NOT NULL, name TEXT NOT NULL,
  external_policy TEXT NOT NULL DEFAULT 'local_only', created_at TEXT NOT NULL,
  UNIQUE(tenant_id, customer_id, name),
  FOREIGN KEY(tenant_id) REFERENCES knowledge_tenants(id) ON DELETE CASCADE,
  FOREIGN KEY(customer_id) REFERENCES knowledge_customers(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS knowledge_collections (
  id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, customer_id TEXT NOT NULL, space_id TEXT NOT NULL,
  name TEXT NOT NULL, created_at TEXT NOT NULL, UNIQUE(space_id, name),
  FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS knowledge_documents (
  id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, customer_id TEXT NOT NULL, space_id TEXT NOT NULL,
  source_kind TEXT NOT NULL, source_id TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'draft', created_at TEXT NOT NULL, deleted_at TEXT,
  FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE
);
-- Space association (§三): a material (document = one update history keyed
-- by tenant+customer+source_kind+source_id) links into any number of spaces
-- through THIS table — one history, many readers. A link either follows the
-- currently effective (latest published, non-revoked) version or pins one
-- stable version number. The document row's space_id stays its HOME space
-- (where the history lives); membership is decided here, nowhere else.
CREATE TABLE IF NOT EXISTS knowledge_space_links (
  space_id TEXT NOT NULL, document_id TEXT NOT NULL,
  binding TEXT NOT NULL DEFAULT 'follow', pinned_version_no INTEGER,
  added_by TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL,
  PRIMARY KEY(space_id, document_id),
  FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE,
  FOREIGN KEY(document_id) REFERENCES knowledge_documents(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS knowledge_space_links_doc_idx ON knowledge_space_links(document_id, space_id);
CREATE TABLE IF NOT EXISTS knowledge_collection_documents (
  collection_id TEXT NOT NULL, document_id TEXT NOT NULL,
  PRIMARY KEY(collection_id, document_id),
  FOREIGN KEY(collection_id) REFERENCES knowledge_collections(id) ON DELETE CASCADE,
  FOREIGN KEY(document_id) REFERENCES knowledge_documents(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS knowledge_document_versions (
  id TEXT PRIMARY KEY, document_id TEXT NOT NULL, version_no INTEGER NOT NULL, sha256 TEXT NOT NULL,
  content TEXT NOT NULL, created_at TEXT NOT NULL, published_at TEXT, revoked_at TEXT, superseded_at TEXT,
  parser_version INTEGER NOT NULL DEFAULT 1,
  UNIQUE(document_id, version_no), FOREIGN KEY(document_id) REFERENCES knowledge_documents(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS knowledge_chunks (
  id TEXT PRIMARY KEY, version_id TEXT NOT NULL, ordinal INTEGER NOT NULL, content TEXT NOT NULL,
  FOREIGN KEY(version_id) REFERENCES knowledge_document_versions(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS knowledge_chunks_version_idx ON knowledge_chunks(version_id, ordinal);
-- Content-first presentation metadata. Documents/versions/chunks remain the
-- single source of identity, permissions and retrieval.
CREATE TABLE IF NOT EXISTS knowledge_personal_spaces (
  subject_id TEXT PRIMARY KEY, space_id TEXT NOT NULL REFERENCES knowledge_spaces(id)
);
CREATE TABLE IF NOT EXISTS knowledge_item_details (
  document_id TEXT PRIMARY KEY REFERENCES knowledge_documents(id) ON DELETE CASCADE,
  version_id TEXT NOT NULL, title TEXT NOT NULL, kind TEXT NOT NULL,
  body TEXT NOT NULL, notes TEXT NOT NULL DEFAULT '', asset_path TEXT, asset_name TEXT,
  parse_status TEXT NOT NULL, parse_error TEXT, topics TEXT NOT NULL DEFAULT '[]',
  suggested_topics TEXT NOT NULL DEFAULT '[]', manual_topics INTEGER NOT NULL DEFAULT 0,
  favorite INTEGER NOT NULL DEFAULT 0, classification_status TEXT NOT NULL DEFAULT 'pending',
  classification_error TEXT, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS knowledge_item_asset_versions (
  version_id TEXT PRIMARY KEY REFERENCES knowledge_document_versions(id) ON DELETE CASCADE,
  asset_path TEXT NOT NULL, asset_name TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS knowledge_space_members (
  space_id TEXT NOT NULL, subject_id TEXT NOT NULL, role TEXT NOT NULL, created_at TEXT NOT NULL,
  PRIMARY KEY(space_id, subject_id), FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS knowledge_sync_sources (
  id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, customer_id TEXT NOT NULL, space_id TEXT NOT NULL,
  source_kind TEXT NOT NULL, source_id TEXT NOT NULL, cursor TEXT, last_sha256 TEXT,
  status TEXT NOT NULL DEFAULT 'idle', retry_count INTEGER NOT NULL DEFAULT 0, last_error TEXT,
  updated_at TEXT NOT NULL, UNIQUE(space_id, source_kind, source_id),
  FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS knowledge_deletion_audits (
  id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, customer_id TEXT NOT NULL, space_id TEXT NOT NULL,
  document_id TEXT NOT NULL, deleted_at TEXT NOT NULL, cascade_versions INTEGER NOT NULL, cascade_chunks INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS knowledge_retention_policies (
  space_id TEXT PRIMARY KEY, retention_days INTEGER NOT NULL, updated_at TEXT NOT NULL,
  FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS knowledge_api_clients (
  id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, customer_id TEXT NOT NULL, space_id TEXT NOT NULL,
  token_sha256 TEXT NOT NULL UNIQUE, label TEXT NOT NULL, max_requests_per_minute INTEGER NOT NULL,
  max_answer_tokens INTEGER NOT NULL, created_at TEXT NOT NULL, revoked_at TEXT,
  FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS knowledge_api_usage (
  id TEXT PRIMARY KEY, client_id TEXT NOT NULL, created_at TEXT NOT NULL, request_tokens INTEGER NOT NULL,
  no_answer INTEGER NOT NULL, citation_count INTEGER NOT NULL,
  FOREIGN KEY(client_id) REFERENCES knowledge_api_clients(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS knowledge_api_usage_client_idx ON knowledge_api_usage(client_id, created_at DESC);
CREATE TABLE IF NOT EXISTS knowledge_feedback (
  id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, customer_id TEXT NOT NULL, space_id TEXT NOT NULL,
  subject_id TEXT NOT NULL, kind TEXT NOT NULL, comment TEXT, status TEXT NOT NULL DEFAULT 'open',
  reviewer_id TEXT, review_note TEXT, created_at TEXT NOT NULL, reviewed_at TEXT,
  FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE
);

-- Knowledge-space baseline (phase 1). SQLite records *observed* state only;
-- wiki page bodies stay in the file-state snapshot tree (current.json is the
-- single source of truth for published content). Nothing here is authoritative
-- over the file tree, and nothing is dual-written forever.
CREATE TABLE IF NOT EXISTS dandelion_meta (
  key TEXT PRIMARY KEY, value TEXT NOT NULL
);
-- Dual anchor for the identity migration `wiki/projects/<ws-hash>/` ->
-- `wiki/spaces/<spaceId>/`: registry row + self-describing origin.json inside
-- the directory. Either side can be rebuilt from the other.
CREATE TABLE IF NOT EXISTS knowledge_space_workspaces (
  space_id TEXT PRIMARY KEY, workspace_hash TEXT NOT NULL UNIQUE,
  workspace_path TEXT NOT NULL, created_at TEXT NOT NULL,
  FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE
);
-- Generalized evidence locators beyond `rel:Lx-Ly` (pdf page, web snapshot
-- section, chat message, manual revision). The legacy token column keeps
-- already-published citations resolvable forever.
CREATE TABLE IF NOT EXISTS knowledge_evidence_spans (
  id TEXT PRIMARY KEY, space_id TEXT NOT NULL, document_id TEXT,
  source_version TEXT, locator TEXT NOT NULL, legacy_token TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS knowledge_evidence_spans_space_idx ON knowledge_evidence_spans(space_id, legacy_token);
-- Mirror of the wiki directory registry (pages.json stays authoritative; the
-- mirror syncs at compile end and self-heals from a fresh compile if stale).
CREATE TABLE IF NOT EXISTS knowledge_pages (
  space_id TEXT NOT NULL, file_name TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'topic',
  grp TEXT NOT NULL DEFAULT '', title TEXT NOT NULL DEFAULT '', sort INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL, PRIMARY KEY(space_id, file_name),
  FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE
);
-- Async bookkeeping for compile/deep-build/replan/migrate runs (status is an
-- observation; the file tree decides what was actually published).
CREATE TABLE IF NOT EXISTS knowledge_build_jobs (
  id TEXT PRIMARY KEY, space_id TEXT NOT NULL, kind TEXT NOT NULL,
  status TEXT NOT NULL, version INTEGER, detail TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  FOREIGN KEY(space_id) REFERENCES knowledge_spaces(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS knowledge_build_jobs_space_idx ON knowledge_build_jobs(space_id, created_at DESC);
-- Distillation proposals; space_id NULL = awaiting assignment (never fall
-- back to guessing the cwd), assigned during review.
CREATE TABLE IF NOT EXISTS knowledge_proposals (
  id TEXT PRIMARY KEY, space_id TEXT, tenant_id TEXT NOT NULL, customer_id TEXT NOT NULL,
  session_id TEXT, kind TEXT NOT NULL, title TEXT NOT NULL, content TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'unassigned', created_at TEXT NOT NULL,
  reviewed_at TEXT, reviewer_id TEXT, review_note TEXT
);
CREATE INDEX IF NOT EXISTS knowledge_proposals_space_idx ON knowledge_proposals(space_id, status);
CREATE INDEX IF NOT EXISTS knowledge_proposals_unassigned_idx ON knowledge_proposals(status, created_at);
"#;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub workspace: String,
    pub parent_id: Option<String>,
    pub kind: String,
    pub depth: i64,
    pub created_at: String,
    pub updated_at: String,
    /// Empty string = ungrouped. Column exists for G5-A; no folder UI in CD1.
    pub folder: String,
    pub pinned: bool,
    pub archived: bool,
}

/// A user-managed workspace. Removing a project never deletes its directory;
/// its sessions are archived so their local history remains recoverable.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    pub workspace: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Per-session task knobs. Missing fields inherit global `Settings` on `prompt`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPrefs {
    /// Agent implementation for this task; absence means the built-in harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<crate::config::PermissionMode>,
    /// User segment assembled in core (`assemble_system_prompt_ex`). Not Vue-stitched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<std::path::PathBuf>,
}

impl SessionPrefs {
    pub fn normalize(mut self) -> Self {
        self.agent_engine = self.agent_engine
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty());
        self.model = self
            .model
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.system_prompt = self
            .system_prompt
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.workspace = self.workspace.filter(|p| !p.as_os_str().is_empty());
        self
    }
}

/// 把一个目录收敛成与 `projects.workspace` 同一口径的规范拼写。
///
/// Windows 上 `canonicalize` 会给路径加上 `\\?\` 扩展长度前缀，那是实现细节而非
/// 用户可见拼写；`project_create` 入库时已经用 `dunce::simplified` 剥掉它。会话
/// 这一侧过去原样存用户选中的字符串，于是同一个目录在两张表里长得不一样
/// （`\\?\E:\dir` vs `E:\dir`），侧栏按精确相等分组时对话永远进不了自己的项目。
/// 路径不存在（或无权限）canonicalize 会失败，退回原值，不能让写入因此中断。
pub(crate) fn canonical_workspace(path: &std::path::Path) -> std::path::PathBuf {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    dunce::simplified(&canonical).to_path_buf()
}

impl SessionPrefs {
    fn with_canonical_workspace(mut self) -> Self {
        self.workspace = self.workspace.as_deref().map(canonical_workspace);
        self
    }

    pub fn merge_from(&mut self, patch: &SessionPrefs) {
        if patch.agent_engine.is_some() {
            self.agent_engine = patch.agent_engine.clone();
        }
        if patch.model.is_some() {
            self.model = patch.model.clone();
        }
        if patch.temperature.is_some() {
            self.temperature = patch.temperature;
        }
        if patch.max_tokens.is_some() {
            self.max_tokens = patch.max_tokens;
        }
        if patch.permission_mode.is_some() {
            self.permission_mode = patch.permission_mode;
        }
        if patch.system_prompt.is_some() {
            self.system_prompt = patch.system_prompt.clone();
        }
        if patch.workspace.is_some() {
            self.workspace = patch.workspace.clone();
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchHit {
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub updated_at: String,
}

/// One past execution of a cron job. Each run is its own session, so
/// `session_id` is what the panel opens when a run row is clicked.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronRunRow {
    pub session_id: String,
    pub started_at: String,
    pub updated_at: String,
    pub finished: bool,
    pub summary: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJobRow {
    pub id: String,
    pub expr: String,
    pub prompt: String,
    pub workspace: String,
    pub model_ref: Option<String>,
    pub enabled: bool,
    pub last_run: Option<String>,
    pub next_run: String,
    pub missed_policy: String,
    pub created_at: String,
}

#[derive(Clone, Debug)]
pub struct ChatRow {
    pub role: String,
    pub content: String,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub acp_message_id: Option<String>,
    pub engine_id: Option<String>,
    /// JSON array of `<data_dir>`-relative image paths (see `attachments` column).
    pub attachments: Option<String>,
}

/// What made a compaction run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactTrigger {
    /// The provider rejected the prompt as too large.
    Overflow,
    /// The estimated prompt exceeded the budget before the request was sent.
    Preflight,
    /// The user asked for it.
    Manual,
}

impl CompactTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            CompactTrigger::Overflow => "overflow",
            CompactTrigger::Preflight => "preflight",
            CompactTrigger::Manual => "manual",
        }
    }
}

/// Input to [`Store::compact_messages`]. `covered_ids` and `tail_ids` are the
/// `messages.id` values (oldest first) of the summarized prefix and the kept
/// tail; together they must equal the session's current convertible rows.
pub struct CompactionInput<'a> {
    pub session_id: &'a SessionId,
    pub covered_ids: &'a [i64],
    pub tail_ids: &'a [i64],
    /// The brief only, without the `[compacted history]` marker.
    pub summary: &'a str,
    pub todo: Option<&'a str>,
    pub trigger: CompactTrigger,
    /// Size of the full transcript handed to the summarizer, before truncation.
    pub source_bytes: usize,
    pub source_truncated: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionCheckpoint {
    pub id: String,
    pub session_id: String,
    pub ordinal: i64,
    pub parent_id: Option<String>,
    pub trigger: String,
    pub summary: String,
    pub todo: Option<String>,
    pub summary_message_id: String,
    pub todo_message_id: Option<String>,
    pub covered_count: i64,
    pub tail_count: i64,
    pub source_bytes: i64,
    pub source_truncated: bool,
    pub created_at: String,
}

/// Roles `load_openai_messages` turns into model messages.
const CONVERTIBLE_ROLES_SQL: &str = "('user','assistant','tool','turn_note')";

/// Convert one stored row to a model message. `None` for unknown roles.
fn row_to_message(row: ChatRow) -> Option<ChatMessage> {
    match row.role.as_str() {
        "user" => Some(ChatMessage::User {
            content: row.content,
            images: Vec::new(),
        }),
        "assistant" => {
            let tool_calls = row
                .tool_calls
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<OpenAiToolCall>>(s).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|t| {
                    // Keep the raw call in SQLite for the UI/audit trail, but
                    // never replay malformed function arguments to a strict
                    // OpenAI-compatible provider. The paired tool result
                    // already reports that the call failed without running.
                    let arguments = if matches!(
                        serde_json::from_str::<serde_json::Value>(&t.function.arguments),
                        Ok(serde_json::Value::Object(_))
                    ) {
                        t.function.arguments
                    } else {
                        tracing::warn!(
                            tool = %t.function.name,
                            id = %t.id,
                            "malformed historical tool arguments replaced for provider replay"
                        );
                        "{}".to_string()
                    };
                    ToolCall {
                        id: t.id,
                        name: t.function.name,
                        arguments,
                    }
                })
                .collect();
            Some(ChatMessage::Assistant {
                content: if row.content.is_empty() {
                    None
                } else {
                    Some(row.content)
                },
                tool_calls,
            })
        }
        "tool" => Some(ChatMessage::Tool {
            tool_call_id: row.tool_call_id.unwrap_or_default(),
            content: row.content,
        }),
        // 轮次边界标记：以 user 身份进入模型上下文（Codex 同款做法），
        // 让它紧贴在下一条真实用户消息之前，打断语义最近。
        "turn_note" => Some(ChatMessage::User {
            content: row.content,
            images: Vec::new(),
        }),
        _ => None,
    }
}

/// Per-session checklist item (C11 `todo` tool).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecRow {
    pub id: String,
    pub session_id: String,
    pub status: String,
    pub version: i64,
    pub path: String,
    pub mode: String,
}

pub struct Store {
    conn: Mutex<Connection>,
    item_write: Mutex<()>,
    item_assets: Mutex<()>,
    context_cache: Mutex<HashMap<String, CachedContextCandidates>>,
    context_generation: AtomicU64,
    /// Host data directory. User-attached images are persisted under
    /// `<data_dir>/attachments/<session_id>/` so chat history keeps a
    /// viewable preview without storing base64 blobs in SQLite.
    data_dir: PathBuf,
}

#[path = "knowledge_items.rs"]
mod knowledge_items;
pub use knowledge_items::*;

#[derive(Clone)]
struct CachedContextCandidates {
    generation: u64,
    candidates: Vec<RankedMemoryEvent>,
}

#[derive(Clone)]
struct RankedMemoryEvent {
    event: MemoryEvent,
    signals: ContextRankSignals,
    why_selected: Vec<String>,
    relevance: f32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightRun {
    pub id: String,
    pub session_id: String,
    pub created_at: String,
    pub outcome: String,
    pub latency_ms: u64,
    pub decision: Option<crate::context_os::RoutingDecision>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunRow {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub stop_reason: Option<String>,
    pub error_message: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightMetrics {
    pub total: u64,
    pub degraded: u64,
    pub average_latency_ms: u64,
    pub outcomes: std::collections::BTreeMap<String, u64>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeTenant {
    pub id: String,
    pub name: String,
    pub created_at: String,
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeCustomer {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub created_at: String,
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSpace {
    pub id: String,
    pub tenant_id: String,
    pub customer_id: String,
    pub name: String,
    pub external_policy: String,
    pub created_at: String,
    /// Optional organizing goal ("onboarding", "research notes", …). It shapes
    /// the TOC plan; it is never an identity and can change anytime.
    pub goal: Option<String>,
    /// 'docs' | 'wiki' | 'corpus'. Legacy document spaces upgrade to 'docs'.
    pub kind: String,
    /// RFC3339 stamp once archived; NULL = active.
    pub archived_at: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeCollection {
    pub id: String,
    pub tenant_id: String,
    pub customer_id: String,
    pub space_id: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeRole {
    Owner,
    Editor,
    Reader,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDocument {
    pub id: String,
    pub tenant_id: String,
    pub customer_id: String,
    pub space_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDocumentVersion {
    pub id: String,
    pub document_id: String,
    pub version_no: i64,
    pub sha256: String,
    pub created_at: String,
    /// Which extraction pipeline produced this version's stored content
    /// (§三; see `wiki_adapters::PARSER_VERSION`).
    pub parser_version: i64,
}

/// One space-resolved document version with its full stored text: what
/// THIS space reads per its link binding (follow → the live version,
/// pinned → its frozen one unless withdrawn). A document contributes at
/// most one row per space — the resolution is per (space, material).
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgePublishedDocument {
    pub document_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub version_id: String,
    pub version_no: i64,
    pub sha256: String,
    pub content: String,
}

/// One recorded citation, as the space deep build writes it (§八): the
/// exact token a reader clicks (`legacy_token`, canonical `rel:Lx-Ly`),
/// the document and version number that token was valid against, and the
/// display locator. Written by the build, never by hand — pages and
/// version contents stay the single source of truth; this is only the
/// index that lets a click resolve months later.
#[derive(Clone, Debug)]
pub struct EvidenceSpanRow {
    pub document_id: String,
    pub source_version: String,
    pub locator: String,
    pub legacy_token: String,
}

/// A cited frozen version with the flags the reader must honor (§八):
/// revoked or gone → refuse; superseded → still readable (A5's law).
#[derive(Clone, Debug)]
pub struct KnowledgeCitedVersion {
    pub source_kind: String,
    pub source_id: String,
    pub source_space_id: String,
    pub version_no: i64,
    pub sha256: String,
    pub parser_version: i64,
    pub revoked: bool,
    pub superseded: bool,
    pub content: String,
}

/// One material as a space sees it (§三 space association): the document
/// identity (its ONE version history lives in its home space), the link's
/// binding (`follow` rides the currently effective version, `pinned` is
/// frozen), and what this space actually reads right now — `resolved` is
/// NULL when a pinned version was revoked, i.e. the material is linked but
/// contributes no evidence here (fail closed).
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSpaceMaterial {
    pub document_id: String,
    pub home_space_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub status: String,
    pub created_at: String,
    pub binding: String,
    pub pinned_version_no: Option<i64>,
    pub latest_published_version_no: Option<i64>,
    pub resolved_version_no: Option<i64>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchHit {
    pub document_id: String,
    pub version_id: String,
    pub version_no: i64,
    pub chunk_id: String,
    pub ordinal: i64,
    pub content: String,
    pub source_kind: String,
    pub source_id: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDocumentPreview {
    pub document_id: String,
    pub version_id: String,
    pub version_no: i64,
    pub source_id: String,
    pub content: String,
    pub enabled: bool,
}

/// A stable, content-addressable reference returned with a knowledge answer.
/// Keep the version and chunk IDs: a document path alone is not sufficient to
/// prove which immutable revision supplied an answer.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeCitation {
    pub document_id: String,
    pub version_id: String,
    pub version_no: i64,
    pub chunk_id: String,
    pub ordinal: i64,
    pub source_kind: String,
    pub source_id: String,
}

/// A local, extractive answer.  This deliberately contains no model output:
/// customer spaces can answer with citations even when their external policy
/// forbids sending raw material to a provider.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeAnswer {
    pub answer: String,
    pub no_answer: bool,
    pub citations: Vec<KnowledgeCitation>,
}

/// A content-free policy decision. A caller must receive this before passing
/// customer material to an external provider.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeExternalDecision {
    pub allowed: bool,
    pub policy: String,
    pub reason: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSyncSource {
    pub id: String,
    pub tenant_id: String,
    pub customer_id: String,
    pub space_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub cursor: Option<String>,
    pub last_sha256: Option<String>,
    pub status: String,
    pub retry_count: i64,
    pub last_error: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSyncPreview {
    pub changed: bool,
    pub action: String,
    pub current_sha256: Option<String>,
    pub incoming_sha256: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDeletionAudit {
    pub id: String,
    pub document_id: String,
    pub deleted_at: String,
    pub cascade_versions: i64,
    pub cascade_chunks: i64,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeRetentionPreview {
    pub retention_days: u32,
    pub eligible_versions: u64,
    pub oldest_eligible_at: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeMember {
    pub subject_id: String,
    pub role: KnowledgeRole,
    pub created_at: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeApiClient {
    pub id: String,
    pub label: String,
    pub max_requests_per_minute: u32,
    pub max_answer_tokens: u32,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeApiProvision {
    pub client: KnowledgeApiClient,
    /// Returned exactly once on creation. The store persists only SHA-256.
    pub token: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeFeedback {
    pub id: String,
    pub subject_id: String,
    pub kind: String,
    pub comment: Option<String>,
    pub status: String,
    pub reviewer_id: Option<String>,
    pub review_note: Option<String>,
    pub created_at: String,
    pub reviewed_at: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeQualityMetrics {
    pub published_documents: u64,
    pub published_chunks: u64,
    pub api_requests: u64,
    pub citation_coverage: f32,
    pub no_answer_rate: f32,
    pub open_feedback: u64,
    pub failed_sync_sources: u64,
    pub isolation_alerts: u64,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDeletionProof {
    pub audit: KnowledgeDeletionAudit,
    pub documents_remaining: u64,
    pub versions_remaining: u64,
    pub chunks_remaining: u64,
    pub verified: bool,
}

fn knowledge_chunks(content: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for paragraph in content.split("\n\n") {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }
        let paragraph_len = paragraph.chars().count();
        if !current.is_empty() && current.chars().count() + paragraph_len + 2 > max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        if paragraph_len <= max_chars {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(paragraph);
            continue;
        }
        if !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        let mut piece = String::new();
        for ch in paragraph.chars() {
            piece.push(ch);
            if piece.chars().count() >= max_chars {
                chunks.push(std::mem::take(&mut piece));
            }
        }
        if !piece.is_empty() {
            current = piece;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn knowledge_question_terms(question: &str) -> Vec<String> {
    const QUESTION_WORDS: &[&str] = &[
        "请问",
        "如何",
        "怎么",
        "怎样",
        "什么",
        "是否",
        "哪里",
        "在哪",
        "多久",
        "多少",
        "步骤",
        "方法",
        "可以",
        "应该",
        "需要",
        "是什么",
    ];
    const ENGLISH_STOP: &[&str] = &[
        "how", "what", "when", "where", "which", "does", "with", "from", "the", "and", "for",
        "are", "can", "should", "you",
    ];
    let mut terms = Vec::new();
    for run in question.split(|c: char| !c.is_alphanumeric()) {
        if run.is_empty() {
            continue;
        }
        let mut chinese = run.to_owned();
        for word in QUESTION_WORDS {
            chinese = chinese.replace(word, " ");
        }
        for segment in chinese.split_whitespace() {
            let chars: Vec<char> = segment.chars().collect();
            if chars
                .iter()
                .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(ch))
            {
                if (2..=12).contains(&chars.len()) {
                    terms.push(segment.to_owned());
                }
                for width in [4, 3, 2] {
                    if chars.len() >= width {
                        for window in chars.windows(width).take(12) {
                            terms.push(window.iter().collect());
                        }
                    }
                }
            } else {
                let word = segment.to_ascii_lowercase();
                if word.len() >= 3 && !ENGLISH_STOP.contains(&word.as_str()) {
                    terms.push(word);
                }
            }
        }
    }
    let mut seen = HashSet::new();
    terms.retain(|term| seen.insert(term.clone()));
    terms.truncate(32);
    terms
}

fn knowledge_excerpt(content: &str, max_chars: usize) -> String {
    let end = content
        .char_indices()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(content.len());
    let excerpt = content[..end].trim();
    if end < content.len() {
        format!("{excerpt}…")
    } else {
        excerpt.into()
    }
}

impl Store {
    /// Desktop-local compatibility scope.  The schema retains tenant/customer
    /// columns for existing data and future server deployment, but the local
    /// product exposes a single flat list of knowledge spaces.
    pub fn knowledge_local_spaces(&self) -> Result<Vec<KnowledgeSpace>> {
        const TENANT: &str = "local";
        const CUSTOMER: &str = "local";
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let now = Utc::now().to_rfc3339();
        conn.execute("INSERT OR IGNORE INTO knowledge_tenants (id,name,created_at) VALUES (?1,'本地知识库',?2)", params![TENANT,now])?;
        conn.execute("INSERT OR IGNORE INTO knowledge_customers (id,tenant_id,name,created_at) VALUES (?1,?2,'本地',?3)", params![CUSTOMER,TENANT,Utc::now().to_rfc3339()])?;
        let mut statement = conn.prepare("SELECT id,tenant_id,customer_id,name,external_policy,created_at,goal,kind,archived_at FROM knowledge_spaces WHERE tenant_id=?1 AND customer_id=?2 ORDER BY name COLLATE NOCASE,id")?;
        let rows = statement.query_map(params![TENANT, CUSTOMER], |r| {
            Ok(KnowledgeSpace {
                id: r.get(0)?,
                tenant_id: r.get(1)?,
                customer_id: r.get(2)?,
                name: r.get(3)?,
                external_policy: r.get(4)?,
                created_at: r.get(5)?,
                goal: r.get(6)?,
                kind: r.get(7)?,
                archived_at: r.get(8)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn knowledge_create_local_space(
        &self,
        subject_id: &str,
        name: &str,
        external_policy: &str,
    ) -> Result<KnowledgeSpace> {
        let _ = self.knowledge_local_spaces()?;
        let space = self.knowledge_create_space("local", "local", name, external_policy)?;
        self.knowledge_set_member(&space.id, subject_id, KnowledgeRole::Owner)?;
        Ok(space)
    }

    pub fn open(data_dir: &Path, busy_ms: u64) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join("sessions.sqlite");
        // sqlite3_auto_extension only fires for connections opened *after*
        // registration, so this must precede Connection::open: the baseline
        // migration creates the vec0 table on this very connection.
        crate::corpus::register_sqlite_vec();
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_millis(busy_ms))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Startup phase timing (work-order §二): debug-level, per-step. This
        // path runs on every open (desktop does it more than once per launch),
        // so the breakdown here is what exposes redundant re-work.
        let t = std::time::Instant::now();
        conn.execute_batch(SCHEMA)?;
        tracing::debug!(elapsed_ms = t.elapsed().as_millis() as u64, "startup: schema batch");
        let t = std::time::Instant::now();
        migrate_sessions(&conn)?;
        tracing::debug!(elapsed_ms = t.elapsed().as_millis() as u64, "startup: migrate sessions");
        let t = std::time::Instant::now();
        repair_legacy_workspaces(&conn)?;
        tracing::debug!(elapsed_ms = t.elapsed().as_millis() as u64, "startup: repair legacy workspaces");
        let t = std::time::Instant::now();
        align_conversation_tree_workspaces(&conn)?;
        tracing::debug!(elapsed_ms = t.elapsed().as_millis() as u64, "startup: align conv tree");
        let t = std::time::Instant::now();
        if !table_columns(&conn, "agent_runs")?.iter().any(|column| column == "error_message") {
            conn.execute("ALTER TABLE agent_runs ADD COLUMN error_message TEXT", [])?;
        }
        // JSON array of `<data_dir>`-relative image paths attached to a user
        // turn. The blobs live on disk; the row only references them.
        if !table_columns(&conn, "messages")?.iter().any(|column| column == "attachments") {
            conn.execute("ALTER TABLE messages ADD COLUMN attachments TEXT", [])?;
        }
        tracing::debug!(elapsed_ms = t.elapsed().as_millis() as u64, "startup: column probes");
        let t = std::time::Instant::now();
        migrate_space_baseline(&conn)?;
        migrate_knowledge_fts(&conn)?;
        tracing::debug!(elapsed_ms = t.elapsed().as_millis() as u64, "startup: space baseline");
        // Mirror the workspace→space registry into the pure-wiki resolver
        // cache (wiki::resolve_project_data_dir) so call sites that only
        // carry `data_dir` agree with the database within one process.
        {
            let mut statement = conn.prepare(
                "SELECT workspace_hash,space_id FROM knowledge_space_workspaces",
            )?;
            let rows = statement
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .filter_map(std::result::Result::ok)
                .collect::<Vec<_>>();
            drop(statement);
            crate::wiki::load_space_dir_cache(data_dir, &rows);
        }
        Ok(Self {
            item_write: Mutex::new(()),
            item_assets: Mutex::new(()),
            conn: Mutex::new(conn),
            context_cache: Mutex::new(HashMap::new()),
            context_generation: AtomicU64::new(0),
            data_dir: data_dir.to_path_buf(),
        })
    }

    pub fn knowledge_create_tenant(&self, name: &str) -> Result<KnowledgeTenant> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Invalid("tenant name is required".into()));
        }
        let item = KnowledgeTenant {
            id: format!("tenant_{}", uuid::Uuid::new_v4().simple()),
            name: name.into(),
            created_at: Utc::now().to_rfc3339(),
        };
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "INSERT INTO knowledge_tenants (id,name,created_at) VALUES (?1,?2,?3)",
            params![item.id, item.name, item.created_at],
        )?;
        Ok(item)
    }

    pub fn knowledge_tenants(&self) -> Result<Vec<KnowledgeTenant>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut statement = conn.prepare(
            "SELECT id,name,created_at FROM knowledge_tenants ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = statement.query_map([], |r| {
            Ok(KnowledgeTenant {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
    pub fn knowledge_create_customer(
        &self,
        tenant_id: &str,
        name: &str,
    ) -> Result<KnowledgeCustomer> {
        let name = name.trim();
        if tenant_id.trim().is_empty() || name.is_empty() {
            return Err(Error::Invalid(
                "customer tenant and name are required".into(),
            ));
        }
        let item = KnowledgeCustomer {
            id: format!("customer_{}", uuid::Uuid::new_v4().simple()),
            tenant_id: tenant_id.into(),
            name: name.into(),
            created_at: Utc::now().to_rfc3339(),
        };
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let exists: Option<String> = conn
            .query_row(
                "SELECT id FROM knowledge_tenants WHERE id=?1",
                params![tenant_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(Error::Invalid("knowledge tenant was not found".into()));
        }
        conn.execute(
            "INSERT INTO knowledge_customers (id,tenant_id,name,created_at) VALUES (?1,?2,?3,?4)",
            params![item.id, item.tenant_id, item.name, item.created_at],
        )?;
        Ok(item)
    }

    pub fn knowledge_customers(&self, tenant_id: &str) -> Result<Vec<KnowledgeCustomer>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut statement = conn.prepare(
            "SELECT id,tenant_id,name,created_at FROM knowledge_customers WHERE tenant_id=?1 ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = statement.query_map(params![tenant_id], |r| {
            Ok(KnowledgeCustomer {
                id: r.get(0)?,
                tenant_id: r.get(1)?,
                name: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
    pub fn knowledge_create_space(
        &self,
        tenant_id: &str,
        customer_id: &str,
        name: &str,
        external_policy: &str,
    ) -> Result<KnowledgeSpace> {
        let name = name.trim();
        if tenant_id.trim().is_empty() || customer_id.trim().is_empty() || name.is_empty() {
            return Err(Error::Invalid(
                "knowledge space scope and name are required".into(),
            ));
        }
        let policy = match external_policy.trim() {
            "" => "local_only",
            "local_only" | "allow_summary" | "deny_cloud" => external_policy.trim(),
            _ => return Err(Error::Invalid("invalid knowledge external policy".into())),
        };
        let item = KnowledgeSpace {
            id: format!("space_{}", uuid::Uuid::new_v4().simple()),
            tenant_id: tenant_id.into(),
            customer_id: customer_id.into(),
            name: name.into(),
            external_policy: policy.into(),
            created_at: Utc::now().to_rfc3339(),
            goal: None,
            // Column default is 'docs'; wiki/corpus spaces are typed by the
            // services that create them (phases 2/5), not by this generic call.
            kind: "docs".into(),
            archived_at: None,
        };
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let valid: Option<String> = conn
            .query_row(
                "SELECT id FROM knowledge_customers WHERE id=?1 AND tenant_id=?2",
                params![customer_id, tenant_id],
                |r| r.get(0),
            )
            .optional()?;
        if valid.is_none() {
            return Err(Error::Invalid(
                "knowledge customer is outside this tenant".into(),
            ));
        }
        conn.execute("INSERT INTO knowledge_spaces (id,tenant_id,customer_id,name,external_policy,created_at) VALUES (?1,?2,?3,?4,?5,?6)",params![item.id,item.tenant_id,item.customer_id,item.name,item.external_policy,item.created_at])?;
        Ok(item)
    }

    pub fn knowledge_spaces(
        &self,
        tenant_id: &str,
        customer_id: &str,
    ) -> Result<Vec<KnowledgeSpace>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut statement = conn.prepare(
            "SELECT id,tenant_id,customer_id,name,external_policy,created_at,goal,kind,archived_at FROM knowledge_spaces WHERE tenant_id=?1 AND customer_id=?2 ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = statement.query_map(params![tenant_id, customer_id], |r| {
            Ok(KnowledgeSpace {
                id: r.get(0)?,
                tenant_id: r.get(1)?,
                customer_id: r.get(2)?,
                name: r.get(3)?,
                external_policy: r.get(4)?,
                created_at: r.get(5)?,
                goal: r.get(6)?,
                kind: r.get(7)?,
                archived_at: r.get(8)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Read one space by id — the identity check behind every host-side
    /// space attribution (wiki submissions, bridge grants, draft
    /// resolution). Names and paths are never consulted: the id is the fact.
    pub fn knowledge_space_by_id(&self, space_id: &str) -> Result<Option<KnowledgeSpace>> {
        if space_id.trim().is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.query_row(
            "SELECT id,tenant_id,customer_id,name,external_policy,created_at,goal,kind,archived_at FROM knowledge_spaces WHERE id=?1",
            params![space_id],
            |r| Ok(KnowledgeSpace {
                id: r.get(0)?,
                tenant_id: r.get(1)?,
                customer_id: r.get(2)?,
                name: r.get(3)?,
                external_policy: r.get(4)?,
                created_at: r.get(5)?,
                goal: r.get(6)?,
                kind: r.get(7)?,
                archived_at: r.get(8)?,
            }),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Rename a space. The name is presentation only — the stable `space_` id
    /// and every binding (documents, proposals, workspace registry, published
    /// wiki directory) survive untouched. Duplicate names inside the same
    /// tenant+customer scope are refused.
    pub fn knowledge_rename_space(&self, space_id: &str, name: &str) -> Result<()> {
        let name = name.trim();
        if space_id.trim().is_empty() || name.is_empty() {
            return Err(Error::Invalid("space id and new name are required".into()));
        }
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let scope: Option<(String, String)> = conn
            .query_row(
                "SELECT tenant_id,customer_id FROM knowledge_spaces WHERE id=?1",
                params![space_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (tenant_id, customer_id) =
            scope.ok_or_else(|| Error::Invalid("knowledge space not found".into()))?;
        let clash: Option<String> = conn
            .query_row(
                "SELECT id FROM knowledge_spaces WHERE tenant_id=?1 AND customer_id=?2 AND name=?3 AND id<>?4",
                params![tenant_id, customer_id, name, space_id],
                |r| r.get(0),
            )
            .optional()?;
        if clash.is_some() {
            return Err(Error::Invalid("a space with this name already exists".into()));
        }
        conn.execute(
            "UPDATE knowledge_spaces SET name=?2 WHERE id=?1",
            params![space_id, name],
        )?;
        Ok(())
    }

    /// Archive (or unarchive) a space. Archiving hides it from active lists;
    /// it never deletes material, versions or citations.
    pub fn knowledge_archive_space(&self, space_id: &str, archived: bool) -> Result<()> {
        if space_id.trim().is_empty() {
            return Err(Error::Invalid("space id is required".into()));
        }
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let updated = conn.execute(
            "UPDATE knowledge_spaces SET archived_at=?2 WHERE id=?1",
            params![space_id, if archived { Some(Utc::now().to_rfc3339()) } else { None }],
        )?;
        if updated == 0 {
            return Err(Error::Invalid("knowledge space not found".into()));
        }
        Ok(())
    }

    /// Set or clear the optional organizing goal. The goal may steer future
    /// planning; changing it never invalidates published content.
    pub fn knowledge_set_space_goal(&self, space_id: &str, goal: Option<&str>) -> Result<()> {
        if space_id.trim().is_empty() {
            return Err(Error::Invalid("space id is required".into()));
        }
        let goal = goal.map(str::trim).filter(|g| !g.is_empty());
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let updated = conn.execute(
            "UPDATE knowledge_spaces SET goal=?2 WHERE id=?1",
            params![space_id, goal],
        )?;
        if updated == 0 {
            return Err(Error::Invalid("knowledge space not found".into()));
        }
        Ok(())
    }

    /// Type a space (`docs` | `wiki` | `corpus`). Only the services that own
    /// a space call this — the generic create API keeps the `docs` default.
    pub fn knowledge_set_space_kind(&self, space_id: &str, kind: &str) -> Result<()> {
        if !matches!(kind, "docs" | "wiki" | "corpus") {
            return Err(Error::Invalid("invalid knowledge space kind".into()));
        }
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let updated =
            conn.execute("UPDATE knowledge_spaces SET kind=?2 WHERE id=?1", params![space_id, kind])?;
        if updated == 0 {
            return Err(Error::Invalid("knowledge space not found".into()));
        }
        Ok(())
    }

    /// Change a space's external-model policy (`allow_summary` | `local_only`
    /// | `deny_cloud`). Changing policy rewrites no content — every serve and
    /// publish decision re-reads this column, so a flip takes effect on the
    /// very next request. The Runtime wrapper gates this as a management
    /// action (Owner), not an authoring one.
    pub fn knowledge_set_space_external_policy(&self, space_id: &str, policy: &str) -> Result<()> {
        if space_id.trim().is_empty() {
            return Err(Error::Invalid("space id is required".into()));
        }
        if !matches!(policy, "allow_summary" | "local_only" | "deny_cloud") {
            return Err(Error::Invalid("invalid external policy".into()));
        }
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let updated = conn.execute(
            "UPDATE knowledge_spaces SET external_policy=?2 WHERE id=?1",
            params![space_id, policy],
        )?;
        if updated == 0 {
            return Err(Error::Invalid("knowledge space not found".into()));
        }
        Ok(())
    }

    /// All `(workspace_hash, space_id)` registry rows (for the resolver cache).
    pub fn knowledge_all_workspace_bindings(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut statement =
            conn.prepare("SELECT workspace_hash,space_id FROM knowledge_space_workspaces")?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Registry row binding a workspace directory (hash + last-known path) to
    /// a space id. Idempotent upsert; re-registering refreshes the path.
    pub fn knowledge_register_workspace(
        &self,
        space_id: &str,
        workspace_hash: &str,
        workspace_path: &str,
    ) -> Result<()> {
        if space_id.trim().is_empty() || workspace_hash.trim().is_empty() {
            return Err(Error::Invalid("space id and workspace hash are required".into()));
        }
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "INSERT INTO knowledge_space_workspaces (space_id,workspace_hash,workspace_path,created_at)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(space_id) DO UPDATE SET workspace_hash=excluded.workspace_hash,
               workspace_path=excluded.workspace_path",
            params![space_id, workspace_hash, workspace_path, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn knowledge_workspace_for_space(
        &self,
        space_id: &str,
    ) -> Result<Option<(String, String)>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.query_row(
            "SELECT workspace_hash,workspace_path FROM knowledge_space_workspaces WHERE space_id=?1",
            params![space_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn knowledge_space_for_workspace(
        &self,
        workspace_hash: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.query_row(
            "SELECT space_id FROM knowledge_space_workspaces WHERE workspace_hash=?1",
            params![workspace_hash],
            |r| r.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Read one `dandelion_meta` flag (migration bookkeeping, plan-rules
    /// version seen, …).
    pub fn meta_value(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.query_row("SELECT value FROM dandelion_meta WHERE key=?1", params![key], |r| {
            r.get(0)
        })
        .optional()
        .map_err(Into::into)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "INSERT INTO dandelion_meta (key,value) VALUES (?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn knowledge_create_collection(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        name: &str,
    ) -> Result<KnowledgeCollection> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Invalid(
                "knowledge collection name is required".into(),
            ));
        }
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Editor)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let item = KnowledgeCollection {
            id: format!("collection_{}", uuid::Uuid::new_v4().simple()),
            tenant_id: tenant_id.into(),
            customer_id: customer_id.into(),
            space_id: space_id.into(),
            name: name.into(),
            created_at: Utc::now().to_rfc3339(),
        };
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "INSERT INTO knowledge_collections (id,tenant_id,customer_id,space_id,name,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![item.id,item.tenant_id,item.customer_id,item.space_id,item.name,item.created_at],
        )?;
        Ok(item)
    }

    pub fn knowledge_collections(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
    ) -> Result<Vec<KnowledgeCollection>> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Reader)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut statement = conn.prepare(
            "SELECT id,tenant_id,customer_id,space_id,name,created_at FROM knowledge_collections WHERE tenant_id=?1 AND customer_id=?2 AND space_id=?3 ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = statement.query_map(params![tenant_id, customer_id, space_id], |r| {
            Ok(KnowledgeCollection {
                id: r.get(0)?,
                tenant_id: r.get(1)?,
                customer_id: r.get(2)?,
                space_id: r.get(3)?,
                name: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn knowledge_assign_collection(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        collection_id: &str,
        document_id: &str,
    ) -> Result<()> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Editor)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let valid: Option<String> = conn
            .query_row(
                "SELECT c.id FROM knowledge_collections c JOIN knowledge_documents d ON d.id=?2
             WHERE c.id=?1 AND c.tenant_id=?3 AND c.customer_id=?4 AND c.space_id=?5
               AND d.tenant_id=?3 AND d.customer_id=?4 AND d.space_id=?5 AND d.deleted_at IS NULL",
                params![collection_id, document_id, tenant_id, customer_id, space_id],
                |r| r.get(0),
            )
            .optional()?;
        if valid.is_none() {
            return Err(Error::Invalid(
                "collection or document is outside the requested scope".into(),
            ));
        }
        conn.execute("INSERT OR IGNORE INTO knowledge_collection_documents (collection_id,document_id) VALUES (?1,?2)", params![collection_id, document_id])?;
        Ok(())
    }

    pub fn knowledge_documents(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
    ) -> Result<Vec<KnowledgeDocument>> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Reader)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        // Membership is the link table (§三): a space sees its own documents
        // plus every material linked in from elsewhere in the same customer.
        let mut statement = conn.prepare(
            "SELECT d.id,d.tenant_id,d.customer_id,d.space_id,d.source_kind,d.source_id,d.status,d.created_at \
             FROM knowledge_documents d \
             WHERE d.tenant_id=?1 AND d.customer_id=?2 AND d.deleted_at IS NULL \
               AND EXISTS (SELECT 1 FROM knowledge_space_links l WHERE l.document_id=d.id AND l.space_id=?3) \
             ORDER BY d.created_at DESC, d.id",
        )?;
        let rows = statement.query_map(params![tenant_id, customer_id, space_id], |r| {
            Ok(KnowledgeDocument {
                id: r.get(0)?,
                tenant_id: r.get(1)?,
                customer_id: r.get(2)?,
                space_id: r.get(3)?,
                source_kind: r.get(4)?,
                source_id: r.get(5)?,
                status: r.get(6)?,
                created_at: r.get(7)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Published (non-revoked) versions with their full text, gated and
    /// scoped exactly like [`Self::knowledge_documents`]. The space deep
    /// build materializes exactly these: drafts and revoked versions never
    /// reach evidence, and a deleted document leaves no row here.
    pub fn knowledge_published_documents(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
    ) -> Result<Vec<KnowledgePublishedDocument>> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Reader)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        // One row per material linked into this space, resolved through the
        // link's binding: `follow` rides the currently effective version
        // (published, not withdrawn, not superseded), `pinned` is frozen on
        // its pinned number — superseding is fine (that is the whole point of
        // a fixed historical version), but a WITHDRAWN pinned version
        // resolves to NOTHING (fail closed: the material simply stops being
        // evidence here while other spaces keep following).
        let mut statement = conn.prepare(
            "SELECT d.id,d.source_kind,d.source_id,v.id,v.version_no,v.sha256,v.content \
             FROM knowledge_space_links l \
             JOIN knowledge_documents d ON d.id=l.document_id \
             JOIN knowledge_document_versions v ON v.document_id=d.id \
             WHERE d.tenant_id=?1 AND d.customer_id=?2 AND l.space_id=?3 \
               AND d.deleted_at IS NULL AND v.published_at IS NOT NULL AND v.revoked_at IS NULL \
               AND v.version_no = CASE WHEN l.binding='pinned' AND l.pinned_version_no IS NOT NULL \
                     THEN l.pinned_version_no \
                     ELSE (SELECT MAX(v2.version_no) FROM knowledge_document_versions v2 \
                            WHERE v2.document_id=d.id AND v2.published_at IS NOT NULL \
                              AND v2.revoked_at IS NULL AND v2.superseded_at IS NULL) END \
             ORDER BY d.source_id, d.id",
        )?;
        let rows = statement.query_map(params![tenant_id, customer_id, space_id], |r| {
            Ok(KnowledgePublishedDocument {
                document_id: r.get(0)?,
                source_kind: r.get(1)?,
                source_id: r.get(2)?,
                version_id: r.get(3)?,
                version_no: r.get(4)?,
                sha256: r.get(5)?,
                content: r.get(6)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn knowledge_set_member(
        &self,
        space_id: &str,
        subject_id: &str,
        role: KnowledgeRole,
    ) -> Result<()> {
        if space_id.trim().is_empty() || subject_id.trim().is_empty() {
            return Err(Error::Invalid("knowledge member scope is required".into()));
        }
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let exists: Option<String> = conn
            .query_row(
                "SELECT id FROM knowledge_spaces WHERE id=?1",
                params![space_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(Error::Invalid("knowledge space was not found".into()));
        }
        conn.execute("INSERT INTO knowledge_space_members (space_id,subject_id,role,created_at) VALUES (?1,?2,?3,?4) ON CONFLICT(space_id,subject_id) DO UPDATE SET role=excluded.role", params![space_id,subject_id,serde_json::to_string(&role)?,Utc::now().to_rfc3339()])?;
        Ok(())
    }

    pub fn knowledge_members(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
    ) -> Result<Vec<KnowledgeMember>> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut statement = conn.prepare("SELECT subject_id,role,created_at FROM knowledge_space_members WHERE space_id=?1 ORDER BY created_at,subject_id")?;
        let rows = statement.query_map(params![space_id], |row| {
            Ok(KnowledgeMember {
                subject_id: row.get(0)?,
                role: serde_json::from_str(&row.get::<_, String>(1)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                created_at: row.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn knowledge_manage_member(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        actor_id: &str,
        member_id: &str,
        role: KnowledgeRole,
    ) -> Result<()> {
        self.knowledge_authorize(space_id, actor_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        if member_id.trim().is_empty() {
            return Err(Error::Invalid("member identity is required".into()));
        }
        self.knowledge_set_member(space_id, member_id.trim(), role)
    }

    pub fn knowledge_remove_member(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        actor_id: &str,
        member_id: &str,
    ) -> Result<()> {
        self.knowledge_authorize(space_id, actor_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let owners: i64 = conn.query_row(
            "SELECT COUNT(*) FROM knowledge_space_members WHERE space_id=?1 AND role='\"owner\"'",
            params![space_id],
            |row| row.get(0),
        )?;
        let target_role: Option<String> = conn
            .query_row(
                "SELECT role FROM knowledge_space_members WHERE space_id=?1 AND subject_id=?2",
                params![space_id, member_id.trim()],
                |row| row.get(0),
            )
            .optional()?;
        if target_role.as_deref() == Some("owner") && owners <= 1 {
            return Err(Error::Invalid(
                "cannot remove the last knowledge-space owner".into(),
            ));
        }
        if conn.execute(
            "DELETE FROM knowledge_space_members WHERE space_id=?1 AND subject_id=?2",
            params![space_id, member_id.trim()],
        )? != 1
        {
            return Err(Error::Invalid("knowledge member was not found".into()));
        }
        Ok(())
    }

    pub fn knowledge_create_api_client(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        label: &str,
        max_requests_per_minute: u32,
        max_answer_tokens: u32,
    ) -> Result<KnowledgeApiProvision> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        if label.trim().is_empty()
            || !(1..=1_000).contains(&max_requests_per_minute)
            || !(64..=16_384).contains(&max_answer_tokens)
        {
            return Err(Error::Invalid("invalid API client limits".into()));
        }
        let client = KnowledgeApiClient {
            id: format!("kbapi_{}", uuid::Uuid::new_v4().simple()),
            label: label.trim().into(),
            max_requests_per_minute,
            max_answer_tokens,
            created_at: Utc::now().to_rfc3339(),
            revoked_at: None,
        };
        let token = format!("dlnkb_{}", uuid::Uuid::new_v4().simple());
        let token_sha256 = sha256_hex(&token);
        let api_subject = format!("api:{}", client.id);
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute("INSERT INTO knowledge_api_clients (id,tenant_id,customer_id,space_id,token_sha256,label,max_requests_per_minute,max_answer_tokens,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![client.id,tenant_id,customer_id,space_id,token_sha256,client.label,client.max_requests_per_minute,client.max_answer_tokens,client.created_at])?;
        conn.execute("INSERT INTO knowledge_space_members (space_id,subject_id,role,created_at) VALUES (?1,?2,'\"reader\"',?3)", params![space_id,api_subject,Utc::now().to_rfc3339()])?;
        Ok(KnowledgeApiProvision { client, token })
    }

    pub fn knowledge_api_answer(
        &self,
        token: &str,
        question: &str,
        requested_tokens: u32,
    ) -> Result<KnowledgeAnswer> {
        let token_sha256 = sha256_hex(token.trim());
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let client: Option<(String,String,String,String,u32,u32)> = conn.query_row("SELECT id,tenant_id,customer_id,space_id,max_requests_per_minute,max_answer_tokens FROM knowledge_api_clients WHERE token_sha256=?1 AND revoked_at IS NULL", params![token_sha256], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).optional()?;
        let Some((id, tenant_id, customer_id, space_id, max_rpm, max_tokens)) = client else {
            return Err(Error::Invalid(
                "knowledge API token is invalid or revoked".into(),
            ));
        };
        if requested_tokens == 0 || requested_tokens > max_tokens {
            return Err(Error::Invalid(
                "knowledge API token budget exceeds client scope".into(),
            ));
        }
        let since = (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
        let requests: u32 = conn.query_row(
            "SELECT COUNT(*) FROM knowledge_api_usage WHERE client_id=?1 AND created_at>=?2",
            params![id, since],
            |row| row.get(0),
        )?;
        if requests >= max_rpm {
            return Err(Error::Invalid("knowledge API rate limit exceeded".into()));
        }
        drop(conn);
        let mut answer = self.knowledge_answer(
            &tenant_id,
            &customer_id,
            &space_id,
            &format!("api:{id}"),
            question,
            6,
        )?;
        answer.answer = truncate_to_tokens(&answer.answer, requested_tokens);
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute("INSERT INTO knowledge_api_usage (id,client_id,created_at,request_tokens,no_answer,citation_count) VALUES (?1,?2,?3,?4,?5,?6)", params![format!("apiuse_{}",uuid::Uuid::new_v4().simple()),id,Utc::now().to_rfc3339(),requested_tokens,answer.no_answer as i32,answer.citations.len() as i64])?;
        Ok(answer)
    }

    pub fn knowledge_authorize(
        &self,
        space_id: &str,
        subject_id: &str,
        required: KnowledgeRole,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT role FROM knowledge_space_members WHERE space_id=?1 AND subject_id=?2",
                params![space_id, subject_id],
                |r| r.get(0),
            )
            .optional()?;
        let actual = raw
            .map(|v| serde_json::from_str::<KnowledgeRole>(&v))
            .transpose()?
            .ok_or_else(|| Error::Invalid("knowledge access denied".into()))?;
        let rank = |role| match role {
            KnowledgeRole::Reader => 0,
            KnowledgeRole::Editor => 1,
            KnowledgeRole::Owner => 2,
        };
        if rank(actual) < rank(required) {
            return Err(Error::Invalid(
                "knowledge role does not permit this operation".into(),
            ));
        }
        Ok(())
    }

    fn knowledge_assert_scope(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let exists: Option<String> = conn
            .query_row(
                "SELECT id FROM knowledge_spaces WHERE id=?1 AND tenant_id=?2 AND customer_id=?3",
                params![space_id, tenant_id, customer_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(Error::Invalid(
                "knowledge space is outside the requested scope".into(),
            ));
        }
        Ok(())
    }

    /// Add an immutable document revision and its local projections.  The
    /// caller supplies the full scope and actor; every lookup verifies that
    /// scope before writing, so a customer document cannot be moved across a
    /// tenant merely by passing another ID.
    pub fn knowledge_import_document(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        source_kind: &str,
        source_id: &str,
        content: &str,
        chunk_chars: usize,
    ) -> Result<(KnowledgeDocument, KnowledgeDocumentVersion)> {
        if source_kind.trim().is_empty() || source_id.trim().is_empty() || content.trim().is_empty()
        {
            return Err(Error::Invalid(
                "knowledge document source and content are required".into(),
            ));
        }
        if !(128..=16_384).contains(&chunk_chars) {
            return Err(Error::Invalid(
                "knowledge chunk size must be 128..16384 characters".into(),
            ));
        }
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Editor)?;
        let now = Utc::now().to_rfc3339();
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let sha256 = format!("sha256:{:x}", hasher.finalize());
        let chunks = knowledge_chunks(content, chunk_chars);
        let mut conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let tx = conn.transaction()?;
        let scoped: Option<String> = tx
            .query_row(
                "SELECT id FROM knowledge_spaces WHERE id=?1 AND tenant_id=?2 AND customer_id=?3",
                params![space_id, tenant_id, customer_id],
                |r| r.get(0),
            )
            .optional()?;
        if scoped.is_none() {
            return Err(Error::Invalid(
                "knowledge space is outside the requested scope".into(),
            ));
        }
        // Material identity (§三): the SAME material — same tenant, customer,
        // source kind and source id — is ONE document with ONE version
        // history, no matter how many spaces claim it. Importing into a
        // second space therefore never forks a parallel history: it resolves
        // the existing document (preferring one already linked to THIS
        // space, so legacy per-space duplicates keep their space's view)
        // and links it. A new material gets a new document and a link.
        let existing: Option<(String, i64, String, String, String)> = tx
            .query_row(
                "SELECT * FROM (\
                   SELECT d.id, COALESCE(MAX(v.version_no), 0) AS max_no, d.created_at, d.status, d.space_id, \
                          (SELECT COUNT(*) FROM knowledge_space_links l \
                            WHERE l.document_id=d.id AND l.space_id=?3) AS linked \
                   FROM knowledge_documents d \
                   LEFT JOIN knowledge_document_versions v ON v.document_id=d.id \
                   WHERE d.tenant_id=?1 AND d.customer_id=?2 AND d.source_kind=?4 AND d.source_id=?5 \
                     AND d.deleted_at IS NULL \
                   GROUP BY d.id \
                 ) ORDER BY linked DESC, created_at, id LIMIT 1",
                params![tenant_id, customer_id, space_id, source_kind.trim(), source_id.trim()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let link_row = |tx: &rusqlite::Transaction<'_>, document_id: &str, now: &str| -> Result<()> {
            tx.execute(
                "INSERT OR IGNORE INTO knowledge_space_links (space_id,document_id,binding,pinned_version_no,added_by,created_at)
                 VALUES (?1,?2,'follow',NULL,?3,?4)",
                params![space_id, document_id, subject_id, now],
            )?;
            Ok(())
        };
        let (document, version_no) = match existing {
            Some((id, prior_version, created_at, status, home_space_id)) => {
                link_row(&tx, &id, &now)?;
                let current: Option<(String, i64, String, String, i64)> = tx
                    .query_row(
                        "SELECT id,version_no,sha256,created_at,parser_version FROM knowledge_document_versions WHERE document_id=?1 ORDER BY version_no DESC LIMIT 1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, Option<i64>>(4)?.unwrap_or(1))),
                    )
                    .optional()?;
                if let Some((version_id, version_no, prior_sha, version_created_at, version_parser)) = current {
                    if prior_sha == sha256 {
                        let document = KnowledgeDocument {
                            id: id.clone(),
                            tenant_id: tenant_id.into(),
                            customer_id: customer_id.into(),
                            space_id: home_space_id.clone(),
                            source_kind: source_kind.trim().into(),
                            source_id: source_id.trim().into(),
                            status,
                            created_at,
                        };
                        tx.commit()?;
                        return Ok((
                            document,
                            KnowledgeDocumentVersion {
                                id: version_id,
                                document_id: id,
                                version_no,
                                sha256,
                                created_at: version_created_at,
                                parser_version: version_parser,
                            },
                        ));
                    }
                }
                tx.execute(
                    "UPDATE knowledge_documents SET status='draft', deleted_at=NULL WHERE id=?1",
                    params![id],
                )?;
                (
                    KnowledgeDocument {
                        id,
                        tenant_id: tenant_id.into(),
                        customer_id: customer_id.into(),
                        space_id: home_space_id,
                        source_kind: source_kind.trim().into(),
                        source_id: source_id.trim().into(),
                        status: "draft".into(),
                        created_at,
                    },
                    prior_version + 1,
                )
            }
            None => {
                let document = KnowledgeDocument {
                    id: format!("document_{}", uuid::Uuid::new_v4().simple()),
                    tenant_id: tenant_id.into(),
                    customer_id: customer_id.into(),
                    space_id: space_id.into(),
                    source_kind: source_kind.trim().into(),
                    source_id: source_id.trim().into(),
                    status: "draft".into(),
                    created_at: now.clone(),
                };
                tx.execute(
                    "INSERT INTO knowledge_documents (id,tenant_id,customer_id,space_id,source_kind,source_id,status,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![document.id, document.tenant_id, document.customer_id, document.space_id,
                        document.source_kind, document.source_id, document.status, document.created_at],
                )?;
                link_row(&tx, &document.id, &now)?;
                (document, 1)
            }
        };
        let version = KnowledgeDocumentVersion {
            id: format!("docver_{}", uuid::Uuid::new_v4().simple()),
            document_id: document.id.clone(),
            version_no,
            sha256,
            created_at: now,
            parser_version: crate::wiki_adapters::PARSER_VERSION,
        };
        tx.execute(
            "INSERT INTO knowledge_document_versions (id,document_id,version_no,sha256,content,created_at,parser_version) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![version.id, version.document_id, version.version_no, version.sha256, content, version.created_at, crate::wiki_adapters::PARSER_VERSION],
        )?;
        for (ordinal, chunk) in chunks.iter().enumerate() {
            tx.execute(
                "INSERT INTO knowledge_chunks (id,version_id,ordinal,content) VALUES (?1,?2,?3,?4)",
                params![
                    format!("chunk_{}", uuid::Uuid::new_v4().simple()),
                    version.id,
                    ordinal as i64,
                    chunk
                ],
            )?;
        }
        tx.commit()?;
        Ok((document, version))
    }

    pub fn knowledge_publish_version(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        document_id: &str,
        version_id: &str,
    ) -> Result<()> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Owner)?;
        let mut conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let tx = conn.transaction()?;
        let scoped: Option<String> = tx.query_row(
            "SELECT v.id FROM knowledge_document_versions v JOIN knowledge_documents d ON d.id=v.document_id
             WHERE v.id=?1 AND v.document_id=?2 AND d.space_id=?3 AND d.tenant_id=?4 AND d.customer_id=?5 AND d.deleted_at IS NULL",
            params![version_id, document_id, space_id, tenant_id, customer_id], |r| r.get(0),
        ).optional()?;
        if scoped.is_none() {
            return Err(Error::Invalid(
                "knowledge document version is outside the requested scope".into(),
            ));
        }
        // Supersede, not revoke: publishing a newer version moves the
        // currently effective pointer, but the older version is NOT withdrawn.
        // Only `knowledge_revoke_version` sets revoked_at — §三's fixed
        // historical versions depend on the difference: a space pinned on a
        // superseded version keeps reading it, while a revoked version is
        // evidence nowhere (fail closed, even for a pin).
        let now = Utc::now().to_rfc3339();
        tx.execute("UPDATE knowledge_document_versions SET superseded_at=?1 WHERE document_id=?2 AND published_at IS NOT NULL AND id<>?3 AND superseded_at IS NULL AND revoked_at IS NULL", params![now, document_id, version_id])?;
        tx.execute(
            "UPDATE knowledge_document_versions SET published_at=?1, revoked_at=NULL, superseded_at=NULL WHERE id=?2",
            params![Utc::now().to_rfc3339(), version_id],
        )?;
        tx.execute(
            "UPDATE knowledge_documents SET status='published' WHERE id=?1",
            params![document_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn knowledge_revoke_version(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        document_id: &str,
        version_id: &str,
    ) -> Result<()> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let changed = conn.execute(
            "UPDATE knowledge_document_versions SET revoked_at=?1 WHERE id=?2 AND document_id=?3 AND published_at IS NOT NULL AND revoked_at IS NULL
             AND EXISTS (SELECT 1 FROM knowledge_documents d WHERE d.id=?3 AND d.tenant_id=?4 AND d.customer_id=?5 AND d.space_id=?6 AND d.deleted_at IS NULL)",
            params![Utc::now().to_rfc3339(), version_id, document_id, tenant_id, customer_id, space_id],
        )?;
        if changed != 1 {
            return Err(Error::Invalid(
                "published knowledge version was not found in the requested scope".into(),
            ));
        }
        // Draft only if NOTHING effective remains: revoking the live version
        // demotes the material, but withdrawing a superseded one (the pin
        // kill-switch) leaves the live version — and the material's status —
        // untouched.
        conn.execute(
            "UPDATE knowledge_documents SET status='draft' WHERE id=?1 AND NOT EXISTS \
             (SELECT 1 FROM knowledge_document_versions v WHERE v.document_id=?1 \
              AND v.published_at IS NOT NULL AND v.revoked_at IS NULL AND v.superseded_at IS NULL)",
            params![document_id],
        )?;
        Ok(())
    }

    // ---- Evidence spans (§八): clickable citations resolve to a FROZEN
    // source version. Written only by the space deep build: every
    // `rel:Lx-Ly` token that survived the publish gate on a published page
    // is recorded with the document id and the version number the build
    // actually read, so 「查看原文」 can show exactly the content the
    // citation was valid for — even after the document moves on (supersede
    // is not revoke) or the reader clicks months later. A rebuild replaces
    // the space's span set atomically, so tokens from withdrawn pages
    // disappear with them. This is an INDEX, not a second knowledge store:
    // the page bodies and version contents stay the single source of truth.

    /// Replace one space's evidence span set in a single transaction
    /// (DELETE + INSERT: readers never observe a half-recorded build).
    /// Rows are expected deduped by `legacy_token` at the call site.
    pub fn knowledge_evidence_spans_replace(
        &self,
        space_id: &str,
        rows: &[EvidenceSpanRow],
    ) -> Result<usize> {
        let mut conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM knowledge_evidence_spans WHERE space_id=?1",
            params![space_id],
        )?;
        let now = Utc::now().to_rfc3339();
        for row in rows {
            tx.execute(
                "INSERT INTO knowledge_evidence_spans (id,space_id,document_id,source_version,locator,legacy_token,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![format!("span_{}", uuid::Uuid::new_v4().simple()), space_id, row.document_id, row.source_version, row.locator, row.legacy_token, now],
            )?;
        }
        tx.commit()?;
        Ok(rows.len())
    }

    /// Resolve a clicked citation token within one space: the document and
    /// the frozen version number it was recorded against. A miss is the
    /// honest answer (no published page carries this token, or the space
    /// was built before recording existed).
    pub fn knowledge_evidence_span_lookup(
        &self,
        space_id: &str,
        legacy_token: &str,
    ) -> Result<Option<(String, String)>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.query_row(
            "SELECT document_id,source_version FROM knowledge_evidence_spans \
             WHERE space_id=?1 AND legacy_token=?2 AND document_id IS NOT NULL AND source_version IS NOT NULL \
             ORDER BY created_at LIMIT 1",
            params![space_id, legacy_token],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Fetch a cited version by (document id, version number) within the
    /// reader's tenant/customer — the SAME scope a shared material crosses
    /// spaces on (A5: history lives in the home space, sharing never
    /// leaves the tenant line). `None` when the document was deleted
    /// (versions cascade away with it): the caller refuses as withdrawn.
    pub fn knowledge_cited_version(
        &self,
        tenant_id: &str,
        customer_id: &str,
        document_id: &str,
        version_no: i64,
    ) -> Result<Option<KnowledgeCitedVersion>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.query_row(
            "SELECT d.source_kind,d.source_id,d.space_id,v.version_no,v.sha256,v.parser_version, \
               v.revoked_at IS NOT NULL, v.superseded_at IS NOT NULL, v.content \
             FROM knowledge_documents d JOIN knowledge_document_versions v ON v.document_id=d.id \
             WHERE d.id=?1 AND d.tenant_id=?2 AND d.customer_id=?3 AND v.version_no=?4",
            params![document_id, tenant_id, customer_id, version_no],
            |r| Ok(KnowledgeCitedVersion {
                source_kind: r.get(0)?,
                source_id: r.get(1)?,
                source_space_id: r.get(2)?,
                version_no: r.get(3)?,
                sha256: r.get(4)?,
                parser_version: r.get::<_, Option<i64>>(5)?.unwrap_or(1),
                revoked: r.get::<_, i64>(6)? != 0,
                superseded: r.get::<_, i64>(7)? != 0,
                content: r.get(8)?,
            }),
        )
        .optional()
        .map_err(Into::into)
    }

    // ---- Space association (§三): one material, many spaces ----
    //
    // A material's version HISTORY belongs to its home space; which spaces
    // may use it, and WHICH version each one uses, lives only in
    // `knowledge_space_links`. Sharing never crosses tenant/customer lines
    // and never copies history: linking the same material into a second
    // space shows the SAME document id and the SAME versions there.

    /// The space's material list with binding and resolution detail
    /// (Reader and up).
    pub fn knowledge_space_materials(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
    ) -> Result<Vec<KnowledgeSpaceMaterial>> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Reader)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut statement = conn.prepare(
            "SELECT d.id,d.space_id,d.source_kind,d.source_id,d.status,d.created_at,l.binding,l.pinned_version_no, \
               (SELECT MAX(v2.version_no) FROM knowledge_document_versions v2 \
                 WHERE v2.document_id=d.id AND v2.published_at IS NOT NULL AND v2.revoked_at IS NULL \
                   AND v2.superseded_at IS NULL), \
               (SELECT v3.version_no FROM knowledge_document_versions v3 \
                 WHERE v3.document_id=d.id AND v3.published_at IS NOT NULL AND v3.revoked_at IS NULL \
                   AND v3.version_no = CASE WHEN l.binding='pinned' THEN l.pinned_version_no \
                        ELSE (SELECT MAX(v4.version_no) FROM knowledge_document_versions v4 \
                               WHERE v4.document_id=d.id AND v4.published_at IS NOT NULL AND v4.revoked_at IS NULL \
                                 AND v4.superseded_at IS NULL) END) \
             FROM knowledge_space_links l JOIN knowledge_documents d ON d.id=l.document_id \
             WHERE d.tenant_id=?1 AND d.customer_id=?2 AND l.space_id=?3 AND d.deleted_at IS NULL \
             ORDER BY d.created_at DESC, d.id",
        )?;
        let rows = statement.query_map(params![tenant_id, customer_id, space_id], |r| {
            Ok(KnowledgeSpaceMaterial {
                document_id: r.get(0)?,
                home_space_id: r.get(1)?,
                source_kind: r.get(2)?,
                source_id: r.get(3)?,
                status: r.get(4)?,
                created_at: r.get(5)?,
                binding: r.get(6)?,
                pinned_version_no: r.get(7)?,
                latest_published_version_no: r.get(8)?,
                resolved_version_no: r.get(9)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Link an existing material into this space (§三). The linker must be
    /// Editor here AND at least a Reader where the material's history lives
    /// — linking is membership, never a way to read through a wall. The
    /// material's binding starts as `follow`.
    pub fn knowledge_space_link_add(
        &self,
        space_id: &str,
        subject_id: &str,
        document_id: &str,
    ) -> Result<()> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Editor)?;
        // Home lookup runs in its own guard, released BEFORE the home-side
        // authorization: `knowledge_authorize` locks the same connection,
        // and re-entering it while a guard is held deadlocks the thread.
        let home: Option<(String, String, String)> = {
            let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            conn.query_row(
                "SELECT tenant_id,customer_id,space_id FROM knowledge_documents WHERE id=?1 AND deleted_at IS NULL",
                params![document_id.trim()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
        };
        let (tenant, customer, home_space) = home.ok_or_else(|| {
            Error::Invalid("knowledge document was not found".into())
        })?;
        if home_space != space_id.trim() {
            self.knowledge_authorize(&home_space, subject_id, KnowledgeRole::Reader)?;
        }
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let scoped: Option<String> = conn
            .query_row(
                "SELECT id FROM knowledge_spaces WHERE id=?1 AND tenant_id=?2 AND customer_id=?3",
                params![space_id.trim(), tenant, customer],
                |r| r.get(0),
            )
            .optional()?;
        scoped.ok_or_else(|| {
            Error::Invalid("knowledge space is outside the requested scope".into())
        })?;
        conn.execute(
            "INSERT OR IGNORE INTO knowledge_space_links (space_id,document_id,binding,pinned_version_no,added_by,created_at)
             VALUES (?1,?2,'follow',NULL,?3,?4)",
            params![space_id.trim(), document_id.trim(), subject_id, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Every space currently linking this material, as
    /// (space_id, tenant_id, customer_id). NOT a read surface — an
    /// invalidation helper: a revocation or deletion must prune EVERY space
    /// that cites the material, and only the link table knows which those
    /// are. Returns membership coordinates only, never content.
    pub fn knowledge_spaces_linking(
        &self,
        document_id: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut statement = conn.prepare(
            "SELECT l.space_id, s.tenant_id, s.customer_id FROM knowledge_space_links l \
             JOIN knowledge_spaces s ON s.id=l.space_id WHERE l.document_id=?1",
        )?;
        let rows = statement.query_map(params![document_id.trim()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Re-binding of one material in one space: `follow` releases a pin back
    /// onto the currently effective version; `pinned` freezes the space on a
    /// SPECIFIC published version. Superseded versions pin fine — a frozen
    /// historical version is the point — but drafts and WITHDRAWN versions
    /// are refused: only revocation may bar a citation (§三).
    pub fn knowledge_space_link_set(
        &self,
        space_id: &str,
        subject_id: &str,
        document_id: &str,
        binding: &str,
        pinned_version_no: Option<i64>,
    ) -> Result<()> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Editor)?;
        let binding = binding.trim();
        if binding != "follow" && binding != "pinned" {
            return Err(Error::Invalid("invalid binding".into()));
        }
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let linked: Option<String> = conn
            .query_row(
                "SELECT document_id FROM knowledge_space_links WHERE space_id=?1 AND document_id=?2",
                params![space_id.trim(), document_id.trim()],
                |r| r.get(0),
            )
            .optional()?;
        if linked.is_none() {
            return Err(Error::Invalid("material is not linked to this space".into()));
        }
        match binding {
            "follow" => {
                conn.execute(
                    "UPDATE knowledge_space_links SET binding='follow', pinned_version_no=NULL WHERE space_id=?1 AND document_id=?2",
                    params![space_id.trim(), document_id.trim()],
                )?;
            }
            _ => {
                let version_no = pinned_version_no.ok_or_else(|| {
                    Error::Invalid("pinned binding requires a version number".into())
                })?;
                let published: Option<i64> = conn
                    .query_row(
                        "SELECT v.version_no FROM knowledge_document_versions v \
                         JOIN knowledge_documents d ON d.id=v.document_id \
                         WHERE v.document_id=?1 AND v.version_no=?2 AND v.published_at IS NOT NULL \
                           AND v.revoked_at IS NULL AND d.deleted_at IS NULL",
                        params![document_id.trim(), version_no],
                        |r| r.get(0),
                    )
                    .optional()?;
                if published.is_none() {
                    return Err(Error::Invalid(
                        "pinned version is not a published version".into(),
                    ));
                }
                conn.execute(
                    "UPDATE knowledge_space_links SET binding='pinned', pinned_version_no=?3 WHERE space_id=?1 AND document_id=?2",
                    params![space_id.trim(), document_id.trim(), version_no],
                )?;
            }
        }
        Ok(())
    }

    /// Remove THIS space's link to a material. Nothing else is touched: the
    /// material and its whole version history stay exactly as they are for
    /// every other space (and for a relink later). Returns whether a link
    /// actually went away — the caller resyncs its mirror only on `true`.
    pub fn knowledge_space_unlink(
        &self,
        space_id: &str,
        subject_id: &str,
        document_id: &str,
    ) -> Result<bool> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Editor)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let changed = conn.execute(
            "DELETE FROM knowledge_space_links WHERE space_id=?1 AND document_id=?2",
            params![space_id.trim(), document_id.trim()],
        )?;
        Ok(changed == 1)
    }

    pub fn knowledge_search(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeSearchHit>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Reader)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let limit = limit.clamp(1, 50) as i64;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        // Same binding resolution as the build surface: retrieval answers
        // from the version THIS space effectively holds — its pinned one, or
        // the currently effective one. A space frozen on v1 never answers
        // from v2 chunks.
        let mut statement = conn.prepare(
            "SELECT d.id,v.id,v.version_no,c.id,c.ordinal,c.content,d.source_kind,d.source_id
             FROM knowledge_space_links l
             JOIN knowledge_documents d ON d.id=l.document_id
             JOIN knowledge_document_versions v ON v.document_id=d.id
             JOIN knowledge_chunks c ON c.version_id=v.id
             WHERE d.tenant_id=?1 AND d.customer_id=?2 AND l.space_id=?3 AND d.deleted_at IS NULL
               AND v.published_at IS NOT NULL AND v.revoked_at IS NULL AND instr(lower(c.content), lower(?4))>0
               AND v.version_no = CASE WHEN l.binding='pinned' AND l.pinned_version_no IS NOT NULL
                     THEN l.pinned_version_no
                     ELSE (SELECT MAX(v2.version_no) FROM knowledge_document_versions v2
                            WHERE v2.document_id=d.id AND v2.published_at IS NOT NULL
                              AND v2.revoked_at IS NULL AND v2.superseded_at IS NULL) END
             ORDER BY d.id, v.version_no DESC, c.ordinal ASC LIMIT ?5",
        )?;
        let rows = statement.query_map(
            params![tenant_id, customer_id, space_id, query.trim(), limit],
            |r| {
                Ok(KnowledgeSearchHit {
                    document_id: r.get(0)?,
                    version_id: r.get(1)?,
                    version_no: r.get(2)?,
                    chunk_id: r.get(3)?,
                    ordinal: r.get(4)?,
                    content: r.get(5)?,
                    source_kind: r.get(6)?,
                    source_id: r.get(7)?,
                })
            },
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Ranked RAG search for the unified knowledge-space surface. The
    /// compatibility `knowledge_search` retains literal matching.
    pub fn knowledge_retrieve(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeSearchHit>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Reader)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let limit = limit.clamp(1, 50) as i64;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        // Same binding resolution as the build surface: retrieval answers
        // from the version THIS space effectively holds — its pinned one, or
        // the currently effective one. A space frozen on v1 never answers
        // from v2 chunks.
        // FTS5 trigram handles Chinese phrases without an external tokenizer.
        // Two-character terms use a scoped substring fallback, since trigram
        // has no posting for them. Scope/version gates precede the final LIMIT.
        let mut terms = knowledge_question_terms(query);
        if terms.is_empty() { terms.push(query.trim().to_lowercase()); }
        let fulltext = terms.iter().filter(|t| t.chars().count() >= 3)
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>().join(" OR ");
        // A valid expression with no postings is preferable to FTS syntax
        // errors for short questions or user-supplied quotes/operators.
        let fulltext = if fulltext.is_empty() { "\"dlnemptyftsquery000\"".to_string() } else { fulltext };
        let short = serde_json::to_string(&terms.iter().filter(|t| t.chars().count() < 3).collect::<Vec<_>>())?;
        let source_terms = serde_json::to_string(&terms)?;
        let mut statement = conn.prepare(
            "WITH ranked AS MATERIALIZED (
               SELECT rowid, rank FROM knowledge_chunks_fts WHERE knowledge_chunks_fts MATCH ?6
             )
             SELECT d.id,v.id,v.version_no,c.id,c.ordinal,c.content,d.source_kind,d.source_id
             FROM knowledge_space_links l
             JOIN knowledge_documents d ON d.id=l.document_id
             JOIN knowledge_document_versions v ON v.document_id=d.id
             JOIN knowledge_chunks c ON c.version_id=v.id
             LEFT JOIN ranked f ON f.rowid=c.rowid
             WHERE d.tenant_id=?1 AND d.customer_id=?2 AND l.space_id=?3 AND d.deleted_at IS NULL
               AND v.published_at IS NOT NULL AND v.revoked_at IS NULL
               AND (f.rowid IS NOT NULL OR instr(lower(c.content),lower(?4))>0
                    OR EXISTS (SELECT 1 FROM json_each(?7) WHERE instr(lower(c.content),lower(value))>0)
                    OR EXISTS (SELECT 1 FROM json_each(?8) WHERE instr(lower(d.source_id),lower(value))>0))
               AND v.version_no = CASE WHEN l.binding='pinned' AND l.pinned_version_no IS NOT NULL
                     THEN l.pinned_version_no
                     ELSE (SELECT MAX(v2.version_no) FROM knowledge_document_versions v2
                            WHERE v2.document_id=d.id AND v2.published_at IS NOT NULL
                              AND v2.revoked_at IS NULL AND v2.superseded_at IS NULL) END
             ORDER BY CASE WHEN instr(lower(c.content),lower(?4))>0 OR instr(lower(d.source_id),lower(?4))>0 THEN 0
                           WHEN f.rank IS NOT NULL THEN 1 ELSE 2 END,
                      f.rank, c.ordinal, c.id LIMIT ?5",
        )?;
        let rows = statement.query_map(
            params![tenant_id, customer_id, space_id, query.trim(), limit, fulltext, short, source_terms],
            |r| {
                Ok(KnowledgeSearchHit {
                    document_id: r.get(0)?,
                    version_id: r.get(1)?,
                    version_no: r.get(2)?,
                    chunk_id: r.get(3)?,
                    ordinal: r.get(4)?,
                    content: r.get(5)?,
                    source_kind: r.get(6)?,
                    source_id: r.get(7)?,
                })
            },
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Latest source preview for the file manager. Drafts are available
    /// only to editors of the source's HOME space, never linked readers.
    pub fn knowledge_document_preview(&self, space_id: &str, subject_id: &str, document_id: &str) -> Result<KnowledgeDocumentPreview> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Reader)?;
        let (preview, home): (KnowledgeDocumentPreview, String) = {
            let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            conn.query_row(
                "SELECT d.id,v.id,v.version_no,d.source_id,v.content,
                        (v.published_at IS NOT NULL AND v.revoked_at IS NULL AND (v.superseded_at IS NULL OR l.binding='pinned')),d.space_id
                 FROM knowledge_space_links l JOIN knowledge_documents d ON d.id=l.document_id
                 JOIN knowledge_document_versions v ON v.document_id=d.id
                 WHERE l.space_id=?1 AND d.id=?2 AND d.deleted_at IS NULL
                   AND v.version_no=CASE WHEN l.binding='pinned' AND l.pinned_version_no IS NOT NULL
                     THEN l.pinned_version_no WHEN d.space_id=l.space_id THEN
                     (SELECT MAX(v2.version_no) FROM knowledge_document_versions v2 WHERE v2.document_id=d.id)
                     ELSE (SELECT MAX(v2.version_no) FROM knowledge_document_versions v2 WHERE v2.document_id=d.id
                       AND v2.published_at IS NOT NULL AND v2.revoked_at IS NULL AND v2.superseded_at IS NULL) END
                 ORDER BY v.version_no DESC LIMIT 1", params![space_id,document_id],
                |r| Ok((KnowledgeDocumentPreview { document_id:r.get(0)?,version_id:r.get(1)?,version_no:r.get(2)?,source_id:r.get(3)?,content:r.get(4)?,enabled:r.get(5)? }, r.get(6)?))
            ).optional()?.ok_or_else(|| Error::Invalid("资料不存在或已被删除。".into()))?
        };
        if !preview.enabled { self.knowledge_authorize(&home, subject_id, KnowledgeRole::Editor)?; }
        Ok(preview)
    }

    /// Open the exact published chunk cited by a conversation. Superseded
    /// revisions remain inspectable; revoked/deleted/unlinked sources do not.
    pub fn knowledge_chunk_source(&self, space_id: &str, subject_id: &str, chunk_id: &str) -> Result<KnowledgeSearchHit> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Reader)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.query_row(
            "SELECT d.id,v.id,v.version_no,c.id,c.ordinal,c.content,d.source_kind,d.source_id
             FROM knowledge_space_links l JOIN knowledge_documents d ON d.id=l.document_id
             JOIN knowledge_document_versions v ON v.document_id=d.id
             JOIN knowledge_chunks c ON c.version_id=v.id
             WHERE l.space_id=?1 AND c.id=?2 AND d.deleted_at IS NULL
               AND v.published_at IS NOT NULL AND v.revoked_at IS NULL
               AND (l.binding<>'pinned' OR v.version_no=l.pinned_version_no)",
            params![space_id, chunk_id], |r| Ok(KnowledgeSearchHit {
                document_id:r.get(0)?, version_id:r.get(1)?, version_no:r.get(2)?,
                chunk_id:r.get(3)?, ordinal:r.get(4)?, content:r.get(5)?,
                source_kind:r.get(6)?, source_id:r.get(7)?,
            })
        ).optional()?.ok_or_else(|| Error::Invalid("引用资料不可用：来源已撤回、删除，或不属于当前知识库。".into()))
    }

    /// Answer only from published, in-scope chunks.  The answer is extractive
    /// by design so every sentence presented to the user has a nearby,
    /// immutable citation and no customer material needs to leave the device.
    pub fn knowledge_answer(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        question: &str,
        limit: usize,
    ) -> Result<KnowledgeAnswer> {
        let question = question.trim();
        if question.is_empty() {
            return Err(Error::Invalid("knowledge question is required".into()));
        }
        let limit = limit.clamp(1, 8);
        let mut hits = self.knowledge_search(
            tenant_id,
            customer_id,
            space_id,
            subject_id,
            question,
            limit,
        )?;
        // Natural-language questions often do not occur verbatim in a chunk.
        // Fall back to distinctive terms while retaining the same scoped,
        // published-only search path and de-duplicating chunks.
        if hits.is_empty() {
            let terms = knowledge_question_terms(question);
            // One shared Chinese two-character word is weak evidence for a
            // longer question (e.g. 查询网关 vs 重置网关).
            let needs_two_clues = terms.iter().any(|term| {
                term.chars().count() >= 4
                    && term
                        .chars()
                        .all(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
            });
            let mut ranked: HashMap<String, (KnowledgeSearchHit, usize)> = HashMap::new();
            for term in terms {
                for hit in
                    self.knowledge_search(tenant_id, customer_id, space_id, subject_id, &term, 50)?
                {
                    let entry = ranked.entry(hit.chunk_id.clone()).or_insert((hit, 0));
                    entry.1 += term.chars().count() * term.chars().count();
                }
            }
            let mut scored: Vec<_> = ranked.into_values().collect();
            if needs_two_clues {
                scored.retain(|(_, score)| *score >= 8);
            }
            scored.sort_by(|a, b| {
                b.1.cmp(&a.1)
                    .then_with(|| a.0.source_id.cmp(&b.0.source_id))
            });
            hits = scored.into_iter().take(limit).map(|(hit, _)| hit).collect();
        }
        if hits.is_empty() {
            return Ok(KnowledgeAnswer {
                answer: "未在当前知识空间的已发布资料中找到可引用的依据。".into(),
                no_answer: true,
                citations: Vec::new(),
            });
        }
        let citations = hits
            .iter()
            .map(|hit| KnowledgeCitation {
                document_id: hit.document_id.clone(),
                version_id: hit.version_id.clone(),
                version_no: hit.version_no,
                chunk_id: hit.chunk_id.clone(),
                ordinal: hit.ordinal,
                source_kind: hit.source_kind.clone(),
                source_id: hit.source_id.clone(),
            })
            .collect();
        let excerpts = hits
            .iter()
            .take(3)
            .map(|hit| knowledge_excerpt(&hit.content, 900))
            .collect::<Vec<_>>();
        Ok(KnowledgeAnswer {
            answer: format!("依据已发布资料：\n\n{}", excerpts.join("\n\n补充资料：\n")),
            no_answer: false,
            citations,
        })
    }

    /// The ONE policy→verdict rule. Both entry points ride it so they can
    /// never drift: the subject-gated read path (`knowledge_external_decision`)
    /// and the publish-time recheck (`knowledge_external_policy_recheck`).
    fn external_decision_from_policy(
        policy: &str,
        requested_mode: &str,
    ) -> KnowledgeExternalDecision {
        let requested_mode = requested_mode.trim();
        let allowed = requested_mode == "local_retrieval"
            || matches!((policy, requested_mode), ("allow_summary", "cloud_summary"));
        let reason = match (policy, requested_mode, allowed) {
            (_, "local_retrieval", _) => "local retrieval does not leave this device".into(),
            ("allow_summary", "cloud_summary", true) => {
                "this space permits cloud summaries only".into()
            }
            ("allow_summary", _, _) => {
                "this space permits cloud summaries, never raw customer content".into()
            }
            ("local_only", _, _) => "this space permits local retrieval only".into(),
            ("deny_cloud", _, _) => "this space explicitly blocks cloud egress".into(),
            _ => "knowledge space policy blocks this external request".into(),
        };
        KnowledgeExternalDecision {
            allowed,
            policy: policy.to_string(),
            reason,
        }
    }

    pub fn knowledge_external_decision(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        requested_mode: &str,
    ) -> Result<KnowledgeExternalDecision> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Reader)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let policy: String = conn.query_row(
            "SELECT external_policy FROM knowledge_spaces WHERE id=?1",
            params![space_id],
            |r| r.get(0),
        )?;
        Ok(Self::external_decision_from_policy(&policy, requested_mode))
    }

    /// Re-adjudicate a space's CURRENT policy at publish time, without a
    /// subject: a completed build carries no user, and the subject gate on
    /// the read path only proves visibility, which nothing here would
    /// tighten. A deleted space has no policy to honour — report it rather
    /// than silently allowing. Fail closed on a missing row.
    pub fn knowledge_external_policy_recheck(
        &self,
        space_id: &str,
        requested_mode: &str,
    ) -> Result<KnowledgeExternalDecision> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let policy: Option<String> = conn
            .query_row(
                "SELECT external_policy FROM knowledge_spaces WHERE id=?1",
                params![space_id],
                |r| r.get(0),
            )
            .optional()?;
        let policy = policy.ok_or_else(|| Error::Invalid("knowledge space not found".into()))?;
        Ok(Self::external_decision_from_policy(&policy, requested_mode))
    }

    pub fn knowledge_sync_preview(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        source_kind: &str,
        source_id: &str,
        content: &str,
    ) -> Result<KnowledgeSyncPreview> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Editor)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        if source_kind.trim().is_empty() || source_id.trim().is_empty() || content.trim().is_empty()
        {
            return Err(Error::Invalid(
                "sync source and content are required".into(),
            ));
        }
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let incoming_sha256 = format!("sha256:{:x}", hasher.finalize());
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let current_sha256: Option<String> = conn.query_row(
            "SELECT last_sha256 FROM knowledge_sync_sources WHERE space_id=?1 AND source_kind=?2 AND source_id=?3",
            params![space_id, source_kind.trim(), source_id.trim()], |r| r.get(0),
        ).optional()?.flatten();
        let changed = current_sha256.as_deref() != Some(&incoming_sha256);
        Ok(KnowledgeSyncPreview {
            changed,
            action: if changed {
                "import".into()
            } else {
                "unchanged".into()
            },
            current_sha256,
            incoming_sha256,
        })
    }

    pub fn knowledge_sync_apply(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        source_kind: &str,
        source_id: &str,
        content: &str,
        cursor: Option<&str>,
        chunk_chars: usize,
    ) -> Result<(KnowledgeSyncPreview, Option<KnowledgeDocumentVersion>)> {
        let preview = self.knowledge_sync_preview(
            tenant_id,
            customer_id,
            space_id,
            subject_id,
            source_kind,
            source_id,
            content,
        )?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let paused: Option<String> = conn.query_row("SELECT status FROM knowledge_sync_sources WHERE space_id=?1 AND source_kind=?2 AND source_id=?3", params![space_id, source_kind.trim(), source_id.trim()], |r| r.get(0)).optional()?;
        drop(conn);
        if paused.as_deref() == Some("paused") {
            return Err(Error::Invalid("knowledge sync source is paused".into()));
        }
        let version = if preview.changed {
            Some(
                self.knowledge_import_document(
                    tenant_id,
                    customer_id,
                    space_id,
                    subject_id,
                    source_kind,
                    source_id,
                    content,
                    chunk_chars,
                )?
                .1,
            )
        } else {
            None
        };
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let now = Utc::now().to_rfc3339();
        conn.execute("INSERT INTO knowledge_sync_sources (id,tenant_id,customer_id,space_id,source_kind,source_id,cursor,last_sha256,status,retry_count,last_error,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'idle',0,NULL,?9) ON CONFLICT(space_id,source_kind,source_id) DO UPDATE SET cursor=excluded.cursor,last_sha256=excluded.last_sha256,status='idle',retry_count=0,last_error=NULL,updated_at=excluded.updated_at", params![format!("sync_{}",uuid::Uuid::new_v4().simple()),tenant_id,customer_id,space_id,source_kind.trim(),source_id.trim(),cursor.map(str::trim).filter(|v|!v.is_empty()),preview.incoming_sha256,now])?;
        Ok((preview, version))
    }

    pub fn knowledge_sync_set_paused(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        source_kind: &str,
        source_id: &str,
        paused: bool,
    ) -> Result<()> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Editor)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let changed = conn.execute("UPDATE knowledge_sync_sources SET status=?1,updated_at=?2 WHERE tenant_id=?3 AND customer_id=?4 AND space_id=?5 AND source_kind=?6 AND source_id=?7", params![if paused {"paused"} else {"idle"},Utc::now().to_rfc3339(),tenant_id,customer_id,space_id,source_kind.trim(),source_id.trim()])?;
        if changed != 1 {
            return Err(Error::Invalid("knowledge sync source was not found".into()));
        }
        Ok(())
    }

    pub fn knowledge_sync_sources(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
    ) -> Result<Vec<KnowledgeSyncSource>> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Reader)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut statement = conn.prepare("SELECT id,tenant_id,customer_id,space_id,source_kind,source_id,cursor,last_sha256,status,retry_count,last_error,updated_at FROM knowledge_sync_sources WHERE tenant_id=?1 AND customer_id=?2 AND space_id=?3 ORDER BY updated_at DESC")?;
        let rows = statement.query_map(params![tenant_id, customer_id, space_id], |r| {
            Ok(KnowledgeSyncSource {
                id: r.get(0)?,
                tenant_id: r.get(1)?,
                customer_id: r.get(2)?,
                space_id: r.get(3)?,
                source_kind: r.get(4)?,
                source_id: r.get(5)?,
                cursor: r.get(6)?,
                last_sha256: r.get(7)?,
                status: r.get(8)?,
                retry_count: r.get(9)?,
                last_error: r.get(10)?,
                updated_at: r.get(11)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn knowledge_sync_record_failure(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        source_kind: &str,
        source_id: &str,
        detail: &str,
    ) -> Result<()> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Editor)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let changed = conn.execute("UPDATE knowledge_sync_sources SET status='failed',retry_count=retry_count+1,last_error=?1,updated_at=?2 WHERE tenant_id=?3 AND customer_id=?4 AND space_id=?5 AND source_kind=?6 AND source_id=?7", params![detail.trim(),Utc::now().to_rfc3339(),tenant_id,customer_id,space_id,source_kind.trim(),source_id.trim()])?;
        if changed != 1 {
            return Err(Error::Invalid("knowledge sync source was not found".into()));
        }
        Ok(())
    }

    pub fn knowledge_delete_document(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        document_id: &str,
    ) -> Result<()> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Editor)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let mut conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let tx = conn.transaction()?;
        let counts: Option<(i64,i64)> = tx.query_row("SELECT COUNT(DISTINCT v.id),COUNT(c.id) FROM knowledge_documents d LEFT JOIN knowledge_document_versions v ON v.document_id=d.id LEFT JOIN knowledge_chunks c ON c.version_id=v.id WHERE d.id=?1 AND d.tenant_id=?2 AND d.customer_id=?3 AND d.space_id=?4", params![document_id,tenant_id,customer_id,space_id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        let changed = tx.execute(
            "DELETE FROM knowledge_documents WHERE id=?1 AND tenant_id=?2 AND customer_id=?3 AND space_id=?4",
            params![document_id, tenant_id, customer_id, space_id],
        )?;
        if changed != 1 {
            return Err(Error::Invalid(
                "knowledge document was not found in the requested scope".into(),
            ));
        }
        let (versions, chunks) = counts.unwrap_or((0, 0));
        tx.execute("INSERT INTO knowledge_deletion_audits (id,tenant_id,customer_id,space_id,document_id,deleted_at,cascade_versions,cascade_chunks) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)", params![format!("delete_{}",uuid::Uuid::new_v4().simple()),tenant_id,customer_id,space_id,document_id,Utc::now().to_rfc3339(),versions,chunks])?;
        tx.commit()?;
        Ok(())
    }

    pub fn knowledge_deletion_audits(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        limit: u32,
    ) -> Result<Vec<KnowledgeDeletionAudit>> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt=conn.prepare("SELECT id,document_id,deleted_at,cascade_versions,cascade_chunks FROM knowledge_deletion_audits WHERE tenant_id=?1 AND customer_id=?2 AND space_id=?3 ORDER BY deleted_at DESC LIMIT ?4")?;
        let rows = stmt.query_map(
            params![tenant_id, customer_id, space_id, limit.clamp(1, 200)],
            |r| {
                Ok(KnowledgeDeletionAudit {
                    id: r.get(0)?,
                    document_id: r.get(1)?,
                    deleted_at: r.get(2)?,
                    cascade_versions: r.get(3)?,
                    cascade_chunks: r.get(4)?,
                })
            },
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn knowledge_deletion_proof(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        audit_id: &str,
    ) -> Result<KnowledgeDeletionProof> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let audit: KnowledgeDeletionAudit=conn.query_row("SELECT id,document_id,deleted_at,cascade_versions,cascade_chunks FROM knowledge_deletion_audits WHERE id=?1 AND tenant_id=?2 AND customer_id=?3 AND space_id=?4",params![audit_id,tenant_id,customer_id,space_id],|r|Ok(KnowledgeDeletionAudit{id:r.get(0)?,document_id:r.get(1)?,deleted_at:r.get(2)?,cascade_versions:r.get(3)?,cascade_chunks:r.get(4)?}))?;
        let documents_remaining: u64 = conn.query_row(
            "SELECT COUNT(*) FROM knowledge_documents WHERE id=?1",
            params![audit.document_id],
            |r| r.get(0),
        )?;
        let versions_remaining: u64 = conn.query_row(
            "SELECT COUNT(*) FROM knowledge_document_versions WHERE document_id=?1",
            params![audit.document_id],
            |r| r.get(0),
        )?;
        let chunks_remaining:u64=conn.query_row("SELECT COUNT(*) FROM knowledge_chunks c JOIN knowledge_document_versions v ON v.id=c.version_id WHERE v.document_id=?1",params![audit.document_id],|r|r.get(0))?;
        Ok(KnowledgeDeletionProof {
            audit,
            documents_remaining,
            versions_remaining,
            chunks_remaining,
            verified: documents_remaining == 0 && versions_remaining == 0 && chunks_remaining == 0,
        })
    }

    pub fn knowledge_set_retention_days(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        retention_days: u32,
    ) -> Result<()> {
        if retention_days == 0 || retention_days > 3650 {
            return Err(Error::Invalid("retention days must be 1..=3650".into()));
        }
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute("INSERT INTO knowledge_retention_policies (space_id,retention_days,updated_at) VALUES (?1,?2,?3) ON CONFLICT(space_id) DO UPDATE SET retention_days=excluded.retention_days,updated_at=excluded.updated_at", params![space_id,retention_days,Utc::now().to_rfc3339()])?;
        Ok(())
    }

    pub fn knowledge_retention_preview(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
    ) -> Result<Option<KnowledgeRetentionPreview>> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let days: Option<u32> = conn
            .query_row(
                "SELECT retention_days FROM knowledge_retention_policies WHERE space_id=?1",
                params![space_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(retention_days) = days else {
            return Ok(None);
        };
        let cutoff = (Utc::now() - chrono::Duration::days(retention_days as i64)).to_rfc3339();
        let (eligible_versions,oldest_eligible_at):(u64,Option<String>)=conn.query_row("SELECT COUNT(*),MIN(v.created_at) FROM knowledge_document_versions v JOIN knowledge_documents d ON d.id=v.document_id WHERE d.tenant_id=?1 AND d.customer_id=?2 AND d.space_id=?3 AND v.created_at<?4 AND (v.revoked_at IS NOT NULL OR v.superseded_at IS NOT NULL OR d.deleted_at IS NOT NULL)",params![tenant_id,customer_id,space_id,cutoff],|r|Ok((r.get(0)?,r.get(1)?)))?;
        Ok(Some(KnowledgeRetentionPreview {
            retention_days,
            eligible_versions,
            oldest_eligible_at,
        }))
    }

    pub fn knowledge_submit_feedback(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
        kind: &str,
        comment: Option<&str>,
    ) -> Result<KnowledgeFeedback> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Reader)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        if !matches!(kind.trim(), "helpful" | "incorrect" | "no_answer") {
            return Err(Error::Invalid(
                "feedback kind must be helpful, incorrect, or no_answer".into(),
            ));
        }
        let item = KnowledgeFeedback {
            id: format!("feedback_{}", uuid::Uuid::new_v4().simple()),
            subject_id: subject_id.into(),
            kind: kind.trim().into(),
            comment: comment
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
            status: "open".into(),
            reviewer_id: None,
            review_note: None,
            created_at: Utc::now().to_rfc3339(),
            reviewed_at: None,
        };
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute("INSERT INTO knowledge_feedback (id,tenant_id,customer_id,space_id,subject_id,kind,comment,status,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![item.id,tenant_id,customer_id,space_id,item.subject_id,item.kind,item.comment,item.status,item.created_at])?;
        Ok(item)
    }

    pub fn knowledge_feedback(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
    ) -> Result<Vec<KnowledgeFeedback>> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt=conn.prepare("SELECT id,subject_id,kind,comment,status,reviewer_id,review_note,created_at,reviewed_at FROM knowledge_feedback WHERE tenant_id=?1 AND customer_id=?2 AND space_id=?3 ORDER BY created_at DESC")?;
        let rows = stmt.query_map(params![tenant_id, customer_id, space_id], |r| {
            Ok(KnowledgeFeedback {
                id: r.get(0)?,
                subject_id: r.get(1)?,
                kind: r.get(2)?,
                comment: r.get(3)?,
                status: r.get(4)?,
                reviewer_id: r.get(5)?,
                review_note: r.get(6)?,
                created_at: r.get(7)?,
                reviewed_at: r.get(8)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn knowledge_review_feedback(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        reviewer_id: &str,
        feedback_id: &str,
        status: &str,
        note: Option<&str>,
    ) -> Result<()> {
        self.knowledge_authorize(space_id, reviewer_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        if !matches!(status.trim(), "accepted" | "dismissed") {
            return Err(Error::Invalid(
                "feedback review must be accepted or dismissed".into(),
            ));
        }
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        if conn.execute("UPDATE knowledge_feedback SET status=?1,reviewer_id=?2,review_note=?3,reviewed_at=?4 WHERE id=?5 AND tenant_id=?6 AND customer_id=?7 AND space_id=?8 AND status='open'",params![status.trim(),reviewer_id,note.map(str::trim).filter(|v|!v.is_empty()),Utc::now().to_rfc3339(),feedback_id,tenant_id,customer_id,space_id])? != 1 { return Err(Error::Invalid("open feedback was not found in this scope".into())); }
        Ok(())
    }

    pub fn knowledge_quality_metrics(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
    ) -> Result<KnowledgeQualityMetrics> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        // Evidence counts ride the same link+binding resolution as retrieval:
        // a space's numbers describe what THIS space can actually answer from.
        let published_documents:u64=conn.query_row("SELECT COUNT(DISTINCT d.id) FROM knowledge_space_links l JOIN knowledge_documents d ON d.id=l.document_id JOIN knowledge_document_versions v ON v.document_id=d.id WHERE d.tenant_id=?1 AND d.customer_id=?2 AND l.space_id=?3 AND d.deleted_at IS NULL AND v.published_at IS NOT NULL AND v.revoked_at IS NULL AND v.version_no = CASE WHEN l.binding='pinned' AND l.pinned_version_no IS NOT NULL THEN l.pinned_version_no ELSE (SELECT MAX(v2.version_no) FROM knowledge_document_versions v2 WHERE v2.document_id=d.id AND v2.published_at IS NOT NULL AND v2.revoked_at IS NULL AND v2.superseded_at IS NULL) END",params![tenant_id,customer_id,space_id],|r|r.get(0))?;
        let published_chunks:u64=conn.query_row("SELECT COUNT(*) FROM knowledge_space_links l JOIN knowledge_documents d ON d.id=l.document_id JOIN knowledge_document_versions v ON v.document_id=d.id JOIN knowledge_chunks c ON c.version_id=v.id WHERE d.tenant_id=?1 AND d.customer_id=?2 AND l.space_id=?3 AND d.deleted_at IS NULL AND v.published_at IS NOT NULL AND v.revoked_at IS NULL AND v.version_no = CASE WHEN l.binding='pinned' AND l.pinned_version_no IS NOT NULL THEN l.pinned_version_no ELSE (SELECT MAX(v2.version_no) FROM knowledge_document_versions v2 WHERE v2.document_id=d.id AND v2.published_at IS NOT NULL AND v2.revoked_at IS NULL AND v2.superseded_at IS NULL) END",params![tenant_id,customer_id,space_id],|r|r.get(0))?;
        let (api_requests,no_answers,cited):(u64,u64,u64)=conn.query_row("SELECT COUNT(*),COALESCE(SUM(no_answer),0),COALESCE(SUM(CASE WHEN citation_count>0 THEN 1 ELSE 0 END),0) FROM knowledge_api_usage u JOIN knowledge_api_clients a ON a.id=u.client_id WHERE a.tenant_id=?1 AND a.customer_id=?2 AND a.space_id=?3",params![tenant_id,customer_id,space_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let open_feedback:u64=conn.query_row("SELECT COUNT(*) FROM knowledge_feedback WHERE tenant_id=?1 AND customer_id=?2 AND space_id=?3 AND status='open'",params![tenant_id,customer_id,space_id],|r|r.get(0))?;
        let failed_sync_sources:u64=conn.query_row("SELECT COUNT(*) FROM knowledge_sync_sources WHERE tenant_id=?1 AND customer_id=?2 AND space_id=?3 AND status='failed'",params![tenant_id,customer_id,space_id],|r|r.get(0))?;
        Ok(KnowledgeQualityMetrics {
            published_documents,
            published_chunks,
            api_requests,
            citation_coverage: if api_requests == 0 {
                0.0
            } else {
                cited as f32 / api_requests as f32
            },
            no_answer_rate: if api_requests == 0 {
                0.0
            } else {
                no_answers as f32 / api_requests as f32
            },
            open_feedback,
            failed_sync_sources,
            isolation_alerts: 0,
        })
    }

    /// Owner-only portable snapshot. It is assembled from the authoritative
    /// document/version rows and intentionally omits membership identities.
    pub fn knowledge_export(
        &self,
        tenant_id: &str,
        customer_id: &str,
        space_id: &str,
        subject_id: &str,
    ) -> Result<serde_json::Value> {
        self.knowledge_authorize(space_id, subject_id, KnowledgeRole::Owner)?;
        self.knowledge_assert_scope(tenant_id, customer_id, space_id)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        // Portable snapshot of every material this space is a member of
        // (link-based), with its FULL version history — the history belongs
        // to the material, not to the viewing space.
        let mut statement = conn.prepare("SELECT d.id,d.source_kind,d.source_id,d.status,v.id,v.version_no,v.sha256,v.content,v.created_at,v.published_at,v.revoked_at,v.superseded_at,v.parser_version FROM knowledge_space_links l JOIN knowledge_documents d ON d.id=l.document_id JOIN knowledge_document_versions v ON v.document_id=d.id WHERE d.tenant_id=?1 AND d.customer_id=?2 AND l.space_id=?3 AND d.deleted_at IS NULL ORDER BY d.id,v.version_no")?;
        let rows = statement.query_map(params![tenant_id,customer_id,space_id], |r| Ok(serde_json::json!({"documentId":r.get::<_,String>(0)?,"sourceKind":r.get::<_,String>(1)?,"sourceId":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?,"versionId":r.get::<_,String>(4)?,"versionNo":r.get::<_,i64>(5)?,"sha256":r.get::<_,String>(6)?,"content":r.get::<_,String>(7)?,"createdAt":r.get::<_,String>(8)?,"publishedAt":r.get::<_,Option<String>>(9)?,"revokedAt":r.get::<_,Option<String>>(10)?,"supersededAt":r.get::<_,Option<String>>(11)?,"parserVersion":r.get::<_,Option<i64>>(12)?.unwrap_or(1)})))?;
        let versions: Vec<serde_json::Value> = rows.collect::<std::result::Result<_, _>>()?;
        Ok(
            serde_json::json!({"format":"dandelion-knowledge-export/v1","tenantId":tenant_id,"customerId":customer_id,"spaceId":space_id,"exportedAt":Utc::now().to_rfc3339(),"versions":versions}),
        )
    }

    pub fn append_memory_event(&self, draft: MemoryEventDraft) -> Result<MemoryEvent> {
        let event = MemoryEvent::from_draft(draft)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        if let Some(target_id) = event.draft.revision_of.as_deref() {
            let target: Option<(String, String)> = conn
                .query_row(
                    "SELECT domain, scope_json FROM memory_events WHERE id = ?1",
                    params![target_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let domain = serde_json::to_string(&event.draft.domain)?;
            let scope = serde_json::to_string(&event.draft.scope)?;
            match target {
                Some((target_domain, target_scope))
                    if target_domain == domain && target_scope == scope => {}
                Some(_) => {
                    return Err(Error::Invalid(
                        "memory revision must keep its domain and scope".into(),
                    ))
                }
                None => {
                    return Err(Error::Invalid(
                        "memory revision target was not found".into(),
                    ))
                }
            }
        }
        conn.execute("INSERT INTO memory_events (id, domain, scope_json, kind, payload_json, source_kind, source_id, source_version, authority, observed_at, valid_from, valid_to, confidence, sensitivity, revision_of) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)", params![event.id, serde_json::to_string(&event.draft.domain)?, serde_json::to_string(&event.draft.scope)?, serde_json::to_string(&event.draft.kind)?, serde_json::to_string(&event.draft.payload)?, event.draft.source_ref.kind, event.draft.source_ref.id, event.draft.source_ref.version, serde_json::to_string(&event.draft.authority)?, event.observed_at, event.draft.valid_from, event.draft.valid_to, event.draft.confidence, serde_json::to_string(&event.draft.sensitivity)?, event.draft.revision_of])?;
        self.context_generation.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut cache) = self.context_cache.lock() {
            cache.clear();
        }
        Ok(event)
    }

    pub fn retract_memory_event(
        &self,
        domain: MemoryDomain,
        scope: &MemoryScope,
        target_id: &str,
        reason: &str,
    ) -> Result<MemoryEvent> {
        let target_id = target_id.trim();
        if target_id.is_empty() {
            return Err(Error::Invalid(
                "memory retraction requires an event id".into(),
            ));
        }
        let events = self.memory_events(domain, scope)?;
        let target = events
            .iter()
            .find(|event| event.id == target_id)
            .ok_or_else(|| {
                Error::Invalid("memory retraction target is outside this scope".into())
            })?;
        if events.iter().any(|event| {
            event.draft.kind == MemoryEventKind::Retraction
                && event.draft.revision_of.as_deref() == Some(target_id)
        }) {
            return Err(Error::Invalid("memory event is already retracted".into()));
        }
        self.append_memory_event(MemoryEventDraft {
            domain,
            scope: scope.clone(),
            kind: MemoryEventKind::Retraction,
            payload: serde_json::json!({"reason": reason.trim()}),
            source_ref: crate::context_os::SourceRef {
                kind: "user_forget".into(),
                id: format!("forget:{target_id}"),
                version: None,
            },
            authority: Authority::UserConfirmed,
            valid_from: None,
            valid_to: None,
            confidence: 1.0,
            sensitivity: target.draft.sensitivity,
            revision_of: Some(target.id.clone()),
        })
    }

    pub fn confirm_memory_candidate(
        &self,
        domain: MemoryDomain,
        scope: &MemoryScope,
        candidate_id: &str,
    ) -> Result<MemoryEvent> {
        let events = self.memory_events(domain, scope)?;
        let candidate = events
            .iter()
            .find(|event| event.id == candidate_id.trim())
            .cloned()
            .ok_or_else(|| Error::Invalid("memory candidate is outside this scope".into()))?;
        if events.iter().any(|event| {
            event.draft.kind == MemoryEventKind::Retraction
                && event.draft.revision_of.as_deref() == Some(candidate.id.as_str())
        }) {
            return Err(Error::Invalid(
                "memory candidate was permanently rejected".into(),
            ));
        }
        if candidate.draft.authority != Authority::ModelCandidate {
            return Err(Error::Invalid(
                "only model candidates can be confirmed".into(),
            ));
        }
        self.append_memory_event(MemoryEventDraft {
            domain,
            scope: scope.clone(),
            kind: candidate.draft.kind,
            payload: candidate.draft.payload,
            source_ref: crate::context_os::SourceRef {
                kind: "candidate_confirmation".into(),
                id: candidate.id.clone(),
                version: None,
            },
            authority: Authority::UserConfirmed,
            valid_from: candidate.draft.valid_from,
            valid_to: candidate.draft.valid_to,
            confidence: candidate.draft.confidence,
            sensitivity: candidate.draft.sensitivity,
            revision_of: Some(candidate.id),
        })
    }

    pub fn revise_memory_event(
        &self,
        domain: MemoryDomain,
        scope: &MemoryScope,
        target_id: &str,
        content: &str,
    ) -> Result<MemoryEvent> {
        let content = content.trim();
        if content.is_empty() {
            return Err(Error::Invalid(
                "memory revision content must not be empty".into(),
            ));
        }
        let target = self
            .memory_events(domain, scope)?
            .into_iter()
            .find(|event| event.id == target_id.trim())
            .ok_or_else(|| Error::Invalid("memory revision target is outside this scope".into()))?;
        if target.draft.kind == MemoryEventKind::Retraction {
            return Err(Error::Invalid("a retraction cannot be revised".into()));
        }
        self.append_memory_event(MemoryEventDraft {
            domain,
            scope: scope.clone(),
            kind: target.draft.kind,
            payload: serde_json::json!({"content": content}),
            source_ref: crate::context_os::SourceRef {
                kind: "user_revision".into(),
                id: target.id.clone(),
                version: None,
            },
            authority: Authority::UserConfirmed,
            valid_from: target.draft.valid_from,
            valid_to: target.draft.valid_to,
            confidence: 1.0,
            sensitivity: target.draft.sensitivity,
            revision_of: Some(target.id),
        })
    }

    /// Idempotency key for imports: a source revision is represented once in a
    /// given domain/scope. A changed source version is a new immutable event.
    pub fn append_memory_event_once(&self, draft: MemoryEventDraft) -> Result<Option<MemoryEvent>> {
        draft.validate()?;
        let domain = serde_json::to_string(&draft.domain)?;
        let scope = serde_json::to_string(&draft.scope)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let exists: Option<String> = conn.query_row(
            "SELECT id FROM memory_events WHERE domain = ?1 AND scope_json = ?2 AND source_kind = ?3 AND source_id = ?4 AND COALESCE(source_version, '') = COALESCE(?5, '') LIMIT 1",
            params![domain, scope, draft.source_ref.kind, draft.source_ref.id, draft.source_ref.version],
            |row| row.get(0),
        ).optional()?;
        drop(conn);
        if exists.is_some() {
            return Ok(None);
        }
        self.append_memory_event(draft).map(Some)
    }

    pub fn memory_events(
        &self,
        domain: crate::context_os::MemoryDomain,
        scope: &MemoryScope,
    ) -> Result<Vec<MemoryEvent>> {
        let scope_json = serde_json::to_string(scope)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare("SELECT id, domain, scope_json, kind, payload_json, source_kind, source_id, source_version, authority, observed_at, valid_from, valid_to, confidence, sensitivity, revision_of FROM memory_events WHERE domain = ?1 AND scope_json = ?2 ORDER BY observed_at DESC")?;
        let rows = stmt.query_map(
            params![serde_json::to_string(&domain)?, scope_json],
            |row| {
                let draft = MemoryEventDraft {
                    domain: serde_json::from_str(&row.get::<_, String>(1)?).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    scope: serde_json::from_str(&row.get::<_, String>(2)?).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    kind: serde_json::from_str(&row.get::<_, String>(3)?).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    payload: serde_json::from_str(&row.get::<_, String>(4)?).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    source_ref: crate::context_os::SourceRef {
                        kind: row.get(5)?,
                        id: row.get(6)?,
                        version: row.get(7)?,
                    },
                    authority: serde_json::from_str(&row.get::<_, String>(8)?).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    valid_from: row.get(10)?,
                    valid_to: row.get(11)?,
                    confidence: row.get(12)?,
                    sensitivity: serde_json::from_str(&row.get::<_, String>(13)?).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            13,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    revision_of: row.get(14)?,
                };
                Ok(MemoryEvent {
                    id: row.get(0)?,
                    draft,
                    observed_at: row.get(9)?,
                })
            },
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Scope-exact lexical retrieval for the P1 Context Builder.  Retrieval is
    /// intentionally conservative: retracted, expired, and model-candidate
    /// facts never become prompt context without an explicit user action.
    pub fn search_memory_events(
        &self,
        domain: MemoryDomain,
        scope: &MemoryScope,
        query: &str,
        limit: u32,
    ) -> Result<Vec<MemoryEvent>> {
        scope.validate_for(domain)?;
        if limit == 0 || limit > 100 {
            return Err(Error::Invalid("memory search limit must be 1..=100".into()));
        }
        let now = Utc::now().to_rfc3339();
        let query = query.trim().to_ascii_lowercase();
        let events = self.memory_events(domain, scope)?;
        let retracted: std::collections::HashSet<String> = events
            .iter()
            .filter(|event| event.draft.kind == MemoryEventKind::Retraction)
            .filter_map(|event| event.draft.revision_of.clone())
            .collect();
        let mut active: Vec<_> = events
            .into_iter()
            .filter(|event| {
                event.draft.kind != MemoryEventKind::Retraction
                    && event.draft.authority != Authority::ModelCandidate
                    && !retracted.contains(&event.id)
                    && event
                        .draft
                        .valid_to
                        .as_deref()
                        .is_none_or(|until| until > now.as_str())
                    && (query.is_empty()
                        || serde_json::to_string(&event.draft.payload)
                            .map(|body| lexical_matches(&body, &query))
                            .unwrap_or(false))
            })
            .collect();
        active.sort_by(|left, right| {
            relevance_score(right, &query)
                .partial_cmp(&relevance_score(left, &query))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.observed_at.cmp(&left.observed_at))
        });
        active.truncate(limit as usize);
        Ok(active)
    }

    fn rank_memory_events(
        &self,
        domain: MemoryDomain,
        scope: &MemoryScope,
        query: &str,
        limit: u32,
    ) -> Result<Vec<RankedMemoryEvent>> {
        scope.validate_for(domain)?;
        let now = Utc::now().to_rfc3339();
        let events = self.memory_events(domain, scope)?;
        let retracted: HashSet<String> = events
            .iter()
            .filter(|event| event.draft.kind == MemoryEventKind::Retraction)
            .filter_map(|event| event.draft.revision_of.clone())
            .collect();
        let replaced: HashSet<String> = events
            .iter()
            .filter(|event| event.draft.kind != MemoryEventKind::Retraction)
            .filter_map(|event| event.draft.revision_of.clone())
            .collect();
        let query_entities = extract_entities_from_text(query);
        let mut ranked = events
            .into_iter()
            .filter(|event| {
                event.draft.kind != MemoryEventKind::Retraction
                    && event.draft.authority != Authority::ModelCandidate
                    && !retracted.contains(&event.id)
                    && !replaced.contains(&event.id)
                    && event
                        .draft
                        .valid_to
                        .as_deref()
                        .is_none_or(|until| until > now.as_str())
            })
            .map(|event| {
                let content = context_content(&event.draft.payload);
                let keyword = keyword_score(&content, query);
                let vector = vector_similarity(&content, query);
                let event_entities = extract_entities(&event.draft.payload);
                let entity = entity_overlap(&event_entities, &query_entities);
                let temporal = temporal_score(&event.observed_at);
                let graph = (event_entities.len().min(4) as f32) * 0.025;
                let authority = match event.draft.authority {
                    Authority::UserConfirmed => 0.45,
                    Authority::ToolVerified => 0.30,
                    Authority::ModelCandidate => 0.0,
                };
                let confidence = event.draft.confidence;
                let relevance = keyword * 1.25
                    + vector * 0.75
                    + entity * 0.65
                    + temporal
                    + graph
                    + authority
                    + confidence;
                let mut why_selected = vec![format!("范围匹配：{}", scope_label(scope))];
                if keyword > 0.0 {
                    why_selected.push(format!("关键词命中 {:.0}%", keyword * 100.0));
                }
                if vector > 0.0 {
                    why_selected.push(format!("本地向量相似度 {:.2}", vector));
                }
                if entity > 0.0 {
                    why_selected.push(format!("实体重合 {:.0}%", entity * 100.0));
                }
                why_selected.push(format!("时间新鲜度 {:.2}", temporal));
                why_selected.push(format!(
                    "来源可信度 {:?} / {:.2}",
                    event.draft.authority, confidence
                ));
                RankedMemoryEvent {
                    event,
                    signals: ContextRankSignals {
                        keyword,
                        vector,
                        entity,
                        temporal,
                        graph,
                        authority,
                        confidence,
                    },
                    why_selected,
                    relevance,
                }
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .relevance
                .partial_cmp(&left.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.event.observed_at.cmp(&left.event.observed_at))
        });
        ranked.truncate(limit.clamp(1, 100) as usize);
        Ok(ranked)
    }

    /// Builds, audits, and returns the bounded ledger segment for one agent
    /// turn. Runtime decides whether that segment is injected into its single
    /// existing loop; this store never invokes a model or tool itself.
    pub fn build_context_pack(&self, request: &ContextBuildRequest) -> Result<ContextPack> {
        request.validate()?;
        let cache_key = context_cache_key(request)?;
        let generation = self.context_generation.load(Ordering::Acquire);
        let cached = self
            .context_cache
            .lock()
            .map_err(|_| Error::Store("context cache lock".into()))?
            .get(&cache_key)
            .filter(|entry| entry.generation == generation)
            .cloned();
        let cache_hit = cached.is_some();
        let candidates = match cached {
            Some(entry) => entry.candidates,
            None => {
                let candidates = self.rank_memory_events(
                    request.domain,
                    &request.scope,
                    &request.query,
                    request.max_items,
                )?;
                self.context_cache
                    .lock()
                    .map_err(|_| Error::Store("context cache lock".into()))?
                    .insert(
                        cache_key.clone(),
                        CachedContextCandidates {
                            generation,
                            candidates: candidates.clone(),
                        },
                    );
                candidates
            }
        };
        let mut pack = ContextPack::new(&request.session_id, request.token_budget);
        pack.cache = ContextCacheStatus {
            hit: cache_hit,
            key: cache_key,
            invalidated_by: (!cache_hit && generation > 0).then(|| "ledger_changed".into()),
        };
        pack.policy_notes = vec![
            "scope_exact".into(),
            "hybrid_rank(keyword,vector,entity,temporal,graph)".into(),
            "deterministic_local_vector".into(),
            "entity_graph_projection".into(),
            "budgeted_extract_compression".into(),
            "model_candidates_excluded".into(),
            "expired_and_retracted_excluded".into(),
        ];
        for candidate in candidates {
            let remaining = request.token_budget.saturating_sub(pack.used_tokens);
            let content = compress_context_content(
                &context_content(&candidate.event.draft.payload),
                &request.query,
                remaining,
            );
            let token_cost = estimate_tokens(&content);
            if token_cost > remaining {
                continue;
            }
            pack.used_tokens += token_cost;
            pack.items.push(ContextItem {
                source_event_id: candidate.event.id,
                domain: candidate.event.draft.domain,
                scope: candidate.event.draft.scope,
                token_cost,
                relevance: candidate.relevance,
                content,
                observed_at: candidate.event.observed_at,
                valid_to: candidate.event.draft.valid_to,
                source_kind: Some(candidate.event.draft.source_ref.kind),
                source_id: Some(candidate.event.draft.source_ref.id),
                authority: serde_json::to_string(&candidate.event.draft.authority)
                    .unwrap_or_else(|_| "unknown".into())
                    .trim_matches('"')
                    .into(),
                confidence: candidate.event.draft.confidence,
                rank_signals: candidate.signals,
                why_selected: candidate.why_selected,
            });
        }
        self.record_context_pack(&pack)?;
        Ok(pack)
    }

    /// Rebuild a read-only entity co-occurrence graph from the eligible ledger
    /// rows.  It never writes graph state, so retractions and revisions are
    /// immediately reflected without a second truth store.
    pub fn entity_graph_projection(
        &self,
        domain: MemoryDomain,
        scope: &MemoryScope,
    ) -> Result<EntityGraphProjection> {
        let ranked = self.rank_memory_events(domain, scope, "", 100)?;
        let mut nodes: BTreeMap<String, u32> = BTreeMap::new();
        let mut edges: BTreeMap<(String, String), u32> = BTreeMap::new();
        for item in ranked {
            let entities = extract_entities(&item.event.draft.payload);
            for entity in &entities {
                *nodes.entry(entity.clone()).or_default() += 1;
            }
            for (index, source) in entities.iter().enumerate() {
                for target in entities.iter().skip(index + 1) {
                    let pair = if source < target {
                        (source.clone(), target.clone())
                    } else {
                        (target.clone(), source.clone())
                    };
                    *edges.entry(pair).or_default() += 1;
                }
            }
        }
        Ok(EntityGraphProjection {
            nodes: nodes
                .into_iter()
                .map(|(label, event_count)| EntityGraphNode {
                    id: entity_id(&label),
                    label,
                    event_count,
                })
                .collect(),
            edges: edges
                .into_iter()
                .map(|((source, target), event_count)| EntityGraphEdge {
                    source: entity_id(&source),
                    target: entity_id(&target),
                    event_count,
                })
                .collect(),
        })
    }

    pub fn record_context_pack(&self, pack: &ContextPack) -> Result<()> {
        pack.validate()?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute("INSERT OR REPLACE INTO context_pack_audits (id, session_id, created_at, pack_json) VALUES (?1, ?2, ?3, ?4)", params![pack.id, pack.session_id, pack.created_at, serde_json::to_string(pack)?])?;
        Ok(())
    }

    pub fn temporal_projection(
        &self,
        domain: MemoryDomain,
        scope: &MemoryScope,
    ) -> Result<Vec<TemporalProjectionItem>> {
        let events = self.memory_events(domain, scope)?;
        let retracted: std::collections::HashSet<String> = events
            .iter()
            .filter(|e| e.draft.kind == MemoryEventKind::Retraction)
            .filter_map(|e| e.draft.revision_of.clone())
            .collect();
        let replaced: std::collections::HashSet<String> = events
            .iter()
            .filter(|e| e.draft.kind != MemoryEventKind::Retraction)
            .filter_map(|e| e.draft.revision_of.clone())
            .collect();
        let mut out: Vec<_> = events
            .into_iter()
            .filter(|e| e.draft.kind != MemoryEventKind::Retraction)
            .map(|e| TemporalProjectionItem {
                current: !retracted.contains(&e.id) && !replaced.contains(&e.id),
                event_id: e.id,
                kind: e.draft.kind,
                content: context_content(&e.draft.payload),
                observed_at: e.observed_at,
                replaces_event_id: e.draft.revision_of,
            })
            .collect();
        out.sort_by(|a, b| b.observed_at.cmp(&a.observed_at));
        Ok(out)
    }

    pub fn context_packs(&self, session_id: &str, limit: u32) -> Result<Vec<ContextPack>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare("SELECT pack_json FROM context_pack_audits WHERE session_id = ?1 ORDER BY created_at DESC LIMIT ?2")?;
        let rows = stmt.query_map(params![session_id, limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str::<ContextPack>(&row?)?))
            .collect()
    }

    pub fn record_preflight_run(&self, run: &PreflightRun) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let decision = run
            .decision
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        conn.execute("INSERT INTO preflight_runs (id, session_id, created_at, outcome, latency_ms, decision_json, detail) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![run.id, run.session_id, run.created_at, run.outcome, run.latency_ms as i64, decision, run.detail])?;
        Ok(())
    }

    pub fn preflight_metrics(&self, limit: u32) -> Result<PreflightMetrics> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut statement = conn.prepare(
            "SELECT outcome,latency_ms FROM preflight_runs ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit.clamp(1, 10_000)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
        })?;
        let mut outcomes = std::collections::BTreeMap::new();
        let mut total = 0_u64;
        let mut latency = 0_u64;
        let mut degraded = 0_u64;
        for row in rows {
            let (outcome, ms) = row?;
            total += 1;
            latency = latency.saturating_add(ms);
            // `accepted` is the normal successful preflight outcome.  The
            // runtime deliberately uses descriptive outcome names rather
            // than a transport-level "ok", so counting every non-"ok" row
            // here would make a healthy router appear permanently degraded.
            if matches!(outcome.as_str(), "degraded" | "circuit_open") {
                degraded += 1;
            }
            *outcomes.entry(outcome).or_insert(0) += 1;
        }
        Ok(PreflightMetrics {
            total,
            degraded,
            average_latency_ms: if total == 0 { 0 } else { latency / total },
            outcomes,
        })
    }

    /// Append a redacted control-plane event. Trace failures must be handled
    /// by callers as observability failures, never as an agent-loop failure.
    pub fn record_trace_event(&self, event: &mut TraceEvent) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let sequence: u64 = conn.query_row(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM trace_events WHERE run_id=?1",
            params![event.run_id],
            |row| row.get(0),
        )?;
        event.sequence = sequence;
        conn.execute(
            "INSERT INTO trace_events (run_id,session_id,sequence,kind,detail_json,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                event.run_id,
                event.session_id,
                event.sequence as i64,
                event.kind,
                serde_json::to_string(&event.detail)?,
                event.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn start_agent_run(&self, run: &AgentRunRow) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "INSERT INTO agent_runs (id,session_id,kind,started_at,status) VALUES (?1,?2,?3,?4,?5)",
            params![run.id, run.session_id, run.kind, run.started_at, run.status],
        )?;
        Ok(())
    }

    pub fn finish_agent_run(
        &self,
        run_id: &str,
        status: &str,
        stop_reason: Option<&str>,
        duration_ms: u64,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let changed = conn.execute(
            "UPDATE agent_runs SET finished_at=?1,status=?2,stop_reason=?3,duration_ms=?4 WHERE id=?5 AND finished_at IS NULL",
            params![Utc::now().to_rfc3339(), status, stop_reason, duration_ms as i64, run_id],
        )?;
        if changed == 0 {
            return Err(Error::Invalid(
                "agent run was not found or already finished".into(),
            ));
        }
        Ok(())
    }

    pub fn set_agent_run_error(&self, run_id: &str, message: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "UPDATE agent_runs SET error_message=?1 WHERE id=?2 AND status='failed'",
            params![message.chars().take(500).collect::<String>(), run_id],
        )?;
        Ok(())
    }

    pub fn agent_runs_for_session(&self, session_id: &str, limit: u32) -> Result<Vec<AgentRunRow>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT id,session_id,kind,started_at,finished_at,status,stop_reason,duration_ms,error_message
             FROM agent_runs WHERE session_id=?1 ORDER BY started_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit.clamp(1, 10_000)], |row| {
            Ok(AgentRunRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                kind: row.get(2)?,
                started_at: row.get(3)?,
                finished_at: row.get(4)?,
                status: row.get(5)?,
                stop_reason: row.get(6)?,
                duration_ms: row.get(7)?,
                error_message: row.get(8)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn trace_events_for_session(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<TraceEvent>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT run_id,session_id,sequence,kind,detail_json,created_at
             FROM trace_events WHERE session_id=?1 ORDER BY created_at ASC, sequence ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit.clamp(1, 10_000)], |row| {
            let detail: String = row.get(4)?;
            Ok(TraceEvent {
                run_id: row.get(0)?,
                session_id: row.get(1)?,
                sequence: row.get::<_, u64>(2)?,
                kind: row.get(3)?,
                detail: serde_json::from_str(&detail).unwrap_or(serde_json::Value::Null),
                created_at: row.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn ensure_session(
        &self,
        session_id: &SessionId,
        workspace: &Path,
        title: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let title: String = title.chars().take(80).collect();
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let exists: Option<String> = conn
            .query_row(
                "SELECT id FROM sessions WHERE id = ?1",
                params![session_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            conn.execute(
                "INSERT INTO sessions (id, title, workspace, created_at, updated_at, kind, depth)
                 VALUES (?1, ?2, ?3, ?4, ?4, 'user', 0)",
                params![
                    session_id.as_str(),
                    title,
                    workspace.to_string_lossy().as_ref(),
                    now
                ],
            )?;
        } else {
            conn.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                params![now, session_id.as_str()],
            )?;
        }
        Ok(())
    }

    pub fn get_session(&self, session_id: &SessionId) -> Result<Option<SessionRow>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let row = conn
            .query_row(
                "SELECT id, title, workspace,
                        parent_id, COALESCE(kind, 'user'), COALESCE(depth, 0),
                        created_at, updated_at, COALESCE(folder, ''), COALESCE(pinned, 0), COALESCE(archived, 0)
                 FROM sessions WHERE id = ?1",
                params![session_id.as_str()],
                map_session_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn set_session_lineage(
        &self,
        session_id: &SessionId,
        parent_id: Option<&str>,
        kind: &str,
        depth: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "UPDATE sessions SET parent_id = ?1, kind = ?2, depth = ?3 WHERE id = ?4",
            params![parent_id, kind, depth, session_id.as_str()],
        )?;
        Ok(())
    }

    /// Tag a session as one cron job's run, so the panel can list run history.
    pub fn set_session_cron_job(&self, session_id: &SessionId, job_id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "UPDATE sessions SET cron_job_id = ?1 WHERE id = ?2",
            params![job_id, session_id.as_str()],
        )?;
        Ok(())
    }

    /// One row per past run of `job_id`, newest first. `finished` is false while
    /// the run has produced no assistant reply yet.
    pub fn cron_runs(&self, job_id: &str, limit: u32) -> Result<Vec<CronRunRow>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT s.id, s.created_at, s.updated_at,
                    (SELECT COUNT(*) FROM messages m
                      WHERE m.session_id = s.id AND m.role = 'assistant'),
                    (SELECT m.content FROM messages m
                      WHERE m.session_id = s.id AND m.role = 'assistant'
                      ORDER BY m.id DESC LIMIT 1)
             FROM sessions s
             WHERE s.cron_job_id = ?1
             ORDER BY s.created_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![job_id, limit as i64], |row| {
            let replies: i64 = row.get(3)?;
            let summary: Option<String> = row.get(4)?;
            Ok(CronRunRow {
                session_id: row.get(0)?,
                started_at: row.get(1)?,
                updated_at: row.get(2)?,
                finished: replies > 0,
                summary: summary.unwrap_or_default(),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn sessions_list(&self, limit: u32, offset: u32) -> Result<Vec<SessionRow>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT id, title, workspace, parent_id,
                    COALESCE(kind, 'user'), COALESCE(depth, 0),
                    created_at, updated_at, COALESCE(folder, ''), COALESCE(pinned, 0), COALESCE(archived, 0)
             FROM sessions
             WHERE COALESCE(kind, 'user') <> 'wiki'
             ORDER BY COALESCE(pinned, 0) DESC, COALESCE(archived, 0) ASC, updated_at DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], map_session_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn projects_list(&self) -> Result<Vec<ProjectRow>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, workspace, created_at, updated_at
             FROM projects ORDER BY updated_at DESC, name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], map_project_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn project_create(&self, workspace: &Path) -> Result<ProjectRow> {
        let workspace = workspace.canonicalize()?;
        // Windows canonicalization may add the extended-length `\\?\\`
        // prefix. Sessions persist the user-selected spelling, so retaining
        // that implementation-only prefix here makes the same directory fail
        // to join its project. Keep one stable, user-visible representation.
        let workspace = dunce::simplified(&workspace).to_path_buf();
        if !workspace.is_dir() {
            return Err(Error::Invalid(
                "project workspace must be a directory".into(),
            ));
        }
        let workspace = workspace.to_string_lossy().into_owned();
        let name = Path::new(&workspace)
            .file_name()
            .and_then(|part| part.to_str())
            .filter(|part| !part.is_empty())
            .unwrap_or("项目")
            .to_string();
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        if let Some(project) = conn
            .query_row(
                "SELECT id, name, workspace, created_at, updated_at FROM projects WHERE workspace = ?1",
                params![workspace],
                map_project_row,
            )
            .optional()?
        {
            return Ok(project);
        }
        let now = Utc::now().to_rfc3339();
        let project = ProjectRow {
            id: format!("project_{}", uuid::Uuid::new_v4().simple()),
            name,
            workspace,
            created_at: now.clone(),
            updated_at: now,
        };
        conn.execute(
            "INSERT INTO projects (id, name, workspace, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                project.id,
                project.name,
                project.workspace,
                project.created_at,
                project.updated_at
            ],
        )?;
        Ok(project)
    }

    /// 删除工作空间。
    ///
    /// `purge_sessions` 为真时把它名下的会话整棵删掉（含子代理会话与消息），
    /// 为假时只把会话归档。桌面端的工作空间是会话的归属单位：留着会话却没有
    /// 空间可进，那些对话就成了没有入口的孤儿，所以那边一律传真。保留归档
    /// 分支是给「只想撤掉一个书签」的调用方留的退路。
    pub fn project_delete(&self, id: &str, purge_sessions: bool) -> Result<Option<ProjectRow>> {
        let mut conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let tx = conn.transaction()?;
        let project = tx
            .query_row(
                "SELECT id, name, workspace, created_at, updated_at FROM projects WHERE id = ?1",
                params![id],
                map_project_row,
            )
            .optional()?;
        if let Some(project) = &project {
            if purge_sessions {
                // 递归收齐该工作空间下的会话：子代理会话可能没有继承父的
                // workspace 列，只按 workspace 删会漏掉它们，留下无人认领的
                // 消息行。
                let doomed: Vec<String> = {
                    let mut stmt = tx.prepare(
                        "WITH RECURSIVE doomed(id) AS (
                             SELECT id FROM sessions WHERE workspace = ?1
                             UNION
                             SELECT s.id FROM sessions s JOIN doomed d ON s.parent_id = d.id
                         )
                         SELECT id FROM doomed",
                    )?;
                    let rows =
                        stmt.query_map(params![project.workspace], |row| row.get::<_, String>(0))?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()?
                };
                for session_id in &doomed {
                    tx.execute(
                        "DELETE FROM message_archive WHERE session_id = ?1",
                        params![session_id],
                    )?;
                    tx.execute(
                        "DELETE FROM compaction_checkpoints WHERE session_id = ?1",
                        params![session_id],
                    )?;
                    tx.execute(
                        "DELETE FROM messages WHERE session_id = ?1",
                        params![session_id],
                    )?;
                    tx.execute(
                        "DELETE FROM task_bindings WHERE session_id = ?1",
                        params![session_id],
                    )?;
                    tx.execute(
                        "DELETE FROM session_todos WHERE session_id = ?1",
                        params![session_id],
                    )?;
                    tx.execute(
                        "DELETE FROM session_todo_meta WHERE session_id = ?1",
                        params![session_id],
                    )?;
                    tx.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
                }
            } else {
                let now = Utc::now().to_rfc3339();
                tx.execute(
                    "UPDATE sessions SET archived = 1, updated_at = ?1 WHERE workspace = ?2",
                    params![now, project.workspace],
                )?;
            }
            tx.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(project)
    }

    /// 给工作空间改名。
    ///
    /// 空间名是**显示名**，不是目录名：它是用户给这个目录起的叫法，改它不动磁盘上的
    /// 任何东西，也不动 `workspace` 列（那才是目录本身）。一条对话归在哪个空间里只看
    /// 目录，所以改名之后列表立刻跟着变，不需要搬任何数据 —— 这正是「名字」该有的
    /// 性质，改名字不该牵动身份。
    pub fn project_rename(&self, id: &str, name: &str) -> Result<Option<ProjectRow>> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Invalid("project name must not be empty".into()));
        }
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let n = conn.execute(
            "UPDATE projects SET name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        if n == 0 {
            return Ok(None);
        }
        conn.query_row(
            "SELECT id, name, workspace, created_at, updated_at FROM projects WHERE id = ?1",
            params![id],
            map_project_row,
        )
        .optional()
        .map_err(Into::into)
    }

    /// 把工作空间从列表里撤掉，**留下**它的对话。
    ///
    /// 和 `project_delete` 的区别只在会话：那边把会话删掉（`purge_sessions`）或归档，
    /// 这边一行会话都不碰，只删 `projects` 那一条。会话的 `workspace` 列还写着那个
    /// 目录，只是再也没有空间登记它了 —— 于是它们自然落进「未归属」那一格。
    ///
    /// 「从列表中移除」的语义就该是这样：撤掉一个书签，不是销毁书签指向的东西。
    /// 目录还在磁盘上，对话还在库里，只是不再作为一个空间出现。
    pub fn project_unlist(&self, id: &str) -> Result<Option<ProjectRow>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let project = conn
            .query_row(
                "SELECT id, name, workspace, created_at, updated_at FROM projects WHERE id = ?1",
                params![id],
                map_project_row,
            )
            .optional()?;
        if let Some(project) = &project {
            conn.execute("DELETE FROM projects WHERE id = ?1", params![project.id])?;
        }
        Ok(project)
    }

    /// 清掉目录已不存在的工作空间条目，连同其名下会话。
    ///
    /// 工作空间就是这个目录本身（`project_create` 已经 canonicalize），目录被
    /// 外部删除或移走之后，条目只是一个点不开的死书签：既进不去，也没法解释
    /// 它为什么还在，所以直接清掉，而不是灰在那儿。返回被清掉的空间，调用方
    /// 可以据此告诉用户发生了什么 —— 这类清理是自动发生的，不能一声不吭。
    pub fn projects_prune_missing(&self) -> Result<Vec<ProjectRow>> {
        let mut removed = Vec::new();
        for project in self.projects_list()? {
            if Path::new(&project.workspace).is_dir() {
                continue;
            }
            if self.project_delete(&project.id, true)?.is_some() {
                removed.push(project);
            }
        }
        Ok(removed)
    }

    /// 清掉「一个对话都没有」的工作空间条目。
    ///
    /// 历史包袱清理：早期「打开文件夹」「选择工作区」「给对话选目录」都会顺手
    /// 建一条项目记录，于是浏览过的目录全都留在空间列表里，空间看起来像是系统
    /// 塞给用户的。那些入口已经不再建记录，但存量条目需要一个用户主动触发的
    /// 出口 —— 不能用「目录还在不在」判断（`projects_prune_missing` 管的是那个），
    /// 只能按「名下有没有会话」判断。
    ///
    /// 判据由 SQL 算，不接受调用方的名单：前端只加载最近一页会话，拿它筛会把
    /// 超出窗口的对话当成不存在，进而删掉一个有对话的空间。
    ///
    /// `dry_run` 为真时只返回名单不落库，供前端先给用户看清要删哪几个。
    /// 只删项目记录，不动会话（按定义这些空间名下没有会话）。
    pub fn projects_prune_empty(&self, dry_run: bool) -> Result<Vec<ProjectRow>> {
        let mut conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let doomed: Vec<ProjectRow> = {
            let mut stmt = conn.prepare(
                "SELECT p.id, p.name, p.workspace, p.created_at, p.updated_at
                 FROM projects p
                 WHERE NOT EXISTS (
                     SELECT 1 FROM sessions s WHERE s.workspace = p.workspace
                 )
                 ORDER BY p.updated_at DESC, p.name COLLATE NOCASE",
            )?;
            let rows = stmt.query_map([], map_project_row)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        if dry_run || doomed.is_empty() {
            return Ok(doomed);
        }
        let tx = conn.transaction()?;
        for project in &doomed {
            tx.execute("DELETE FROM projects WHERE id = ?1", params![project.id])?;
        }
        tx.commit()?;
        Ok(doomed)
    }

    /// `workspace` 为 `Some` 时只在那个工作空间内搜。
    ///
    /// 会话按工作空间隔离之后，搜索必须跟着收窄：侧栏写着「只属于这个空间」，
    /// 搜索框却能把别的空间的对话捞出来，隔离就是假的。`None` 不限，留给 CLI
    /// 这类没有「当前空间」概念的调用方。
    pub fn sessions_search(
        &self,
        query: &str,
        workspace: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SessionSearchHit>> {
        let needle = like_contains(query);
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT s.id, s.title, s.updated_at, m.content
             FROM messages m
             JOIN sessions s ON s.id = m.session_id
             WHERE m.content LIKE ?1 ESCAPE '\\' AND (?3 IS NULL OR s.workspace = ?3)
             ORDER BY s.updated_at DESC, m.id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![needle, limit as i64, workspace], |row| {
            Ok(SessionSearchHit {
                id: row.get(0)?,
                title: row.get(1)?,
                updated_at: row.get(2)?,
                snippet: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for row in rows {
            let hit = row?;
            if seen.insert(hit.id.clone()) {
                out.push(hit);
            }
        }
        Ok(out)
    }

    pub fn session_export(&self, session_id: &SessionId) -> Result<serde_json::Value> {
        let row = self
            .get_session(session_id)?
            .ok_or_else(|| Error::SessionNotFound(session_id.to_string()))?;
        let messages = self.load_rows(session_id)?;
        let msgs: Vec<serde_json::Value> = messages
            .into_iter()
            .map(|m| {
                serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                    "toolCalls": m.tool_calls,
                    "toolCallId": m.tool_call_id,
                    "engineId": m.engine_id,
                    "attachments": m.attachments.as_deref()
                        .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok()),
                })
            })
            .collect();
        let prefs = self.session_prefs(session_id)?;
        Ok(serde_json::json!({
            "id": row.id,
            "title": row.title,
            "workspace": row.workspace,
            "kind": row.kind,
            "parentId": row.parent_id,
            "depth": row.depth,
            "folder": row.folder,
            "prefs": prefs,
            "createdAt": row.created_at,
            "updatedAt": row.updated_at,
            "messages": msgs,
        }))
    }

    /// Return one newest-first window of a session's transcript, serialized in
    /// chronological order for a UI.  Keeping this separate from
    /// `session_export` preserves the complete CLI/export artifact while
    /// allowing desktop startup to avoid shipping and rendering an unbounded
    /// transcript before its first frame.
    pub fn session_export_window(
        &self,
        session_id: &SessionId,
        offset: u32,
        limit: u32,
    ) -> Result<serde_json::Value> {
        let row = self
            .get_session(session_id)?
            .ok_or_else(|| Error::SessionNotFound(session_id.to_string()))?;
        let limit = limit.clamp(1, 200) as i64;
        let offset = offset as i64;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let total_messages: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
            params![session_id.as_str()],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(
            "SELECT id, role, content, tool_calls, tool_call_id, engine_id, attachments
             FROM (
                SELECT m.id, m.role, m.content, m.tool_calls, m.tool_call_id, e.engine_id, m.attachments
                FROM messages m LEFT JOIN message_agent_engines e
                  ON e.session_id = m.session_id AND e.message_id = m.acp_message_id
                WHERE m.session_id = ?1
                ORDER BY m.id DESC
                LIMIT ?2 OFFSET ?3
             )
             ORDER BY id ASC",
        )?;
        let messages = stmt
            .query_map(params![session_id.as_str(), limit, offset], |row| {
                let attachments: Option<String> = row.get(6)?;
                Ok(serde_json::json!({
                    "id": row.get::<_, i64>(0)?,
                    "role": row.get::<_, String>(1)?,
                    "content": row.get::<_, String>(2)?,
                    "toolCalls": row.get::<_, Option<String>>(3)?,
                    "toolCallId": row.get::<_, Option<String>>(4)?,
                    "engineId": row.get::<_, Option<String>>(5)?,
                    "attachments": attachments.as_deref()
                        .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok()),
                }))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        // `session_prefs` takes the same connection mutex.  Release the
        // statement and read guard before looking up preferences.
        drop(stmt);
        drop(conn);
        let prefs = self.session_prefs(session_id)?;
        Ok(serde_json::json!({
            "id": row.id,
            "title": row.title,
            "workspace": row.workspace,
            "kind": row.kind,
            "parentId": row.parent_id,
            "depth": row.depth,
            "folder": row.folder,
            "prefs": prefs,
            "createdAt": row.created_at,
            "updatedAt": row.updated_at,
            "totalMessages": total_messages,
            "messages": messages,
        }))
    }

    /// Full conversation history across compactions, paged newest-first like
    /// `session_export_window` and serialized oldest-first. Archived rows come
    /// first (in archive order, which is chronological because every
    /// compaction covers a newer prefix than the last one), then the current
    /// rows. Rows an earlier compaction injected (`is_derived`) are left out;
    /// the latest checkpoint's summary/todo rows are tagged `checkpoint`.
    pub fn session_history_window(
        &self,
        session_id: &SessionId,
        offset: u32,
        limit: u32,
    ) -> Result<serde_json::Value> {
        self.get_session(session_id)?
            .ok_or_else(|| Error::SessionNotFound(session_id.to_string()))?;
        let limit = limit.clamp(1, 200) as i64;
        let offset = offset as i64;
        let sid = session_id.as_str();
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let latest: Option<(String, String, Option<String>)> = conn
            .query_row(
                "SELECT id, summary_message_id, todo_message_id FROM compaction_checkpoints
                 WHERE session_id = ?1 ORDER BY ordinal DESC LIMIT 1",
                params![sid],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (cp_id, cp_summary, cp_todo) = match latest {
            Some((a, b, c)) => (Some(a), Some(b), c),
            None => (None, None, None),
        };
        // `src` 0 = archive, 1 = current; `key` is archive_id / id.
        const UNION: &str = "
            SELECT 0 AS src, a.archive_id AS key, a.orig_id AS row_id, a.checkpoint_id AS cp,
                   a.role, a.content, a.tool_calls, a.tool_call_id, a.acp_message_id,
                   a.attachments, a.created_at
            FROM message_archive a WHERE a.session_id = ?1 AND a.is_derived = 0
            UNION ALL
            SELECT 1, m.id, m.id, NULL, m.role, m.content, m.tool_calls, m.tool_call_id,
                   m.acp_message_id, m.attachments, m.created_at
            FROM messages m WHERE m.session_id = ?1";
        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM ({UNION})"),
            params![sid],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(&format!(
            "SELECT u.src, u.key, u.row_id, u.cp, u.role, u.content, u.tool_calls,
                    u.tool_call_id, u.acp_message_id, e.engine_id, u.attachments, u.created_at
             FROM (
                SELECT * FROM ({UNION}) ORDER BY src DESC, key DESC LIMIT ?2 OFFSET ?3
             ) u
             LEFT JOIN message_agent_engines e
               ON e.session_id = ?1 AND e.message_id = u.acp_message_id
             ORDER BY u.src ASC, u.key ASC"
        ))?;
        let rows = stmt
            .query_map(params![sid, limit, offset], |row| {
                let src: i64 = row.get(0)?;
                let acp: Option<String> = row.get(8)?;
                let is_checkpoint = src == 1
                    && acp.is_some()
                    && (acp == cp_summary || acp == cp_todo);
                let source = if is_checkpoint {
                    "checkpoint"
                } else if src == 0 {
                    "archive"
                } else {
                    "current"
                };
                let attachments: Option<String> = row.get(10)?;
                Ok(serde_json::json!({
                    "source": source,
                    "seq": row.get::<_, i64>(1)?,
                    "id": row.get::<_, i64>(2)?,
                    "checkpointId": if is_checkpoint {
                        cp_id.clone()
                    } else {
                        row.get::<_, Option<String>>(3)?
                    },
                    "role": row.get::<_, String>(4)?,
                    "content": row.get::<_, String>(5)?,
                    "toolCalls": row.get::<_, Option<String>>(6)?,
                    "toolCallId": row.get::<_, Option<String>>(7)?,
                    "engineId": row.get::<_, Option<String>>(9)?,
                    "attachments": attachments.as_deref()
                        .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok()),
                    "createdAt": row.get::<_, String>(11)?,
                }))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(serde_json::json!({
            "id": sid,
            "totalMessages": total,
            "messages": rows,
        }))
    }

    pub fn set_session_title(&self, session_id: &SessionId, title: &str) -> Result<bool> {
        let title: String = title.chars().take(80).collect();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let n = conn.execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now, session_id.as_str()],
        )?;
        Ok(n > 0)
    }

    pub fn set_session_pinned(&self, session_id: &SessionId, pinned: bool) -> Result<bool> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        Ok(conn.execute(
            "UPDATE sessions SET pinned = ?1 WHERE id = ?2",
            params![pinned as i64, session_id.as_str()],
        )? > 0)
    }

    pub fn set_session_archived(&self, session_id: &SessionId, archived: bool) -> Result<bool> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        Ok(conn.execute(
            "UPDATE sessions SET archived = ?1 WHERE id = ?2",
            params![archived as i64, session_id.as_str()],
        )? > 0)
    }

    pub fn set_session_folder(&self, session_id: &SessionId, folder: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let n = conn.execute(
            "UPDATE sessions SET folder = ?1, updated_at = ?2 WHERE id = ?3",
            params![folder, now, session_id.as_str()],
        )?;
        Ok(n > 0)
    }

    pub fn session_prefs(&self, session_id: &SessionId) -> Result<SessionPrefs> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT COALESCE(prefs, '{}') FROM sessions WHERE id = ?1",
                params![session_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        match raw {
            None => Ok(SessionPrefs::default()),
            Some(s) => Ok(serde_json::from_str(&s).unwrap_or_default()),
        }
    }

    pub fn set_session_prefs(&self, session_id: &SessionId, prefs: &SessionPrefs) -> Result<bool> {
        // 「这次没带 workspace」和「明确要清空 workspace」必须区分开。`normalize()` 会把
        // 空路径过滤成 `None`，只看归一化后的结果两者长得一样，于是清空动作永远不进
        // 下面那个 `if let Some`，`sessions.workspace` 那一列一直留着旧路径 —— 界面上
        // 清工作区那个 × 看着就像没生效（prefs 清了、列没清，两侧读的是同一件事）。
        //
        // invoke 传 `""` 与「不传」在前端是两回事，所以按「传了但为空」认清空。
        let clearing = prefs
            .workspace
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty());
        // 规范拼写在落库前统一做一次：prefs JSON 与 sessions.workspace 列出自同一次
        // 归一化，两张表、两个读面都指向同一个字符串，分组比较（前端全用 `===`）
        // 才不会因 `\\?\` 前缀这类实现细节拼写而错配。
        let prefs = prefs.clone().normalize().with_canonical_workspace();
        let json = serde_json::to_string(&prefs)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let n = conn.execute(
            "UPDATE sessions SET prefs = ?1, updated_at = ?2 WHERE id = ?3",
            params![json, now, session_id.as_str()],
        )?;
        if n > 0 {
            match &prefs.workspace {
                Some(ws) => {
                    let workspace = ws.to_string_lossy();
                    conn.execute(
                        "UPDATE sessions SET workspace = ?1 WHERE id = ?2",
                        params![workspace.as_ref(), session_id.as_str()],
                    )?;
                    conn.execute(
                        "UPDATE projects SET updated_at = ?1 WHERE workspace = ?2",
                        params![now, workspace.as_ref()],
                    )?;
                    propagate_workspace_down(&conn, session_id.as_str(), &workspace)?;
                }
                // 清空：列也要跟着空，否则这条会话仍会被算进那个空间名下（侧栏分组、
                // 删除空间时的级联范围都按这一列算）。
                None if clearing => {
                    conn.execute(
                        "UPDATE sessions SET workspace = '' WHERE id = ?1",
                        params![session_id.as_str()],
                    )?;
                    propagate_workspace_down(&conn, session_id.as_str(), "")?;
                }
                None => {}
            }
        }
        Ok(n > 0)
    }

    pub fn session_tree_ids(&self, session_id: &SessionId) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let id = session_id.as_str().to_string();
        let exists: Option<String> = conn
            .query_row(
                "SELECT id FROM sessions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Ok(Vec::new());
        }
        let mut ids = vec![id.clone()];
        let mut stmt = conn.prepare("SELECT id FROM sessions WHERE parent_id = ?1")?;
        let children = stmt.query_map(params![id], |row| row.get::<_, String>(0))?;
        for c in children {
            ids.push(c?);
        }
        Ok(ids)
    }

    pub fn delete_session_tree(&self, session_id: &SessionId) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let id = session_id.as_str().to_string();
        let mut ids = vec![id.clone()];
        let mut stmt = conn.prepare("SELECT id FROM sessions WHERE parent_id = ?1")?;
        let children = stmt.query_map(params![id], |row| row.get::<_, String>(0))?;
        for c in children {
            ids.push(c?);
        }
        drop(stmt);
        for child in &ids {
            conn.execute(
                "DELETE FROM message_archive WHERE session_id = ?1",
                params![child],
            )?;
            conn.execute(
                "DELETE FROM compaction_checkpoints WHERE session_id = ?1",
                params![child],
            )?;
            conn.execute("DELETE FROM messages WHERE session_id = ?1", params![child])?;
            conn.execute(
                "DELETE FROM task_bindings WHERE session_id = ?1",
                params![child],
            )?;
            conn.execute(
                "DELETE FROM session_todos WHERE session_id = ?1",
                params![child],
            )?;
            conn.execute(
                "DELETE FROM session_todo_meta WHERE session_id = ?1",
                params![child],
            )?;
            conn.execute("DELETE FROM sessions WHERE id = ?1", params![child])?;
            self.remove_session_attachments(child);
        }
        Ok(())
    }

    pub fn list_todos(&self, session_id: &SessionId) -> Result<Vec<TodoItem>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT id, content, status, parent FROM session_todos
             WHERE session_id = ?1 ORDER BY position, id",
        )?;
        let rows = stmt.query_map(params![session_id.as_str()], |r| {
            Ok(TodoItem {
                id: r.get(0)?,
                content: r.get(1)?,
                status: r.get(2)?,
                parent: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn todo_revision(&self, session_id: &SessionId) -> Result<u32> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let rev: Option<i64> = conn
            .query_row(
                "SELECT revision FROM session_todo_meta WHERE session_id = ?1",
                params![session_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(rev.unwrap_or(0) as u32)
    }

    pub fn save_todos(
        &self,
        session_id: &SessionId,
        items: &[TodoItem],
        revision: u32,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "DELETE FROM session_todos WHERE session_id = ?1",
            params![session_id.as_str()],
        )?;
        for (i, item) in items.iter().enumerate() {
            conn.execute(
                "INSERT INTO session_todos (session_id, id, content, status, parent, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    session_id.as_str(),
                    item.id,
                    item.content,
                    item.status,
                    item.parent,
                    i as i64
                ],
            )?;
        }
        conn.execute(
            "INSERT INTO session_todo_meta (session_id, revision) VALUES (?1, ?2)
             ON CONFLICT(session_id) DO UPDATE SET revision = excluded.revision",
            params![session_id.as_str(), revision as i64],
        )?;
        Ok(())
    }

    pub fn append_user(
        &self,
        session_id: &SessionId,
        content: &str,
        message_id: &MessageId,
    ) -> Result<()> {
        self.append(
            session_id,
            "user",
            content,
            None,
            None,
            Some(message_id.as_str()),
        )
    }

    /// Persist a user turn's images to `<data_dir>/attachments/<session>/`
    /// and store their relative paths on the message row, so chat history
    /// keeps viewable previews. The provider still receives the transient
    /// data URLs; SQLite only ever holds the relative paths. Invalid entries
    /// (bad data URL, unsupported type, oversized) are skipped rather than
    /// failing the turn — the preview is auxiliary to the actual model input.
    pub fn persist_message_images(
        &self,
        session_id: &SessionId,
        message_id: &MessageId,
        data_urls: &[String],
    ) -> Result<Vec<String>> {
        let mut files = Vec::new();
        let dir = self.data_dir.join("attachments").join(session_id.as_str());
        for (index, data_url) in data_urls.iter().take(16).enumerate() {
            let Some((mime, payload)) = data_url.split_once(',') else { continue };
            let ext = match mime {
                "data:image/png;base64" => "png",
                "data:image/jpeg;base64" => "jpg",
                "data:image/webp;base64" => "webp",
                "data:image/gif;base64" => "gif",
                _ => continue,
            };
            let Some(bytes) = crate::acp::b64::decode_base64(payload) else { continue };
            // Hard ceiling; the user-facing budget is `dandelion.vision.*`.
            if bytes.is_empty() || bytes.len() > 33_554_432 {
                continue;
            }
            if std::fs::create_dir_all(&dir).is_err() {
                continue;
            }
            let name = format!("{}-{index}.{ext}", message_id.as_str());
            let path = dir.join(&name);
            if std::fs::write(&path, &bytes).is_err() {
                continue;
            }
            files.push(format!("attachments/{}/{}", session_id.as_str(), name));
        }
        if files.is_empty() {
            return Ok(files);
        }
        let json = serde_json::to_string(&files)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "UPDATE messages SET attachments = ?1 WHERE session_id = ?2 AND acp_message_id = ?3",
            params![json, session_id.as_str(), message_id.as_str()],
        )?;
        Ok(files)
    }

    /// Resolve a stored relative attachment path (forward slashes) back to a
    /// data URL for rendering. Paths are validated to stay inside the
    /// attachments directory.
    pub fn attachment_data_url(&self, rel: &str) -> Result<String> {
        let rel = rel.trim().replace('\\', "/");
        if rel.is_empty()
            || !rel.starts_with("attachments/")
            || rel.split('/').any(|part| part == ".." || part.is_empty())
        {
            return Err(Error::Invalid("无效的附件路径".into()));
        }
        let path = self.data_dir.join(&rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        let canonical = path.canonicalize().map_err(|_| Error::Invalid("附件不存在".into()))?;
        let root = self.data_dir.join("attachments");
        let root = std::fs::canonicalize(&root)
            .unwrap_or_else(|_| root.clone());
        if !canonical.starts_with(&root) {
            return Err(Error::Invalid("附件路径越界".into()));
        }
        let bytes = std::fs::read(&canonical)?;
        if bytes.len() > 33_554_432 {
            return Err(Error::Invalid("附件过大".into()));
        }
        let mime = match canonical.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            _ => return Err(Error::Invalid("不支持的附件类型".into())),
        };
        Ok(format!("data:{mime};base64,{}", crate::acp::b64::encode_base64(&bytes)))
    }

    /// Delete every persisted image of a session (called on session removal).
    pub fn remove_session_attachments(&self, session_id: &str) {
        if session_id.is_empty()
            || !session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return;
        }
        let dir = self.data_dir.join("attachments").join(session_id);
        let _ = std::fs::remove_dir_all(dir);
    }

    pub fn append_assistant(
        &self,
        session_id: &SessionId,
        content: &str,
        tool_calls: &[ToolCall],
        message_id: &MessageId,
    ) -> Result<()> {
        let tool_json = if tool_calls.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&openai_tool_calls(tool_calls))?)
        };
        self.append(
            session_id,
            "assistant",
            content,
            tool_json.as_deref(),
            None,
            Some(message_id.as_str()),
        )
    }

    pub fn tag_message_engine(&self, session_id: &SessionId, message_id: &MessageId, engine_id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "INSERT OR REPLACE INTO message_agent_engines(session_id, message_id, engine_id) VALUES (?1, ?2, ?3)",
            params![session_id.as_str(), message_id.as_str(), engine_id],
        )?;
        Ok(())
    }

    pub fn append_tool(
        &self,
        session_id: &SessionId,
        tool_call_id: &str,
        content: &str,
    ) -> Result<()> {
        self.append(session_id, "tool", content, None, Some(tool_call_id), None)
    }

    /// 轮次边界标记（对齐 Codex 的 `<turn_aborted>`）：轮次被中断或未正常
    /// 结束时写入，`load_openai_messages` 把它转成模型可见的消息，UI 的
    /// 导出映射只认 user/assistant/tool，因此这条对界面不可见。
    pub fn append_turn_note(&self, session_id: &SessionId, content: &str) -> Result<()> {
        self.append(session_id, "turn_note", content, None, None, None)
    }

    fn append(
        &self,
        session_id: &SessionId,
        role: &str,
        content: &str,
        tool_calls: Option<&str>,
        tool_call_id: Option<&str>,
        acp_message_id: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, acp_message_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session_id.as_str(),
                role,
                content,
                tool_calls,
                tool_call_id,
                acp_message_id,
                now
            ],
        )?;
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now, session_id.as_str()],
        )?;
        Ok(())
    }

    /// Rows of the current context with their `messages.id`, oldest first.
    fn load_rows_with_ids(&self, session_id: &SessionId) -> Result<Vec<(i64, ChatRow)>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.role, m.content, m.tool_calls, m.tool_call_id, m.acp_message_id, e.engine_id, m.attachments
             FROM messages m LEFT JOIN message_agent_engines e
             ON e.session_id = m.session_id AND e.message_id = m.acp_message_id
             WHERE m.session_id = ?1 ORDER BY m.id ASC",
        )?;
        let rows = stmt.query_map(params![session_id.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                ChatRow {
                    role: row.get(1)?,
                    content: row.get(2)?,
                    tool_calls: row.get(3)?,
                    tool_call_id: row.get(4)?,
                    acp_message_id: row.get(5)?,
                    engine_id: row.get(6)?,
                    attachments: row.get(7)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn load_rows(&self, session_id: &SessionId) -> Result<Vec<ChatRow>> {
        Ok(self
            .load_rows_with_ids(session_id)?
            .into_iter()
            .map(|(_, row)| row)
            .collect())
    }

    pub fn clone_messages(&self, from: &SessionId, to: &SessionId) -> Result<()> {
        let rows = self.load_rows(from)?;
        for row in rows {
            self.append(
                to,
                &row.role,
                &row.content,
                row.tool_calls.as_deref(),
                row.tool_call_id.as_deref(),
                row.acp_message_id.as_deref(),
            )?;
            // Carry the attachment reference over. The image files live under
            // `<data_dir>/attachments/<from-session>/...`; the copied row keeps
            // that same relative path (shared blobs, no re-copy).
            if let Some(attachments) = row.attachments.as_deref() {
                if let Some(acp_id) = row.acp_message_id.as_deref() {
                    let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
                    conn.execute(
                        "UPDATE messages SET attachments = ?1 WHERE session_id = ?2 AND acp_message_id = ?3",
                        params![attachments, to.as_str(), acp_id],
                    )?;
                }
            }
        }
        Ok(())
    }

    pub fn load_openai_messages(&self, session_id: &SessionId) -> Result<Vec<ChatMessage>> {
        Ok(self.load_openai_messages_with_ids(session_id)?.0)
    }

    /// Model messages plus the `messages.id` each one came from
    /// (`ids[i]` belongs to `messages[i]`). Rows with unknown roles are skipped
    /// in both, so the two vectors always line up.
    pub fn load_openai_messages_with_ids(
        &self,
        session_id: &SessionId,
    ) -> Result<(Vec<ChatMessage>, Vec<i64>)> {
        let mut messages = Vec::new();
        let mut ids = Vec::new();
        for (id, row) in self.load_rows_with_ids(session_id)? {
            if let Some(msg) = row_to_message(row) {
                messages.push(msg);
                ids.push(id);
            }
        }
        Ok((messages, ids))
    }

    /// Compact a session in one transaction: archive the covered prefix
    /// verbatim, insert the summary (and optional todo) rows, and move the kept
    /// tail behind them by copying whole rows, so `created_at`,
    /// `acp_message_id`, `tool_calls` and `attachments` survive byte for byte.
    /// Nothing changes if any step fails or the session no longer matches
    /// `covered_ids ++ tail_ids`.
    pub fn compact_messages(&self, input: &CompactionInput<'_>) -> Result<CompactionCheckpoint> {
        if input.covered_ids.is_empty() || input.tail_ids.is_empty() {
            return Err(Error::Invalid(
                "compaction needs a non-empty prefix and tail".into(),
            ));
        }
        let sid = input.session_id.as_str();
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let current: Vec<i64> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT id FROM messages WHERE session_id = ?1 AND role IN {CONVERTIBLE_ROLES_SQL} ORDER BY id ASC"
            ))?;
            let rows = stmt.query_map(params![sid], |row| row.get::<_, i64>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let expected: Vec<i64> = input
            .covered_ids
            .iter()
            .chain(input.tail_ids.iter())
            .copied()
            .collect();
        if current != expected {
            return Err(Error::Invalid(
                "compaction: session messages changed since they were read".into(),
            ));
        }
        // A tail must start a turn; otherwise a tool result could be split
        // from the assistant call that produced it.
        let first_tail_role: String = tx.query_row(
            "SELECT role FROM messages WHERE id = ?1 AND session_id = ?2",
            params![input.tail_ids[0], sid],
            |row| row.get(0),
        )?;
        if first_tail_role != "user" && first_tail_role != "turn_note" {
            return Err(Error::Invalid(
                "compaction: kept tail must start with a user turn".into(),
            ));
        }

        let last_covered = *input.covered_ids.last().expect("checked non-empty");
        let max_old: i64 = tx.query_row(
            "SELECT MAX(id) FROM messages WHERE session_id = ?1",
            params![sid],
            |row| row.get(0),
        )?;

        let prev: Option<(String, i64, String, Option<String>)> = tx
            .query_row(
                "SELECT id, ordinal, summary_message_id, todo_message_id
                 FROM compaction_checkpoints WHERE session_id = ?1
                 ORDER BY ordinal DESC LIMIT 1",
                params![sid],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let checkpoint = CompactionCheckpoint {
            id: format!("cp_{}", uuid::Uuid::new_v4().simple()),
            session_id: sid.to_string(),
            ordinal: prev.as_ref().map_or(1, |p| p.1 + 1),
            parent_id: prev.as_ref().map(|p| p.0.clone()),
            trigger: input.trigger.as_str().to_string(),
            summary: input.summary.to_string(),
            todo: input.todo.map(str::to_string),
            summary_message_id: MessageId::new().as_str().to_string(),
            todo_message_id: input
                .todo
                .map(|_| MessageId::new().as_str().to_string()),
            covered_count: input.covered_ids.len() as i64,
            tail_count: input.tail_ids.len() as i64,
            source_bytes: input.source_bytes as i64,
            source_truncated: input.source_truncated,
            created_at: now.clone(),
        };
        tx.execute(
            "INSERT INTO compaction_checkpoints
               (id, session_id, ordinal, parent_id, trigger_kind, summary, todo,
                summary_message_id, todo_message_id, covered_count, tail_count,
                source_bytes, source_truncated, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                checkpoint.id,
                sid,
                checkpoint.ordinal,
                checkpoint.parent_id,
                checkpoint.trigger,
                checkpoint.summary,
                checkpoint.todo,
                checkpoint.summary_message_id,
                checkpoint.todo_message_id,
                checkpoint.covered_count,
                checkpoint.tail_count,
                checkpoint.source_bytes,
                checkpoint.source_truncated as i64,
                checkpoint.created_at,
            ],
        )?;

        // Everything up to the last covered row goes to the archive, including
        // rows with roles the model never sees.
        tx.execute(
            "INSERT INTO message_archive
               (session_id, checkpoint_id, orig_id, role, content, tool_calls,
                tool_call_id, acp_message_id, attachments, created_at, is_derived)
             SELECT session_id, ?2, id, role, content, tool_calls,
                    tool_call_id, acp_message_id, attachments, created_at, 0
             FROM messages WHERE session_id = ?1 AND id <= ?3 ORDER BY id ASC",
            params![sid, checkpoint.id, last_covered],
        )?;
        match &prev {
            // The previous compaction's own summary/todo rows are not
            // conversation; flag them so history can skip them.
            Some((_, _, summary_mid, todo_mid)) => {
                tx.execute(
                    "UPDATE message_archive SET is_derived = 1
                     WHERE checkpoint_id = ?1 AND acp_message_id IN (?2, ?3)",
                    params![checkpoint.id, summary_mid, todo_mid],
                )?;
            }
            // Databases compacted before checkpoints existed: only the very
            // first row, and only with the exact marker, counts as a summary.
            None => {
                let marker = crate::agent_loop::compact::COMPACTED_PREFIX;
                tx.execute(
                    "UPDATE message_archive SET is_derived = 1
                     WHERE archive_id = (SELECT MIN(archive_id) FROM message_archive WHERE checkpoint_id = ?1)
                       AND role = 'user' AND substr(content, 1, ?2) = ?3",
                    params![checkpoint.id, marker.chars().count() as i64, marker],
                )?;
            }
        }

        let insert_row = "INSERT INTO messages
               (session_id, role, content, tool_calls, tool_call_id, acp_message_id, created_at)
             VALUES (?1, 'user', ?2, NULL, NULL, ?3, ?4)";
        tx.execute(
            insert_row,
            params![
                sid,
                format!(
                    "{}{}",
                    crate::agent_loop::compact::COMPACTED_PREFIX,
                    input.summary
                ),
                checkpoint.summary_message_id,
                now
            ],
        )?;
        if let (Some(todo), Some(todo_mid)) = (input.todo, checkpoint.todo_message_id.as_deref()) {
            tx.execute(insert_row, params![sid, todo, todo_mid, now])?;
        }
        tx.execute(
            "INSERT INTO messages
               (session_id, role, content, tool_calls, tool_call_id, acp_message_id, attachments, created_at)
             SELECT session_id, role, content, tool_calls, tool_call_id, acp_message_id, attachments, created_at
             FROM messages WHERE session_id = ?1 AND id > ?2 AND id <= ?3 ORDER BY id ASC",
            params![sid, last_covered, max_old],
        )?;
        tx.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND id <= ?2",
            params![sid, max_old],
        )?;
        tx.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now, sid],
        )?;
        tx.commit()?;
        Ok(checkpoint)
    }

    /// Compaction checkpoints of a session, oldest first.
    pub fn compaction_checkpoints(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<CompactionCheckpoint>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, ordinal, parent_id, trigger_kind, summary, todo,
                    summary_message_id, todo_message_id, covered_count, tail_count,
                    source_bytes, source_truncated, created_at
             FROM compaction_checkpoints WHERE session_id = ?1 ORDER BY ordinal ASC",
        )?;
        let rows = stmt.query_map(params![session_id.as_str()], |row| {
            Ok(CompactionCheckpoint {
                id: row.get(0)?,
                session_id: row.get(1)?,
                ordinal: row.get(2)?,
                parent_id: row.get(3)?,
                trigger: row.get(4)?,
                summary: row.get(5)?,
                todo: row.get(6)?,
                summary_message_id: row.get(7)?,
                todo_message_id: row.get(8)?,
                covered_count: row.get(9)?,
                tail_count: row.get(10)?,
                source_bytes: row.get(11)?,
                source_truncated: row.get::<_, i64>(12)? != 0,
                created_at: row.get(13)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Destructive: drops every existing row (the prefix is lost for good).
    /// Context compaction uses [`Store::compact_messages`] instead, which keeps
    /// the originals in `message_archive`.
    pub fn replace_messages(&self, session_id: &SessionId, messages: &[ChatMessage]) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            params![session_id.as_str()],
        )?;
        for msg in messages {
            match msg {
                ChatMessage::User { content, .. } => {
                    let mid = MessageId::new();
                    tx.execute(
                        "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, acp_message_id, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            session_id.as_str(),
                            "user",
                            content,
                            None::<&str>,
                            None::<&str>,
                            Some(mid.as_str()),
                            now.as_str()
                        ],
                    )?;
                }
                ChatMessage::Assistant {
                    content,
                    tool_calls,
                } => {
                    let tool_json = if tool_calls.is_empty() {
                        None
                    } else {
                        Some(serde_json::to_string(&openai_tool_calls(tool_calls))?)
                    };
                    let mid = MessageId::new();
                    let body = content.clone().unwrap_or_default();
                    tx.execute(
                        "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, acp_message_id, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            session_id.as_str(),
                            "assistant",
                            body,
                            tool_json.as_deref(),
                            None::<&str>,
                            Some(mid.as_str()),
                            now.as_str()
                        ],
                    )?;
                }
                ChatMessage::Tool {
                    tool_call_id,
                    content,
                } => {
                    tx.execute(
                        "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, acp_message_id, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            session_id.as_str(),
                            "tool",
                            content,
                            None::<&str>,
                            Some(tool_call_id.as_str()),
                            None::<&str>,
                            now.as_str()
                        ],
                    )?;
                }
            }
        }
        tx.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now.as_str(), session_id.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn bindings(&self, session_id: &SessionId, kind: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT resource_id FROM task_bindings
             WHERE session_id = ?1 AND kind = ?2 AND enabled = 1
             ORDER BY resource_id",
        )?;
        let rows = stmt.query_map(params![session_id.as_str(), kind], |row| row.get(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn all_bindings(&self, session_id: &SessionId) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT kind, resource_id FROM task_bindings
             WHERE session_id = ?1 AND enabled = 1
             ORDER BY kind, resource_id",
        )?;
        let rows = stmt.query_map(params![session_id.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn upsert_bindings(
        &self,
        session_id: &SessionId,
        kind: &str,
        ids: &[String],
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        for id in ids {
            conn.execute(
                "INSERT INTO task_bindings (session_id, kind, resource_id, enabled)
                 VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(session_id, kind, resource_id) DO UPDATE SET enabled = 1",
                params![session_id.as_str(), kind, id],
            )?;
        }
        Ok(())
    }

    /// Replace all bindings of `kind` for this session. Empty `ids` unbinds.
    /// Unlike [`Self::upsert_bindings`], this is not additive.
    pub fn replace_bindings(
        &self,
        session_id: &SessionId,
        kind: &str,
        ids: &[String],
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "DELETE FROM task_bindings WHERE session_id = ?1 AND kind = ?2",
            params![session_id.as_str(), kind],
        )?;
        for id in ids {
            conn.execute(
                "INSERT INTO task_bindings (session_id, kind, resource_id, enabled)
                 VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(session_id, kind, resource_id) DO UPDATE SET enabled = 1",
                params![session_id.as_str(), kind, id],
            )?;
        }
        Ok(())
    }

    pub fn cron_insert(&self, job: &CronJobRow) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "INSERT INTO cron_jobs
             (id, expr, prompt, workspace, model_ref, enabled, last_run, next_run, missed_policy, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                job.id,
                job.expr,
                job.prompt,
                job.workspace,
                job.model_ref,
                if job.enabled { 1 } else { 0 },
                job.last_run,
                job.next_run,
                job.missed_policy,
                job.created_at
            ],
        )?;
        Ok(())
    }

    pub fn cron_list(&self) -> Result<Vec<CronJobRow>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT id, expr, prompt, workspace, model_ref, enabled, last_run, next_run, missed_policy, created_at
             FROM cron_jobs ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], map_cron_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn cron_get(&self, id: &str) -> Result<Option<CronJobRow>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let row = conn
            .query_row(
                "SELECT id, expr, prompt, workspace, model_ref, enabled, last_run, next_run, missed_policy, created_at
                 FROM cron_jobs WHERE id = ?1",
                params![id],
                map_cron_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn cron_set_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let n = conn.execute(
            "UPDATE cron_jobs SET enabled = ?1 WHERE id = ?2",
            params![if enabled { 1 } else { 0 }, id],
        )?;
        Ok(n > 0)
    }

    pub fn cron_remove(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let n = conn.execute("DELETE FROM cron_jobs WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    pub fn cron_mark_run(&self, id: &str, last_run: &str, next_run: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "UPDATE cron_jobs SET last_run = ?1, next_run = ?2 WHERE id = ?3",
            params![last_run, next_run, id],
        )?;
        Ok(())
    }

    /// Atomically claim only the jobs that are still due. Multiple Runtime
    /// instances may share this SQLite file (for example CLI and Desktop), so
    /// reading `cron_due` alone must never grant a job to more than one host.
    pub fn cron_claim_due(&self, now: &str, next_runs: &[(String, String)]) -> Result<Vec<String>> {
        let mut conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let tx = conn.transaction()?;
        let mut claimed = Vec::new();
        for (id, next_run) in next_runs {
            let changed = tx.execute(
                "UPDATE cron_jobs
                 SET last_run = ?1, next_run = ?2
                 WHERE id = ?3 AND enabled = 1 AND next_run <= ?1",
                params![now, next_run, id],
            )?;
            if changed == 1 {
                claimed.push(id.clone());
            }
        }
        tx.commit()?;
        Ok(claimed)
    }

    pub fn cron_due(&self, now: &str) -> Result<Vec<CronJobRow>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut stmt = conn.prepare(
            "SELECT id, expr, prompt, workspace, model_ref, enabled, last_run, next_run, missed_policy, created_at
             FROM cron_jobs WHERE enabled = 1 AND next_run <= ?1 ORDER BY next_run ASC",
        )?;
        let rows = stmt.query_map(params![now], map_cron_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn upsert_spec(&self, row: &SpecRow) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute(
            "INSERT INTO specs (id, session_id, status, version, path, mode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               session_id = excluded.session_id,
               status = excluded.status,
               version = excluded.version,
               path = excluded.path,
               mode = excluded.mode",
            params![
                row.id,
                row.session_id,
                row.status,
                row.version,
                row.path,
                row.mode
            ],
        )?;
        Ok(())
    }

    pub fn get_spec(&self, spec_id: &str) -> Result<Option<SpecRow>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let row = conn
            .query_row(
                "SELECT id, session_id, status, version, path, mode FROM specs WHERE id = ?1",
                params![spec_id],
                map_spec_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn get_spec_by_session(&self, session_id: &SessionId) -> Result<Option<SpecRow>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let row = conn
            .query_row(
                "SELECT id, session_id, status, version, path, mode FROM specs WHERE session_id = ?1",
                params![session_id.as_str()],
                map_spec_row,
            )
            .optional()?;
        Ok(row)
    }
}

fn context_content(payload: &serde_json::Value) -> String {
    payload
        .get("content")
        .or_else(|| payload.get("text"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string(payload).unwrap_or_default())
}

fn estimate_tokens(text: &str) -> u32 {
    // A stable, intentionally conservative estimate for byte-less local
    // previews. Provider-specific tokenization is not a security boundary.
    ((text.chars().count() as u32).saturating_add(3) / 4).max(1)
}

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn truncate_to_tokens(text: &str, token_budget: u32) -> String {
    let max_chars = token_budget.saturating_mul(4) as usize;
    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    if end < text.len() {
        format!("{}…", text[..end].trim())
    } else {
        text.into()
    }
}

fn relevance_score(event: &MemoryEvent, query: &str) -> f32 {
    let body = serde_json::to_string(&event.draft.payload)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let hits = lexical_terms(query)
        .iter()
        .filter(|term| body.contains(term.as_str()))
        .count() as f32;
    let authority = match event.draft.authority {
        Authority::UserConfirmed => 0.45,
        Authority::ToolVerified => 0.30,
        Authority::ModelCandidate => 0.0,
    };
    // Time is a mild tiebreaker, never a permission or truth decision. Facts
    // with explicit validity are handled before this score by the active-set
    // filter; this only avoids stale observations dominating equal evidence.
    let age_days = chrono::DateTime::parse_from_rfc3339(&event.observed_at)
        .ok()
        .map(|then| (Utc::now() - then.with_timezone(&Utc)).num_days().max(0) as f32)
        .unwrap_or(365.0);
    let recency = 0.12 / (1.0 + age_days / 30.0);
    event.draft.confidence + hits + authority + recency
}

fn context_cache_key(request: &ContextBuildRequest) -> Result<String> {
    let serialized = serde_json::to_vec(&(
        &request.domain,
        &request.scope,
        request.query.trim(),
        request.max_items,
    ))?;
    let mut hasher = Sha256::new();
    hasher.update(serialized);
    Ok(format!("hybrid:{:x}", hasher.finalize()))
}

fn keyword_score(content: &str, query: &str) -> f32 {
    let terms = lexical_terms(query);
    if terms.is_empty() {
        return 0.0;
    }
    let lower = content.to_ascii_lowercase();
    terms
        .iter()
        .filter(|term| lower.contains(term.as_str()))
        .count() as f32
        / terms.len() as f32
}

/// A local signed-hashing embedding.  It is intentionally deterministic and
/// rebuildable from ledger content; an accelerator-backed embedding projection
/// can replace it later without changing ranking/audit contracts.
fn vector_similarity(left: &str, right: &str) -> f32 {
    if right.trim().is_empty() {
        return 0.0;
    }
    const DIMENSIONS: usize = 48;
    fn embed(text: &str) -> [f32; DIMENSIONS] {
        let mut vector = [0.0; DIMENSIONS];
        for term in lexical_terms(text) {
            let mut hasher = Sha256::new();
            hasher.update(term.as_bytes());
            let digest = hasher.finalize();
            let index = digest[0] as usize % DIMENSIONS;
            vector[index] += if digest[1] & 1 == 0 { 1.0 } else { -1.0 };
        }
        vector
    }
    let left = embed(left);
    let right = embed(right);
    let dot: f32 = left.iter().zip(&right).map(|(a, b)| a * b).sum();
    let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        (dot / (left_norm * right_norm)).max(0.0)
    }
}

fn temporal_score(observed_at: &str) -> f32 {
    let age_days = chrono::DateTime::parse_from_rfc3339(observed_at)
        .ok()
        .map(|then| (Utc::now() - then.with_timezone(&Utc)).num_days().max(0) as f32)
        .unwrap_or(365.0);
    0.18 / (1.0 + age_days / 30.0)
}

fn extract_entities(payload: &serde_json::Value) -> Vec<String> {
    let mut entities = Vec::new();
    if let Some(values) = payload
        .get("entities")
        .and_then(serde_json::Value::as_array)
    {
        for value in values {
            let name = value
                .as_str()
                .or_else(|| value.get("name").and_then(serde_json::Value::as_str));
            if let Some(name) = name {
                entities.push(name.trim().to_owned());
            }
        }
    }
    entities.extend(extract_entities_from_text(&context_content(payload)));
    entities.retain(|entity| !entity.is_empty());
    entities.sort();
    entities.dedup();
    entities.truncate(16);
    entities
}

fn extract_entities_from_text(text: &str) -> Vec<String> {
    let mut entities = Vec::new();
    let mut current = Vec::new();
    for word in text.split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-') {
        if word.chars().next().is_some_and(char::is_uppercase) && word.chars().count() > 1 {
            current.push(word.to_owned());
        } else if !current.is_empty() {
            entities.push(current.join(" "));
            current.clear();
        }
    }
    if !current.is_empty() {
        entities.push(current.join(" "));
    }
    entities.sort();
    entities.dedup();
    entities
}

fn entity_overlap(event_entities: &[String], query_entities: &[String]) -> f32 {
    if event_entities.is_empty() || query_entities.is_empty() {
        return 0.0;
    }
    event_entities
        .iter()
        .filter(|entity| {
            query_entities
                .iter()
                .any(|query| query.eq_ignore_ascii_case(entity))
        })
        .count() as f32
        / query_entities.len() as f32
}

fn entity_id(label: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    format!("entity_{:x}", hasher.finalize())
}

fn scope_label(scope: &MemoryScope) -> &'static str {
    if scope.knowledge_space_id.is_some() {
        "知识空间"
    } else if scope.project_id.is_some() {
        "项目"
    } else if scope.session_id.is_some() {
        "会话"
    } else if scope.user_id.is_some() {
        "用户"
    } else {
        "全局内部"
    }
}

fn compress_context_content(content: &str, query: &str, remaining_tokens: u32) -> String {
    if estimate_tokens(content) <= remaining_tokens {
        return content.to_owned();
    }
    if remaining_tokens < 8 {
        return String::new();
    }
    let terms = lexical_terms(query);
    let mut selected = content
        .split_inclusive(['。', '！', '？', '.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .filter(|sentence| terms.is_empty() || keyword_score(sentence, query) > 0.0)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        selected = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
    }
    let char_budget = (remaining_tokens.saturating_sub(3) as usize).saturating_mul(4);
    let mut out = String::from("摘要：");
    for sentence in selected {
        if out.chars().count() + sentence.chars().count() + 1 > char_budget {
            break;
        }
        if out.len() > "摘要：".len() {
            out.push(' ');
        }
        out.push_str(sentence);
    }
    if out == "摘要：" {
        let end = content
            .char_indices()
            .nth(char_budget)
            .map(|(index, _)| index)
            .unwrap_or(content.len());
        out.push_str(content[..end].trim());
    }
    out
}

/// P1 stays deliberately lexical, but a full natural-language sentence is
/// not itself a useful search token. ASCII words and CJK bigrams make the
/// match deterministic without pretending to be semantic retrieval (P4).
fn lexical_terms(query: &str) -> Vec<String> {
    let lower = query.to_ascii_lowercase();
    let mut terms: Vec<String> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|term| term.len() >= 2)
        .map(str::to_owned)
        .collect();
    let cjk_like: Vec<char> = query
        .chars()
        .filter(|c| !c.is_ascii() && !c.is_whitespace() && !c.is_ascii_punctuation())
        .collect();
    terms.extend(cjk_like.windows(2).map(|pair| pair.iter().collect()));
    terms.sort();
    terms.dedup();
    terms
}

fn lexical_matches(body: &str, query: &str) -> bool {
    let body = body.to_ascii_lowercase();
    lexical_terms(query)
        .iter()
        .any(|term| body.contains(term.as_str()))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OpenAiToolCall {
    id: String,
    #[serde(rename = "type")]
    type_: String,
    function: OpenAiFn,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OpenAiFn {
    name: String,
    arguments: String,
}

fn openai_tool_calls(calls: &[ToolCall]) -> Vec<OpenAiToolCall> {
    calls
        .iter()
        .map(|c| OpenAiToolCall {
            id: c.id.clone(),
            type_: "function".into(),
            function: OpenAiFn {
                name: c.name.clone(),
                arguments: c.arguments.clone(),
            },
        })
        .collect()
}

fn map_session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: row.get(0)?,
        title: row.get(1)?,
        workspace: row.get(2)?,
        parent_id: row.get(3)?,
        kind: row.get(4)?,
        depth: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        folder: row.get(8)?,
        pinned: row.get::<_, i64>(9)? != 0,
        archived: row.get::<_, i64>(10)? != 0,
    })
}

fn map_project_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRow> {
    Ok(ProjectRow {
        id: row.get(0)?,
        name: row.get(1)?,
        workspace: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn map_cron_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CronJobRow> {
    Ok(CronJobRow {
        id: row.get(0)?,
        expr: row.get(1)?,
        prompt: row.get(2)?,
        workspace: row.get(3)?,
        model_ref: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        last_run: row.get(6)?,
        next_run: row.get(7)?,
        missed_policy: row.get(8)?,
        created_at: row.get(9)?,
    })
}

fn map_spec_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SpecRow> {
    Ok(SpecRow {
        id: row.get(0)?,
        session_id: row.get(1)?,
        status: row.get(2)?,
        version: row.get(3)?,
        path: row.get(4)?,
        mode: row.get(5)?,
    })
}

fn like_contains(query: &str) -> String {
    let mut out = String::from("%");
    for ch in query.chars() {
        match ch {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('%');
    out
}

/// 一次性清理历史脏数据：旧版目录选择器把 Windows 的 `\\?\` 扩展长度拼写写进了
/// `sessions.workspace` 列与 prefs JSON 的 workspace 字段，而 `projects.workspace`
/// 一直是剥掉前缀的用户可见拼写。侧栏分组按精确相等匹配，这些对话于是永远回不到
/// 自己的项目名下。开库时统一剥一次。`dunce::simplified` 是纯字符串处理，不碰
/// 文件系统，路径此刻不存在也不会失败；不带该前缀的值原样保留（比对结果相同就
/// 不发 UPDATE），updated_at 也不动 —— 迁移不该把旧会话顶到列表最前。
fn repair_legacy_workspaces(conn: &Connection) -> Result<()> {
    let rows: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare("SELECT id, workspace, prefs FROM sessions")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?;
        rows
    };
    for (id, workspace, prefs) in rows {
        let fixed_workspace = strip_ext_prefix(&workspace);
        // prefs 是一段 JSON 文本；只在里面确实含扩展前缀时才解析重写，避免为
        // 绝大多数本来就干净的行做无谓的 parse + 再序列化。
        let fixed_prefs = if prefs.contains(r"\") && prefs.contains("?\\") {
            strip_ext_prefix_in_prefs(&prefs)
        } else {
            None
        };
        if fixed_workspace.is_none() && fixed_prefs.is_none() {
            continue;
        }
        let ws_sql = fixed_workspace.is_some().then_some("workspace = ?1").unwrap_or("");
        let prefs_sql = fixed_prefs.is_some().then_some("prefs = ?2").unwrap_or("");
        let sets = [ws_sql, prefs_sql]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        match (fixed_workspace, fixed_prefs) {
            (Some(ws), Some(p)) => conn.execute(
                &format!("UPDATE sessions SET {sets} WHERE id = ?3"),
                params![ws, p, id],
            )?,
            (Some(ws), None) => conn.execute(
                "UPDATE sessions SET workspace = ?1 WHERE id = ?2",
                params![ws, id],
            )?,
            (None, Some(p)) => conn.execute(
                "UPDATE sessions SET prefs = ?1 WHERE id = ?2",
                params![p, id],
            )?,
            (None, None) => 0,
        };
    }
    Ok(())
}

/// 剥掉字符串里的 `\\?\` 扩展长度前缀；没有变化时返回 `None`。
fn strip_ext_prefix(raw: &str) -> Option<String> {
    let simplified = dunce::simplified(std::path::Path::new(raw)).to_string_lossy().into_owned();
    (simplified != raw).then_some(simplified)
}

/// prefs JSON 里的 workspace 字段同样可能带前缀（set_session_prefs 修好后写入的
/// 都是干净值，这里只处理存量）。用 `serde_json::Value` 而不是 `SessionPrefs`
/// 原样改写，未知字段不丢；解析失败就跳过，留给下一次正常写入去收敛。
fn strip_ext_prefix_in_prefs(raw: &str) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let changed = match value.get_mut("workspace").and_then(|v| v.as_str()) {
        Some(ws) => match strip_ext_prefix(ws) {
            Some(fixed) => {
                *value.get_mut("workspace").unwrap() = serde_json::Value::String(fixed);
                true
            }
            None => false,
        },
        None => false,
    };
    changed.then(|| serde_json::to_string(&value).ok()).flatten()
}

/// 会话树是归属的单位：根会话换了工作目录，树下挂着的一切（子代理等只读会话）
/// 要跟着走。侧栏分组按根节点的 workspace 渲染、子会话嵌在父行下面，根挪进新
/// 项目而子会话留着旧目录时，会出现「树挂在新项目下、子会话自己的目录却显示
/// 旧项目」的分裂（tab 上的项目标签和它所在的分组各说一套）。子代理出生时就是
/// 拷贝父当时的目录（`Runtime` spawn），漂移只可能来自根后来的移动。
fn propagate_workspace_down(conn: &Connection, root: &str, workspace: &str) -> Result<()> {
    let ids: Vec<String> = {
        let mut stmt = conn.prepare(
            "WITH RECURSIVE subtree(id) AS (
               SELECT ?1
               UNION ALL
               SELECT s.id FROM sessions s JOIN subtree t ON s.parent_id = t.id
             )
             SELECT id FROM subtree WHERE id != ?1",
        )?;
        let ids = stmt
            .query_map(params![root], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        ids
    };
    for id in ids {
        conn.execute(
            "UPDATE sessions SET workspace = ?1 WHERE id = ?2 AND workspace != ?1",
            params![workspace, id],
        )?;
        // prefs JSON 是 prefs 的读面（session_prefs 从这里取），镜像要跟列一致。
        let prefs: String = conn.query_row(
            "SELECT prefs FROM sessions WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        if let Some(fixed) = set_prefs_workspace(&prefs, workspace) {
            conn.execute(
                "UPDATE sessions SET prefs = ?1 WHERE id = ?2",
                params![fixed, id],
            )?;
        }
    }
    Ok(())
}

/// 把 prefs JSON 的 workspace 字段设为 `workspace`（空串＝移除该字段）；
/// 无变化返回 `None`。解析失败或非对象同样返回 `None`（留给正常写入收敛）。
fn set_prefs_workspace(raw: &str, workspace: &str) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = value.as_object_mut()?;
    if workspace.is_empty() {
        obj.remove("workspace")?;
    } else if obj.get("workspace").and_then(|v| v.as_str()) != Some(workspace) {
        obj.insert(
            "workspace".into(),
            serde_json::Value::String(workspace.to_string()),
        );
    } else {
        return None;
    }
    serde_json::to_string(&value).ok()
}

/// 一次性收敛存量：每条根会话的子树按根对齐（幂等，之后每次开库都是空转）。
/// 只动子会话，不动 updated_at。
fn align_conversation_tree_workspaces(conn: &Connection) -> Result<()> {
    let roots: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, workspace FROM sessions WHERE parent_id IS NULL OR parent_id = ''",
        )?;
        let roots = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        roots
    };
    for (id, workspace) in roots {
        propagate_workspace_down(conn, &id, &workspace)?;
    }
    Ok(())
}

fn migrate_sessions(conn: &Connection) -> Result<()> {
    let cols = table_columns(conn, "sessions")?;
    if !cols.iter().any(|c| c == "parent_id") {
        conn.execute("ALTER TABLE sessions ADD COLUMN parent_id TEXT", [])?;
    }
    if !cols.iter().any(|c| c == "kind") {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN kind TEXT NOT NULL DEFAULT 'user'",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "depth") {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN depth INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "folder") {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN folder TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "prefs") {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN prefs TEXT NOT NULL DEFAULT '{}'",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "pinned") {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !cols.iter().any(|c| c == "archived") {
        conn.execute(
            "ALTER TABLE sessions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    // 定时任务的每次执行都开一条独立 session；记下它属于哪个 job，
    // 面板才能列出运行记录并跳回对应会话。旧库里为 NULL。
    if !cols.iter().any(|c| c == "cron_job_id") {
        conn.execute("ALTER TABLE sessions ADD COLUMN cron_job_id TEXT", [])?;
    }
    Ok(())
}

/// Schema version stamped via `PRAGMA user_version`; also mirrored into
/// `dandelion_meta('schema_baseline')` so a copied/merged DB keeps the fact.
const SCHEMA_BASELINE_VERSION: i64 = 1;

/// Phase-1 space baseline: additive columns, the vec virtual table and the
/// version stamp. Everything is probe-guarded, so a second open on a current
/// DB is a true no-op (c22 asserts zero side effects).
fn migrate_space_baseline(conn: &Connection) -> Result<()> {
    let space_cols = table_columns(conn, "knowledge_spaces")?;
    if !space_cols.iter().any(|c| c == "goal") {
        conn.execute("ALTER TABLE knowledge_spaces ADD COLUMN goal TEXT", [])?;
    }
    if !space_cols.iter().any(|c| c == "kind") {
        conn.execute(
            "ALTER TABLE knowledge_spaces ADD COLUMN kind TEXT NOT NULL DEFAULT 'docs'",
            [],
        )?;
    }
    if !space_cols.iter().any(|c| c == "archived_at") {
        conn.execute(
            "ALTER TABLE knowledge_spaces ADD COLUMN archived_at TEXT",
            [],
        )?;
    }
    if !table_columns(conn, "knowledge_documents")?
        .iter()
        .any(|c| c == "parse_report")
    {
        // Honest parse outcomes from source adapters (never a fake "added").
        conn.execute(
            "ALTER TABLE knowledge_documents ADD COLUMN parse_report TEXT",
            [],
        )?;
    }
    // §三 fixed historical versions need supersede and withdraw to be
    // different facts: publishing a newer version marks the old one
    // superseded (pinned spaces keep reading it), only revocation withdraws
    // it (nothing may still cite a revoked version). Pre-existing rows had
    // both merged into revoked_at; they cannot be honestly told apart after
    // the fact, so they stay revoked — fail closed, never a resurrection.
    if !table_columns(conn, "knowledge_document_versions")?
        .iter()
        .any(|c| c == "superseded_at")
    {
        conn.execute(
            "ALTER TABLE knowledge_document_versions ADD COLUMN superseded_at TEXT",
            [],
        )?;
    }
    // §三: an immutable version records WHICH parser produced its stored
    // content, next to its hash. Pre-existing rows were all written by the
    // baseline pipeline (raw passthrough / PDF text), so DEFAULT 1 states
    // the truth for them rather than leaving the fact unknown.
    if !table_columns(conn, "knowledge_document_versions")?
        .iter()
        .any(|c| c == "parser_version")
    {
        conn.execute(
            "ALTER TABLE knowledge_document_versions ADD COLUMN parser_version INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }
    if !table_columns(conn, "task_bindings")?.iter().any(|c| c == "role") {
        // read|write; at most one write space per session (enforced in phase 7).
        conn.execute(
            "ALTER TABLE task_bindings ADD COLUMN role TEXT NOT NULL DEFAULT 'read'",
            [],
        )?;
    }
    // Space association (§三): every pre-existing live document already WAS
    // a member of its home space — stamp one `follow` link per document once
    // (meta-guarded, so a second open on a current DB is a true no-op);
    // every import after this point maintains links inline.
    let stamped: Option<String> = conn
        .query_row(
            "SELECT value FROM dandelion_meta WHERE key='space_links_backfilled'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if stamped.is_none() {
        conn.execute_batch(
            "BEGIN;
             INSERT OR IGNORE INTO knowledge_space_links (space_id,document_id,binding,pinned_version_no,added_by,created_at)
               SELECT space_id,id,'follow',NULL,'migration',created_at FROM knowledge_documents WHERE deleted_at IS NULL;
             INSERT INTO dandelion_meta (key,value) VALUES ('space_links_backfilled','1');
             COMMIT;",
        )?;
    }
    // Corpus vectors land here in phase 5 (embeddings move as-is, no
    // recompute). Same statically-linked sqlite-vec extension corpus.rs
    // uses; Store::open registers it before opening this connection.
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_chunk_vec USING vec0(
             chunk_id TEXT NOT NULL PRIMARY KEY,
             embedding float[64],
             +space_id TEXT,
             +document_id TEXT,
             +ordinal INTEGER
           );",
    )?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < SCHEMA_BASELINE_VERSION {
        conn.pragma_update(None, "user_version", SCHEMA_BASELINE_VERSION)?;
    }
    conn.execute(
        "INSERT OR IGNORE INTO dandelion_meta (key,value) VALUES ('schema_baseline',?1)",
        params![SCHEMA_BASELINE_VERSION.to_string()],
    )?;
    Ok(())
}

/// Derived index only: old databases are rebuilt once, and SQL triggers
/// keep all writers (including the legacy corpus importer) consistent.
fn migrate_knowledge_fts(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "BEGIN;
         CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_chunks_fts USING fts5(
           content, content='knowledge_chunks', content_rowid='rowid', tokenize='trigram'
         );
         CREATE TRIGGER IF NOT EXISTS knowledge_chunks_fts_insert AFTER INSERT ON knowledge_chunks BEGIN
           INSERT INTO knowledge_chunks_fts(rowid,content) VALUES(new.rowid,new.content);
         END;
         CREATE TRIGGER IF NOT EXISTS knowledge_chunks_fts_delete AFTER DELETE ON knowledge_chunks BEGIN
           INSERT INTO knowledge_chunks_fts(knowledge_chunks_fts,rowid,content) VALUES('delete',old.rowid,old.content);
         END;
         CREATE TRIGGER IF NOT EXISTS knowledge_chunks_fts_update AFTER UPDATE OF content ON knowledge_chunks BEGIN
           INSERT INTO knowledge_chunks_fts(knowledge_chunks_fts,rowid,content) VALUES('delete',old.rowid,old.content);
           INSERT INTO knowledge_chunks_fts(rowid,content) VALUES(new.rowid,new.content);
         END;"
    )?;
    let result = (|| -> Result<()> {
        let ready: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM dandelion_meta WHERE key='knowledge_fts_v1')", [], |r| r.get(0))?;
        if !ready {
            conn.execute("INSERT INTO knowledge_chunks_fts(knowledge_chunks_fts) VALUES('rebuild')", [])?;
            conn.execute("INSERT INTO dandelion_meta(key,value) VALUES('knowledge_fts_v1','1')", [])?;
        }
        conn.execute_batch("COMMIT;")?;
        Ok(())
    })();
    if result.is_err() { let _ = conn.execute_batch("ROLLBACK;"); }
    result
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_os::{Authority, MemoryDomain, MemoryEventKind, Sensitivity, SourceRef};
    use crate::trace::RunId;
    use serde_json::json;

    #[test]
    fn failed_external_run_keeps_error_after_legacy_database_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join("sessions.sqlite")).unwrap();
        conn.execute_batch("CREATE TABLE agent_runs (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, kind TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT, status TEXT NOT NULL, stop_reason TEXT, duration_ms INTEGER);").unwrap();
        drop(conn);
        let sid = SessionId::new();
        {
            let store = Store::open(dir.path(), 5000).unwrap();
            store.ensure_session(&sid, dir.path(), "Claude task").unwrap();
            store.append_user(&sid, "hello", &MessageId::new()).unwrap();
            store.start_agent_run(&AgentRunRow {
                id: "run-1".into(), session_id: sid.to_string(), kind: "engine:claude_code".into(),
                started_at: Utc::now().to_rfc3339(), finished_at: None,
                status: "running".into(), stop_reason: None, error_message: None, duration_ms: None,
            }).unwrap();
            store.finish_agent_run("run-1", "failed", Some("error"), 42).unwrap();
            store.set_agent_run_error("run-1", "Claude Code could not connect").unwrap();
        }
        let store = Store::open(dir.path(), 5000).unwrap();
        let runs = store.agent_runs_for_session(sid.as_str(), 1).unwrap();
        assert_eq!(runs[0].error_message.as_deref(), Some("Claude Code could not connect"));
        assert_eq!(serde_json::to_value(&runs[0]).unwrap()["errorMessage"], "Claude Code could not connect");
    }

    #[test]
    fn external_agent_attribution_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let mid = MessageId::new();
        {
            let store = Store::open(dir.path(), 5000).unwrap();
            store.ensure_session(&sid, Path::new(""), "agent task").unwrap();
            store.append_assistant(&sid, "done", &[], &mid).unwrap();
            store.tag_message_engine(&sid, &mid, "pi").unwrap();
        }
        let store = Store::open(dir.path(), 5000).unwrap();
        let page = store.session_export_window(&sid, 0, 10).unwrap();
        assert_eq!(page["messages"][0]["engineId"], "pi");
        assert_eq!(store.session_export(&sid).unwrap()["messages"][0]["engineId"], "pi");
    }

    #[test]
    fn message_images_persist_and_resolve_with_path_guard() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let mid = MessageId::new();
        // 1x1 透明 PNG。
        let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let store = Store::open(dir.path(), 5000).unwrap();
        store.ensure_session(&sid, dir.path(), "vision").unwrap();
        store.append_user(&sid, "look", &mid).unwrap();
        let files = store
            .persist_message_images(&sid, &mid, &[png.to_string(), "not-a-data-url".into()])
            .unwrap();
        assert_eq!(files.len(), 1, "无效条目应被跳过");
        assert_eq!(files[0], format!("attachments/{}/{}-0.png", sid.as_str(), mid.as_str()));
        assert_eq!(store.attachment_data_url(&files[0]).unwrap(), png);
        let page = store.session_export_window(&sid, 0, 10).unwrap();
        assert_eq!(page["messages"][0]["attachments"][0], files[0].as_str());
        // 路径守卫：越界 / 空 / 不存在都必须拒绝。
        assert!(store.attachment_data_url("../secret.png").is_err());
        assert!(store.attachment_data_url("attachments/../x.png").is_err());
        assert!(store.attachment_data_url("").is_err());
        assert!(store.attachment_data_url("attachments/nonexistent.png").is_err());
        // 会话删除时清除落盘图片。
        store.remove_session_attachments(sid.as_str());
        assert!(store.attachment_data_url(&files[0]).is_err());
    }

    #[test]
    fn sqlite_roundtrip_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        {
            let store = Store::open(dir.path(), 5000).unwrap();
            store
                .ensure_session(&sid, dir.path(), "hello world title")
                .unwrap();
            store.append_user(&sid, "hello", &MessageId::new()).unwrap();
            store
                .append_assistant(&sid, "hi", &[], &MessageId::new())
                .unwrap();
        }
        let store = Store::open(dir.path(), 5000).unwrap();
        let msgs = store.load_openai_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            ChatMessage::User { content, .. } => assert_eq!(content, "hello"),
            _ => panic!("expected user"),
        }
        let row = store.get_session(&sid).unwrap().unwrap();
        assert!(row.title.starts_with("hello"));
    }

    #[test]
    fn malformed_tool_arguments_remain_auditable_but_cannot_poison_replay() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let broken = "{\"content\":\"unfinished";
        {
            let store = Store::open(dir.path(), 5000).unwrap();
            store.ensure_session(&sid, dir.path(), "wiki task").unwrap();
            store.append_assistant(
                &sid,
                "",
                &[
                    ToolCall {
                        id: "bad".into(),
                        name: "write_file".into(),
                        arguments: broken.into(),
                    },
                    ToolCall {
                        id: "good".into(),
                        name: "read_file".into(),
                        arguments: "{\"path\":\"README.md\"}".into(),
                    },
                ],
                &MessageId::new(),
            )
            .unwrap();
            store
                .append_tool(&sid, "bad", "Error: invalid arguments; no file was written")
                .unwrap();
            store.append_tool(&sid, "good", "read succeeded").unwrap();
            store
                .append_user(&sid, "continue", &MessageId::new())
                .unwrap();
        }
        let store = Store::open(dir.path(), 5000).unwrap();
        let rows = store.load_rows(&sid).unwrap();
        let raw: Vec<OpenAiToolCall> =
            serde_json::from_str(rows[0].tool_calls.as_deref().unwrap()).unwrap();
        assert_eq!(raw[0].function.arguments, broken, "audit record is unchanged");

        let messages = store.load_openai_messages(&sid).unwrap();
        let ChatMessage::Assistant { tool_calls, .. } = &messages[0] else {
            panic!("assistant call");
        };
        assert_eq!(tool_calls[0].id, "bad");
        assert_eq!(tool_calls[0].arguments, "{}");
        assert_eq!(tool_calls[1].arguments, "{\"path\":\"README.md\"}");
        assert!(matches!(&messages[1], ChatMessage::Tool { tool_call_id, .. } if tool_call_id == "bad"));
        assert!(matches!(&messages[2], ChatMessage::Tool { tool_call_id, .. } if tool_call_id == "good"));
        assert!(matches!(&messages[3], ChatMessage::User { content, .. } if content == "continue"));
    }

    #[test]
    fn context_os_ledger_and_pack_audit_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5000).unwrap();
        let scope = MemoryScope {
            project_id: Some("project-a".into()),
            ..Default::default()
        };
        let event = store
            .append_memory_event(MemoryEventDraft {
                domain: MemoryDomain::Internal,
                scope: scope.clone(),
                kind: MemoryEventKind::Decision,
                payload: json!({"decision":"use sqlite ledger"}),
                source_ref: SourceRef {
                    kind: "user_confirmation".into(),
                    id: "msg-1".into(),
                    version: None,
                },
                authority: Authority::UserConfirmed,
                valid_from: None,
                valid_to: None,
                confidence: 1.0,
                sensitivity: Sensitivity::Internal,
                revision_of: None,
            })
            .unwrap();
        assert_eq!(
            store.memory_events(MemoryDomain::Internal, &scope).unwrap()[0].id,
            event.id
        );
        let pack = ContextPack::new("session-a", 100);
        store.record_context_pack(&pack).unwrap();
        assert_eq!(store.context_packs("session-a", 10).unwrap()[0].id, pack.id);
    }

    #[test]
    fn replace_messages_rewrites_session() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        store.ensure_session(&sid, dir.path(), "t").unwrap();
        store.append_user(&sid, "old", &MessageId::new()).unwrap();
        store
            .append_assistant(&sid, "gone", &[], &MessageId::new())
            .unwrap();
        store
            .replace_messages(
                &sid,
                &[
                    ChatMessage::User {
                        content: "[compacted history]\nbrief".into(),
                        images: Vec::new(),
                    },
                    ChatMessage::User {
                        content: "now".into(),
                        images: Vec::new(),
                    },
                ],
            )
            .unwrap();
        let msgs = store.load_openai_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            ChatMessage::User { content, .. } => assert!(content.contains("brief")),
            _ => panic!("expected compact user"),
        }
        match &msgs[1] {
            ChatMessage::User { content, .. } => assert_eq!(content, "now"),
            _ => panic!("expected tail user"),
        }
    }

    /// u1, a1(+call c1), tool c1, a2, u2, a3 — the first four are the usual prefix.
    fn seed_compactable(store: &Store, sid: &SessionId, dir: &Path) -> Vec<MessageId> {
        store.ensure_session(sid, dir, "t").unwrap();
        let ids: Vec<MessageId> = (0..4).map(|_| MessageId::new()).collect();
        store.append_user(sid, "u1", &ids[0]).unwrap();
        store
            .append_assistant(
                sid,
                "a1",
                &[ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.rs"}"#.into(),
                }],
                &ids[1],
            )
            .unwrap();
        store.append_tool(sid, "c1", "file body").unwrap();
        store.append_assistant(sid, "a2", &[], &ids[2]).unwrap();
        store.append_user(sid, "u2", &ids[3]).unwrap();
        store
            .append_assistant(sid, "a3", &[], &MessageId::new())
            .unwrap();
        ids
    }

    fn compact_at(store: &Store, sid: &SessionId, covered: usize, summary: &str) -> Result<CompactionCheckpoint> {
        let (_, ids) = store.load_openai_messages_with_ids(sid).unwrap();
        store.compact_messages(&CompactionInput {
            session_id: sid,
            covered_ids: &ids[..covered],
            tail_ids: &ids[covered..],
            summary,
            todo: None,
            trigger: CompactTrigger::Manual,
            source_bytes: 10,
            source_truncated: false,
        })
    }

    fn count(store: &Store, table: &str, sid: &SessionId) -> i64 {
        let conn = store.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
            params![sid.as_str()],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn raw_rows(store: &Store, sid: &SessionId) -> Vec<(String, String, Option<String>, Option<String>, String)> {
        let conn = store.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT role, content, tool_calls, acp_message_id, created_at FROM messages WHERE session_id = ?1 ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map(params![sid.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    fn history_pairs(store: &Store, sid: &SessionId) -> Vec<(String, String)> {
        let page = store.session_history_window(sid, 0, 200).unwrap();
        page["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["source"] != "checkpoint")
            .map(|m| {
                (
                    m["role"].as_str().unwrap().to_string(),
                    m["content"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn compact_keeps_full_history_and_tail_rows_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        let mids = seed_compactable(&store, &sid, dir.path());
        let before_pairs: Vec<(String, String)> = store
            .load_rows(&sid)
            .unwrap()
            .into_iter()
            .map(|r| (r.role, r.content))
            .collect();
        let before = raw_rows(&store, &sid);
        // The engine tag sits on a tail row (u2 = mids[3]).
        store.tag_message_engine(&sid, &mids[3], "claude").unwrap();
        std::thread::sleep(Duration::from_millis(5));

        let cp = compact_at(&store, &sid, 4, "brief").unwrap();
        assert_eq!(cp.ordinal, 1);
        assert_eq!(cp.parent_id, None);
        assert_eq!(cp.trigger, "manual");
        assert_eq!((cp.covered_count, cp.tail_count), (4, 2));

        // History = every original row, in order, with derived rows hidden.
        let after_pairs = history_pairs(&store, &sid);
        let mut expected = before_pairs[..4].to_vec();
        expected.extend(before_pairs[4..].iter().cloned());
        assert_eq!(after_pairs, expected);

        // Current context = summary + the two tail rows, tail byte-for-byte.
        let after = raw_rows(&store, &sid);
        assert_eq!(after.len(), 3);
        assert_eq!(after[0].1, "[compacted history]\nbrief");
        assert_eq!(after[0].3.as_deref(), Some(cp.summary_message_id.as_str()));
        assert_eq!(after[1..], before[4..]);
        // Archived prefix keeps its original ids and timestamps too.
        let archived: Vec<(Option<String>, String)> = {
            let conn = store.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT acp_message_id, created_at FROM message_archive WHERE session_id = ?1 ORDER BY archive_id")
                .unwrap();
            let rows = stmt
                .query_map(params![sid.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        let expected_archive: Vec<_> = before[..4].iter().map(|b| (b.3.clone(), b.4.clone())).collect();
        assert_eq!(archived, expected_archive);
        // The engine tag still resolves through the preserved acp_message_id.
        let rows = store.load_rows(&sid).unwrap();
        assert_eq!(rows[1].engine_id.as_deref(), Some("claude"));
    }

    #[test]
    fn compact_refuses_to_split_a_tool_pair_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        seed_compactable(&store, &sid, dir.path());
        let before = raw_rows(&store, &sid);
        // Tail would start at the tool result (index 2).
        let err = compact_at(&store, &sid, 2, "brief").unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
        assert_eq!(raw_rows(&store, &sid), before);
        assert_eq!(count(&store, "message_archive", &sid), 0);
        assert_eq!(count(&store, "compaction_checkpoints", &sid), 0);
    }

    #[test]
    fn compact_rejects_a_session_that_changed_after_loading() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        seed_compactable(&store, &sid, dir.path());
        let (_, ids) = store.load_openai_messages_with_ids(&sid).unwrap();
        store.append_user(&sid, "late", &MessageId::new()).unwrap();
        let before = raw_rows(&store, &sid);
        let err = store
            .compact_messages(&CompactionInput {
                session_id: &sid,
                covered_ids: &ids[..4],
                tail_ids: &ids[4..],
                summary: "brief",
                todo: None,
                trigger: CompactTrigger::Overflow,
                source_bytes: 1,
                source_truncated: false,
            })
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
        assert_eq!(raw_rows(&store, &sid), before);
        assert_eq!(count(&store, "compaction_checkpoints", &sid), 0);
    }

    #[test]
    fn compact_failure_rolls_back_every_table() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        seed_compactable(&store, &sid, dir.path());
        let before = raw_rows(&store, &sid);
        store
            .conn
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER boom BEFORE INSERT ON messages
                 WHEN instr(NEW.content, '__boom__') > 0
                 BEGIN SELECT RAISE(ABORT, 'boom'); END;",
            )
            .unwrap();
        // Fails on the summary insert, after the checkpoint and archive rows.
        assert!(compact_at(&store, &sid, 4, "__boom__").is_err());
        assert_eq!(raw_rows(&store, &sid), before);
        assert_eq!(count(&store, "message_archive", &sid), 0);
        assert_eq!(count(&store, "compaction_checkpoints", &sid), 0);
    }

    #[test]
    fn repeated_compaction_chains_checkpoints_and_hides_derived_rows() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        seed_compactable(&store, &sid, dir.path());
        let originals: Vec<(String, String)> = store
            .load_rows(&sid)
            .unwrap()
            .into_iter()
            .map(|r| (r.role, r.content))
            .collect();
        let first = compact_at(&store, &sid, 4, "one").unwrap();
        store.append_user(&sid, "u3", &MessageId::new()).unwrap();
        store
            .append_assistant(&sid, "a4", &[], &MessageId::new())
            .unwrap();
        store.append_user(&sid, "u4", &MessageId::new()).unwrap();
        store
            .append_assistant(&sid, "a5", &[], &MessageId::new())
            .unwrap();
        // Current: summary, u2, a3, u3, a4 | u4, a5
        let second = compact_at(&store, &sid, 5, "two").unwrap();
        assert_eq!(second.ordinal, 2);
        assert_eq!(second.parent_id.as_deref(), Some(first.id.as_str()));
        let cps = store.compaction_checkpoints(&sid).unwrap();
        assert_eq!(cps.len(), 2);

        let derived: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM message_archive WHERE session_id = ?1 AND is_derived = 1",
                params![sid.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(derived, 1);

        let mut expected = originals;
        for (role, text) in [("user", "u3"), ("assistant", "a4"), ("user", "u4"), ("assistant", "a5")] {
            expected.push((role.into(), text.into()));
        }
        assert_eq!(history_pairs(&store, &sid), expected);
        let page = store.session_history_window(&sid, 0, 200).unwrap();
        let tagged: Vec<_> = page["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["source"] == "checkpoint")
            .collect();
        assert_eq!(tagged.len(), 1);
        assert_eq!(tagged[0]["content"], "[compacted history]\ntwo");
        assert_eq!(tagged[0]["checkpointId"], second.id.as_str());
    }

    #[test]
    fn legacy_summary_is_derived_only_when_it_is_the_first_row() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        store.ensure_session(&sid, dir.path(), "t").unwrap();
        store
            .append_user(&sid, "[compacted history]\nold brief", &MessageId::new())
            .unwrap();
        store
            .append_user(&sid, "[compacted history]\ntyped by a user", &MessageId::new())
            .unwrap();
        store
            .append_assistant(&sid, "a", &[], &MessageId::new())
            .unwrap();
        store.append_user(&sid, "now", &MessageId::new()).unwrap();
        compact_at(&store, &sid, 3, "new").unwrap();
        let derived: Vec<String> = {
            let conn = store.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT content FROM message_archive WHERE session_id = ?1 AND is_derived = 1")
                .unwrap();
            let rows = stmt.query_map(params![sid.as_str()], |r| r.get(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert_eq!(derived, vec!["[compacted history]\nold brief".to_string()]);
    }

    #[test]
    fn deleting_a_session_removes_archive_and_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        seed_compactable(&store, &sid, dir.path());
        compact_at(&store, &sid, 4, "brief").unwrap();
        assert!(count(&store, "message_archive", &sid) > 0);
        store.delete_session_tree(&sid).unwrap();
        assert_eq!(count(&store, "message_archive", &sid), 0);
        assert_eq!(count(&store, "compaction_checkpoints", &sid), 0);
        assert_eq!(count(&store, "messages", &sid), 0);
    }

    #[test]
    fn reopening_a_database_without_the_new_tables_creates_them() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path(), 5000).unwrap();
            let conn = store.conn.lock().unwrap();
            conn.execute_batch("DROP TABLE message_archive; DROP TABLE compaction_checkpoints;")
                .unwrap();
        }
        let store = Store::open(dir.path(), 5000).unwrap();
        let conn = store.conn.lock().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('message_archive','compaction_checkpoints')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn task_bindings_upsert_and_inherit() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        store.ensure_session(&sid, dir.path(), "t").unwrap();
        store
            .upsert_bindings(&sid, "skill", &["demo-skill".into(), "other".into()])
            .unwrap();
        store
            .upsert_bindings(&sid, "mcp", &["echo".into()])
            .unwrap();
        assert_eq!(
            store.bindings(&sid, "skill").unwrap(),
            vec!["demo-skill".to_string(), "other".to_string()]
        );
        assert_eq!(
            store.bindings(&sid, "mcp").unwrap(),
            vec!["echo".to_string()]
        );
        store
            .upsert_bindings(&sid, "skill", &["demo-skill".into()])
            .unwrap();
        assert_eq!(
            store.bindings(&sid, "skill").unwrap(),
            vec!["demo-skill".to_string(), "other".to_string()]
        );
    }

    #[test]
    fn bindings_empty_without_rows() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        assert!(store.bindings(&sid, "skill").unwrap().is_empty());
        store.ensure_session(&sid, dir.path(), "t").unwrap();
        assert!(store.bindings(&sid, "mcp").unwrap().is_empty());
    }

    #[test]
    fn list_search_export_and_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let a = SessionId::new();
        let b = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        store.ensure_session(&a, dir.path(), "alpha").unwrap();
        store
            .append_user(&a, "hello unique-needle", &MessageId::new())
            .unwrap();
        store.ensure_session(&b, dir.path(), "beta").unwrap();
        store
            .append_user(&b, "other text", &MessageId::new())
            .unwrap();
        store
            .set_session_lineage(&b, Some(a.as_str()), "subagent", 1)
            .unwrap();
        let listed = store.sessions_list(50, 0).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, b.as_str(), "newer first");
        assert_eq!(listed[0].kind, "subagent");
        assert_eq!(listed[0].depth, 1);
        let hits = store.sessions_search("unique-needle", None, 50).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, a.as_str());
        let export = store.session_export(&a).unwrap();
        assert!(export.get("apiKey").is_none());
        assert_eq!(export["id"], a.as_str());
        let dump = export.to_string();
        assert!(!dump.contains("apiKey"));
        assert!(dump.contains("unique-needle"));
    }

    #[test]
    fn session_export_window_returns_newest_page_in_chronological_order() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        store.ensure_session(&sid, dir.path(), "paged").unwrap();
        for content in ["one", "two", "three", "four", "five"] {
            store.append_user(&sid, content, &MessageId::new()).unwrap();
        }

        let newest = store.session_export_window(&sid, 0, 2).unwrap();
        assert_eq!(newest["totalMessages"], 5);
        assert_eq!(newest["messages"][0]["content"], "four");
        assert_eq!(newest["messages"][1]["content"], "five");

        let older = store.session_export_window(&sid, 2, 2).unwrap();
        assert_eq!(older["messages"][0]["content"], "two");
        assert_eq!(older["messages"][1]["content"], "three");
    }

    #[test]
    fn delete_session_tree_removes_children() {
        let dir = tempfile::tempdir().unwrap();
        let parent = SessionId::new();
        let child = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        store.ensure_session(&parent, dir.path(), "p").unwrap();
        store.ensure_session(&child, dir.path(), "c").unwrap();
        store
            .set_session_lineage(&child, Some(parent.as_str()), "subagent", 1)
            .unwrap();
        store.delete_session_tree(&parent).unwrap();
        assert!(store.get_session(&parent).unwrap().is_none());
        assert!(store.get_session(&child).unwrap().is_none());
    }

    #[test]
    fn search_escapes_like_wildcards() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        store.ensure_session(&sid, dir.path(), "t").unwrap();
        store
            .append_user(&sid, "literal 100% done", &MessageId::new())
            .unwrap();
        let hits = store.sessions_search("100%", None, 50).unwrap();
        assert_eq!(hits.len(), 1);
        let miss = store.sessions_search("100_", None, 50).unwrap();
        assert!(miss.is_empty());
    }

    #[test]
    fn cron_runs_list_tagged_sessions_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5000).unwrap();

        // Two runs of one job, plus an untagged user session that must not leak in.
        let first = SessionId::new();
        store.ensure_session(&first, dir.path(), "run 1").unwrap();
        store.set_session_cron_job(&first, "cron_a").unwrap();
        store
            .append_assistant(&first, "第一次跑完了", &[], &MessageId::new())
            .unwrap();

        let second = SessionId::new();
        store.ensure_session(&second, dir.path(), "run 2").unwrap();
        store.set_session_cron_job(&second, "cron_a").unwrap();

        let other = SessionId::new();
        store
            .ensure_session(&other, dir.path(), "手工会话")
            .unwrap();

        let runs = store.cron_runs("cron_a", 20).unwrap();
        assert_eq!(runs.len(), 2, "only this job's runs");
        assert!(runs.iter().all(|r| r.session_id != other.to_string()));
        // Newest first; the reply-less run is not marked finished.
        let by_id = |id: &SessionId| {
            runs.iter()
                .find(|r| r.session_id == id.to_string())
                .expect("run present")
        };
        assert!(by_id(&first).finished);
        assert_eq!(by_id(&first).summary, "第一次跑完了");
        assert!(!by_id(&second).finished);
        assert!(by_id(&second).summary.is_empty());
        assert!(store.cron_runs("cron_missing", 20).unwrap().is_empty());
    }

    #[test]
    fn todos_roundtrip_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        store.ensure_session(&sid, dir.path(), "t").unwrap();
        store
            .save_todos(
                &sid,
                &[crate::store::TodoItem {
                    id: "1".into(),
                    content: "A".into(),
                    status: "pending".into(),
                    parent: None,
                }],
                3,
            )
            .unwrap();
        let items = store.list_todos(&sid).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].content, "A");
        assert_eq!(store.todo_revision(&sid).unwrap(), 3);
        store.delete_session_tree(&sid).unwrap();
        assert!(store.list_todos(&sid).unwrap().is_empty());
    }

    #[test]
    fn cron_jobs_crud_and_due() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5000).unwrap();
        let job = crate::cron::new_job("30m", "hello cron", ".", None, "skip").unwrap();
        store.cron_insert(&job).unwrap();
        assert_eq!(store.cron_list().unwrap().len(), 1);
        assert_eq!(
            store.cron_get(&job.id).unwrap().unwrap().prompt,
            "hello cron"
        );
        store
            .cron_mark_run(
                &job.id,
                "1999-01-01T00:00:00+00:00",
                "2000-01-01T00:00:00+00:00",
            )
            .unwrap();
        let due = store.cron_due("2001-01-01T00:00:00+00:00").unwrap();
        assert_eq!(due.len(), 1);
        store.cron_set_enabled(&job.id, false).unwrap();
        assert!(store
            .cron_due("2001-01-01T00:00:00+00:00")
            .unwrap()
            .is_empty());
        assert!(store.cron_remove(&job.id).unwrap());
        assert!(store.cron_list().unwrap().is_empty());
    }

    #[test]
    fn cron_claim_due_is_exclusive_across_store_instances() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path(), 1_000).unwrap();
        let second = Store::open(dir.path(), 1_000).unwrap();
        let job = crate::cron::new_job("1h", "ping", ".", None, "skip").unwrap();
        first.cron_insert(&job).unwrap();
        first
            .cron_mark_run(&job.id, "1999-01-01T00:00:00Z", "2000-01-01T00:00:00Z")
            .unwrap();
        let claim = vec![(job.id.clone(), "2000-01-01T01:00:00Z".to_string())];
        assert_eq!(
            first
                .cron_claim_due("2000-01-01T00:30:00Z", &claim)
                .unwrap(),
            vec![job.id.clone()]
        );
        assert!(second
            .cron_claim_due("2000-01-01T00:30:00Z", &claim)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn folder_prefs_title_and_replace_corpus_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        store.ensure_session(&sid, dir.path(), "orig").unwrap();
        assert_eq!(store.get_session(&sid).unwrap().unwrap().folder, "");
        assert!(store.set_session_title(&sid, "renamed-task").unwrap());
        assert!(store.set_session_folder(&sid, "inbox").unwrap());
        let row = store.get_session(&sid).unwrap().unwrap();
        assert_eq!(row.title, "renamed-task");
        assert_eq!(row.folder, "inbox");
        let prefs = SessionPrefs {
            temperature: Some(0.5),
            system_prompt: Some("  keep this  ".into()),
            ..SessionPrefs::default()
        };
        assert!(store.set_session_prefs(&sid, &prefs).unwrap());
        let got = store.session_prefs(&sid).unwrap();
        assert_eq!(got.temperature, Some(0.5));
        assert_eq!(got.system_prompt.as_deref(), Some("keep this"));
        store
            .replace_bindings(&sid, "corpus", &["doc-a".into(), "doc-b".into()])
            .unwrap();
        assert_eq!(
            store.bindings(&sid, "corpus").unwrap(),
            vec!["doc-a".to_string(), "doc-b".to_string()]
        );
        store.replace_bindings(&sid, "corpus", &[]).unwrap();
        assert!(store.bindings(&sid, "corpus").unwrap().is_empty());
        let export = store.session_export(&sid).unwrap();
        assert_eq!(export["folder"], "inbox");
        assert_eq!(export["prefs"]["temperature"], 0.5);
    }

    #[test]
    fn agent_engine_can_switch_back_to_native() {
        let mut prefs = SessionPrefs { agent_engine: Some("pi".into()), ..SessionPrefs::default() };
        let patch = SessionPrefs { agent_engine: Some("native".into()), ..SessionPrefs::default() };
        prefs.merge_from(&patch.normalize());
        assert_eq!(prefs.normalize().agent_engine.as_deref(), Some("native"));
    }

    #[test]
    fn clearing_workspace_pref_empties_the_column_too() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        let home = tempfile::tempdir().unwrap();
        store.ensure_session(&sid, home.path(), "t").unwrap();

        // 设：列和 prefs 一起落
        let workspace = SessionPrefs {
            workspace: Some(dir.path().to_path_buf()),
            ..SessionPrefs::default()
        };
        assert!(store.set_session_prefs(&sid, &workspace).unwrap());
        let row = store.get_session(&sid).unwrap().unwrap();
        assert_eq!(row.workspace, dir.path().to_string_lossy());
        assert_eq!(
            store.session_prefs(&sid).unwrap().workspace,
            Some(dir.path().to_path_buf())
        );

        // 清：prefs 里是一个空路径（界面清工作区就是发这个），列必须跟着空 —— 只看
        // prefs 的话这里会「清干净了」，但列留着旧路径，侧栏分组和 chip 都还按旧值
        // 渲染，用户看到的就是「点了 × 没反应」。
        let cleared = SessionPrefs {
            workspace: Some(std::path::PathBuf::from("")),
            ..SessionPrefs::default()
        };
        assert!(store.set_session_prefs(&sid, &cleared).unwrap());
        assert_eq!(store.get_session(&sid).unwrap().unwrap().workspace, "");
        assert_eq!(store.session_prefs(&sid).unwrap().workspace, None);

        // 没带 workspace 的写入（改模型之类）不该顺手把工作目录清掉。
        let untouched = SessionPrefs {
            model: Some("other-model".into()),
            ..SessionPrefs::default()
        };
        assert!(store.set_session_prefs(&sid, &workspace).unwrap());
        assert!(store.set_session_prefs(&sid, &untouched).unwrap());
        assert_eq!(
            store.get_session(&sid).unwrap().unwrap().workspace,
            dir.path().to_string_lossy()
        );
    }

    /// Windows 的目录选择器曾把 `\\?\C:\…` 扩展长度拼写原样写进 `sessions.workspace`
    /// 列和 prefs JSON，而项目登记的是剥掉前缀的用户可见拼写。侧栏分组是精确字符串
    /// 比较，这些存量对话于是永远回不到自己的项目。开库时统一剥一次。
    #[cfg(windows)]
    #[test]
    fn opening_the_store_repairs_prefixed_legacy_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let dirty = format!(r"\\?\{}", target.path().display());
        let sid = SessionId::new();
        {
            let store = Store::open(dir.path(), 5000).unwrap();
            store
                .ensure_session(&sid, std::path::Path::new(""), "t")
                .unwrap();
            let prefs = serde_json::json!({ "workspace": dirty }).to_string();
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE sessions SET workspace = ?1, prefs = ?2 WHERE id = ?3",
                params![dirty, prefs, sid.as_str()],
            )
            .unwrap();
        }
        let store = Store::open(dir.path(), 5000).unwrap();
        let row = store.get_session(&sid).unwrap().unwrap();
        assert!(!row.workspace.starts_with(r"\\?\"), "列仍带前缀: {}", row.workspace);
        assert_eq!(row.workspace, target.path().to_string_lossy());
        assert_eq!(
            store.session_prefs(&sid).unwrap().workspace,
            Some(target.path().to_path_buf())
        );
    }

    /// 新写入的目录与 `project_create` 登记的是同一口径的拼写 —— 侧栏分组全靠
    /// 这两个字符串 `===` 相等，任何一侧漏收敛都会让对话掉出项目。
    #[test]
    fn session_workspace_spelling_matches_project_registration() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5000).unwrap();
        let sid = SessionId::new();
        let home = tempfile::tempdir().unwrap();
        store.ensure_session(&sid, home.path(), "t").unwrap();
        let target = tempfile::tempdir().unwrap();
        // Windows 历史拼写（扩展长度前缀）从写入路径进来；Unix 上退化为普通路径。
        #[cfg(windows)]
        let chosen = std::path::PathBuf::from(format!(r"\\?\{}", target.path().display()));
        #[cfg(not(windows))]
        let chosen = target.path().to_path_buf();
        let prefs = SessionPrefs {
            workspace: Some(chosen),
            ..SessionPrefs::default()
        };
        assert!(store.set_session_prefs(&sid, &prefs).unwrap());
        let row = store.get_session(&sid).unwrap().unwrap();
        assert!(!row.workspace.contains(r"\\?\"), "列不该带扩展前缀: {}", row.workspace);
        let project = store.project_create(target.path()).unwrap();
        assert_eq!(project.workspace, row.workspace);
    }

    /// 会话树是归属的单位：根会话换目录，树下挂的子代理只读会话必须跟着走。
    /// 否则整个树按根渲染进新项目分组，子会话自己的目录却留着旧项目 ——
    /// tab 上的项目标签和它所在的分组各说一套。
    #[test]
    fn moving_the_root_workspace_moves_its_subagent_children() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5000).unwrap();
        let home = tempfile::tempdir().unwrap();
        let root = SessionId::new();
        let child = SessionId::new();
        store.ensure_session(&root, home.path(), "父任务").unwrap();
        store.ensure_session(&child, home.path(), "子代理").unwrap();
        store
            .set_session_lineage(&child, Some(root.as_str()), "subagent", 1)
            .unwrap();

        let old_ws = tempfile::tempdir().unwrap();
        let new_ws = tempfile::tempdir().unwrap();
        // 出生时子会话拷贝父当时的目录（spawn 语义），再模拟用户后来把根挪走。
        store
            .set_session_prefs(
                &root,
                &SessionPrefs {
                    workspace: Some(old_ws.path().to_path_buf()),
                    ..SessionPrefs::default()
                },
            )
            .unwrap();
        assert_eq!(
            store.get_session(&child).unwrap().unwrap().workspace,
            canonical_workspace(old_ws.path()).to_string_lossy()
        );
        store
            .set_session_prefs(
                &root,
                &SessionPrefs {
                    workspace: Some(new_ws.path().to_path_buf()),
                    ..SessionPrefs::default()
                },
            )
            .unwrap();

        let expected = canonical_workspace(new_ws.path()).to_string_lossy().into_owned();
        assert_eq!(store.get_session(&child).unwrap().unwrap().workspace, expected);
        assert_eq!(
            store.session_prefs(&child).unwrap().workspace,
            Some(canonical_workspace(new_ws.path()))
        );
    }

    /// 存量漂移的收敛：库里已经存在「根在新目录、子还留旧目录」的树，开库时
    /// 按根对齐（这是上一缺陷留下的形状，修复后新数据不会再出现）。
    #[test]
    fn opening_the_store_realigns_child_workspaces_to_their_root() {
        let dir = tempfile::tempdir().unwrap();
        let root_ws = tempfile::tempdir().unwrap();
        let stale_ws = tempfile::tempdir().unwrap();
        let root = SessionId::new();
        let child = SessionId::new();
        {
            let store = Store::open(dir.path(), 5000).unwrap();
            store.ensure_session(&root, root_ws.path(), "父任务").unwrap();
            store.ensure_session(&child, stale_ws.path(), "子代理").unwrap();
            store
                .set_session_lineage(&child, Some(root.as_str()), "subagent", 1)
                .unwrap();
        }
        let store = Store::open(dir.path(), 5000).unwrap();
        // 对齐口径＝跟根落库的值逐字相等（分组靠精确匹配，语义就是"和根一样"）。
        let root_stored = store.get_session(&root).unwrap().unwrap().workspace;
        assert_eq!(
            store.get_session(&child).unwrap().unwrap().workspace,
            root_stored
        );
        assert_eq!(
            store.session_prefs(&child).unwrap().workspace,
            Some(std::path::PathBuf::from(root_stored))
        );
    }

    #[test]
    fn replace_bindings_does_not_change_upsert_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new();
        let store = Store::open(dir.path(), 5000).unwrap();
        store.ensure_session(&sid, dir.path(), "t").unwrap();
        store
            .upsert_bindings(&sid, "skill", &["keep".into(), "drop-me".into()])
            .unwrap();
        store
            .replace_bindings(&sid, "skill", &["keep".into(), "fresh".into()])
            .unwrap();
        assert_eq!(
            store.bindings(&sid, "skill").unwrap(),
            vec!["fresh".to_string(), "keep".to_string()]
        );
    }

    #[test]
    fn context_builder_is_scope_exact_bounded_and_audited() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5_000).unwrap();
        let scope = MemoryScope {
            project_id: Some("project-a".into()),
            ..MemoryScope::default()
        };
        let other_scope = MemoryScope {
            project_id: Some("project-b".into()),
            ..MemoryScope::default()
        };
        let draft = |scope: MemoryScope, content: &str, authority| MemoryEventDraft {
            domain: MemoryDomain::Internal,
            scope,
            kind: MemoryEventKind::Fact,
            payload: json!({"content": content}),
            source_ref: SourceRef {
                kind: "test".into(),
                id: uuid::Uuid::new_v4().to_string(),
                version: None,
            },
            authority,
            valid_from: None,
            valid_to: None,
            confidence: 0.8,
            sensitivity: Sensitivity::Internal,
            revision_of: None,
        };
        store
            .append_memory_event(draft(
                scope.clone(),
                "alpha retained fact",
                Authority::UserConfirmed,
            ))
            .unwrap();
        store
            .append_memory_event(draft(
                scope.clone(),
                "alpha candidate",
                Authority::ModelCandidate,
            ))
            .unwrap();
        store
            .append_memory_event(draft(
                other_scope,
                "alpha other project",
                Authority::UserConfirmed,
            ))
            .unwrap();
        let pack = store
            .build_context_pack(&ContextBuildRequest {
                session_id: "s-preview".into(),
                domain: MemoryDomain::Internal,
                scope,
                query: "alpha".into(),
                token_budget: 100,
                max_items: 12,
            })
            .unwrap();
        assert_eq!(pack.items.len(), 1);
        assert!(pack.items[0].content.contains("retained"));
        assert_eq!(store.context_packs("s-preview", 1).unwrap()[0].id, pack.id);
    }

    #[test]
    fn hybrid_context_rank_explains_caches_invalidates_and_projects_entities() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5_000).unwrap();
        let scope = MemoryScope {
            project_id: Some("project-a".into()),
            ..MemoryScope::default()
        };
        let draft = |content: &str, entities: serde_json::Value| MemoryEventDraft {
            domain: MemoryDomain::Internal,
            scope: scope.clone(),
            kind: MemoryEventKind::Fact,
            payload: json!({"content": content, "entities": entities}),
            source_ref: SourceRef {
                kind: "test".into(),
                id: uuid::Uuid::new_v4().to_string(),
                version: None,
            },
            authority: Authority::UserConfirmed,
            valid_from: None,
            valid_to: None,
            confidence: 0.9,
            sensitivity: Sensitivity::Internal,
            revision_of: None,
        };
        store
            .append_memory_event(draft(
                "Acme Gateway needs a connection reset before retry.",
                json!(["Acme", "Gateway"]),
            ))
            .unwrap();
        store
            .append_memory_event(draft(
                "Acme Support owns the Gateway runbook.",
                json!(["Acme", "Gateway", "Support"]),
            ))
            .unwrap();
        let request = ContextBuildRequest {
            session_id: "s-hybrid".into(),
            domain: MemoryDomain::Internal,
            scope: scope.clone(),
            query: "Acme Gateway retry".into(),
            token_budget: 100,
            max_items: 12,
        };
        let first = store.build_context_pack(&request).unwrap();
        assert!(!first.cache.hit);
        assert!(!first.items.is_empty());
        assert!(!first.items[0].why_selected.is_empty());
        assert!(first.items[0].rank_signals.keyword > 0.0);
        let second = store.build_context_pack(&request).unwrap();
        assert!(second.cache.hit);
        let graph = store
            .entity_graph_projection(MemoryDomain::Internal, &scope)
            .unwrap();
        assert!(graph.nodes.iter().any(|node| node.label == "Acme"));
        assert!(graph.edges.iter().any(|edge| edge.event_count >= 1));
        store
            .append_memory_event(draft(
                "Acme Gateway retry was verified.",
                json!(["Acme", "Gateway"]),
            ))
            .unwrap();
        let third = store.build_context_pack(&request).unwrap();
        assert!(!third.cache.hit);
        assert_eq!(
            third.cache.invalidated_by.as_deref(),
            Some("ledger_changed")
        );
    }

    #[test]
    fn retract_and_confirm_are_scope_bound_and_change_retrieval() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5_000).unwrap();
        let scope = MemoryScope {
            project_id: Some("project-a".into()),
            ..MemoryScope::default()
        };
        let make = |authority| MemoryEventDraft {
            domain: MemoryDomain::Internal,
            scope: scope.clone(),
            kind: MemoryEventKind::Fact,
            payload: json!({"content":"alpha"}),
            source_ref: SourceRef {
                kind: "test".into(),
                id: uuid::Uuid::new_v4().to_string(),
                version: None,
            },
            authority,
            valid_from: None,
            valid_to: None,
            confidence: 1.0,
            sensitivity: Sensitivity::Internal,
            revision_of: None,
        };
        let remembered = store
            .append_memory_event(make(Authority::UserConfirmed))
            .unwrap();
        store
            .retract_memory_event(MemoryDomain::Internal, &scope, &remembered.id, "obsolete")
            .unwrap();
        assert!(store
            .search_memory_events(MemoryDomain::Internal, &scope, "alpha", 10)
            .unwrap()
            .is_empty());
        let candidate = store
            .append_memory_event(make(Authority::ModelCandidate))
            .unwrap();
        let confirmed = store
            .confirm_memory_candidate(MemoryDomain::Internal, &scope, &candidate.id)
            .unwrap();
        let found = store
            .search_memory_events(MemoryDomain::Internal, &scope, "alpha", 10)
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, confirmed.id);
    }

    #[test]
    fn trace_events_are_ordered_and_redacted_before_storage() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5_000).unwrap();
        let run = RunId::new();
        let mut first = TraceEvent::new(
            &run,
            "session-a",
            "tool.started",
            json!({"tool":"terminal", "authorization":"secret"}),
        );
        let mut second = TraceEvent::new(
            &run,
            "session-a",
            "tool.finished",
            json!({"tool":"terminal", "ok":true}),
        );
        store.record_trace_event(&mut first).unwrap();
        store.record_trace_event(&mut second).unwrap();
        assert_eq!((first.sequence, second.sequence), (0, 1));
        let events = store.trace_events_for_session("session-a", 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "tool.started");
        assert_eq!(events[0].detail["authorization"], "[REDACTED]");
        assert_eq!(events[1].kind, "tool.finished");
    }

    #[test]
    fn preflight_metrics_do_not_count_accepted_runs_as_degraded() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5_000).unwrap();
        for (id, outcome, latency_ms) in [
            ("accepted", "accepted", 10),
            ("degraded", "degraded", 20),
            ("open", "circuit_open", 30),
            ("cancelled", "cancelled", 40),
        ] {
            store
                .record_preflight_run(&PreflightRun {
                    id: id.into(),
                    session_id: "session".into(),
                    created_at: format!("2026-01-01T00:00:0{}Z", latency_ms / 10),
                    outcome: outcome.into(),
                    latency_ms,
                    decision: None,
                    detail: None,
                })
                .unwrap();
        }
        let metrics = store.preflight_metrics(10).unwrap();
        assert_eq!(metrics.total, 4);
        assert_eq!(metrics.degraded, 2);
        assert_eq!(metrics.average_latency_ms, 25);
        assert_eq!(metrics.outcomes["accepted"], 1);
    }

    #[test]
    fn knowledge_documents_are_versioned_published_and_tenant_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5_000).unwrap();
        let tenant = store.knowledge_create_tenant("Acme tenant").unwrap();
        let customer = store.knowledge_create_customer(&tenant.id, "Acme").unwrap();
        let space = store
            .knowledge_create_space(&tenant.id, &customer.id, "Support", "local_only")
            .unwrap();
        store
            .knowledge_set_member(&space.id, "owner", KnowledgeRole::Owner)
            .unwrap();
        store
            .knowledge_set_member(&space.id, "editor", KnowledgeRole::Editor)
            .unwrap();
        store
            .knowledge_set_member(&space.id, "reader", KnowledgeRole::Reader)
            .unwrap();

        let (document, first) = store
            .knowledge_import_document(
                &tenant.id,
                &customer.id,
                &space.id,
                "editor",
                "file",
                "runbook.md",
                "Reset the Acme gateway before retrying the failed deployment.",
                128,
            )
            .unwrap();
        let collection = store
            .knowledge_create_collection(
                &tenant.id,
                &customer.id,
                &space.id,
                "editor",
                "Operations",
            )
            .unwrap();
        store
            .knowledge_assign_collection(
                &tenant.id,
                &customer.id,
                &space.id,
                "editor",
                &collection.id,
                &document.id,
            )
            .unwrap();
        assert_eq!(
            store
                .knowledge_collections(&tenant.id, &customer.id, &space.id, "reader")
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .knowledge_search(&tenant.id, &customer.id, &space.id, "reader", "gateway", 10,)
            .unwrap()
            .is_empty());
        assert!(store
            .knowledge_publish_version(
                &tenant.id,
                &customer.id,
                &space.id,
                "editor",
                &document.id,
                &first.id,
            )
            .is_err());
        store
            .knowledge_publish_version(
                &tenant.id,
                &customer.id,
                &space.id,
                "owner",
                &document.id,
                &first.id,
            )
            .unwrap();
        assert_eq!(
            store
                .knowledge_search(&tenant.id, &customer.id, &space.id, "reader", "gateway", 10,)
                .unwrap()
                .len(),
            1
        );

        let (_, second) = store
            .knowledge_import_document(
                &tenant.id,
                &customer.id,
                &space.id,
                "editor",
                "file",
                "runbook.md",
                "Use the Acme gateway retry procedure after the deployment has failed.",
                128,
            )
            .unwrap();
        assert_eq!(second.version_no, 2);
        let (_, duplicate) = store
            .knowledge_import_document(
                &tenant.id,
                &customer.id,
                &space.id,
                "editor",
                "file",
                "runbook.md",
                "Use the Acme gateway retry procedure after the deployment has failed.",
                128,
            )
            .unwrap();
        assert_eq!(duplicate.id, second.id);
        assert_eq!(duplicate.version_no, 2);
        let sync_preview = store
            .knowledge_sync_preview(
                &tenant.id,
                &customer.id,
                &space.id,
                "editor",
                "remote_file",
                "faq.txt",
                "Acme FAQ: gateway retry is safe.",
            )
            .unwrap();
        assert!(sync_preview.changed);
        let (_, synced) = store
            .knowledge_sync_apply(
                &tenant.id,
                &customer.id,
                &space.id,
                "editor",
                "remote_file",
                "faq.txt",
                "Acme FAQ: gateway retry is safe.",
                Some("cursor-1"),
                128,
            )
            .unwrap();
        assert!(synced.is_some());
        let (_, unchanged) = store
            .knowledge_sync_apply(
                &tenant.id,
                &customer.id,
                &space.id,
                "editor",
                "remote_file",
                "faq.txt",
                "Acme FAQ: gateway retry is safe.",
                Some("cursor-1"),
                128,
            )
            .unwrap();
        assert!(unchanged.is_none());
        store
            .knowledge_publish_version(
                &tenant.id,
                &customer.id,
                &space.id,
                "owner",
                &document.id,
                &second.id,
            )
            .unwrap();
        let hits = store
            .knowledge_search(
                &tenant.id,
                &customer.id,
                &space.id,
                "reader",
                "retry procedure",
                10,
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].version_id, second.id);
        store
            .knowledge_revoke_version(
                &tenant.id,
                &customer.id,
                &space.id,
                "owner",
                &document.id,
                &second.id,
            )
            .unwrap();
        assert!(store
            .knowledge_search(
                &tenant.id,
                &customer.id,
                &space.id,
                "reader",
                "retry procedure",
                10,
            )
            .unwrap()
            .is_empty());
        store
            .knowledge_publish_version(
                &tenant.id,
                &customer.id,
                &space.id,
                "owner",
                &document.id,
                &second.id,
            )
            .unwrap();
        let local = store
            .knowledge_external_decision(
                &tenant.id,
                &customer.id,
                &space.id,
                "reader",
                "local_retrieval",
            )
            .unwrap();
        assert!(local.allowed);
        let cloud = store
            .knowledge_external_decision(
                &tenant.id,
                &customer.id,
                &space.id,
                "reader",
                "cloud_full",
            )
            .unwrap();
        assert!(!cloud.allowed);
        assert_eq!(cloud.policy, "local_only");

        let other_tenant = store.knowledge_create_tenant("Other tenant").unwrap();
        let other_customer = store
            .knowledge_create_customer(&other_tenant.id, "Other")
            .unwrap();
        assert!(store
            .knowledge_search(
                &other_tenant.id,
                &other_customer.id,
                &space.id,
                "reader",
                "gateway",
                10,
            )
            .is_err());
        store
            .knowledge_delete_document(&tenant.id, &customer.id, &space.id, "editor", &document.id)
            .unwrap();
        assert!(store
            .knowledge_search(&tenant.id, &customer.id, &space.id, "reader", "gateway", 10,)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn knowledge_answer_is_extractive_cited_and_published_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5_000).unwrap();
        let tenant = store.knowledge_create_tenant("Acme tenant").unwrap();
        let customer = store.knowledge_create_customer(&tenant.id, "Acme").unwrap();
        let space = store
            .knowledge_create_space(&tenant.id, &customer.id, "Support", "local_only")
            .unwrap();
        store
            .knowledge_set_member(&space.id, "owner", KnowledgeRole::Owner)
            .unwrap();
        store
            .knowledge_set_member(&space.id, "editor", KnowledgeRole::Editor)
            .unwrap();
        store
            .knowledge_set_member(&space.id, "reader", KnowledgeRole::Reader)
            .unwrap();
        let (document, version) = store
            .knowledge_import_document(
                &tenant.id,
                &customer.id,
                &space.id,
                "editor",
                "markdown",
                "support.md",
                "Retry the gateway only after resetting its connection.",
                128,
            )
            .unwrap();
        let no_answer = store
            .knowledge_answer(
                &tenant.id,
                &customer.id,
                &space.id,
                "reader",
                "How do I retry the gateway?",
                3,
            )
            .unwrap();
        assert!(
            no_answer.no_answer,
            "draft material must not answer questions"
        );
        store
            .knowledge_publish_version(
                &tenant.id,
                &customer.id,
                &space.id,
                "owner",
                &document.id,
                &version.id,
            )
            .unwrap();
        let answer = store
            .knowledge_answer(
                &tenant.id,
                &customer.id,
                &space.id,
                "reader",
                "How do I retry the gateway?",
                3,
            )
            .unwrap();
        assert!(!answer.no_answer);
        assert!(answer.answer.contains("resetting its connection"));
        assert_eq!(answer.citations.len(), 1);
        assert_eq!(answer.citations[0].version_id, version.id);
        assert_eq!(answer.citations[0].source_id, "support.md");
    }

    #[test]
    fn knowledge_chinese_question_finds_published_source_without_crossing_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 5_000).unwrap();
        let first = store
            .knowledge_create_local_space("local-user", "模拟产品资料", "local_only")
            .unwrap();
        let second = store
            .knowledge_create_local_space("local-user", "其他资料", "local_only")
            .unwrap();
        let (document, version) = store
            .knowledge_import_document(
                "local",
                "local",
                &first.id,
                "local-user",
                "markdown",
                "knowledge-demo.md",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../examples/knowledge-demo.md"
                )),
                128,
            )
            .unwrap();
        let ask = |space_id: &str| {
            store
                .knowledge_answer(
                    "local",
                    "local",
                    space_id,
                    "local-user",
                    "如何重置网关？",
                    6,
                )
                .unwrap()
        };
        assert!(ask(&first.id).no_answer, "草稿不能用于回答");
        store
            .knowledge_publish_version(
                "local",
                "local",
                &first.id,
                "local-user",
                &document.id,
                &version.id,
            )
            .unwrap();
        let answer = ask(&first.id);
        assert!(!answer.no_answer);
        assert!(answer.answer.contains("等待三分钟"));
        assert_eq!(answer.citations.len(), 1);
        assert_eq!(answer.citations[0].source_id, "knowledge-demo.md");
        let follow_up = store
            .knowledge_answer(
                "local",
                "local",
                &first.id,
                "local-user",
                "网关重置后要等待多久？",
                6,
            )
            .unwrap();
        assert!(!follow_up.no_answer);
        assert!(follow_up.answer.contains("三分钟"));
        assert!(ask(&second.id).no_answer, "其他资料库不能引用本资料");
        assert!(
            store
                .knowledge_answer("local", "local", &first.id, "local-user", "如何退货？", 6)
                .unwrap()
                .no_answer
        );
        assert!(
            store
                .knowledge_answer(
                    "local",
                    "local",
                    &first.id,
                    "local-user",
                    "如何查询网关？",
                    6
                )
                .unwrap()
                .no_answer
        );
        store
            .knowledge_revoke_version(
                "local",
                "local",
                &first.id,
                "local-user",
                &document.id,
                &version.id,
            )
            .unwrap();
        assert!(ask(&first.id).no_answer, "撤回后不能继续引用");
    }
}
