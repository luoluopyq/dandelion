//! Merged Settings. Factory defaults live here; runtime never copies the numbers elsewhere.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

const KEY_BASE_URL: &str = "dandelion.baseUrl";
const KEY_DEFAULT_MODEL: &str = "dandelion.defaultModel";
const KEY_API_KEY: &str = "dandelion.apiKey";
const KEY_DEFAULT_BACKEND: &str = "dandelion.defaultBackend";
const KEY_SERVICE_BACKEND: &str = "dandelion.service.backend";
const KEY_SERVICE_MODEL: &str = "dandelion.service.model";
const KEY_TEMPERATURE: &str = "dandelion.temperature";
const KEY_MAX_TOKENS: &str = "dandelion.maxTokens";
const KEY_DYNAMIC_TOKEN_BUDGET: &str = "dandelion.tokenBudget.enabled";
const KEY_TOKEN_BUDGET_MARGIN_TOKENS: &str = "dandelion.tokenBudget.marginTokens";
const KEY_TOKEN_BUDGET_MIN_OUTPUT_TOKENS: &str = "dandelion.tokenBudget.minOutputTokens";
const KEY_MAX_ITERATIONS: &str = "dandelion.maxIterations";
const KEY_PERMISSION_MODE: &str = "dandelion.permissionMode";
const KEY_ALLOW_SHELL: &str = "dandelion.allowShell";
const KEY_WORKSPACE: &str = "dandelion.workspace";
const KEY_HTTP_CONNECT_TIMEOUT_MS: &str = "dandelion.httpConnectTimeoutMs";
const KEY_SSE_IDLE_TIMEOUT_MS: &str = "dandelion.sseIdleTimeoutMs";
const KEY_SIDECAR_SSE_IDLE_TIMEOUT_MS: &str = "dandelion.sidecar.sseIdleTimeoutMs";
const KEY_RETRY_MAX: &str = "dandelion.retryMax";
const KEY_RETRY_BASE_MS: &str = "dandelion.retryBaseMs";
const KEY_RETRY_CAP_MS: &str = "dandelion.retryCapMs";
const KEY_TERMINAL_FOREGROUND_TIMEOUT_SEC: &str = "dandelion.terminal.foregroundTimeoutSec";
const KEY_TERMINAL_FOREGROUND_TIMEOUT_MAX_SEC: &str = "dandelion.terminal.foregroundTimeoutMaxSec";
const KEY_TERMINAL_OUTPUT_MAX_BYTES: &str = "dandelion.terminal.outputMaxBytes";
const KEY_TERMINAL_LIVE_CHUNK_BYTES: &str = "dandelion.terminal.liveChunkBytes";
const KEY_TERMINAL_LIVE_FLUSH_MS: &str = "dandelion.terminal.liveFlushMs";
const KEY_FILE_READ_DEFAULT_LIMIT: &str = "dandelion.file.readDefaultLimit";
const KEY_FILE_READ_MAX_LIMIT: &str = "dandelion.file.readMaxLimit";
const KEY_FILE_READ_MAX_BYTES: &str = "dandelion.file.readMaxBytes";
const KEY_FILE_WRITE_MAX_BYTES: &str = "dandelion.file.writeMaxBytes";
const KEY_SEARCH_MAX_RESULTS: &str = "dandelion.search.maxResults";
const KEY_SEARCH_MAX_FILE_BYTES: &str = "dandelion.search.maxFileBytes";
const KEY_PROCESS_LOG_DEFAULT_LINES: &str = "dandelion.process.logDefaultLines";
const KEY_PERMISSION_TIMEOUT_SEC: &str = "dandelion.permissionTimeoutSec";
const KEY_SQLITE_BUSY_MS: &str = "dandelion.sqliteBusyMs";
const KEY_BUS_CAPACITY: &str = "dandelion.busCapacity";
const KEY_THINKING_INTENSITY: &str = "dandelion.thinkingIntensity";
const KEY_TOOLS_PARALLEL_INDEPENDENT: &str = "dandelion.tools.parallelIndependent";
const KEY_TOOLS_PARALLEL_MAX: &str = "dandelion.tools.parallelMax";
const KEY_TOOLS_START_ON_READY: &str = "dandelion.tools.startOnReady";
const KEY_COMPACT_ENABLED: &str = "dandelion.compact.enabled";
const KEY_COMPACT_MAX_PER_PROMPT: &str = "dandelion.compact.maxPerPrompt";
const KEY_COMPACT_KEEP_TURNS: &str = "dandelion.compact.keepTurns";
const KEY_COMPACT_SOURCE_MAX_BYTES: &str = "dandelion.compact.sourceMaxBytes";
const KEY_COMPACT_PREFLIGHT_ENABLED: &str = "dandelion.compact.preflightEnabled";
const KEY_COMPACT_PREFLIGHT_RESERVE_TOKENS: &str = "dandelion.compact.preflightReserveTokens";
const KEY_SPEC_GENERATE_MAX_ITERATIONS: &str = "dandelion.spec.generateMaxIterations";
const KEY_SPEC_ADAPT_MAX_ITERATIONS: &str = "dandelion.spec.adaptMaxIterations";
const KEY_SPEC_MAX_FILE_BYTES: &str = "dandelion.spec.maxFileBytes";
const KEY_SPEC_EXECUTE_MAX_ITERATIONS: &str = "dandelion.spec.executeMaxIterations";
const KEY_HOOKS_TIMEOUT_MS: &str = "dandelion.hooks.timeoutMs";
const KEY_HOOKS_ENABLED: &str = "dandelion.hooks.enabled";
const KEY_SKILLS_MAX_INJECT_BYTES: &str = "dandelion.skills.maxInjectBytes";
const KEY_SKILLS_DISABLED_IDS: &str = "dandelion.skills.disabledIds";
const KEY_SANDBOX_ALLOW_ROOTS: &str = "dandelion.sandbox.allowRoots";
const KEY_MCP_SERVERS: &str = "dandelion.mcp.servers";
const KEY_MCP_SPAWN_TIMEOUT_MS: &str = "dandelion.mcp.spawnTimeoutMs";
const KEY_MCP_FAILURE_THRESHOLD: &str = "dandelion.mcp.failureThreshold";
const KEY_MCP_RECONNECT_BACKOFF_SEC: &str = "dandelion.mcp.reconnectBackoffSec";
const KEY_MCP_ACTIVATE_MAX_PER_TURN: &str = "dandelion.mcp.activateMaxPerTurn";
const KEY_MCP_MAX_TOOL_SCHEMA_BYTES: &str = "dandelion.mcp.maxToolSchemaBytes";
const KEY_MCP_DESCRIPTION_MAX_CHARS: &str = "dandelion.mcp.descriptionMaxChars";
const KEY_MCP_MAX_SCHEMA_BYTES: &str = "dandelion.mcp.maxSchemaBytes";
const KEY_REGISTRY_MCP_URL: &str = "dandelion.registry.mcpUrl";
const KEY_REGISTRY_TIMEOUT_MS: &str = "dandelion.registry.timeoutMs";
const KEY_REGISTRY_MAX_RESULTS: &str = "dandelion.registry.maxResults";
const KEY_REGISTRY_SKILL_ALLOWED_HOSTS: &str = "dandelion.registry.skillAllowedHosts";
const KEY_CONTEXT_ENABLED: &str = "dandelion.context.enabled";
const KEY_CONTEXT_MAX_INJECT_BYTES: &str = "dandelion.context.maxInjectBytes";
const KEY_CONTEXT_MAX_FILE_BYTES: &str = "dandelion.context.maxFileBytes";
const KEY_CONTEXT_PACK_ENABLED: &str = "dandelion.contextPack.enabled";
const KEY_CONTEXT_PACK_TOKEN_BUDGET: &str = "dandelion.contextPack.tokenBudget";
const KEY_CONTEXT_PACK_MAX_ITEMS: &str = "dandelion.contextPack.maxItems";
const KEY_MEMORY_ENABLED: &str = "dandelion.memory.enabled";
const KEY_MEMORY_MAX_INJECT_BYTES: &str = "dandelion.memory.maxInjectBytes";
const KEY_MEMORY_SOUL_MAX_BYTES: &str = "dandelion.memory.soulMaxBytes";
const KEY_MEMORY_USER_MAX_BYTES: &str = "dandelion.memory.userMaxBytes";
const KEY_MEMORY_MEMORY_MAX_BYTES: &str = "dandelion.memory.memoryMaxBytes";
const KEY_MEMORY_AUTO_CANDIDATES: &str = "dandelion.memory.autoCandidates";
const KEY_CORPUS_MAX_HITS: &str = "dandelion.corpus.maxHits";
const KEY_CORPUS_MAX_INJECT_BYTES: &str = "dandelion.corpus.maxInjectBytes";
const KEY_CORPUS_CHUNK_CHARS: &str = "dandelion.corpus.chunkChars";
const KEY_CORPUS_MIN_DISK_FREE_MB: &str = "dandelion.corpus.minDiskFreeMb";
const KEY_CORPUS_MAX_FILE_BYTES: &str = "dandelion.corpus.maxFileBytes";
const KEY_TODO_MAX_ITEMS: &str = "dandelion.todo.maxItems";
const KEY_TODO_MAX_CONTENT_CHARS: &str = "dandelion.todo.maxContentChars";
const KEY_WEB_TIMEOUT_MS: &str = "dandelion.web.timeoutMs";
const KEY_WEB_MAX_RESULTS: &str = "dandelion.web.maxResults";
const KEY_WEB_MAX_EXTRACT_BYTES: &str = "dandelion.web.maxExtractBytes";
const KEY_WEB_MAX_EXTRACT_URLS: &str = "dandelion.web.maxExtractUrls";
const KEY_WEB_SEARCH_URL: &str = "dandelion.web.searchUrl";
const KEY_WEB_USER_AGENT: &str = "dandelion.web.userAgent";
const KEY_PERMISSION_RULES: &str = "dandelion.permission.rules";
const KEY_DELEGATION_MAX_DEPTH: &str = "dandelion.delegation.maxDepth";
const KEY_DELEGATION_MAX_CONCURRENT: &str = "dandelion.delegation.maxConcurrent";
const KEY_DELEGATION_CHILD_MAX_ITERATIONS: &str = "dandelion.delegation.childMaxIterations";
const KEY_DELEGATION_CHILD_TIMEOUT_SEC: &str = "dandelion.delegation.childTimeoutSec";
const KEY_DELEGATION_INHERIT_SHELL: &str = "dandelion.delegation.inheritShell";
const KEY_DELEGATION_MODEL: &str = "dandelion.delegation.model";
const KEY_DELEGATION_SUMMARY_MAX_CHARS: &str = "dandelion.delegation.summaryMaxChars";
const KEY_DELEGATION_STALE_IDLE_SEC: &str = "dandelion.delegation.staleIdleSec";
const KEY_DELEGATION_STALE_IN_TOOL_SEC: &str = "dandelion.delegation.staleInToolSec";
const KEY_DELEGATION_WORKTREE_ISOLATION: &str = "dandelion.delegation.worktreeIsolation";
const KEY_ACCOUNT_RECHECK_MS: &str = "dandelion.account.recheckMs";
const KEY_CRON_TICK_SEC: &str = "dandelion.cron.tickSec";
const KEY_CRON_MAX_CONCURRENT: &str = "dandelion.cron.maxConcurrent";
const KEY_SESSION_LIST_LIMIT: &str = "dandelion.session.listLimit";
const KEY_MAX_LOCAL_CONCURRENCY: &str = "dandelion.maxLocalConcurrency";
const KEY_KILL_SIDECAR_ON_EXIT: &str = "dandelion.killSidecarOnExit";
const KEY_RECOMMEND_QUANT: &str = "dandelion.recommendQuant";
const KEY_LOCAL_MODEL_DIR: &str = "dandelion.localModelDir";
const KEY_SIDECAR_READY_TIMEOUT_MS: &str = "dandelion.sidecar.readyTimeoutMs";
const KEY_SIDECAR_LISTEN_PORT: &str = "dandelion.sidecar.listenPort";
const KEY_CC_SWITCH_PROXY_PORT: &str = "dandelion.agent.ccSwitchProxyPort";
const KEY_SIDECAR_MLX_CACHE_BYTES: &str = "dandelion.sidecar.mlxPromptCacheBytes";
const KEY_SIDECAR_MLX_KV_BITS: &str = "dandelion.sidecar.mlxKvBits";
const KEY_PREFLIGHT_ENABLED: &str = "dandelion.preflight.enabled";
const KEY_PREFLIGHT_TIMEOUT_MS: &str = "dandelion.preflight.timeoutMs";
const KEY_PREFLIGHT_FAILURE_THRESHOLD: &str = "dandelion.preflight.failureThreshold";
const KEY_PREFLIGHT_COOLDOWN_SEC: &str = "dandelion.preflight.cooldownSec";
const KEY_ENCRYPT_API_KEY: &str = "dandelion.encryptApiKey";
const KEY_UI_WINDOW_WIDTH: &str = "dandelion.ui.windowWidth";
const KEY_UI_WINDOW_HEIGHT: &str = "dandelion.ui.windowHeight";
const KEY_UI_WINDOW_MAXIMIZED: &str = "dandelion.ui.windowMaximized";

/// 上限匹配主流 2K 桌面显示器（2560×1440 物理像素 @ 150% DPI 的逻辑尺寸）。
/// 即使在 4K / 高分屏上拖动窗口，写回值也夹紧到这里，不会出现"双屏之间拖一次窗口就爆"。
pub const UI_WINDOW_MAX_W: u32 = 1920;
pub const UI_WINDOW_MAX_H: u32 = 1200;
const UI_WINDOW_MIN_W: u32 = 900;
const UI_WINDOW_MIN_H: u32 = 600;
const KEY_WIKI_COMPILE_ON_IDLE: &str = "dandelion.wiki.compileOnIdle";
const KEY_WIKI_MAX_SCAN_FILES: &str = "dandelion.wiki.maxScanFiles";
const KEY_WIKI_MAX_FILE_BYTES: &str = "dandelion.wiki.maxFileBytes";
const KEY_WIKI_LLM_INPUT_BYTES: &str = "dandelion.wiki.llmInputBytes";
const KEY_WIKI_LLM_MAX_TOKENS: &str = "dandelion.wiki.llmMaxTokens";
const KEY_WIKI_PROPOSAL_ENABLED: &str = "dandelion.wiki.proposalEnabled";
const KEY_WIKI_PROPOSAL_MAX_BYTES: &str = "dandelion.wiki.proposalMaxBytes";
/// Model override for Wiki model turns; empty = the session default model.
const KEY_WIKI_LLM_MODEL: &str = "dandelion.wiki.llmModel";
/// Max articles one deep Wiki build may produce.
const KEY_WIKI_BUILD_MAX_ARTICLES: &str = "dandelion.wiki.buildMaxArticles";
/// Evidence-pack byte cap fed to one article-writing turn.
const KEY_WIKI_BUILD_EVIDENCE_BYTES: &str = "dandelion.wiki.buildEvidenceBytes";
/// Output-token cap for one article-writing turn.
const KEY_WIKI_BUILD_ARTICLE_MAX_TOKENS: &str = "dandelion.wiki.buildArticleMaxTokens";
/// Metadata-tier cap: files up to this size still get hash/lines/symbols/
/// sections indexed even when the body tier (maxFileBytes) will not carry
/// their text into evidence. Files above it are the honest `tooLarge` set.
const KEY_WIKI_META_MAX_FILE_BYTES: &str = "dandelion.wiki.metaMaxFileBytes";
/// Metadata-tier cap on how many discovered files the evidence index keeps
/// (lookup/QA can then see far beyond the body budget). Body budget stays
/// `maxScanFiles`; this only widens what the index *knows about*.
const KEY_WIKI_MAX_INDEX_FILES: &str = "dandelion.wiki.maxIndexFiles";
/// Share (percent) of body-budget slots the Doc category may occupy when
/// filling it — stops multilingual READMEs and doc trees crowding out
/// entry points and sources.
const KEY_WIKI_BUILD_DOC_BUDGET_PCT: &str = "dandelion.wiki.buildDocBudgetPct";
/// How many article-writing model turns one deep build may run at once.
const KEY_WIKI_BUILD_CONCURRENCY: &str = "dandelion.wiki.buildConcurrency";
/// Incremental rebuild: minimum added files inside one module before that
/// module may count as a structural change worth re-planning.
const KEY_WIKI_STRUCTURAL_NEW_FILE_MIN: &str = "dandelion.wiki.structuralNewFileMin";
/// Incremental rebuild: added files as a percent of the module's files.
const KEY_WIKI_STRUCTURAL_NEW_FILE_PCT: &str = "dandelion.wiki.structuralNewFilePct";
/// A module this large counts as structural on a looser ratio instead.
const KEY_WIKI_STRUCTURAL_LARGE_FILE_MIN: &str = "dandelion.wiki.structuralLargeFileMin";
/// Percent threshold applied to large modules (>= the min above).
const KEY_WIKI_STRUCTURAL_LARGE_FILE_PCT: &str = "dandelion.wiki.structuralLargeFilePct";
/// When the structure changed, ask the model for a plan *delta* instead of
/// only rebuilding stale articles. Off switches back to stale-only.
const KEY_WIKI_INCREMENTAL_REPLAN_ENABLED: &str = "dandelion.wiki.incrementalReplanEnabled";
/// Extra model turns a malformed plan/delta answer may be retried with
/// before falling back (0 = never retry). Counted in build model_calls.
const KEY_WIKI_BUILD_PLAN_RETRY_MAX: &str = "dandelion.wiki.buildPlanRetryMax";
/// Re-evidence rounds for sentences whose citations failed the gate (0 = off).
const KEY_WIKI_EVIDENCE_REFETCH_MAX: &str = "dandelion.wiki.evidenceRefetchMax";
/// Rewrite rounds for an article that still has invalid citations after
/// verification, before demotion kicks in (0 = off, max 3).
const KEY_WIKI_BUILD_EVIDENCE_RETRY_MAX: &str = "dandelion.wiki.buildEvidenceRetryMax";
/// How many immutable wiki page versions (`wiki/versions/<n>`) to keep after
/// a publish/scan prunes older ones. The current version is never pruned.
const KEY_WIKI_VERSIONS_KEEP: &str = "dandelion.wiki.versionsKeep";
/// Auto-migrate legacy `wiki/projects/<ws-hash>/` directories into the space
/// tree (`wiki/spaces/<spaceId>/`) at startup. Off = resolve-on-use only.
const KEY_WIKI_AUTO_MIGRATE: &str = "dandelion.wiki.autoMigrate";
const KEY_WIKI_IGNORE_PATTERNS: &str = "dandelion.wiki.ignorePatterns";
/// Maximum bytes per query-centered RAG excerpt.
const KEY_KNOWLEDGE_EXCERPT_BYTES: &str = "dandelion.knowledge.excerptBytes";
/// Max hits one unified space search returns per request.
const KEY_SPACE_SEARCH_MAX_HITS: &str = "dandelion.space.searchMaxHits";
/// Evidence byte budget for one space-ask turn.
const KEY_SPACE_ASK_EVIDENCE_BYTES: &str = "dandelion.space.askEvidenceBytes";
/// Max bound knowledge spaces one chat turn may search.
const KEY_KNOWLEDGE_CHAT_MAX_SPACES: &str = "dandelion.knowledge.chatMaxSpaces";
/// Upper/lower bounds on topics a generic organizing plan may propose.
const KEY_PLAN_MAX_TOPICS: &str = "dandelion.plan.maxTopics";
const KEY_PLAN_MIN_TOPICS: &str = "dandelion.plan.minTopics";
const KEY_VISION_MAX_IMAGES: &str = "dandelion.vision.maxImages";
const KEY_VISION_MAX_IMAGE_BYTES: &str = "dandelion.vision.maxImageBytes";
const KEY_SERVE_BIND: &str = "dandelion.serve.bind";

const KNOWN_KEYS: &[&str] = &[
    KEY_BASE_URL,
    KEY_DEFAULT_MODEL,
    KEY_API_KEY,
    KEY_DEFAULT_BACKEND,
    KEY_SERVICE_BACKEND,
    KEY_SERVICE_MODEL,
    KEY_TEMPERATURE,
    KEY_MAX_TOKENS,
    KEY_DYNAMIC_TOKEN_BUDGET,
    KEY_TOKEN_BUDGET_MARGIN_TOKENS,
    KEY_TOKEN_BUDGET_MIN_OUTPUT_TOKENS,
    KEY_MAX_ITERATIONS,
    KEY_PERMISSION_MODE,
    KEY_ALLOW_SHELL,
    KEY_WORKSPACE,
    KEY_HTTP_CONNECT_TIMEOUT_MS,
    KEY_SSE_IDLE_TIMEOUT_MS,
    KEY_SIDECAR_SSE_IDLE_TIMEOUT_MS,
    KEY_RETRY_MAX,
    KEY_RETRY_BASE_MS,
    KEY_RETRY_CAP_MS,
    KEY_TERMINAL_FOREGROUND_TIMEOUT_SEC,
    KEY_TERMINAL_FOREGROUND_TIMEOUT_MAX_SEC,
    KEY_TERMINAL_OUTPUT_MAX_BYTES,
    KEY_TERMINAL_LIVE_CHUNK_BYTES,
    KEY_TERMINAL_LIVE_FLUSH_MS,
    KEY_FILE_READ_DEFAULT_LIMIT,
    KEY_FILE_READ_MAX_LIMIT,
    KEY_FILE_READ_MAX_BYTES,
    KEY_FILE_WRITE_MAX_BYTES,
    KEY_SEARCH_MAX_RESULTS,
    KEY_SEARCH_MAX_FILE_BYTES,
    KEY_PROCESS_LOG_DEFAULT_LINES,
    KEY_PERMISSION_TIMEOUT_SEC,
    KEY_SQLITE_BUSY_MS,
    KEY_BUS_CAPACITY,
    KEY_THINKING_INTENSITY,
    KEY_TOOLS_PARALLEL_INDEPENDENT,
    KEY_TOOLS_PARALLEL_MAX,
    KEY_TOOLS_START_ON_READY,
    KEY_COMPACT_ENABLED,
    KEY_COMPACT_MAX_PER_PROMPT,
    KEY_COMPACT_KEEP_TURNS,
    KEY_COMPACT_SOURCE_MAX_BYTES,
    KEY_COMPACT_PREFLIGHT_ENABLED,
    KEY_COMPACT_PREFLIGHT_RESERVE_TOKENS,
    KEY_SPEC_GENERATE_MAX_ITERATIONS,
    KEY_SPEC_ADAPT_MAX_ITERATIONS,
    KEY_SPEC_MAX_FILE_BYTES,
    KEY_SPEC_EXECUTE_MAX_ITERATIONS,
    KEY_HOOKS_TIMEOUT_MS,
    KEY_HOOKS_ENABLED,
    KEY_SKILLS_MAX_INJECT_BYTES,
    KEY_SKILLS_DISABLED_IDS,
    KEY_SANDBOX_ALLOW_ROOTS,
    KEY_MCP_SERVERS,
    KEY_MCP_SPAWN_TIMEOUT_MS,
    KEY_MCP_FAILURE_THRESHOLD,
    KEY_MCP_RECONNECT_BACKOFF_SEC,
    KEY_MCP_ACTIVATE_MAX_PER_TURN,
    KEY_MCP_MAX_TOOL_SCHEMA_BYTES,
    KEY_MCP_DESCRIPTION_MAX_CHARS,
    KEY_MCP_MAX_SCHEMA_BYTES,
    KEY_REGISTRY_MCP_URL,
    KEY_REGISTRY_TIMEOUT_MS,
    KEY_REGISTRY_MAX_RESULTS,
    KEY_REGISTRY_SKILL_ALLOWED_HOSTS,
    KEY_CONTEXT_ENABLED,
    KEY_CONTEXT_MAX_INJECT_BYTES,
    KEY_CONTEXT_MAX_FILE_BYTES,
    KEY_CONTEXT_PACK_ENABLED,
    KEY_CONTEXT_PACK_TOKEN_BUDGET,
    KEY_CONTEXT_PACK_MAX_ITEMS,
    KEY_MEMORY_ENABLED,
    KEY_MEMORY_MAX_INJECT_BYTES,
    KEY_MEMORY_SOUL_MAX_BYTES,
    KEY_MEMORY_USER_MAX_BYTES,
    KEY_MEMORY_MEMORY_MAX_BYTES,
    KEY_MEMORY_AUTO_CANDIDATES,
    KEY_CORPUS_MAX_HITS,
    KEY_CORPUS_MAX_INJECT_BYTES,
    KEY_CORPUS_CHUNK_CHARS,
    KEY_CORPUS_MIN_DISK_FREE_MB,
    KEY_CORPUS_MAX_FILE_BYTES,
    KEY_TODO_MAX_ITEMS,
    KEY_TODO_MAX_CONTENT_CHARS,
    KEY_WEB_TIMEOUT_MS,
    KEY_WEB_MAX_RESULTS,
    KEY_WEB_MAX_EXTRACT_BYTES,
    KEY_WEB_MAX_EXTRACT_URLS,
    KEY_WEB_SEARCH_URL,
    KEY_WEB_USER_AGENT,
    KEY_PERMISSION_RULES,
    KEY_DELEGATION_MAX_DEPTH,
    KEY_DELEGATION_MAX_CONCURRENT,
    KEY_DELEGATION_CHILD_MAX_ITERATIONS,
    KEY_DELEGATION_CHILD_TIMEOUT_SEC,
    KEY_DELEGATION_INHERIT_SHELL,
    KEY_DELEGATION_MODEL,
    KEY_DELEGATION_SUMMARY_MAX_CHARS,
    KEY_DELEGATION_STALE_IDLE_SEC,
    KEY_DELEGATION_STALE_IN_TOOL_SEC,
    KEY_DELEGATION_WORKTREE_ISOLATION,
    KEY_ACCOUNT_RECHECK_MS,
    KEY_CRON_TICK_SEC,
    KEY_CRON_MAX_CONCURRENT,
    KEY_SESSION_LIST_LIMIT,
    KEY_MAX_LOCAL_CONCURRENCY,
    KEY_KILL_SIDECAR_ON_EXIT,
    KEY_RECOMMEND_QUANT,
    KEY_LOCAL_MODEL_DIR,
    KEY_SIDECAR_READY_TIMEOUT_MS,
    KEY_SIDECAR_LISTEN_PORT,
    KEY_CC_SWITCH_PROXY_PORT,
    KEY_SIDECAR_MLX_CACHE_BYTES,
    KEY_SIDECAR_MLX_KV_BITS,
    KEY_PREFLIGHT_ENABLED,
    KEY_PREFLIGHT_TIMEOUT_MS,
    KEY_PREFLIGHT_FAILURE_THRESHOLD,
    KEY_PREFLIGHT_COOLDOWN_SEC,
    KEY_ENCRYPT_API_KEY,
    KEY_UI_WINDOW_WIDTH,
    KEY_UI_WINDOW_HEIGHT,
    KEY_UI_WINDOW_MAXIMIZED,
    KEY_WIKI_COMPILE_ON_IDLE,
    KEY_WIKI_MAX_SCAN_FILES,
    KEY_WIKI_MAX_FILE_BYTES,
    KEY_WIKI_LLM_INPUT_BYTES,
    KEY_WIKI_LLM_MAX_TOKENS,
    KEY_WIKI_LLM_MODEL,
    KEY_WIKI_BUILD_MAX_ARTICLES,
    KEY_WIKI_BUILD_EVIDENCE_BYTES,
    KEY_WIKI_BUILD_ARTICLE_MAX_TOKENS,
    KEY_WIKI_META_MAX_FILE_BYTES,
    KEY_WIKI_MAX_INDEX_FILES,
    KEY_WIKI_BUILD_DOC_BUDGET_PCT,
    KEY_WIKI_BUILD_CONCURRENCY,
    KEY_WIKI_STRUCTURAL_NEW_FILE_MIN,
    KEY_WIKI_STRUCTURAL_NEW_FILE_PCT,
    KEY_WIKI_STRUCTURAL_LARGE_FILE_MIN,
    KEY_WIKI_STRUCTURAL_LARGE_FILE_PCT,
    KEY_WIKI_INCREMENTAL_REPLAN_ENABLED,
    KEY_WIKI_BUILD_PLAN_RETRY_MAX,
    KEY_WIKI_EVIDENCE_REFETCH_MAX,
    KEY_WIKI_BUILD_EVIDENCE_RETRY_MAX,
    KEY_WIKI_VERSIONS_KEEP,
    KEY_WIKI_AUTO_MIGRATE,
    KEY_WIKI_IGNORE_PATTERNS,
    KEY_WIKI_PROPOSAL_ENABLED,
    KEY_WIKI_PROPOSAL_MAX_BYTES,
    KEY_KNOWLEDGE_EXCERPT_BYTES,
    KEY_SPACE_SEARCH_MAX_HITS,
    KEY_SPACE_ASK_EVIDENCE_BYTES,
    KEY_KNOWLEDGE_CHAT_MAX_SPACES,
    KEY_PLAN_MAX_TOPICS,
    KEY_PLAN_MIN_TOPICS,
    KEY_SERVE_BIND,
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Local stdio executable. Empty when this entry is remote (`url`).
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Remote MCP endpoint (Streamable HTTP). Mutually exclusive with `command`.
    #[serde(default)]
    pub url: Option<String>,
    /// Extra HTTP headers for remote servers (`Authorization` may also come from `bearer_token_env_var`).
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Read a bearer token from this environment variable (value is not stored in settings.json).
    #[serde(default, alias = "bearerTokenEnvVar")]
    pub bearer_token_env_var: Option<String>,
    /// `stdio` | `http` | `sse`. Empty = infer (`url` → http, else stdio).
    /// `sse` is accepted for Cursor/Hermes snippets; the client is still Streamable HTTP.
    #[serde(default)]
    pub transport: String,
    /// User-layer deny list of raw (remote) tool names this server may never
    /// expose. Project/session layers can only add to it (`mcp/scopes.rs`),
    /// never lift entries.
    #[serde(default, alias = "disabledTools")]
    pub disabled_tools: Vec<String>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            enabled: true,
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            url: None,
            headers: HashMap::new(),
            bearer_token_env_var: None,
            transport: String::new(),
            disabled_tools: Vec::new(),
        }
    }
}

impl McpServerConfig {
    pub fn is_remote(&self) -> bool {
        self.url
            .as_ref()
            .map(|u| !u.trim().is_empty())
            .unwrap_or(false)
    }
}

fn default_true() -> bool {
    true
}

/// Tool rules: deny wins over ask, and ask wins over allow.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRules {
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    #[default]
    Ask,
    Auto,
    Deny,
}

impl PermissionMode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ask" => Ok(Self::Ask),
            "auto" => Ok(Self::Auto),
            "deny" => Ok(Self::Deny),
            other => Err(Error::Settings(format!(
                "invalid {KEY_PERMISSION_MODE}: {other} (expected ask|auto|deny)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::Deny => "deny",
        }
    }
}

/// OpenAI-compatible `reasoning_effort`. Sent on Chat Completions as-is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingIntensity {
    #[default]
    Xhigh,
    Medium,
    Low,
    Off,
}

impl ThinkingIntensity {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "xhigh" | "x-high" => Ok(Self::Xhigh),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            "off" => Ok(Self::Off),
            other => Err(Error::Settings(format!(
                "invalid {KEY_THINKING_INTENSITY}: {other} (expected xhigh|medium|low|off)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Xhigh => "xhigh",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Off => "off",
        }
    }

    pub fn reasoning_effort(self) -> Option<String> {
        (self != Self::Off).then(|| self.as_str().to_string())
    }
}

#[derive(Clone, Default)]
pub struct SettingsOverlay {
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub api_key: Option<String>,
    pub workspace: Option<PathBuf>,
    pub permission_mode: Option<PermissionMode>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub max_iterations: Option<u32>,
    pub thinking_intensity: Option<ThinkingIntensity>,
    pub allow_shell: Option<bool>,
}

#[derive(Clone)]
pub struct Settings {
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub api_key: Option<String>,
    pub default_backend: String,
    /// Saved model-service selection, independent of the active default backend.
    pub service_backend: Option<String>,
    pub service_model: Option<String>,
    pub temperature: f32,
    pub max_tokens: u32,
    /// Use vLLM's /tokenize count and max_model_len before Chat Completions.
    pub dynamic_token_budget: bool,
    pub token_budget_margin_tokens: u32,
    pub token_budget_min_output_tokens: u32,
    pub max_iterations: u32,
    pub permission_mode: PermissionMode,
    /// G1: `terminal` / `process` stay in the tool schema; dispatch returns
    /// `Error: shell disabled by settings` when this is false.
    pub allow_shell: bool,
    pub workspace: Option<PathBuf>,
    pub http_connect_timeout_ms: u64,
    pub sse_idle_timeout_ms: u64,
    /// Local models may spend minutes generating a tool argument without SSE bytes.
    pub sidecar_sse_idle_timeout_ms: u64,
    pub retry_max: u32,
    pub retry_base_ms: u64,
    pub retry_cap_ms: u64,
    pub terminal_foreground_timeout_sec: u64,
    pub terminal_foreground_timeout_max_sec: u64,
    pub terminal_output_max_bytes: usize,
    pub terminal_live_chunk_bytes: usize,
    pub terminal_live_flush_ms: u64,
    pub file_read_default_limit: u32,
    pub file_read_max_limit: u32,
    pub file_read_max_bytes: u64,
    pub file_write_max_bytes: u64,
    pub search_max_results: u32,
    pub search_max_file_bytes: u64,
    pub process_log_default_lines: u32,
    pub permission_timeout_sec: u64,
    pub sqlite_busy_ms: u64,
    pub bus_capacity: usize,
    pub thinking_intensity: ThinkingIntensity,
    /// Independent read/search (and independent writes in auto) may share a segment.
    pub tools_parallel_independent: bool,
    pub tools_parallel_max: usize,
    /// Codex `OutputItemDone` analogue: start a tool when its Chat Completions
    /// `tool_call` slot is complete (next index or stream end), before SSE ends.
    pub tools_start_on_ready: bool,
    /// On HTTP context overflow, summarize older turns and retry in the same loop.
    pub compact_enabled: bool,
    pub compact_max_per_prompt: u32,
    /// User-started turns kept verbatim (assistant + tool rows stay paired).
    pub compact_keep_turns: u32,
    pub compact_source_max_bytes: usize,
    /// Before `complete`, estimate prompt tokens and compact if over the remaining window.
    pub compact_preflight_enabled: bool,
    pub compact_preflight_reserve_tokens: u32,
    pub spec_generate_max_iterations: u32,
    pub spec_adapt_max_iterations: u32,
    pub spec_max_file_bytes: usize,
    /// `0` = use global `maxIterations` for Execute.
    pub spec_execute_max_iterations: u32,
    pub hooks_timeout_ms: u64,
    pub hooks_enabled: Vec<String>,
    pub skills_max_inject_bytes: usize,
    /// Skill ids that 03 has globally disabled; 02 must not inject them.
    pub skills_disabled_ids: Vec<String>,
    /// Extra WorkspaceFs roots (CD3). Primary workspace is `workspace`.
    pub sandbox_allow_roots: Vec<PathBuf>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub mcp_spawn_timeout_ms: u64,
    /// Consecutive transport errors before an MCP server is marked failed.
    pub mcp_failure_threshold: u32,
    /// Base reconnect delay after an MCP server failure.
    pub mcp_reconnect_backoff_sec: u64,
    /// Max external MCP tools activated per `find_tools` search, and the
    /// direct-injection threshold: a fleet larger than this goes lazy.
    pub mcp_activate_max_per_turn: u32,
    pub mcp_max_tool_schema_bytes: usize,
    pub mcp_description_max_chars: usize,
    pub mcp_max_schema_bytes: usize,
    /// C18 MCP registry list endpoint (full path, so a `/v0` -> `/v0.1`
    /// migration is a settings change rather than a rebuild).
    pub registry_mcp_url: String,
    pub registry_timeout_ms: u64,
    pub registry_max_results: u32,
    /// Hosts allowed to install a Skill from. Empty = refuse every remote
    /// install (this is a deny-by-default list, not an "unset means any").
    pub registry_skill_allowed_hosts: Vec<String>,
    pub context_enabled: bool,
    pub context_max_inject_bytes: usize,
    pub context_max_file_bytes: usize,
    /// P1 ledger retrieval. This is independent from project context-file discovery.
    pub context_pack_enabled: bool,
    pub context_pack_token_budget: u32,
    pub context_pack_max_items: u32,
    pub memory_enabled: bool,
    pub memory_max_inject_bytes: usize,
    pub memory_soul_max_bytes: usize,
    pub memory_user_max_bytes: usize,
    pub memory_memory_max_bytes: usize,
    /// Keyword heuristic that turns "记住…" style messages into pending
    /// candidates. Off only means silent: agent `memory_propose` keeps working.
    pub memory_auto_candidates: bool,
    pub corpus_max_hits: u32,
    pub corpus_max_inject_bytes: usize,
    pub corpus_chunk_chars: usize,
    pub corpus_min_disk_free_mb: u64,
    pub corpus_max_file_bytes: usize,
    pub todo_max_items: u32,
    pub todo_max_content_chars: usize,
    pub web_timeout_ms: u64,
    pub web_max_results: u32,
    pub web_max_extract_bytes: usize,
    pub web_max_extract_urls: u32,
    pub web_search_url: String,
    pub web_user_agent: String,
    pub permission_rules: PermissionRules,
    pub delegation_max_depth: u32,
    pub delegation_max_concurrent: u32,
    pub delegation_child_max_iterations: u32,
    pub delegation_child_timeout_sec: u64,
    pub delegation_inherit_shell: bool,
    pub delegation_model: Option<String>,
    pub delegation_summary_max_chars: usize,
    pub delegation_stale_idle_sec: u64,
    pub delegation_stale_in_tool_sec: u64,
    /// H2: children whose workspace is a git checkout run in an isolated
    /// worktree by default; the parent merges or discards explicitly.
    pub delegation_worktree_isolation: bool,
    pub account_recheck_ms: u64,
    pub cron_tick_sec: u64,
    pub cron_max_concurrent: u32,
    pub session_list_limit: u32,
    /// Local sidecar GPU slots. Remote OpenAI/Ollama/vLLM do not consume these.
    pub max_local_concurrency: u32,
    pub kill_sidecar_on_exit: bool,
    pub recommend_quant: Option<String>,
    pub local_model_dir: Option<PathBuf>,
    pub sidecar_ready_timeout_ms: u64,
    /// Optional stable loopback port for local routing clients such as CC Switch.
    pub sidecar_listen_port: Option<u16>,
    /// Loopback port for Dandelion's private CC Switch CLI proxy.
    pub cc_switch_proxy_port: u16,
    pub sidecar_mlx_prompt_cache_bytes: Option<u64>,
    pub sidecar_mlx_kv_bits: Option<u8>,
    /// P2 advisory preflight. Disabled by default to preserve existing turns.
    pub preflight_enabled: bool,
    pub preflight_timeout_ms: u64,
    pub preflight_failure_threshold: u32,
    pub preflight_cooldown_sec: u64,
    /// When true, `dandelion.apiKey` is sealed on disk (DPAPI on Windows, `enc:file` elsewhere).
    pub encrypt_api_key: bool,
    pub ui_window_width: u32,
    pub ui_window_height: u32,
    pub ui_window_maximized: bool,
    /// After a successful prompt, queue one wiki compile (C17). Off by default.
    pub wiki_compile_on_idle: bool,
    pub wiki_max_scan_files: u32,
    pub wiki_max_file_bytes: usize,
    pub wiki_llm_input_bytes: usize,
    pub wiki_llm_max_tokens: u32,
    /// Expose `wiki_propose` to the model (off = agents can neither propose
    /// nor write the wiki; manual distill and the panel keep working).
    pub wiki_proposal_enabled: bool,
    /// Content cap for one wiki proposal / manual edit, in bytes.
    pub wiki_proposal_max_bytes: usize,
    /// Model override for Wiki plan/article turns; "" = session default.
    pub wiki_llm_model: String,
    /// Deep Wiki build: maximum number of articles per build.
    pub wiki_build_max_articles: usize,
    /// Deep Wiki build: evidence-pack byte cap per article turn.
    pub wiki_build_evidence_bytes: usize,
    /// Deep Wiki build: output-token cap per article turn.
    pub wiki_build_article_max_tokens: u32,
    /// Evidence index: size cap for metadata extraction (hash/lines/symbols/
    /// sections) — independent from the body tier `wiki_max_file_bytes`.
    pub wiki_meta_max_file_bytes: usize,
    /// Evidence index: how many discovered files keep metadata (search/QA
    /// reach), on top of the `wiki_max_scan_files` body budget.
    pub wiki_max_index_files: u32,
    /// Deep Wiki build: percent of body-budget slots Doc files may take.
    pub wiki_build_doc_budget_pct: u32,
    /// Deep Wiki build: concurrent article-writing model turns.
    pub wiki_build_concurrency: usize,
    /// Incremental rebuild: added-file floor before a module counts structural.
    pub wiki_structural_new_file_min: usize,
    /// Incremental rebuild: added files as percent of module files to trigger.
    pub wiki_structural_new_file_pct: u32,
    /// Module size at which the looser large-module rule applies.
    pub wiki_structural_large_file_min: usize,
    /// Percent threshold for large modules.
    pub wiki_structural_large_file_pct: u32,
    /// Structural change => ask the model for a plan delta (default on).
    pub wiki_incremental_replan_enabled: bool,
    /// Retries allowed for a malformed plan/delta JSON answer.
    pub wiki_build_plan_retry_max: usize,
    /// Re-evidence rounds per article for failed-citation sentences.
    pub wiki_evidence_refetch_max: usize,
    /// Rewrite rounds per article for failed citations before demotion.
    pub wiki_build_evidence_retry_max: usize,
    /// Immutable page versions kept under `wiki/versions/` (1..=16).
    pub wiki_versions_keep: usize,
    /// On: each Wiki resolution lazily moves a legacy
    /// `wiki/projects/<hash>` tree into its space home
    /// `wiki/spaces/<id>/` (copy → sha256-verify → delete, never before
    /// verification) and creates a space eagerly for a fresh workspace;
    /// Runtime construction additionally rebuilds the workspace→space
    /// registry from the on-disk `origin.json` anchors. Off: spaces are
    /// neither created nor moved — resolution keeps returning the legacy
    /// path exactly as before spaces existed.
    pub wiki_auto_migrate: bool,
    /// Extra gitignore-syntax patterns applied to every Wiki scan on top
    /// of the tree's own `.gitignore` files (anchored / `**` / negation
    /// semantics identical). The escape hatch when a repo's ignore set
    /// hides content the space should index — or when the index should
    /// skip trees the repo itself tracks.
    pub wiki_ignore_patterns: Vec<String>,
    /// Unified space search: max hits per request (1..=50).
    pub space_search_max_hits: u32,
    /// Per-hit RAG excerpt cap, centered on the matching passage.
    pub knowledge_excerpt_bytes: usize,
    /// Unified space ask: evidence byte cap per question.
    pub space_ask_evidence_bytes: usize,
    /// Chat auto-retrieval: max bound spaces searched per turn (1..=8).
    /// Was a hardcoded `take(4)` in the pre-unification chat path.
    pub knowledge_chat_max_spaces: u32,
    /// Generic organizing plan topic bounds (min <= max enforced).
    pub plan_max_topics: usize,
    pub plan_min_topics: usize,
    /// Vision input budget for one user turn (drag/paste images).
    pub vision_max_images: u32,
    /// Decoded-byte cap per attached image.
    pub vision_max_image_bytes: usize,
    /// G6 `dln serve` bind. Must be loopback unless CLI passes explicit non-loopback.
    pub serve_bind: String,
    extra: Map<String, Value>,
}

impl Settings {
    pub fn defaults() -> Self {
        Self {
            base_url: None,
            default_model: None,
            api_key: None,
            default_backend: "openai_compat".into(),
            service_backend: None,
            service_model: None,
            temperature: 0.2,
            max_tokens: 8192,
            dynamic_token_budget: false,
            token_budget_margin_tokens: 256,
            token_budget_min_output_tokens: 1024,
            max_iterations: 90,
            permission_mode: PermissionMode::Ask,
            allow_shell: true,
            workspace: None,
            http_connect_timeout_ms: 30_000,
            sse_idle_timeout_ms: 120_000,
            sidecar_sse_idle_timeout_ms: 900_000,
            retry_max: 5,
            retry_base_ms: 500,
            retry_cap_ms: 8_000,
            terminal_foreground_timeout_sec: 60,
            terminal_foreground_timeout_max_sec: 300,
            terminal_output_max_bytes: 65_536,
            terminal_live_chunk_bytes: 1_024,
            terminal_live_flush_ms: 80,
            file_read_default_limit: 500,
            file_read_max_limit: 2000,
            file_read_max_bytes: 1_048_576,
            file_write_max_bytes: 1_048_576,
            search_max_results: 50,
            search_max_file_bytes: 1_048_576,
            process_log_default_lines: 200,
            permission_timeout_sec: 120,
            sqlite_busy_ms: 5_000,
            bus_capacity: 256,
            thinking_intensity: ThinkingIntensity::Xhigh,
            tools_parallel_independent: true,
            tools_parallel_max: 8,
            tools_start_on_ready: true,
            compact_enabled: true,
            compact_max_per_prompt: 2,
            compact_keep_turns: 4,
            compact_source_max_bytes: 100_000,
            compact_preflight_enabled: false,
            compact_preflight_reserve_tokens: 2048,
            spec_generate_max_iterations: 8,
            spec_adapt_max_iterations: 6,
            spec_max_file_bytes: 65_536,
            spec_execute_max_iterations: 0,
            hooks_timeout_ms: 5_000,
            hooks_enabled: Vec::new(),
            skills_max_inject_bytes: 32_768,
            skills_disabled_ids: Vec::new(),
            sandbox_allow_roots: Vec::new(),
            mcp_servers: Vec::new(),
            mcp_spawn_timeout_ms: 15_000,
            mcp_failure_threshold: 3,
            mcp_reconnect_backoff_sec: 5,
            mcp_activate_max_per_turn: 12,
            mcp_max_tool_schema_bytes: 16_384,
            mcp_description_max_chars: 1_024,
            mcp_max_schema_bytes: 131_072,
            registry_mcp_url: "https://registry.modelcontextprotocol.io/v0/servers".into(),
            registry_timeout_ms: 15_000,
            registry_max_results: 50,
            registry_skill_allowed_hosts: vec![
                "github.com".into(),
                "raw.githubusercontent.com".into(),
                "gitee.com".into(),
            ],
            context_enabled: true,
            context_max_inject_bytes: 32_768,
            context_max_file_bytes: 65_536,
            context_pack_enabled: true,
            context_pack_token_budget: 1_200,
            context_pack_max_items: 12,
            memory_enabled: true,
            memory_max_inject_bytes: 16_384,
            memory_soul_max_bytes: 4_096,
            memory_user_max_bytes: 2_048,
            memory_memory_max_bytes: 4_096,
            memory_auto_candidates: true,
            corpus_max_hits: 8,
            corpus_max_inject_bytes: 16_384,
            corpus_chunk_chars: 800,
            corpus_min_disk_free_mb: 256,
            corpus_max_file_bytes: 1_048_576,
            todo_max_items: 256,
            todo_max_content_chars: 4_000,
            web_timeout_ms: 15_000,
            web_max_results: 5,
            web_max_extract_bytes: 32_768,
            web_max_extract_urls: 5,
            web_search_url: "https://html.duckduckgo.com/html/?q={query}".into(),
            web_user_agent: "Dandelion/0.1".into(),
            permission_rules: PermissionRules::default(),
            delegation_max_depth: 1,
            delegation_max_concurrent: 3,
            delegation_child_max_iterations: 50,
            delegation_child_timeout_sec: 0,
            delegation_inherit_shell: false,
            delegation_model: None,
            delegation_summary_max_chars: 8_000,
            delegation_stale_idle_sec: 450,
            delegation_stale_in_tool_sec: 1_200,
            delegation_worktree_isolation: true,
            account_recheck_ms: 30_000,
            cron_tick_sec: 60,
            cron_max_concurrent: 1,
            session_list_limit: 50,
            max_local_concurrency: 2,
            kill_sidecar_on_exit: true,
            recommend_quant: None,
            local_model_dir: None,
            sidecar_ready_timeout_ms: 120_000,
            sidecar_listen_port: None,
            cc_switch_proxy_port: 15721,
            sidecar_mlx_prompt_cache_bytes: None,
            sidecar_mlx_kv_bits: None,
            preflight_enabled: false,
            preflight_timeout_ms: 500,
            preflight_failure_threshold: 3,
            preflight_cooldown_sec: 60,
            encrypt_api_key: true,
            ui_window_width: 1120,
            ui_window_height: 720,
            ui_window_maximized: false,
            wiki_compile_on_idle: false,
            wiki_max_scan_files: 200,
            wiki_max_file_bytes: 65_536,
            wiki_llm_input_bytes: 12_000,
            // Real-model turns (plan/QA) need headroom: the plan JSON lists
            // up to `wiki_build_max_articles` topics with file assignments
            // and 768 truncated it; the script-model tests never exercised
            // the budget. Still clamped by validate to 128..=8192.
            wiki_llm_max_tokens: 2048,
            wiki_proposal_enabled: true,
            wiki_proposal_max_bytes: 2_000,
            wiki_llm_model: String::new(),
            wiki_build_max_articles: 8,
            wiki_build_evidence_bytes: 49_152,
            wiki_build_article_max_tokens: 4_096,
            // 2 MB covers essentially every hand-written source/doc file;
            // bigger ones stay metadata-less (honest `tooLarge`).
            wiki_meta_max_file_bytes: 2_000_000,
            // Body budget stays 200; the metadata tier widens lookup/QA
            // reach to the 3-4k files a real repo legitimately has.
            wiki_max_index_files: 4000,
            wiki_build_doc_budget_pct: 40,
            wiki_build_concurrency: 1,
            wiki_structural_new_file_min: 2,
            wiki_structural_new_file_pct: 20,
            wiki_structural_large_file_min: 20,
            wiki_structural_large_file_pct: 40,
            wiki_incremental_replan_enabled: true,
            wiki_build_plan_retry_max: 1,
            wiki_evidence_refetch_max: 1,
            wiki_build_evidence_retry_max: 1,
            wiki_versions_keep: 3,
            wiki_auto_migrate: true,
            wiki_ignore_patterns: Vec::new(),
            space_search_max_hits: 8,
            knowledge_excerpt_bytes: 1_200,
            space_ask_evidence_bytes: 32_768,
            knowledge_chat_max_spaces: 4,
            plan_max_topics: 12,
            plan_min_topics: 3,
            vision_max_images: 8,
            vision_max_image_bytes: 12_582_912,
            serve_bind: "127.0.0.1:3927".into(),
            extra: Map::new(),
        }
    }

    pub fn uses_sidecar(&self) -> bool {
        self.default_backend == "sidecar"
    }

    /// For tests: factory defaults plus dummy provider coordinates.
    pub fn defaults_configured() -> Self {
        let mut s = Self::defaults();
        s.base_url = Some("http://127.0.0.1:9/v1".into());
        s.default_model = Some("test-model".into());
        s
    }

    pub fn extra(&self) -> &Map<String, Value> {
        &self.extra
    }

    pub fn is_configured(&self) -> bool {
        if self.uses_sidecar() {
            nonempty(&self.default_model)
        } else {
            nonempty(&self.base_url) && nonempty(&self.default_model)
        }
    }

    pub fn missing_required(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.uses_sidecar() && !nonempty(&self.base_url) {
            missing.push(KEY_BASE_URL);
        }
        if !nonempty(&self.default_model) {
            missing.push(KEY_DEFAULT_MODEL);
        }
        missing
    }

    pub fn load(data_dir: &Path, overlay: &SettingsOverlay) -> Result<Self> {
        let file = read_settings_file(data_dir)?;
        let env = os_dandelion_env();
        let mut settings = Self::merge(file.as_ref(), &env, overlay)?;
        crate::secret::unseal_api_key(&mut settings, data_dir)?;
        Ok(settings)
    }

    /// Decrypt a sealed `dandelion.apiKey` after [`Self::merge`] (file JSON still holds the blob).
    pub fn unseal_secrets(&mut self, data_dir: &Path) -> Result<()> {
        crate::secret::unseal_api_key(self, data_dir)
    }

    pub fn merge(
        file: Option<&Value>,
        env: &HashMap<String, String>,
        overlay: &SettingsOverlay,
    ) -> Result<Self> {
        let mut settings = Self::defaults();
        if let Some(value) = file {
            apply_json(&mut settings, value)?;
        }
        apply_env(&mut settings, env)?;
        apply_overlay(&mut settings, overlay);
        // 窗口几何：夹紧上限到合理范围。
        // 旧 settings.json / env 注入 / DPI 异常都可能让尺寸越界（窗口被拖到比显示器还大）；
        // 启动阶段把越界上限截断，避免 launch 失败或窗口瞬间撑满屏幕。
        // 下限不让 clamp — 0 / 100 这种是配置错误，应当由 validate() 报错暴露。
        settings.ui_window_width = settings.ui_window_width.min(UI_WINDOW_MAX_W);
        settings.ui_window_height = settings.ui_window_height.min(UI_WINDOW_MAX_H);
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(backend) = &self.service_backend {
            if !matches!(backend.as_str(), "openai_compat" | "openai_responses" | "ollama" | "vllm") {
                return Err(Error::Settings(format!("{KEY_SERVICE_BACKEND} must name a model service")));
            }
        }
        for (name, value) in [
            (KEY_HTTP_CONNECT_TIMEOUT_MS, self.http_connect_timeout_ms),
            (KEY_SSE_IDLE_TIMEOUT_MS, self.sse_idle_timeout_ms),
            (
                KEY_SIDECAR_SSE_IDLE_TIMEOUT_MS,
                self.sidecar_sse_idle_timeout_ms,
            ),
            (KEY_RETRY_BASE_MS, self.retry_base_ms),
            (KEY_RETRY_CAP_MS, self.retry_cap_ms),
            (
                KEY_TERMINAL_FOREGROUND_TIMEOUT_SEC,
                self.terminal_foreground_timeout_sec,
            ),
            (
                KEY_TERMINAL_FOREGROUND_TIMEOUT_MAX_SEC,
                self.terminal_foreground_timeout_max_sec,
            ),
            (KEY_PERMISSION_TIMEOUT_SEC, self.permission_timeout_sec),
            (KEY_SQLITE_BUSY_MS, self.sqlite_busy_ms),
        ] {
            if value == 0 {
                return Err(Error::Settings(format!("{name} must be > 0")));
            }
        }
        if self.retry_max == 0 {
            return Err(Error::Settings(format!("{KEY_RETRY_MAX} must be > 0")));
        }
        if self.bus_capacity == 0 {
            return Err(Error::Settings(format!("{KEY_BUS_CAPACITY} must be > 0")));
        }
        if self.tools_parallel_max == 0 {
            return Err(Error::Settings(format!(
                "{KEY_TOOLS_PARALLEL_MAX} must be > 0"
            )));
        }
        if self.compact_max_per_prompt == 0 {
            return Err(Error::Settings(format!(
                "{KEY_COMPACT_MAX_PER_PROMPT} must be > 0"
            )));
        }
        if self.compact_keep_turns == 0 {
            return Err(Error::Settings(format!(
                "{KEY_COMPACT_KEEP_TURNS} must be >= 1"
            )));
        }
        if self.compact_source_max_bytes < 1024 {
            return Err(Error::Settings(format!(
                "{KEY_COMPACT_SOURCE_MAX_BYTES} must be >= 1024"
            )));
        }
        if self.compact_preflight_reserve_tokens < 256 {
            return Err(Error::Settings(format!(
                "{KEY_COMPACT_PREFLIGHT_RESERVE_TOKENS} must be >= 256"
            )));
        }
        if self.spec_generate_max_iterations == 0 {
            return Err(Error::Settings(format!(
                "{KEY_SPEC_GENERATE_MAX_ITERATIONS} must be > 0"
            )));
        }
        if self.spec_adapt_max_iterations == 0 {
            return Err(Error::Settings(format!(
                "{KEY_SPEC_ADAPT_MAX_ITERATIONS} must be > 0"
            )));
        }
        if self.spec_max_file_bytes < 1024 {
            return Err(Error::Settings(format!(
                "{KEY_SPEC_MAX_FILE_BYTES} must be >= 1024"
            )));
        }
        if self.hooks_timeout_ms == 0 {
            return Err(Error::Settings(format!(
                "{KEY_HOOKS_TIMEOUT_MS} must be > 0"
            )));
        }
        if self.mcp_spawn_timeout_ms == 0 {
            return Err(Error::Settings(format!(
                "{KEY_MCP_SPAWN_TIMEOUT_MS} must be > 0"
            )));
        }
        if self.mcp_failure_threshold == 0 {
            return Err(Error::Settings(format!(
                "{KEY_MCP_FAILURE_THRESHOLD} must be > 0"
            )));
        }
        if self.mcp_reconnect_backoff_sec == 0 {
            return Err(Error::Settings(format!(
                "{KEY_MCP_RECONNECT_BACKOFF_SEC} must be > 0"
            )));
        }
        if self.mcp_activate_max_per_turn == 0 {
            return Err(Error::Settings(format!(
                "{KEY_MCP_ACTIVATE_MAX_PER_TURN} must be > 0"
            )));
        }
        if self.skills_max_inject_bytes < 1024 {
            return Err(Error::Settings(format!(
                "{KEY_SKILLS_MAX_INJECT_BYTES} must be >= 1024"
            )));
        }
        if self.context_max_inject_bytes < 1024 {
            return Err(Error::Settings(format!(
                "{KEY_CONTEXT_MAX_INJECT_BYTES} must be >= 1024"
            )));
        }
        if self.context_max_file_bytes < 1024 {
            return Err(Error::Settings(format!(
                "{KEY_CONTEXT_MAX_FILE_BYTES} must be >= 1024"
            )));
        }
        if self.context_pack_token_budget < 64 {
            return Err(Error::Settings(format!(
                "{KEY_CONTEXT_PACK_TOKEN_BUDGET} must be >= 64"
            )));
        }
        if self.context_pack_max_items < 1 || self.context_pack_max_items > 100 {
            return Err(Error::Settings(format!(
                "{KEY_CONTEXT_PACK_MAX_ITEMS} must be 1..=100"
            )));
        }
        if self.mcp_max_tool_schema_bytes < 1024 {
            return Err(Error::Settings(format!(
                "{KEY_MCP_MAX_TOOL_SCHEMA_BYTES} must be >= 1024"
            )));
        }
        if self.mcp_description_max_chars < 64 {
            return Err(Error::Settings(format!(
                "{KEY_MCP_DESCRIPTION_MAX_CHARS} must be >= 64"
            )));
        }
        if self.mcp_max_schema_bytes != 0
            && self.mcp_max_schema_bytes < self.mcp_max_tool_schema_bytes
        {
            return Err(Error::Settings(format!(
                "{KEY_MCP_MAX_SCHEMA_BYTES} must be 0 or >= {KEY_MCP_MAX_TOOL_SCHEMA_BYTES}"
            )));
        }
        if self.registry_timeout_ms == 0 {
            return Err(Error::Settings(format!(
                "{KEY_REGISTRY_TIMEOUT_MS} must be > 0"
            )));
        }
        if self.registry_max_results < 1 || self.registry_max_results > 500 {
            return Err(Error::Settings(format!(
                "{KEY_REGISTRY_MAX_RESULTS} must be 1..=500"
            )));
        }
        if crate::registry::require_secure_url(&self.registry_mcp_url).is_err() {
            return Err(Error::Settings(format!(
                "{KEY_REGISTRY_MCP_URL} must be an https:// URL (http:// only for loopback)"
            )));
        }
        {
            let mut seen = std::collections::HashSet::new();
            for srv in &self.mcp_servers {
                if srv.id.trim().is_empty() {
                    return Err(Error::Settings(format!(
                        "{KEY_MCP_SERVERS} id must be non-empty"
                    )));
                }
                if !seen.insert(srv.id.clone()) {
                    return Err(Error::Settings(format!(
                        "{KEY_MCP_SERVERS} duplicate id: {}",
                        srv.id
                    )));
                }
                let has_cmd = !srv.command.trim().is_empty();
                let has_url = srv.is_remote();
                if has_cmd == has_url {
                    return Err(Error::Settings(format!(
                        "{KEY_MCP_SERVERS} {}: set exactly one of command (stdio) or url (remote HTTP)",
                        srv.id
                    )));
                }
                let t = srv.transport.trim().to_ascii_lowercase();
                if has_url {
                    if !t.is_empty()
                        && t != "http"
                        && t != "sse"
                        && t != "streamablehttp"
                        && t != "streamable-http"
                    {
                        return Err(Error::Settings(format!(
                            "{KEY_MCP_SERVERS} {}: transport must be http or sse (got {t})",
                            srv.id
                        )));
                    }
                } else if !t.is_empty() && t != "stdio" {
                    return Err(Error::Settings(format!(
                        "{KEY_MCP_SERVERS} {}: stdio entries cannot set transport={t}",
                        srv.id
                    )));
                }
            }
        }
        if self.terminal_output_max_bytes == 0
            || self.file_read_default_limit == 0
            || self.file_read_max_limit == 0
            || self.file_read_max_bytes == 0
            || self.file_write_max_bytes == 0
            || self.search_max_results == 0
            || self.search_max_file_bytes == 0
            || self.process_log_default_lines == 0
            || self.max_tokens == 0
            || self.token_budget_min_output_tokens == 0
            || self.max_iterations == 0
        {
            return Err(Error::Settings(
                "numeric limits must be > 0".into(),
            ));
        }
        if self.terminal_foreground_timeout_sec > self.terminal_foreground_timeout_max_sec {
            return Err(Error::Settings(format!(
                "{KEY_TERMINAL_FOREGROUND_TIMEOUT_SEC} must be <= {KEY_TERMINAL_FOREGROUND_TIMEOUT_MAX_SEC}"
            )));
        }
        for (name, value) in [
            (KEY_MEMORY_MAX_INJECT_BYTES, self.memory_max_inject_bytes),
            (KEY_MEMORY_SOUL_MAX_BYTES, self.memory_soul_max_bytes),
            (KEY_MEMORY_USER_MAX_BYTES, self.memory_user_max_bytes),
            (KEY_MEMORY_MEMORY_MAX_BYTES, self.memory_memory_max_bytes),
        ] {
            if value < 256 {
                return Err(Error::Settings(format!("{name} must be >= 256")));
            }
        }
        if self.corpus_max_inject_bytes < 1024 {
            return Err(Error::Settings(format!(
                "{KEY_CORPUS_MAX_INJECT_BYTES} must be >= 1024"
            )));
        }
        if self.corpus_chunk_chars < 256 {
            return Err(Error::Settings(format!(
                "{KEY_CORPUS_CHUNK_CHARS} must be >= 256"
            )));
        }
        if self.corpus_min_disk_free_mb < 1 {
            return Err(Error::Settings(format!(
                "{KEY_CORPUS_MIN_DISK_FREE_MB} must be >= 1"
            )));
        }
        if self.corpus_max_file_bytes < 1024 {
            return Err(Error::Settings(format!(
                "{KEY_CORPUS_MAX_FILE_BYTES} must be >= 1024"
            )));
        }
        if self.todo_max_items < 1 {
            return Err(Error::Settings(format!(
                "{KEY_TODO_MAX_ITEMS} must be >= 1"
            )));
        }
        if self.todo_max_content_chars < 64 {
            return Err(Error::Settings(format!(
                "{KEY_TODO_MAX_CONTENT_CHARS} must be >= 64"
            )));
        }
        if self.web_timeout_ms == 0 {
            return Err(Error::Settings(format!("{KEY_WEB_TIMEOUT_MS} must be > 0")));
        }
        if self.web_max_results < 1 || self.web_max_results > 100 {
            return Err(Error::Settings(format!(
                "{KEY_WEB_MAX_RESULTS} must be 1..100"
            )));
        }
        if self.web_max_extract_bytes < 1024 {
            return Err(Error::Settings(format!(
                "{KEY_WEB_MAX_EXTRACT_BYTES} must be >= 1024"
            )));
        }
        if self.web_max_extract_urls < 1 || self.web_max_extract_urls > 16 {
            return Err(Error::Settings(format!(
                "{KEY_WEB_MAX_EXTRACT_URLS} must be 1..16"
            )));
        }
        if self.web_search_url.trim().is_empty() {
            return Err(Error::Settings(format!(
                "{KEY_WEB_SEARCH_URL} must be non-empty"
            )));
        }
        if self.web_user_agent.trim().is_empty() {
            return Err(Error::Settings(format!(
                "{KEY_WEB_USER_AGENT} must be non-empty"
            )));
        }
        if self.terminal_live_chunk_bytes < 64 {
            return Err(Error::Settings(format!(
                "{KEY_TERMINAL_LIVE_CHUNK_BYTES} must be >= 64"
            )));
        }
        if self.terminal_live_flush_ms == 0 {
            return Err(Error::Settings(format!(
                "{KEY_TERMINAL_LIVE_FLUSH_MS} must be > 0"
            )));
        }
        if self.delegation_max_depth != 1 && self.delegation_max_depth != 2 {
            return Err(Error::Settings(format!(
                "{KEY_DELEGATION_MAX_DEPTH} must be 1 or 2"
            )));
        }
        if self.delegation_max_concurrent < 1 {
            return Err(Error::Settings(format!(
                "{KEY_DELEGATION_MAX_CONCURRENT} must be >= 1"
            )));
        }
        if self.delegation_summary_max_chars < 256 {
            return Err(Error::Settings(format!(
                "{KEY_DELEGATION_SUMMARY_MAX_CHARS} must be >= 256"
            )));
        }
        if self.delegation_stale_idle_sec < 1 {
            return Err(Error::Settings(format!(
                "{KEY_DELEGATION_STALE_IDLE_SEC} must be >= 1"
            )));
        }
        if self.delegation_stale_in_tool_sec < self.delegation_stale_idle_sec {
            return Err(Error::Settings(format!(
                "{KEY_DELEGATION_STALE_IN_TOOL_SEC} must be >= {KEY_DELEGATION_STALE_IDLE_SEC}"
            )));
        }
        if !(1000..=300_000).contains(&self.account_recheck_ms) {
            return Err(Error::Settings(format!("{KEY_ACCOUNT_RECHECK_MS} must be 1000–300000")));
        }
        if self.cron_tick_sec < 1 {
            return Err(Error::Settings(format!("{KEY_CRON_TICK_SEC} must be >= 1")));
        }
        if self.cron_max_concurrent < 1 {
            return Err(Error::Settings(format!(
                "{KEY_CRON_MAX_CONCURRENT} must be >= 1"
            )));
        }
        if self.session_list_limit < 1 || self.session_list_limit > 200 {
            return Err(Error::Settings(format!(
                "{KEY_SESSION_LIST_LIMIT} must be 1..200"
            )));
        }
        if self.max_local_concurrency < 1 {
            return Err(Error::Settings(format!(
                "{KEY_MAX_LOCAL_CONCURRENCY} must be >= 1"
            )));
        }
        if self.sidecar_ready_timeout_ms < 1 {
            return Err(Error::Settings(format!(
                "{KEY_SIDECAR_READY_TIMEOUT_MS} must be > 0"
            )));
        }
        if self.sidecar_listen_port.is_some_and(|port| port < 1024) {
            return Err(Error::Settings(format!("{KEY_SIDECAR_LISTEN_PORT} must be 1024..65535")));
        }
        if self.cc_switch_proxy_port < 1024 {
            return Err(Error::Settings(format!("{KEY_CC_SWITCH_PROXY_PORT} must be 1024..65535")));
        }
        if self.sidecar_mlx_prompt_cache_bytes.is_some_and(|bytes| bytes < 1_048_576) {
            return Err(Error::Settings(format!("{KEY_SIDECAR_MLX_CACHE_BYTES} must be >= 1048576")));
        }
        if self.sidecar_mlx_kv_bits.is_some_and(|bits| bits != 4 && bits != 8) {
            return Err(Error::Settings(format!("{KEY_SIDECAR_MLX_KV_BITS} must be 4 or 8")));
        }
        if self.preflight_timeout_ms < 1 || self.preflight_timeout_ms > 10_000 {
            return Err(Error::Settings(format!(
                "{KEY_PREFLIGHT_TIMEOUT_MS} must be 1..10000"
            )));
        }
        if self.preflight_failure_threshold < 1 {
            return Err(Error::Settings(format!(
                "{KEY_PREFLIGHT_FAILURE_THRESHOLD} must be >= 1"
            )));
        }
        if self.preflight_cooldown_sec < 1 {
            return Err(Error::Settings(format!(
                "{KEY_PREFLIGHT_COOLDOWN_SEC} must be >= 1"
            )));
        }
        if !(UI_WINDOW_MIN_W..=UI_WINDOW_MAX_W).contains(&self.ui_window_width) {
            return Err(Error::Settings(format!(
                "{KEY_UI_WINDOW_WIDTH} must be {}..{}",
                UI_WINDOW_MIN_W, UI_WINDOW_MAX_W
            )));
        }
        if !(UI_WINDOW_MIN_H..=UI_WINDOW_MAX_H).contains(&self.ui_window_height) {
            return Err(Error::Settings(format!(
                "{KEY_UI_WINDOW_HEIGHT} must be {}..{}",
                UI_WINDOW_MIN_H, UI_WINDOW_MAX_H
            )));
        }
        if self.wiki_max_scan_files == 0 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_MAX_SCAN_FILES} must be > 0"
            )));
        }
        if self.wiki_max_file_bytes < 1024 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_MAX_FILE_BYTES} must be >= 1024"
            )));
        }
        if self.wiki_llm_input_bytes < 1024 || self.wiki_llm_input_bytes > 131_072 {
            return Err(Error::Settings(format!("{KEY_WIKI_LLM_INPUT_BYTES} must be 1024..=131072")));
        }
        if self.wiki_llm_max_tokens < 128 || self.wiki_llm_max_tokens > 8192 {
            return Err(Error::Settings(format!("{KEY_WIKI_LLM_MAX_TOKENS} must be 128..=8192")));
        }
        if self.wiki_proposal_max_bytes < 200 || self.wiki_proposal_max_bytes > 16_384 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_PROPOSAL_MAX_BYTES} must be 200..=16384"
            )));
        }
        if self.wiki_build_max_articles == 0 || self.wiki_build_max_articles > 32 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_BUILD_MAX_ARTICLES} must be 1..=32"
            )));
        }
        if self.wiki_build_evidence_bytes < 2_048 || self.wiki_build_evidence_bytes > 262_144 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_BUILD_EVIDENCE_BYTES} must be 2048..=262144"
            )));
        }
        if self.wiki_build_article_max_tokens < 512 || self.wiki_build_article_max_tokens > 16_384
        {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_BUILD_ARTICLE_MAX_TOKENS} must be 512..=16384"
            )));
        }
        if self.wiki_meta_max_file_bytes < 65_536 || self.wiki_meta_max_file_bytes > 16_777_216 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_META_MAX_FILE_BYTES} must be 65536..=16777216"
            )));
        }
        if self.wiki_max_index_files < 50 || self.wiki_max_index_files > 20_000 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_MAX_INDEX_FILES} must be 50..=20000"
            )));
        }
        if self.wiki_build_doc_budget_pct < 5 || self.wiki_build_doc_budget_pct > 80 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_BUILD_DOC_BUDGET_PCT} must be 5..=80"
            )));
        }
        if self.wiki_build_concurrency == 0 || self.wiki_build_concurrency > 4 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_BUILD_CONCURRENCY} must be 1..=4"
            )));
        }
        if self.wiki_structural_new_file_min == 0 || self.wiki_structural_new_file_min > 1000 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_STRUCTURAL_NEW_FILE_MIN} must be 1..=1000"
            )));
        }
        if self.wiki_structural_new_file_pct < 5 || self.wiki_structural_new_file_pct > 100 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_STRUCTURAL_NEW_FILE_PCT} must be 5..=100"
            )));
        }
        if self.wiki_structural_large_file_min < 2 || self.wiki_structural_large_file_min > 10000 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_STRUCTURAL_LARGE_FILE_MIN} must be 2..=10000"
            )));
        }
        if self.wiki_structural_large_file_pct < 5 || self.wiki_structural_large_file_pct > 100 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_STRUCTURAL_LARGE_FILE_PCT} must be 5..=100"
            )));
        }
        if self.wiki_build_plan_retry_max > 2 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_BUILD_PLAN_RETRY_MAX} must be 0..=2"
            )));
        }
        if self.wiki_evidence_refetch_max > 2 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_EVIDENCE_REFETCH_MAX} must be 0..=2"
            )));
        }
        if self.wiki_build_evidence_retry_max > 3 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_BUILD_EVIDENCE_RETRY_MAX} must be 0..=3"
            )));
        }
        if self.wiki_versions_keep == 0 || self.wiki_versions_keep > 16 {
            return Err(Error::Settings(format!(
                "{KEY_WIKI_VERSIONS_KEEP} must be 1..=16"
            )));
        }
        if !(256..=16_384).contains(&self.knowledge_excerpt_bytes) {
            return Err(Error::Settings(format!("{KEY_KNOWLEDGE_EXCERPT_BYTES} must be 256..=16384")));
        }
        if self.space_search_max_hits == 0 || self.space_search_max_hits > 50 {
            return Err(Error::Settings(format!(
                "{KEY_SPACE_SEARCH_MAX_HITS} must be 1..=50"
            )));
        }
        if self.space_ask_evidence_bytes < 4_096 || self.space_ask_evidence_bytes > 262_144 {
            return Err(Error::Settings(format!(
                "{KEY_SPACE_ASK_EVIDENCE_BYTES} must be 4096..=262144"
            )));
        }
        if self.knowledge_chat_max_spaces == 0 || self.knowledge_chat_max_spaces > 8 {
            return Err(Error::Settings(format!(
                "{KEY_KNOWLEDGE_CHAT_MAX_SPACES} must be 1..=8"
            )));
        }
        if self.plan_max_topics == 0 || self.plan_max_topics > 32 {
            return Err(Error::Settings(format!(
                "{KEY_PLAN_MAX_TOPICS} must be 1..=32"
            )));
        }
        if self.plan_min_topics == 0 || self.plan_min_topics > 16 {
            return Err(Error::Settings(format!(
                "{KEY_PLAN_MIN_TOPICS} must be 1..=16"
            )));
        }
        if self.plan_min_topics > self.plan_max_topics {
            return Err(Error::Settings(format!(
                "{KEY_PLAN_MIN_TOPICS} must not exceed {KEY_PLAN_MAX_TOPICS}"
            )));
        }
        if self.vision_max_images == 0 || self.vision_max_images > 16 {
            return Err(Error::Settings(format!("{KEY_VISION_MAX_IMAGES} must be 1..=16")));
        }
        if self.vision_max_image_bytes < 65_536 || self.vision_max_image_bytes > 33_554_432 {
            return Err(Error::Settings(format!("{KEY_VISION_MAX_IMAGE_BYTES} must be 65536..=33554432")));
        }
        if self.serve_bind.trim().is_empty() {
            return Err(Error::Settings(format!(
                "{KEY_SERVE_BIND} must be non-empty"
            )));
        }
        Ok(())
    }

    pub fn child_max_iterations(&self) -> u32 {
        if self.delegation_child_max_iterations == 0 {
            self.max_iterations
        } else {
            self.delegation_child_max_iterations
        }
    }

    pub fn to_json_value(&self) -> Value {
        let mut map = Map::new();
        put_opt_str(&mut map, KEY_BASE_URL, self.base_url.as_deref());
        put_opt_str(&mut map, KEY_DEFAULT_MODEL, self.default_model.as_deref());
        put_opt_str(&mut map, KEY_API_KEY, self.api_key.as_deref());
        map.insert(
            KEY_DEFAULT_BACKEND.into(),
            Value::String(self.default_backend.clone()),
        );
        put_opt_str(&mut map, KEY_SERVICE_BACKEND, self.service_backend.as_deref());
        put_opt_str(&mut map, KEY_SERVICE_MODEL, self.service_model.as_deref());
        map.insert(KEY_TEMPERATURE.into(), json_f64(self.temperature as f64));
        map.insert(KEY_MAX_TOKENS.into(), json_u64(self.max_tokens as u64));
        map.insert(
            KEY_DYNAMIC_TOKEN_BUDGET.into(),
            Value::Bool(self.dynamic_token_budget),
        );
        map.insert(
            KEY_TOKEN_BUDGET_MARGIN_TOKENS.into(),
            json_u64(self.token_budget_margin_tokens as u64),
        );
        map.insert(
            KEY_TOKEN_BUDGET_MIN_OUTPUT_TOKENS.into(),
            json_u64(self.token_budget_min_output_tokens as u64),
        );
        map.insert(
            KEY_MAX_ITERATIONS.into(),
            json_u64(self.max_iterations as u64),
        );
        map.insert(
            KEY_PERMISSION_MODE.into(),
            Value::String(self.permission_mode.as_str().into()),
        );
        map.insert(KEY_ALLOW_SHELL.into(), Value::Bool(self.allow_shell));
        if let Some(ws) = &self.workspace {
            map.insert(
                KEY_WORKSPACE.into(),
                Value::String(ws.to_string_lossy().into_owned()),
            );
        }
        map.insert(
            KEY_HTTP_CONNECT_TIMEOUT_MS.into(),
            json_u64(self.http_connect_timeout_ms),
        );
        map.insert(
            KEY_SSE_IDLE_TIMEOUT_MS.into(),
            json_u64(self.sse_idle_timeout_ms),
        );
        map.insert(
            KEY_SIDECAR_SSE_IDLE_TIMEOUT_MS.into(),
            json_u64(self.sidecar_sse_idle_timeout_ms),
        );
        map.insert(KEY_RETRY_MAX.into(), json_u64(self.retry_max as u64));
        map.insert(KEY_RETRY_BASE_MS.into(), json_u64(self.retry_base_ms));
        map.insert(KEY_RETRY_CAP_MS.into(), json_u64(self.retry_cap_ms));
        map.insert(
            KEY_TERMINAL_FOREGROUND_TIMEOUT_SEC.into(),
            json_u64(self.terminal_foreground_timeout_sec),
        );
        map.insert(
            KEY_TERMINAL_FOREGROUND_TIMEOUT_MAX_SEC.into(),
            json_u64(self.terminal_foreground_timeout_max_sec),
        );
        map.insert(
            KEY_TERMINAL_OUTPUT_MAX_BYTES.into(),
            json_u64(self.terminal_output_max_bytes as u64),
        );
        map.insert(
            KEY_TERMINAL_LIVE_CHUNK_BYTES.into(),
            json_u64(self.terminal_live_chunk_bytes as u64),
        );
        map.insert(
            KEY_TERMINAL_LIVE_FLUSH_MS.into(),
            json_u64(self.terminal_live_flush_ms),
        );
        map.insert(
            KEY_FILE_READ_DEFAULT_LIMIT.into(),
            json_u64(self.file_read_default_limit as u64),
        );
        map.insert(
            KEY_FILE_READ_MAX_LIMIT.into(),
            json_u64(self.file_read_max_limit as u64),
        );
        map.insert(
            KEY_FILE_READ_MAX_BYTES.into(),
            json_u64(self.file_read_max_bytes),
        );
        map.insert(
            KEY_FILE_WRITE_MAX_BYTES.into(),
            json_u64(self.file_write_max_bytes),
        );
        map.insert(
            KEY_SEARCH_MAX_RESULTS.into(),
            json_u64(self.search_max_results as u64),
        );
        map.insert(
            KEY_SEARCH_MAX_FILE_BYTES.into(),
            json_u64(self.search_max_file_bytes),
        );
        map.insert(
            KEY_PROCESS_LOG_DEFAULT_LINES.into(),
            json_u64(self.process_log_default_lines as u64),
        );
        map.insert(
            KEY_PERMISSION_TIMEOUT_SEC.into(),
            json_u64(self.permission_timeout_sec),
        );
        map.insert(KEY_SQLITE_BUSY_MS.into(), json_u64(self.sqlite_busy_ms));
        map.insert(KEY_BUS_CAPACITY.into(), json_u64(self.bus_capacity as u64));
        map.insert(
            KEY_THINKING_INTENSITY.into(),
            Value::String(self.thinking_intensity.as_str().into()),
        );
        map.insert(
            KEY_TOOLS_PARALLEL_INDEPENDENT.into(),
            Value::Bool(self.tools_parallel_independent),
        );
        map.insert(
            KEY_TOOLS_PARALLEL_MAX.into(),
            json_u64(self.tools_parallel_max as u64),
        );
        map.insert(
            KEY_TOOLS_START_ON_READY.into(),
            Value::Bool(self.tools_start_on_ready),
        );
        map.insert(
            KEY_COMPACT_ENABLED.into(),
            Value::Bool(self.compact_enabled),
        );
        map.insert(
            KEY_COMPACT_MAX_PER_PROMPT.into(),
            json_u64(self.compact_max_per_prompt as u64),
        );
        map.insert(
            KEY_COMPACT_KEEP_TURNS.into(),
            json_u64(self.compact_keep_turns as u64),
        );
        map.insert(
            KEY_COMPACT_SOURCE_MAX_BYTES.into(),
            json_u64(self.compact_source_max_bytes as u64),
        );
        map.insert(
            KEY_COMPACT_PREFLIGHT_ENABLED.into(),
            Value::Bool(self.compact_preflight_enabled),
        );
        map.insert(
            KEY_COMPACT_PREFLIGHT_RESERVE_TOKENS.into(),
            json_u64(self.compact_preflight_reserve_tokens as u64),
        );
        map.insert(
            KEY_SPEC_GENERATE_MAX_ITERATIONS.into(),
            json_u64(self.spec_generate_max_iterations as u64),
        );
        map.insert(
            KEY_SPEC_ADAPT_MAX_ITERATIONS.into(),
            json_u64(self.spec_adapt_max_iterations as u64),
        );
        map.insert(
            KEY_SPEC_MAX_FILE_BYTES.into(),
            json_u64(self.spec_max_file_bytes as u64),
        );
        map.insert(
            KEY_SPEC_EXECUTE_MAX_ITERATIONS.into(),
            json_u64(self.spec_execute_max_iterations as u64),
        );
        map.insert(KEY_HOOKS_TIMEOUT_MS.into(), json_u64(self.hooks_timeout_ms));
        map.insert(
            KEY_HOOKS_ENABLED.into(),
            Value::Array(
                self.hooks_enabled
                    .iter()
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        );
        map.insert(
            KEY_SKILLS_MAX_INJECT_BYTES.into(),
            json_u64(self.skills_max_inject_bytes as u64),
        );
        map.insert(
            KEY_SKILLS_DISABLED_IDS.into(),
            Value::Array(
                self.skills_disabled_ids
                    .iter()
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        );
        map.insert(
            KEY_SANDBOX_ALLOW_ROOTS.into(),
            Value::Array(
                self.sandbox_allow_roots
                    .iter()
                    .map(|p| Value::String(p.to_string_lossy().into_owned()))
                    .collect(),
            ),
        );
        map.insert(
            KEY_MCP_SERVERS.into(),
            serde_json::to_value(&self.mcp_servers).unwrap_or(Value::Array(Vec::new())),
        );
        map.insert(
            KEY_MCP_SPAWN_TIMEOUT_MS.into(),
            json_u64(self.mcp_spawn_timeout_ms),
        );
        map.insert(
            KEY_MCP_FAILURE_THRESHOLD.into(),
            json_u64(self.mcp_failure_threshold as u64),
        );
        map.insert(
            KEY_MCP_RECONNECT_BACKOFF_SEC.into(),
            json_u64(self.mcp_reconnect_backoff_sec),
        );
        map.insert(
            KEY_MCP_ACTIVATE_MAX_PER_TURN.into(),
            json_u64(self.mcp_activate_max_per_turn as u64),
        );
        map.insert(
            KEY_MCP_MAX_TOOL_SCHEMA_BYTES.into(),
            json_u64(self.mcp_max_tool_schema_bytes as u64),
        );
        map.insert(
            KEY_MCP_DESCRIPTION_MAX_CHARS.into(),
            json_u64(self.mcp_description_max_chars as u64),
        );
        map.insert(
            KEY_MCP_MAX_SCHEMA_BYTES.into(),
            json_u64(self.mcp_max_schema_bytes as u64),
        );
        map.insert(
            KEY_REGISTRY_MCP_URL.into(),
            Value::String(self.registry_mcp_url.clone()),
        );
        map.insert(
            KEY_REGISTRY_TIMEOUT_MS.into(),
            json_u64(self.registry_timeout_ms),
        );
        map.insert(
            KEY_REGISTRY_MAX_RESULTS.into(),
            json_u64(self.registry_max_results as u64),
        );
        map.insert(
            KEY_REGISTRY_SKILL_ALLOWED_HOSTS.into(),
            Value::Array(
                self.registry_skill_allowed_hosts
                    .iter()
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        );
        map.insert(
            KEY_CONTEXT_ENABLED.into(),
            Value::Bool(self.context_enabled),
        );
        map.insert(
            KEY_CONTEXT_MAX_INJECT_BYTES.into(),
            json_u64(self.context_max_inject_bytes as u64),
        );
        map.insert(
            KEY_CONTEXT_MAX_FILE_BYTES.into(),
            json_u64(self.context_max_file_bytes as u64),
        );
        map.insert(
            KEY_CONTEXT_PACK_ENABLED.into(),
            Value::Bool(self.context_pack_enabled),
        );
        map.insert(
            KEY_CONTEXT_PACK_TOKEN_BUDGET.into(),
            json_u64(self.context_pack_token_budget as u64),
        );
        map.insert(
            KEY_CONTEXT_PACK_MAX_ITEMS.into(),
            json_u64(self.context_pack_max_items as u64),
        );
        map.insert(KEY_MEMORY_ENABLED.into(), Value::Bool(self.memory_enabled));
        map.insert(
            KEY_MEMORY_MAX_INJECT_BYTES.into(),
            json_u64(self.memory_max_inject_bytes as u64),
        );
        map.insert(
            KEY_MEMORY_SOUL_MAX_BYTES.into(),
            json_u64(self.memory_soul_max_bytes as u64),
        );
        map.insert(
            KEY_MEMORY_USER_MAX_BYTES.into(),
            json_u64(self.memory_user_max_bytes as u64),
        );
        map.insert(
            KEY_MEMORY_MEMORY_MAX_BYTES.into(),
            json_u64(self.memory_memory_max_bytes as u64),
        );
        map.insert(
            KEY_MEMORY_AUTO_CANDIDATES.into(),
            Value::Bool(self.memory_auto_candidates),
        );
        map.insert(
            KEY_CORPUS_MAX_HITS.into(),
            json_u64(self.corpus_max_hits as u64),
        );
        map.insert(
            KEY_CORPUS_MAX_INJECT_BYTES.into(),
            json_u64(self.corpus_max_inject_bytes as u64),
        );
        map.insert(
            KEY_CORPUS_CHUNK_CHARS.into(),
            json_u64(self.corpus_chunk_chars as u64),
        );
        map.insert(
            KEY_CORPUS_MIN_DISK_FREE_MB.into(),
            json_u64(self.corpus_min_disk_free_mb),
        );
        map.insert(
            KEY_CORPUS_MAX_FILE_BYTES.into(),
            json_u64(self.corpus_max_file_bytes as u64),
        );
        map.insert(
            KEY_TODO_MAX_ITEMS.into(),
            json_u64(self.todo_max_items as u64),
        );
        map.insert(
            KEY_TODO_MAX_CONTENT_CHARS.into(),
            json_u64(self.todo_max_content_chars as u64),
        );
        map.insert(KEY_WEB_TIMEOUT_MS.into(), json_u64(self.web_timeout_ms));
        map.insert(
            KEY_WEB_MAX_RESULTS.into(),
            json_u64(self.web_max_results as u64),
        );
        map.insert(
            KEY_WEB_MAX_EXTRACT_BYTES.into(),
            json_u64(self.web_max_extract_bytes as u64),
        );
        map.insert(
            KEY_WEB_MAX_EXTRACT_URLS.into(),
            json_u64(self.web_max_extract_urls as u64),
        );
        map.insert(
            KEY_WEB_SEARCH_URL.into(),
            Value::String(self.web_search_url.clone()),
        );
        map.insert(
            KEY_WEB_USER_AGENT.into(),
            Value::String(self.web_user_agent.clone()),
        );
        map.insert(
            KEY_PERMISSION_RULES.into(),
            serde_json::to_value(&self.permission_rules).unwrap_or(json_rules_empty()),
        );
        map.insert(
            KEY_DELEGATION_MAX_DEPTH.into(),
            json_u64(self.delegation_max_depth as u64),
        );
        map.insert(
            KEY_DELEGATION_MAX_CONCURRENT.into(),
            json_u64(self.delegation_max_concurrent as u64),
        );
        map.insert(
            KEY_DELEGATION_CHILD_MAX_ITERATIONS.into(),
            json_u64(self.delegation_child_max_iterations as u64),
        );
        map.insert(
            KEY_DELEGATION_CHILD_TIMEOUT_SEC.into(),
            json_u64(self.delegation_child_timeout_sec),
        );
        map.insert(
            KEY_DELEGATION_INHERIT_SHELL.into(),
            Value::Bool(self.delegation_inherit_shell),
        );
        put_opt_str(
            &mut map,
            KEY_DELEGATION_MODEL,
            self.delegation_model.as_deref(),
        );
        map.insert(
            KEY_DELEGATION_SUMMARY_MAX_CHARS.into(),
            json_u64(self.delegation_summary_max_chars as u64),
        );
        map.insert(
            KEY_DELEGATION_STALE_IDLE_SEC.into(),
            json_u64(self.delegation_stale_idle_sec),
        );
        map.insert(
            KEY_DELEGATION_STALE_IN_TOOL_SEC.into(),
            json_u64(self.delegation_stale_in_tool_sec),
        );
        map.insert(
            KEY_DELEGATION_WORKTREE_ISOLATION.into(),
            Value::Bool(self.delegation_worktree_isolation),
        );
        map.insert(KEY_ACCOUNT_RECHECK_MS.into(), json_u64(self.account_recheck_ms));
        map.insert(KEY_CRON_TICK_SEC.into(), json_u64(self.cron_tick_sec));
        map.insert(
            KEY_CRON_MAX_CONCURRENT.into(),
            json_u64(self.cron_max_concurrent as u64),
        );
        map.insert(
            KEY_SESSION_LIST_LIMIT.into(),
            json_u64(self.session_list_limit as u64),
        );
        map.insert(
            KEY_MAX_LOCAL_CONCURRENCY.into(),
            json_u64(self.max_local_concurrency as u64),
        );
        map.insert(
            KEY_KILL_SIDECAR_ON_EXIT.into(),
            Value::Bool(self.kill_sidecar_on_exit),
        );
        put_opt_str(
            &mut map,
            KEY_RECOMMEND_QUANT,
            self.recommend_quant.as_deref(),
        );
        if let Some(dir) = &self.local_model_dir {
            map.insert(
                KEY_LOCAL_MODEL_DIR.into(),
                Value::String(dir.to_string_lossy().into_owned()),
            );
        }
        map.insert(
            KEY_SIDECAR_READY_TIMEOUT_MS.into(),
            json_u64(self.sidecar_ready_timeout_ms),
        );
        if let Some(port) = self.sidecar_listen_port {
            map.insert(KEY_SIDECAR_LISTEN_PORT.into(), json_u64(port as u64));
        }
        map.insert(KEY_CC_SWITCH_PROXY_PORT.into(), json_u64(self.cc_switch_proxy_port as u64));
        if let Some(bytes) = self.sidecar_mlx_prompt_cache_bytes {
            map.insert(KEY_SIDECAR_MLX_CACHE_BYTES.into(), json_u64(bytes));
        }
        if let Some(bits) = self.sidecar_mlx_kv_bits {
            map.insert(KEY_SIDECAR_MLX_KV_BITS.into(), json_u64(bits as u64));
        }
        map.insert(
            KEY_PREFLIGHT_ENABLED.into(),
            Value::Bool(self.preflight_enabled),
        );
        map.insert(
            KEY_PREFLIGHT_TIMEOUT_MS.into(),
            json_u64(self.preflight_timeout_ms),
        );
        map.insert(
            KEY_PREFLIGHT_FAILURE_THRESHOLD.into(),
            json_u64(self.preflight_failure_threshold as u64),
        );
        map.insert(
            KEY_PREFLIGHT_COOLDOWN_SEC.into(),
            json_u64(self.preflight_cooldown_sec),
        );
        map.insert(
            KEY_ENCRYPT_API_KEY.into(),
            Value::Bool(self.encrypt_api_key),
        );
        map.insert(
            KEY_UI_WINDOW_WIDTH.into(),
            json_u64(self.ui_window_width as u64),
        );
        map.insert(
            KEY_UI_WINDOW_HEIGHT.into(),
            json_u64(self.ui_window_height as u64),
        );
        map.insert(
            KEY_UI_WINDOW_MAXIMIZED.into(),
            Value::Bool(self.ui_window_maximized),
        );
        map.insert(
            KEY_WIKI_COMPILE_ON_IDLE.into(),
            Value::Bool(self.wiki_compile_on_idle),
        );
        map.insert(
            KEY_WIKI_MAX_SCAN_FILES.into(),
            json_u64(self.wiki_max_scan_files as u64),
        );
        map.insert(
            KEY_WIKI_MAX_FILE_BYTES.into(),
            json_u64(self.wiki_max_file_bytes as u64),
        );
        map.insert(KEY_WIKI_LLM_INPUT_BYTES.into(), json_u64(self.wiki_llm_input_bytes as u64));
        map.insert(KEY_WIKI_LLM_MAX_TOKENS.into(), json_u64(self.wiki_llm_max_tokens as u64));
        map.insert(KEY_WIKI_LLM_MODEL.into(), Value::String(self.wiki_llm_model.clone()));
        map.insert(
            KEY_WIKI_BUILD_MAX_ARTICLES.into(),
            json_u64(self.wiki_build_max_articles as u64),
        );
        map.insert(
            KEY_WIKI_BUILD_EVIDENCE_BYTES.into(),
            json_u64(self.wiki_build_evidence_bytes as u64),
        );
        map.insert(
            KEY_WIKI_BUILD_ARTICLE_MAX_TOKENS.into(),
            json_u64(self.wiki_build_article_max_tokens as u64),
        );
        map.insert(
            KEY_WIKI_META_MAX_FILE_BYTES.into(),
            json_u64(self.wiki_meta_max_file_bytes as u64),
        );
        map.insert(
            KEY_WIKI_MAX_INDEX_FILES.into(),
            json_u64(self.wiki_max_index_files as u64),
        );
        map.insert(
            KEY_WIKI_BUILD_DOC_BUDGET_PCT.into(),
            json_u64(self.wiki_build_doc_budget_pct as u64),
        );
        map.insert(
            KEY_WIKI_BUILD_CONCURRENCY.into(),
            json_u64(self.wiki_build_concurrency as u64),
        );
        map.insert(
            KEY_WIKI_STRUCTURAL_NEW_FILE_MIN.into(),
            json_u64(self.wiki_structural_new_file_min as u64),
        );
        map.insert(
            KEY_WIKI_STRUCTURAL_NEW_FILE_PCT.into(),
            json_u64(self.wiki_structural_new_file_pct as u64),
        );
        map.insert(
            KEY_WIKI_STRUCTURAL_LARGE_FILE_MIN.into(),
            json_u64(self.wiki_structural_large_file_min as u64),
        );
        map.insert(
            KEY_WIKI_STRUCTURAL_LARGE_FILE_PCT.into(),
            json_u64(self.wiki_structural_large_file_pct as u64),
        );
        map.insert(
            KEY_WIKI_INCREMENTAL_REPLAN_ENABLED.into(),
            Value::Bool(self.wiki_incremental_replan_enabled),
        );
        map.insert(
            KEY_WIKI_BUILD_PLAN_RETRY_MAX.into(),
            json_u64(self.wiki_build_plan_retry_max as u64),
        );
        map.insert(
            KEY_WIKI_EVIDENCE_REFETCH_MAX.into(),
            json_u64(self.wiki_evidence_refetch_max as u64),
        );
        map.insert(
            KEY_WIKI_BUILD_EVIDENCE_RETRY_MAX.into(),
            json_u64(self.wiki_build_evidence_retry_max as u64),
        );
        map.insert(
            KEY_WIKI_VERSIONS_KEEP.into(),
            json_u64(self.wiki_versions_keep as u64),
        );
        map.insert(
            KEY_WIKI_AUTO_MIGRATE.into(),
            Value::Bool(self.wiki_auto_migrate),
        );
        map.insert(
            KEY_WIKI_IGNORE_PATTERNS.into(),
            Value::Array(
                self.wiki_ignore_patterns
                    .iter()
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        );
        map.insert(KEY_KNOWLEDGE_EXCERPT_BYTES.into(), json_u64(self.knowledge_excerpt_bytes as u64));
        map.insert(
            KEY_SPACE_SEARCH_MAX_HITS.into(),
            json_u64(self.space_search_max_hits as u64),
        );
        map.insert(
            KEY_SPACE_ASK_EVIDENCE_BYTES.into(),
            json_u64(self.space_ask_evidence_bytes as u64),
        );
        map.insert(
            KEY_KNOWLEDGE_CHAT_MAX_SPACES.into(),
            json_u64(self.knowledge_chat_max_spaces as u64),
        );
        map.insert(KEY_PLAN_MAX_TOPICS.into(), json_u64(self.plan_max_topics as u64));
        map.insert(KEY_PLAN_MIN_TOPICS.into(), json_u64(self.plan_min_topics as u64));
        map.insert(
            KEY_WIKI_PROPOSAL_ENABLED.into(),
            Value::Bool(self.wiki_proposal_enabled),
        );
        map.insert(
            KEY_WIKI_PROPOSAL_MAX_BYTES.into(),
            json_u64(self.wiki_proposal_max_bytes as u64),
        );
        map.insert(KEY_VISION_MAX_IMAGES.into(), json_u64(self.vision_max_images as u64));
        map.insert(KEY_VISION_MAX_IMAGE_BYTES.into(), json_u64(self.vision_max_image_bytes as u64));
        map.insert(
            KEY_SERVE_BIND.into(),
            Value::String(self.serve_bind.clone()),
        );
        for (k, v) in &self.extra {
            map.insert(k.clone(), v.clone());
        }
        Value::Object(map)
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let mut map = match self.to_json_value() {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        crate::secret::seal_api_key_in_map(&mut map, self.encrypt_api_key, data_dir)?;
        write_settings_map(data_dir, map)
    }

    /// Merge `patch` into `{data_dir}/settings.json` without dropping unknown keys.
    pub fn patch_file(data_dir: &Path, patch: &Map<String, Value>) -> Result<()> {
        Self::merge_patch_and_write(data_dir, patch, &SettingsOverlay::default()).map(|_| ())
    }

    /// Apply `patch` in memory, validate, then write. Invalid values leave the file untouched.
    pub fn merge_patch_and_write(
        data_dir: &Path,
        patch: &Map<String, Value>,
        overlay: &SettingsOverlay,
    ) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let mut file = match read_settings_file(data_dir)? {
            Some(Value::Object(map)) => map,
            None => Map::new(),
            Some(_) => {
                return Err(Error::Settings(
                    "settings.json must be a JSON object".into(),
                ))
            }
        };
        for (k, v) in patch {
            if v.is_null() {
                file.remove(k);
            } else {
                file.insert(k.clone(), v.clone());
            }
        }
        let env = os_dandelion_env();
        let mut settings = Self::merge(Some(&Value::Object(file.clone())), &env, overlay)?;
        crate::secret::unseal_api_key(&mut settings, data_dir)?;
        crate::secret::seal_api_key_in_map(&mut file, settings.encrypt_api_key, data_dir)?;
        write_settings_map(data_dir, file)?;
        Ok(settings)
    }

    /// Replace the file with `obj` after validation. Invalid JSON objects do not write.
    pub fn replace_file_object(
        data_dir: &Path,
        obj: Map<String, Value>,
        overlay: &SettingsOverlay,
    ) -> Result<Self> {
        let env = os_dandelion_env();
        let mut settings = Self::merge(Some(&Value::Object(obj.clone())), &env, overlay)?;
        crate::secret::unseal_api_key(&mut settings, data_dir)?;
        let mut obj = obj;
        crate::secret::seal_api_key_in_map(&mut obj, settings.encrypt_api_key, data_dir)?;
        write_settings_map(data_dir, obj)?;
        Ok(settings)
    }

    /// Write factory defaults if `{data_dir}/settings.json` is missing. Does not overwrite.
    pub fn write_template_if_absent(data_dir: &Path) -> Result<bool> {
        std::fs::create_dir_all(data_dir)?;
        let path = settings_path(data_dir);
        if path.exists() {
            return Ok(false);
        }
        Self::defaults().save(data_dir)?;
        Ok(true)
    }

    pub fn api_key_configured(&self) -> bool {
        nonempty(&self.api_key)
    }

    pub fn normalize_base_url(raw: &str) -> String {
        let mut s = raw.trim().trim_end_matches('/').to_string();
        if let Some(stripped) = s.strip_suffix("/chat/completions") {
            s = stripped.trim_end_matches('/').to_string();
        }
        if let Some(stripped) = s.strip_suffix("/models") {
            s = stripped.trim_end_matches('/').to_string();
        }
        s
    }
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Settings")
            .field("base_url", &self.base_url)
            .field("default_model", &self.default_model)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("temperature", &self.temperature)
            .field("thinking_intensity", &self.thinking_intensity)
            .finish_non_exhaustive()
    }
}

pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

fn write_settings_map(data_dir: &Path, map: Map<String, Value>) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let json = serde_json::to_string_pretty(&Value::Object(map))?;
    std::fs::write(settings_path(data_dir), json)?;
    Ok(())
}

fn read_settings_file(data_dir: &Path) -> Result<Option<Value>> {
    let path = settings_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)?;
    let value: Value = serde_json::from_str(&text).map_err(|err| {
        Error::Settings(format!("invalid settings.json ({}): {err}", path.display()))
    })?;
    if !value.is_object() {
        return Err(Error::Settings(
            "settings.json must be a JSON object".into(),
        ));
    }
    Ok(Some(value))
}

fn os_dandelion_env() -> HashMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| k.starts_with("DANDELION_"))
        .collect()
}

fn apply_json(settings: &mut Settings, value: &Value) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| Error::Settings("settings.json must be a JSON object".into()))?;
    for (key, val) in obj {
        match key.as_str() {
            KEY_BASE_URL => settings.base_url = opt_string(key, val)?,
            KEY_DEFAULT_MODEL => settings.default_model = opt_string(key, val)?,
            KEY_API_KEY => settings.api_key = opt_string(key, val)?,
            KEY_DEFAULT_BACKEND => {
                settings.default_backend = req_string(key, val)?;
            }
            KEY_SERVICE_BACKEND => settings.service_backend = opt_string(key, val)?,
            KEY_SERVICE_MODEL => settings.service_model = opt_string(key, val)?,
            KEY_TEMPERATURE => settings.temperature = req_f32(key, val)?,
            KEY_MAX_TOKENS => settings.max_tokens = req_u32(key, val)?,
            KEY_DYNAMIC_TOKEN_BUDGET => settings.dynamic_token_budget = req_bool(key, val)?,
            KEY_TOKEN_BUDGET_MARGIN_TOKENS => {
                settings.token_budget_margin_tokens = req_u32(key, val)?;
            }
            KEY_TOKEN_BUDGET_MIN_OUTPUT_TOKENS => {
                settings.token_budget_min_output_tokens = req_u32(key, val)?;
            }
            KEY_MAX_ITERATIONS => settings.max_iterations = req_u32(key, val)?,
            KEY_PERMISSION_MODE => {
                settings.permission_mode = PermissionMode::parse(&req_string(key, val)?)?;
            }
            KEY_ALLOW_SHELL => settings.allow_shell = req_bool(key, val)?,
            KEY_WORKSPACE => {
                settings.workspace = opt_string(key, val)?.map(PathBuf::from);
            }
            KEY_HTTP_CONNECT_TIMEOUT_MS => {
                settings.http_connect_timeout_ms = req_u64(key, val)?;
            }
            KEY_SSE_IDLE_TIMEOUT_MS => settings.sse_idle_timeout_ms = req_u64(key, val)?,
            KEY_SIDECAR_SSE_IDLE_TIMEOUT_MS => {
                settings.sidecar_sse_idle_timeout_ms = req_u64(key, val)?;
            }
            KEY_RETRY_MAX => settings.retry_max = req_u32(key, val)?,
            KEY_RETRY_BASE_MS => settings.retry_base_ms = req_u64(key, val)?,
            KEY_RETRY_CAP_MS => settings.retry_cap_ms = req_u64(key, val)?,
            KEY_TERMINAL_FOREGROUND_TIMEOUT_SEC => {
                settings.terminal_foreground_timeout_sec = req_u64(key, val)?;
            }
            KEY_TERMINAL_FOREGROUND_TIMEOUT_MAX_SEC => {
                settings.terminal_foreground_timeout_max_sec = req_u64(key, val)?;
            }
            KEY_TERMINAL_OUTPUT_MAX_BYTES => {
                settings.terminal_output_max_bytes = req_usize(key, val)?;
            }
            KEY_TERMINAL_LIVE_CHUNK_BYTES => {
                settings.terminal_live_chunk_bytes = req_usize(key, val)?;
            }
            KEY_TERMINAL_LIVE_FLUSH_MS => {
                settings.terminal_live_flush_ms = req_u64(key, val)?;
            }
            KEY_FILE_READ_DEFAULT_LIMIT => {
                settings.file_read_default_limit = req_u32(key, val)?;
            }
            KEY_FILE_READ_MAX_LIMIT => settings.file_read_max_limit = req_u32(key, val)?,
            KEY_FILE_READ_MAX_BYTES => settings.file_read_max_bytes = req_u64(key, val)?,
            KEY_FILE_WRITE_MAX_BYTES => settings.file_write_max_bytes = req_u64(key, val)?,
            KEY_SEARCH_MAX_RESULTS => settings.search_max_results = req_u32(key, val)?,
            KEY_SEARCH_MAX_FILE_BYTES => settings.search_max_file_bytes = req_u64(key, val)?,
            KEY_PROCESS_LOG_DEFAULT_LINES => {
                settings.process_log_default_lines = req_u32(key, val)?;
            }
            KEY_PERMISSION_TIMEOUT_SEC => {
                settings.permission_timeout_sec = req_u64(key, val)?;
            }
            KEY_SQLITE_BUSY_MS => settings.sqlite_busy_ms = req_u64(key, val)?,
            KEY_BUS_CAPACITY => settings.bus_capacity = req_usize(key, val)?,
            KEY_THINKING_INTENSITY => {
                settings.thinking_intensity = ThinkingIntensity::parse(&req_string(key, val)?)?;
            }
            KEY_TOOLS_PARALLEL_INDEPENDENT => {
                settings.tools_parallel_independent = req_bool(key, val)?;
            }
            KEY_TOOLS_PARALLEL_MAX => {
                settings.tools_parallel_max = req_usize(key, val)?;
            }
            KEY_TOOLS_START_ON_READY => {
                settings.tools_start_on_ready = req_bool(key, val)?;
            }
            KEY_COMPACT_ENABLED => settings.compact_enabled = req_bool(key, val)?,
            KEY_COMPACT_MAX_PER_PROMPT => {
                settings.compact_max_per_prompt = req_u32(key, val)?;
            }
            KEY_COMPACT_KEEP_TURNS => settings.compact_keep_turns = req_u32(key, val)?,
            KEY_COMPACT_SOURCE_MAX_BYTES => {
                settings.compact_source_max_bytes = req_usize(key, val)?;
            }
            KEY_COMPACT_PREFLIGHT_ENABLED => {
                settings.compact_preflight_enabled = req_bool(key, val)?;
            }
            KEY_COMPACT_PREFLIGHT_RESERVE_TOKENS => {
                settings.compact_preflight_reserve_tokens = req_u32(key, val)?;
            }
            KEY_SPEC_GENERATE_MAX_ITERATIONS => {
                settings.spec_generate_max_iterations = req_u32(key, val)?;
            }
            KEY_SPEC_ADAPT_MAX_ITERATIONS => {
                settings.spec_adapt_max_iterations = req_u32(key, val)?;
            }
            KEY_SPEC_MAX_FILE_BYTES => {
                settings.spec_max_file_bytes = req_usize(key, val)?;
            }
            KEY_SPEC_EXECUTE_MAX_ITERATIONS => {
                settings.spec_execute_max_iterations = req_u32(key, val)?;
            }
            KEY_HOOKS_TIMEOUT_MS => settings.hooks_timeout_ms = req_u64(key, val)?,
            KEY_HOOKS_ENABLED => settings.hooks_enabled = req_string_array(key, val)?,
            KEY_SKILLS_MAX_INJECT_BYTES => {
                settings.skills_max_inject_bytes = req_usize(key, val)?;
            }
            KEY_SKILLS_DISABLED_IDS => {
                settings.skills_disabled_ids = req_string_array(key, val)?;
            }
            KEY_SANDBOX_ALLOW_ROOTS => {
                settings.sandbox_allow_roots = parse_path_list(key, val)?;
            }
            KEY_MCP_SERVERS => settings.mcp_servers = parse_mcp_servers(val)?,
            KEY_MCP_SPAWN_TIMEOUT_MS => settings.mcp_spawn_timeout_ms = req_u64(key, val)?,
            KEY_MCP_FAILURE_THRESHOLD => {
                settings.mcp_failure_threshold = req_u32(key, val)?;
            }
            KEY_MCP_RECONNECT_BACKOFF_SEC => {
                settings.mcp_reconnect_backoff_sec = req_u64(key, val)?;
            }
            KEY_MCP_ACTIVATE_MAX_PER_TURN => {
                settings.mcp_activate_max_per_turn = req_u32(key, val)?;
            }
            KEY_MCP_MAX_TOOL_SCHEMA_BYTES => {
                settings.mcp_max_tool_schema_bytes = req_usize(key, val)?;
            }
            KEY_MCP_DESCRIPTION_MAX_CHARS => {
                settings.mcp_description_max_chars = req_usize(key, val)?;
            }
            KEY_MCP_MAX_SCHEMA_BYTES => settings.mcp_max_schema_bytes = req_usize(key, val)?,
            KEY_REGISTRY_MCP_URL => settings.registry_mcp_url = req_string(key, val)?,
            KEY_REGISTRY_TIMEOUT_MS => settings.registry_timeout_ms = req_u64(key, val)?,
            KEY_REGISTRY_MAX_RESULTS => settings.registry_max_results = req_u32(key, val)?,
            KEY_REGISTRY_SKILL_ALLOWED_HOSTS => {
                settings.registry_skill_allowed_hosts = req_string_array(key, val)?;
            }
            KEY_CONTEXT_ENABLED => settings.context_enabled = req_bool(key, val)?,
            KEY_CONTEXT_MAX_INJECT_BYTES => {
                settings.context_max_inject_bytes = req_usize(key, val)?;
            }
            KEY_CONTEXT_MAX_FILE_BYTES => settings.context_max_file_bytes = req_usize(key, val)?,
            KEY_CONTEXT_PACK_ENABLED => settings.context_pack_enabled = req_bool(key, val)?,
            KEY_CONTEXT_PACK_TOKEN_BUDGET => {
                settings.context_pack_token_budget = req_u32(key, val)?
            }
            KEY_CONTEXT_PACK_MAX_ITEMS => settings.context_pack_max_items = req_u32(key, val)?,
            KEY_MEMORY_ENABLED => settings.memory_enabled = req_bool(key, val)?,
            KEY_MEMORY_MAX_INJECT_BYTES => {
                settings.memory_max_inject_bytes = req_usize(key, val)?;
            }
            KEY_MEMORY_SOUL_MAX_BYTES => settings.memory_soul_max_bytes = req_usize(key, val)?,
            KEY_MEMORY_USER_MAX_BYTES => settings.memory_user_max_bytes = req_usize(key, val)?,
            KEY_MEMORY_MEMORY_MAX_BYTES => {
                settings.memory_memory_max_bytes = req_usize(key, val)?;
            }
            KEY_MEMORY_AUTO_CANDIDATES => settings.memory_auto_candidates = req_bool(key, val)?,
            KEY_CORPUS_MAX_HITS => settings.corpus_max_hits = req_u32(key, val)?,
            KEY_CORPUS_MAX_INJECT_BYTES => {
                settings.corpus_max_inject_bytes = req_usize(key, val)?;
            }
            KEY_CORPUS_CHUNK_CHARS => settings.corpus_chunk_chars = req_usize(key, val)?,
            KEY_CORPUS_MIN_DISK_FREE_MB => {
                settings.corpus_min_disk_free_mb = req_u64(key, val)?;
            }
            KEY_CORPUS_MAX_FILE_BYTES => settings.corpus_max_file_bytes = req_usize(key, val)?,
            KEY_TODO_MAX_ITEMS => settings.todo_max_items = req_u32(key, val)?,
            KEY_TODO_MAX_CONTENT_CHARS => settings.todo_max_content_chars = req_usize(key, val)?,
            KEY_WEB_TIMEOUT_MS => settings.web_timeout_ms = req_u64(key, val)?,
            KEY_WEB_MAX_RESULTS => settings.web_max_results = req_u32(key, val)?,
            KEY_WEB_MAX_EXTRACT_BYTES => settings.web_max_extract_bytes = req_usize(key, val)?,
            KEY_WEB_MAX_EXTRACT_URLS => settings.web_max_extract_urls = req_u32(key, val)?,
            KEY_WEB_SEARCH_URL => settings.web_search_url = req_string(key, val)?,
            KEY_WEB_USER_AGENT => settings.web_user_agent = req_string(key, val)?,
            KEY_PERMISSION_RULES => settings.permission_rules = parse_permission_rules(val)?,
            KEY_DELEGATION_MAX_DEPTH => {
                settings.delegation_max_depth = clamp_max_depth(req_u32(key, val)?);
            }
            KEY_DELEGATION_MAX_CONCURRENT => {
                settings.delegation_max_concurrent = req_u32(key, val)?;
            }
            KEY_DELEGATION_CHILD_MAX_ITERATIONS => {
                settings.delegation_child_max_iterations = req_u32(key, val)?;
            }
            KEY_DELEGATION_CHILD_TIMEOUT_SEC => {
                settings.delegation_child_timeout_sec = req_u64(key, val)?;
            }
            KEY_DELEGATION_INHERIT_SHELL => {
                settings.delegation_inherit_shell = req_bool(key, val)?;
            }
            KEY_DELEGATION_MODEL => settings.delegation_model = opt_string(key, val)?,
            KEY_DELEGATION_SUMMARY_MAX_CHARS => {
                settings.delegation_summary_max_chars = req_usize(key, val)?;
            }
            KEY_DELEGATION_STALE_IDLE_SEC => {
                settings.delegation_stale_idle_sec = req_u64(key, val)?;
            }
            KEY_DELEGATION_STALE_IN_TOOL_SEC => {
                settings.delegation_stale_in_tool_sec = req_u64(key, val)?;
            }
            KEY_DELEGATION_WORKTREE_ISOLATION => {
                settings.delegation_worktree_isolation = req_bool(key, val)?;
            }
            KEY_ACCOUNT_RECHECK_MS => settings.account_recheck_ms = req_u64(key, val)?,
            KEY_CRON_TICK_SEC => settings.cron_tick_sec = req_u64(key, val)?,
            KEY_CRON_MAX_CONCURRENT => settings.cron_max_concurrent = req_u32(key, val)?,
            KEY_SESSION_LIST_LIMIT => settings.session_list_limit = req_u32(key, val)?,
            KEY_MAX_LOCAL_CONCURRENCY => settings.max_local_concurrency = req_u32(key, val)?,
            KEY_KILL_SIDECAR_ON_EXIT => settings.kill_sidecar_on_exit = req_bool(key, val)?,
            KEY_RECOMMEND_QUANT => {
                settings.recommend_quant = opt_string(key, val)?;
            }
            KEY_LOCAL_MODEL_DIR => {
                settings.local_model_dir = opt_string(key, val)?.map(PathBuf::from);
            }
            KEY_SIDECAR_READY_TIMEOUT_MS => {
                settings.sidecar_ready_timeout_ms = req_u64(key, val)?;
            }
            KEY_SIDECAR_LISTEN_PORT => {
                let port = req_u64(key, val)?;
                settings.sidecar_listen_port = Some(u16::try_from(port).map_err(|_| Error::Settings(format!("{key} must be 1024..65535")))?);
            }
            KEY_CC_SWITCH_PROXY_PORT => {
                settings.cc_switch_proxy_port = u16::try_from(req_u64(key, val)?)
                    .map_err(|_| Error::Settings(format!("{key} must be 1024..65535")))?;
            }
            KEY_SIDECAR_MLX_CACHE_BYTES => settings.sidecar_mlx_prompt_cache_bytes = Some(req_u64(key, val)?),
            KEY_SIDECAR_MLX_KV_BITS => settings.sidecar_mlx_kv_bits = Some(u8::try_from(req_u64(key, val)?)
                .map_err(|_| Error::Settings(format!("{key} must be 4 or 8")))?),
            KEY_PREFLIGHT_ENABLED => settings.preflight_enabled = req_bool(key, val)?,
            KEY_PREFLIGHT_TIMEOUT_MS => settings.preflight_timeout_ms = req_u64(key, val)?,
            KEY_PREFLIGHT_FAILURE_THRESHOLD => {
                settings.preflight_failure_threshold = req_u32(key, val)?
            }
            KEY_PREFLIGHT_COOLDOWN_SEC => settings.preflight_cooldown_sec = req_u64(key, val)?,
            KEY_ENCRYPT_API_KEY => settings.encrypt_api_key = req_bool(key, val)?,
            KEY_UI_WINDOW_WIDTH => settings.ui_window_width = req_u32(key, val)?,
            KEY_UI_WINDOW_HEIGHT => settings.ui_window_height = req_u32(key, val)?,
            KEY_UI_WINDOW_MAXIMIZED => settings.ui_window_maximized = req_bool(key, val)?,
            KEY_WIKI_COMPILE_ON_IDLE => settings.wiki_compile_on_idle = req_bool(key, val)?,
            KEY_WIKI_MAX_SCAN_FILES => settings.wiki_max_scan_files = req_u32(key, val)?,
            KEY_WIKI_MAX_FILE_BYTES => settings.wiki_max_file_bytes = req_usize(key, val)?,
            KEY_WIKI_LLM_INPUT_BYTES => settings.wiki_llm_input_bytes = req_usize(key, val)?,
            KEY_WIKI_LLM_MAX_TOKENS => settings.wiki_llm_max_tokens = req_u32(key, val)?,
            KEY_WIKI_PROPOSAL_ENABLED => settings.wiki_proposal_enabled = req_bool(key, val)?,
            KEY_WIKI_PROPOSAL_MAX_BYTES => settings.wiki_proposal_max_bytes = req_usize(key, val)?,
            KEY_WIKI_LLM_MODEL => settings.wiki_llm_model = req_string(key, val)?,
            KEY_WIKI_BUILD_MAX_ARTICLES => settings.wiki_build_max_articles = req_usize(key, val)?,
            KEY_WIKI_BUILD_EVIDENCE_BYTES => {
                settings.wiki_build_evidence_bytes = req_usize(key, val)?
            }
            KEY_WIKI_BUILD_ARTICLE_MAX_TOKENS => {
                settings.wiki_build_article_max_tokens = req_u32(key, val)?
            }
            KEY_WIKI_META_MAX_FILE_BYTES => {
                settings.wiki_meta_max_file_bytes = req_usize(key, val)?
            }
            KEY_WIKI_MAX_INDEX_FILES => settings.wiki_max_index_files = req_u32(key, val)?,
            KEY_WIKI_BUILD_DOC_BUDGET_PCT => settings.wiki_build_doc_budget_pct = req_u32(key, val)?,
            KEY_WIKI_BUILD_CONCURRENCY => settings.wiki_build_concurrency = req_usize(key, val)?,
            KEY_WIKI_STRUCTURAL_NEW_FILE_MIN => {
                settings.wiki_structural_new_file_min = req_usize(key, val)?
            }
            KEY_WIKI_STRUCTURAL_NEW_FILE_PCT => {
                settings.wiki_structural_new_file_pct = req_u32(key, val)?
            }
            KEY_WIKI_STRUCTURAL_LARGE_FILE_MIN => {
                settings.wiki_structural_large_file_min = req_usize(key, val)?
            }
            KEY_WIKI_STRUCTURAL_LARGE_FILE_PCT => {
                settings.wiki_structural_large_file_pct = req_u32(key, val)?
            }
            KEY_WIKI_INCREMENTAL_REPLAN_ENABLED => {
                settings.wiki_incremental_replan_enabled = req_bool(key, val)?
            }
            KEY_WIKI_BUILD_PLAN_RETRY_MAX => settings.wiki_build_plan_retry_max = req_usize(key, val)?,
            KEY_WIKI_EVIDENCE_REFETCH_MAX => settings.wiki_evidence_refetch_max = req_usize(key, val)?,
            KEY_WIKI_BUILD_EVIDENCE_RETRY_MAX => {
                settings.wiki_build_evidence_retry_max = req_usize(key, val)?
            }
            KEY_WIKI_VERSIONS_KEEP => settings.wiki_versions_keep = req_usize(key, val)?,
            KEY_WIKI_AUTO_MIGRATE => settings.wiki_auto_migrate = req_bool(key, val)?,
            KEY_WIKI_IGNORE_PATTERNS => {
                settings.wiki_ignore_patterns = req_string_array(key, val)?;
            }
            KEY_KNOWLEDGE_EXCERPT_BYTES => settings.knowledge_excerpt_bytes = req_usize(key, val)?,
            KEY_SPACE_SEARCH_MAX_HITS => settings.space_search_max_hits = req_u32(key, val)?,
            KEY_SPACE_ASK_EVIDENCE_BYTES => {
                settings.space_ask_evidence_bytes = req_usize(key, val)?
            }
            KEY_KNOWLEDGE_CHAT_MAX_SPACES => {
                settings.knowledge_chat_max_spaces = req_u32(key, val)?
            }
            KEY_PLAN_MAX_TOPICS => settings.plan_max_topics = req_usize(key, val)?,
            KEY_PLAN_MIN_TOPICS => settings.plan_min_topics = req_usize(key, val)?,
            KEY_VISION_MAX_IMAGES => settings.vision_max_images = req_u32(key, val)?,
            KEY_VISION_MAX_IMAGE_BYTES => settings.vision_max_image_bytes = req_usize(key, val)?,
            KEY_SERVE_BIND => {
                settings.serve_bind = req_string(key, val)?;
            }
            _ => {
                settings.extra.insert(key.clone(), val.clone());
            }
        }
    }
    if let Some(url) = settings.base_url.take() {
        let normalized = Settings::normalize_base_url(&url);
        settings.base_url = if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        };
    }
    let _ = KNOWN_KEYS;
    Ok(())
}

fn apply_env(settings: &mut Settings, env: &HashMap<String, String>) -> Result<()> {
    if let Some(v) = env_first(env, &["DANDELION_BASE_URL"]) {
        let n = Settings::normalize_base_url(&v);
        settings.base_url = if n.is_empty() { None } else { Some(n) };
    }
    if let Some(v) = env_first(env, &["DANDELION_MODEL", "DANDELION_DEFAULT_MODEL"]) {
        settings.default_model = nonempty_owned(v);
    }
    if let Some(v) = env_first(env, &["DANDELION_API_KEY"]) {
        settings.api_key = nonempty_owned(v);
    }
    if let Some(v) = env_first(env, &["DANDELION_DEFAULT_BACKEND"]) {
        settings.default_backend = v;
    }
    if let Some(v) = env_first(env, &["DANDELION_TEMPERATURE"]) {
        settings.temperature = parse_f32(KEY_TEMPERATURE, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MAX_TOKENS"]) {
        settings.max_tokens = parse_u32(KEY_MAX_TOKENS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TOKEN_BUDGET_ENABLED"]) {
        settings.dynamic_token_budget = parse_bool(KEY_DYNAMIC_TOKEN_BUDGET, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TOKEN_BUDGET_MARGIN_TOKENS"]) {
        settings.token_budget_margin_tokens = parse_u32(KEY_TOKEN_BUDGET_MARGIN_TOKENS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TOKEN_BUDGET_MIN_OUTPUT_TOKENS"]) {
        settings.token_budget_min_output_tokens =
            parse_u32(KEY_TOKEN_BUDGET_MIN_OUTPUT_TOKENS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MAX_ITERATIONS"]) {
        settings.max_iterations = parse_u32(KEY_MAX_ITERATIONS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_PERMISSION_MODE"]) {
        settings.permission_mode = PermissionMode::parse(&v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_ALLOW_SHELL"]) {
        settings.allow_shell = parse_bool(KEY_ALLOW_SHELL, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WORKSPACE"]) {
        settings.workspace = nonempty_owned(v).map(PathBuf::from);
    }
    if let Some(v) = env_first(env, &["DANDELION_HTTP_CONNECT_TIMEOUT_MS"]) {
        settings.http_connect_timeout_ms = parse_u64(KEY_HTTP_CONNECT_TIMEOUT_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SSE_IDLE_TIMEOUT_MS"]) {
        settings.sse_idle_timeout_ms = parse_u64(KEY_SSE_IDLE_TIMEOUT_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SIDECAR_SSE_IDLE_TIMEOUT_MS"]) {
        settings.sidecar_sse_idle_timeout_ms = parse_u64(KEY_SIDECAR_SSE_IDLE_TIMEOUT_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_RETRY_MAX"]) {
        settings.retry_max = parse_u32(KEY_RETRY_MAX, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_RETRY_BASE_MS"]) {
        settings.retry_base_ms = parse_u64(KEY_RETRY_BASE_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_RETRY_CAP_MS"]) {
        settings.retry_cap_ms = parse_u64(KEY_RETRY_CAP_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TERMINAL_FOREGROUND_TIMEOUT_SEC"]) {
        settings.terminal_foreground_timeout_sec =
            parse_u64(KEY_TERMINAL_FOREGROUND_TIMEOUT_SEC, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TERMINAL_FOREGROUND_TIMEOUT_MAX_SEC"]) {
        settings.terminal_foreground_timeout_max_sec =
            parse_u64(KEY_TERMINAL_FOREGROUND_TIMEOUT_MAX_SEC, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TERMINAL_OUTPUT_MAX_BYTES"]) {
        settings.terminal_output_max_bytes = parse_usize(KEY_TERMINAL_OUTPUT_MAX_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TERMINAL_LIVE_CHUNK_BYTES"]) {
        settings.terminal_live_chunk_bytes = parse_usize(KEY_TERMINAL_LIVE_CHUNK_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TERMINAL_LIVE_FLUSH_MS"]) {
        settings.terminal_live_flush_ms = parse_u64(KEY_TERMINAL_LIVE_FLUSH_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_FILE_READ_DEFAULT_LIMIT"]) {
        settings.file_read_default_limit = parse_u32(KEY_FILE_READ_DEFAULT_LIMIT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_FILE_READ_MAX_LIMIT"]) {
        settings.file_read_max_limit = parse_u32(KEY_FILE_READ_MAX_LIMIT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_FILE_READ_MAX_BYTES"]) {
        settings.file_read_max_bytes = parse_u64(KEY_FILE_READ_MAX_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_FILE_WRITE_MAX_BYTES"]) {
        settings.file_write_max_bytes = parse_u64(KEY_FILE_WRITE_MAX_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SEARCH_MAX_RESULTS"]) {
        settings.search_max_results = parse_u32(KEY_SEARCH_MAX_RESULTS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SEARCH_MAX_FILE_BYTES"]) {
        settings.search_max_file_bytes = parse_u64(KEY_SEARCH_MAX_FILE_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_PROCESS_LOG_DEFAULT_LINES"]) {
        settings.process_log_default_lines = parse_u32(KEY_PROCESS_LOG_DEFAULT_LINES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_PERMISSION_TIMEOUT_SEC"]) {
        settings.permission_timeout_sec = parse_u64(KEY_PERMISSION_TIMEOUT_SEC, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SQLITE_BUSY_MS"]) {
        settings.sqlite_busy_ms = parse_u64(KEY_SQLITE_BUSY_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_BUS_CAPACITY"]) {
        settings.bus_capacity = parse_usize(KEY_BUS_CAPACITY, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_THINKING_INTENSITY"]) {
        settings.thinking_intensity = ThinkingIntensity::parse(&v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TOOLS_PARALLEL_INDEPENDENT"]) {
        settings.tools_parallel_independent = parse_bool(KEY_TOOLS_PARALLEL_INDEPENDENT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TOOLS_PARALLEL_MAX"]) {
        settings.tools_parallel_max = parse_usize(KEY_TOOLS_PARALLEL_MAX, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TOOLS_START_ON_READY"]) {
        settings.tools_start_on_ready = parse_bool(KEY_TOOLS_START_ON_READY, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_COMPACT_ENABLED"]) {
        settings.compact_enabled = parse_bool(KEY_COMPACT_ENABLED, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_COMPACT_MAX_PER_PROMPT"]) {
        settings.compact_max_per_prompt = parse_u32(KEY_COMPACT_MAX_PER_PROMPT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_COMPACT_KEEP_TURNS"]) {
        settings.compact_keep_turns = parse_u32(KEY_COMPACT_KEEP_TURNS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_COMPACT_SOURCE_MAX_BYTES"]) {
        settings.compact_source_max_bytes = parse_usize(KEY_COMPACT_SOURCE_MAX_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_COMPACT_PREFLIGHT_ENABLED"]) {
        settings.compact_preflight_enabled = parse_bool(KEY_COMPACT_PREFLIGHT_ENABLED, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_COMPACT_PREFLIGHT_RESERVE_TOKENS"]) {
        settings.compact_preflight_reserve_tokens =
            parse_u32(KEY_COMPACT_PREFLIGHT_RESERVE_TOKENS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SPEC_GENERATE_MAX_ITERATIONS"]) {
        settings.spec_generate_max_iterations = parse_u32(KEY_SPEC_GENERATE_MAX_ITERATIONS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SPEC_ADAPT_MAX_ITERATIONS"]) {
        settings.spec_adapt_max_iterations = parse_u32(KEY_SPEC_ADAPT_MAX_ITERATIONS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SPEC_MAX_FILE_BYTES"]) {
        settings.spec_max_file_bytes = parse_usize(KEY_SPEC_MAX_FILE_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SPEC_EXECUTE_MAX_ITERATIONS"]) {
        settings.spec_execute_max_iterations = parse_u32(KEY_SPEC_EXECUTE_MAX_ITERATIONS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_HOOKS_TIMEOUT_MS"]) {
        settings.hooks_timeout_ms = parse_u64(KEY_HOOKS_TIMEOUT_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SKILLS_MAX_INJECT_BYTES"]) {
        settings.skills_max_inject_bytes = parse_usize(KEY_SKILLS_MAX_INJECT_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MCP_SPAWN_TIMEOUT_MS"]) {
        settings.mcp_spawn_timeout_ms = parse_u64(KEY_MCP_SPAWN_TIMEOUT_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MCP_MAX_TOOL_SCHEMA_BYTES"]) {
        settings.mcp_max_tool_schema_bytes = parse_usize(KEY_MCP_MAX_TOOL_SCHEMA_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MCP_DESCRIPTION_MAX_CHARS"]) {
        settings.mcp_description_max_chars = parse_usize(KEY_MCP_DESCRIPTION_MAX_CHARS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MCP_MAX_SCHEMA_BYTES"]) {
        settings.mcp_max_schema_bytes = parse_usize(KEY_MCP_MAX_SCHEMA_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_REGISTRY_MCP_URL"]) {
        settings.registry_mcp_url = v;
    }
    if let Some(v) = env_first(env, &["DANDELION_REGISTRY_TIMEOUT_MS"]) {
        settings.registry_timeout_ms = parse_u64(KEY_REGISTRY_TIMEOUT_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_REGISTRY_MAX_RESULTS"]) {
        settings.registry_max_results = parse_u32(KEY_REGISTRY_MAX_RESULTS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CONTEXT_ENABLED"]) {
        settings.context_enabled = parse_bool(KEY_CONTEXT_ENABLED, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CONTEXT_MAX_INJECT_BYTES"]) {
        settings.context_max_inject_bytes = parse_usize(KEY_CONTEXT_MAX_INJECT_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CONTEXT_MAX_FILE_BYTES"]) {
        settings.context_max_file_bytes = parse_usize(KEY_CONTEXT_MAX_FILE_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CONTEXT_PACK_ENABLED"]) {
        settings.context_pack_enabled = parse_bool(KEY_CONTEXT_PACK_ENABLED, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CONTEXT_PACK_TOKEN_BUDGET"]) {
        settings.context_pack_token_budget = parse_u32(KEY_CONTEXT_PACK_TOKEN_BUDGET, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CONTEXT_PACK_MAX_ITEMS"]) {
        settings.context_pack_max_items = parse_u32(KEY_CONTEXT_PACK_MAX_ITEMS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MEMORY_ENABLED"]) {
        settings.memory_enabled = parse_bool(KEY_MEMORY_ENABLED, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MEMORY_AUTO_CANDIDATES"]) {
        settings.memory_auto_candidates = parse_bool(KEY_MEMORY_AUTO_CANDIDATES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MEMORY_MAX_INJECT_BYTES"]) {
        settings.memory_max_inject_bytes = parse_usize(KEY_MEMORY_MAX_INJECT_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MEMORY_SOUL_MAX_BYTES"]) {
        settings.memory_soul_max_bytes = parse_usize(KEY_MEMORY_SOUL_MAX_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MEMORY_USER_MAX_BYTES"]) {
        settings.memory_user_max_bytes = parse_usize(KEY_MEMORY_USER_MAX_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MEMORY_MEMORY_MAX_BYTES"]) {
        settings.memory_memory_max_bytes = parse_usize(KEY_MEMORY_MEMORY_MAX_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CORPUS_MAX_HITS"]) {
        settings.corpus_max_hits = parse_u32(KEY_CORPUS_MAX_HITS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CORPUS_MAX_INJECT_BYTES"]) {
        settings.corpus_max_inject_bytes = parse_usize(KEY_CORPUS_MAX_INJECT_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CORPUS_CHUNK_CHARS"]) {
        settings.corpus_chunk_chars = parse_usize(KEY_CORPUS_CHUNK_CHARS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CORPUS_MIN_DISK_FREE_MB"]) {
        settings.corpus_min_disk_free_mb = parse_u64(KEY_CORPUS_MIN_DISK_FREE_MB, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CORPUS_MAX_FILE_BYTES"]) {
        settings.corpus_max_file_bytes = parse_usize(KEY_CORPUS_MAX_FILE_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TODO_MAX_ITEMS"]) {
        settings.todo_max_items = parse_u32(KEY_TODO_MAX_ITEMS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_TODO_MAX_CONTENT_CHARS"]) {
        settings.todo_max_content_chars = parse_usize(KEY_TODO_MAX_CONTENT_CHARS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WEB_TIMEOUT_MS"]) {
        settings.web_timeout_ms = parse_u64(KEY_WEB_TIMEOUT_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WEB_MAX_RESULTS"]) {
        settings.web_max_results = parse_u32(KEY_WEB_MAX_RESULTS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WEB_MAX_EXTRACT_BYTES"]) {
        settings.web_max_extract_bytes = parse_usize(KEY_WEB_MAX_EXTRACT_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WEB_MAX_EXTRACT_URLS"]) {
        settings.web_max_extract_urls = parse_u32(KEY_WEB_MAX_EXTRACT_URLS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WEB_SEARCH_URL"]) {
        settings.web_search_url = v;
    }
    if let Some(v) = env_first(env, &["DANDELION_WEB_USER_AGENT"]) {
        settings.web_user_agent = v;
    }
    if let Some(v) = env_first(env, &["DANDELION_DELEGATION_MAX_DEPTH"]) {
        settings.delegation_max_depth = clamp_max_depth(parse_u32(KEY_DELEGATION_MAX_DEPTH, &v)?);
    }
    if let Some(v) = env_first(env, &["DANDELION_DELEGATION_MAX_CONCURRENT"]) {
        settings.delegation_max_concurrent = parse_u32(KEY_DELEGATION_MAX_CONCURRENT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_DELEGATION_CHILD_MAX_ITERATIONS"]) {
        settings.delegation_child_max_iterations =
            parse_u32(KEY_DELEGATION_CHILD_MAX_ITERATIONS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_DELEGATION_CHILD_TIMEOUT_SEC"]) {
        settings.delegation_child_timeout_sec = parse_u64(KEY_DELEGATION_CHILD_TIMEOUT_SEC, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_DELEGATION_INHERIT_SHELL"]) {
        settings.delegation_inherit_shell = parse_bool(KEY_DELEGATION_INHERIT_SHELL, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_DELEGATION_MODEL"]) {
        settings.delegation_model = nonempty_owned(v);
    }
    if let Some(v) = env_first(env, &["DANDELION_DELEGATION_SUMMARY_MAX_CHARS"]) {
        settings.delegation_summary_max_chars = parse_usize(KEY_DELEGATION_SUMMARY_MAX_CHARS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_DELEGATION_STALE_IDLE_SEC"]) {
        settings.delegation_stale_idle_sec = parse_u64(KEY_DELEGATION_STALE_IDLE_SEC, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_DELEGATION_STALE_IN_TOOL_SEC"]) {
        settings.delegation_stale_in_tool_sec = parse_u64(KEY_DELEGATION_STALE_IN_TOOL_SEC, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CRON_TICK_SEC"]) {
        settings.cron_tick_sec = parse_u64(KEY_CRON_TICK_SEC, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_CRON_MAX_CONCURRENT"]) {
        settings.cron_max_concurrent = parse_u32(KEY_CRON_MAX_CONCURRENT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SESSION_LIST_LIMIT"]) {
        settings.session_list_limit = parse_u32(KEY_SESSION_LIST_LIMIT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_MAX_LOCAL_CONCURRENCY"]) {
        settings.max_local_concurrency = parse_u32(KEY_MAX_LOCAL_CONCURRENCY, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_KILL_SIDECAR_ON_EXIT"]) {
        settings.kill_sidecar_on_exit = parse_bool(KEY_KILL_SIDECAR_ON_EXIT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_RECOMMEND_QUANT"]) {
        settings.recommend_quant = nonempty_owned(v);
    }
    if let Some(v) = env_first(env, &["DANDELION_LOCAL_MODEL_DIR"]) {
        settings.local_model_dir = nonempty_owned(v).map(PathBuf::from);
    }
    if let Some(v) = env_first(env, &["DANDELION_SIDECAR_READY_TIMEOUT_MS"]) {
        settings.sidecar_ready_timeout_ms = parse_u64(KEY_SIDECAR_READY_TIMEOUT_MS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SIDECAR_LISTEN_PORT"]) {
        settings.sidecar_listen_port = Some(u16::try_from(parse_u64(KEY_SIDECAR_LISTEN_PORT, &v)?)
            .map_err(|_| Error::Settings(format!("{KEY_SIDECAR_LISTEN_PORT} must be 1024..65535")))?);
    }
    if let Some(v) = env_first(env, &["DANDELION_CC_SWITCH_PROXY_PORT"]) {
        settings.cc_switch_proxy_port = u16::try_from(parse_u64(KEY_CC_SWITCH_PROXY_PORT, &v)?)
            .map_err(|_| Error::Settings(format!("{KEY_CC_SWITCH_PROXY_PORT} must be 1024..65535")))?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SIDECAR_MLX_CACHE_BYTES"]) {
        settings.sidecar_mlx_prompt_cache_bytes = Some(parse_u64(KEY_SIDECAR_MLX_CACHE_BYTES, &v)?);
    }
    if let Some(v) = env_first(env, &["DANDELION_SIDECAR_MLX_KV_BITS"]) {
        settings.sidecar_mlx_kv_bits = Some(u8::try_from(parse_u64(KEY_SIDECAR_MLX_KV_BITS, &v)?)
            .map_err(|_| Error::Settings(format!("{KEY_SIDECAR_MLX_KV_BITS} must be 4 or 8")))?);
    }
    if let Some(v) = env_first(env, &["DANDELION_ENCRYPT_API_KEY"]) {
        settings.encrypt_api_key = parse_bool(KEY_ENCRYPT_API_KEY, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_UI_WINDOW_WIDTH"]) {
        settings.ui_window_width = parse_u32(KEY_UI_WINDOW_WIDTH, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_UI_WINDOW_HEIGHT"]) {
        settings.ui_window_height = parse_u32(KEY_UI_WINDOW_HEIGHT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_UI_WINDOW_MAXIMIZED"]) {
        settings.ui_window_maximized = parse_bool(KEY_UI_WINDOW_MAXIMIZED, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_COMPILE_ON_IDLE"]) {
        settings.wiki_compile_on_idle = parse_bool(KEY_WIKI_COMPILE_ON_IDLE, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_MAX_SCAN_FILES"]) {
        settings.wiki_max_scan_files = parse_u32(KEY_WIKI_MAX_SCAN_FILES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_META_MAX_FILE_BYTES"]) {
        settings.wiki_meta_max_file_bytes = parse_usize(KEY_WIKI_META_MAX_FILE_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_MAX_INDEX_FILES"]) {
        settings.wiki_max_index_files = parse_u32(KEY_WIKI_MAX_INDEX_FILES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_BUILD_DOC_BUDGET_PCT"]) {
        settings.wiki_build_doc_budget_pct = parse_u32(KEY_WIKI_BUILD_DOC_BUDGET_PCT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_BUILD_CONCURRENCY"]) {
        settings.wiki_build_concurrency = parse_usize(KEY_WIKI_BUILD_CONCURRENCY, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_STRUCTURAL_NEW_FILE_MIN"]) {
        settings.wiki_structural_new_file_min = parse_usize(KEY_WIKI_STRUCTURAL_NEW_FILE_MIN, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_STRUCTURAL_NEW_FILE_PCT"]) {
        settings.wiki_structural_new_file_pct = parse_u32(KEY_WIKI_STRUCTURAL_NEW_FILE_PCT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_STRUCTURAL_LARGE_FILE_MIN"]) {
        settings.wiki_structural_large_file_min = parse_usize(KEY_WIKI_STRUCTURAL_LARGE_FILE_MIN, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_STRUCTURAL_LARGE_FILE_PCT"]) {
        settings.wiki_structural_large_file_pct = parse_u32(KEY_WIKI_STRUCTURAL_LARGE_FILE_PCT, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_INCREMENTAL_REPLAN_ENABLED"]) {
        settings.wiki_incremental_replan_enabled =
            parse_bool(KEY_WIKI_INCREMENTAL_REPLAN_ENABLED, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_BUILD_PLAN_RETRY_MAX"]) {
        settings.wiki_build_plan_retry_max = parse_usize(KEY_WIKI_BUILD_PLAN_RETRY_MAX, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_EVIDENCE_REFETCH_MAX"]) {
        settings.wiki_evidence_refetch_max = parse_usize(KEY_WIKI_EVIDENCE_REFETCH_MAX, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_BUILD_EVIDENCE_RETRY_MAX"]) {
        settings.wiki_build_evidence_retry_max = parse_usize(KEY_WIKI_BUILD_EVIDENCE_RETRY_MAX, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_VERSIONS_KEEP"]) {
        settings.wiki_versions_keep = parse_usize(KEY_WIKI_VERSIONS_KEEP, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_AUTO_MIGRATE"]) {
        settings.wiki_auto_migrate = parse_bool(KEY_WIKI_AUTO_MIGRATE, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_KNOWLEDGE_EXCERPT_BYTES"]) {
        settings.knowledge_excerpt_bytes = parse_usize(KEY_KNOWLEDGE_EXCERPT_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SPACE_SEARCH_MAX_HITS"]) {
        settings.space_search_max_hits = parse_u32(KEY_SPACE_SEARCH_MAX_HITS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SPACE_ASK_EVIDENCE_BYTES"]) {
        settings.space_ask_evidence_bytes = parse_usize(KEY_SPACE_ASK_EVIDENCE_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_KNOWLEDGE_CHAT_MAX_SPACES"]) {
        settings.knowledge_chat_max_spaces = parse_u32(KEY_KNOWLEDGE_CHAT_MAX_SPACES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_PLAN_MAX_TOPICS"]) {
        settings.plan_max_topics = parse_usize(KEY_PLAN_MAX_TOPICS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_PLAN_MIN_TOPICS"]) {
        settings.plan_min_topics = parse_usize(KEY_PLAN_MIN_TOPICS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_MAX_FILE_BYTES"]) {
        settings.wiki_max_file_bytes = parse_usize(KEY_WIKI_MAX_FILE_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_LLM_INPUT_BYTES"]) {
        settings.wiki_llm_input_bytes = parse_usize(KEY_WIKI_LLM_INPUT_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_PROPOSAL_ENABLED"]) {
        settings.wiki_proposal_enabled = parse_bool(KEY_WIKI_PROPOSAL_ENABLED, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_PROPOSAL_MAX_BYTES"]) {
        settings.wiki_proposal_max_bytes = parse_usize(KEY_WIKI_PROPOSAL_MAX_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_WIKI_LLM_MAX_TOKENS"]) {
        settings.wiki_llm_max_tokens = parse_u32(KEY_WIKI_LLM_MAX_TOKENS, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_VISION_MAX_IMAGES"]) {
        settings.vision_max_images = parse_u32(KEY_VISION_MAX_IMAGES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_VISION_MAX_IMAGE_BYTES"]) {
        settings.vision_max_image_bytes = parse_usize(KEY_VISION_MAX_IMAGE_BYTES, &v)?;
    }
    if let Some(v) = env_first(env, &["DANDELION_SERVE_BIND"]) {
        settings.serve_bind = v;
    }
    Ok(())
}

fn apply_overlay(settings: &mut Settings, overlay: &SettingsOverlay) {
    if let Some(v) = &overlay.base_url {
        let n = Settings::normalize_base_url(v);
        settings.base_url = if n.is_empty() { None } else { Some(n) };
    }
    if let Some(v) = &overlay.default_model {
        settings.default_model = nonempty_owned(v.clone());
    }
    if let Some(v) = &overlay.api_key {
        settings.api_key = nonempty_owned(v.clone());
    }
    if let Some(v) = &overlay.workspace {
        settings.workspace = Some(v.clone());
    }
    if let Some(v) = overlay.permission_mode {
        settings.permission_mode = v;
    }
    if let Some(v) = overlay.temperature {
        settings.temperature = v;
    }
    if let Some(v) = overlay.max_tokens {
        settings.max_tokens = v;
    }
    if let Some(v) = overlay.max_iterations {
        settings.max_iterations = v;
    }
    if let Some(v) = overlay.thinking_intensity {
        settings.thinking_intensity = v;
    }
    if let Some(v) = overlay.allow_shell {
        settings.allow_shell = v;
    }
}

fn parse_path_list(key: &str, val: &Value) -> Result<Vec<PathBuf>> {
    Ok(req_string_array(key, val)?
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .collect())
}

fn req_string_array(key: &str, val: &Value) -> Result<Vec<String>> {
    let arr = val
        .as_array()
        .ok_or_else(|| Error::Settings(format!("{key} must be an array of strings")))?;
    let mut out = Vec::new();
    for item in arr {
        out.push(req_string(key, item)?);
    }
    Ok(out)
}

fn clamp_max_depth(raw: u32) -> u32 {
    raw.clamp(1, 2)
}

fn json_rules_empty() -> Value {
    serde_json::json!({ "deny": [], "ask": [], "allow": [] })
}

fn parse_permission_rules(val: &Value) -> Result<PermissionRules> {
    let obj = val
        .as_object()
        .ok_or_else(|| Error::Settings(format!("{KEY_PERMISSION_RULES} must be an object")))?;
    let deny = match obj.get("deny") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => req_string_array(&format!("{KEY_PERMISSION_RULES}.deny"), v)?,
    };
    let ask = match obj.get("ask") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => req_string_array(&format!("{KEY_PERMISSION_RULES}.ask"), v)?,
    };
    let allow = match obj.get("allow") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => req_string_array(&format!("{KEY_PERMISSION_RULES}.allow"), v)?,
    };
    Ok(PermissionRules { deny, ask, allow })
}

fn parse_mcp_servers(val: &Value) -> Result<Vec<McpServerConfig>> {
    let arr = val
        .as_array()
        .ok_or_else(|| Error::Settings(format!("{KEY_MCP_SERVERS} must be an array")))?;
    let mut out = Vec::new();
    for item in arr {
        let obj = item
            .as_object()
            .ok_or_else(|| Error::Settings(format!("{KEY_MCP_SERVERS} entries must be objects")))?;
        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Settings(format!("{KEY_MCP_SERVERS} id must be a string")))?
            .to_string();
        let enabled = match obj.get("enabled") {
            None => true,
            Some(v) => req_bool(&format!("{KEY_MCP_SERVERS}.enabled"), v)?,
        };
        let command = match obj.get("command") {
            None | Some(Value::Null) => String::new(),
            Some(v) => req_string(&format!("{KEY_MCP_SERVERS}.command"), v)?,
        };
        let url = match obj.get("url") {
            None | Some(Value::Null) => None,
            Some(v) => Some(req_string(&format!("{KEY_MCP_SERVERS}.url"), v)?),
        };
        let args = match obj.get("args") {
            None => Vec::new(),
            Some(v) => req_string_array(&format!("{KEY_MCP_SERVERS}.args"), v)?,
        };
        let mut env = HashMap::new();
        if let Some(v) = obj.get("env") {
            let map = v.as_object().ok_or_else(|| {
                Error::Settings(format!("{KEY_MCP_SERVERS} env must be an object"))
            })?;
            for (k, val) in map {
                env.insert(
                    k.clone(),
                    req_string(&format!("{KEY_MCP_SERVERS}.env.{k}"), val)?,
                );
            }
        }
        let cwd = match obj.get("cwd") {
            None | Some(Value::Null) => None,
            Some(v) => Some(req_string(&format!("{KEY_MCP_SERVERS}.cwd"), v)?),
        };
        let headers_val = obj.get("headers").or_else(|| obj.get("http_headers"));
        let mut headers = HashMap::new();
        if let Some(v) = headers_val {
            let map = v.as_object().ok_or_else(|| {
                Error::Settings(format!("{KEY_MCP_SERVERS} headers must be an object"))
            })?;
            for (k, val) in map {
                headers.insert(
                    k.clone(),
                    req_string(&format!("{KEY_MCP_SERVERS}.headers.{k}"), val)?,
                );
            }
        }
        let bearer_token_env_var = match obj
            .get("bearerTokenEnvVar")
            .or_else(|| obj.get("bearer_token_env_var"))
        {
            None | Some(Value::Null) => None,
            Some(v) => Some(req_string(
                &format!("{KEY_MCP_SERVERS}.bearerTokenEnvVar"),
                v,
            )?),
        };
        let transport = match obj.get("transport") {
            None | Some(Value::Null) => String::new(),
            Some(v) => req_string(&format!("{KEY_MCP_SERVERS}.transport"), v)?,
        };
        let disabled_tools = match obj
            .get("disabledTools")
            .or_else(|| obj.get("disabled_tools"))
        {
            None => Vec::new(),
            Some(v) => req_string_array(&format!("{KEY_MCP_SERVERS}.disabledTools"), v)?,
        };
        out.push(McpServerConfig {
            id,
            enabled,
            command,
            args,
            env,
            cwd,
            url,
            headers,
            bearer_token_env_var,
            transport,
            disabled_tools,
        });
    }
    Ok(out)
}

fn env_first(env: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(v) = env.get(*key) {
            return Some(v.clone());
        }
    }
    None
}

fn nonempty(v: &Option<String>) -> bool {
    v.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false)
}

fn nonempty_owned(v: String) -> Option<String> {
    let t = v.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn opt_string(key: &str, val: &Value) -> Result<Option<String>> {
    if val.is_null() {
        return Ok(None);
    }
    let s = req_string(key, val)?;
    Ok(nonempty_owned(s))
}

fn req_string(key: &str, val: &Value) -> Result<String> {
    val.as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Settings(format!("{key} must be a string")))
}

fn req_bool(key: &str, val: &Value) -> Result<bool> {
    val.as_bool()
        .ok_or_else(|| Error::Settings(format!("{key} must be a boolean")))
}

fn req_f32(key: &str, val: &Value) -> Result<f32> {
    val.as_f64()
        .map(|n| n as f32)
        .ok_or_else(|| Error::Settings(format!("{key} must be a number")))
}

fn req_u32(key: &str, val: &Value) -> Result<u32> {
    req_u64(key, val)?
        .try_into()
        .map_err(|_| Error::Settings(format!("{key} is out of range")))
}

fn req_u64(key: &str, val: &Value) -> Result<u64> {
    if let Some(n) = val.as_u64() {
        return Ok(n);
    }
    if let Some(n) = val.as_i64() {
        if n < 0 {
            return Err(Error::Settings(format!("{key} must be >= 0")));
        }
        return Ok(n as u64);
    }
    Err(Error::Settings(format!("{key} must be an integer")))
}

fn req_usize(key: &str, val: &Value) -> Result<usize> {
    req_u64(key, val)?
        .try_into()
        .map_err(|_| Error::Settings(format!("{key} is out of range")))
}

fn parse_f32(key: &str, raw: &str) -> Result<f32> {
    raw.parse::<f32>()
        .map_err(|_| Error::Settings(format!("{key} must be a number (got {raw:?})")))
}

fn parse_u32(key: &str, raw: &str) -> Result<u32> {
    raw.parse::<u32>()
        .map_err(|_| Error::Settings(format!("{key} must be an integer (got {raw:?})")))
}

fn parse_u64(key: &str, raw: &str) -> Result<u64> {
    raw.parse::<u64>()
        .map_err(|_| Error::Settings(format!("{key} must be an integer (got {raw:?})")))
}

fn parse_usize(key: &str, raw: &str) -> Result<usize> {
    raw.parse::<usize>()
        .map_err(|_| Error::Settings(format!("{key} must be an integer (got {raw:?})")))
}

fn parse_bool(key: &str, raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(Error::Settings(format!(
            "{key} must be boolean (got {other:?})"
        ))),
    }
}

fn json_u64(n: u64) -> Value {
    Value::Number(n.into())
}

fn json_f64(n: f64) -> Value {
    Value::Number(serde_json::Number::from_f64(n).unwrap_or_else(|| 0.into()))
}

fn put_opt_str(map: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(v) = value {
        if !v.is_empty() {
            map.insert(key.into(), Value::String(v.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_order_overlay_wins() {
        let file = json!({
            "dandelion.temperature": 0.9,
            "dandelion.baseUrl": "http://file.example/v1",
            "dandelion.defaultModel": "file-model"
        });
        let mut env = HashMap::new();
        env.insert("DANDELION_TEMPERATURE".into(), "0.5".into());
        env.insert("DANDELION_BASE_URL".into(), "http://env.example/v1".into());
        let overlay = SettingsOverlay {
            temperature: Some(0.1),
            base_url: Some("http://cli.example/v1".into()),
            ..Default::default()
        };
        let s = Settings::merge(Some(&file), &env, &overlay).expect("merge");
        assert_eq!(s.temperature, 0.1);
        assert_eq!(s.base_url.as_deref(), Some("http://cli.example/v1"));
        assert_eq!(s.default_model.as_deref(), Some("file-model"));
    }

    #[test]
    fn stable_sidecar_port_round_trips_and_rejects_privileged_ports() {
        let file = json!({"dandelion.sidecar.listenPort": 8001});
        let settings = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default()).unwrap();
        assert_eq!(settings.sidecar_listen_port, Some(8001));
        assert_eq!(settings.to_json_value()["dandelion.sidecar.listenPort"], 8001);
        let invalid = json!({"dandelion.sidecar.listenPort": 80});
        assert!(Settings::merge(Some(&invalid), &HashMap::new(), &SettingsOverlay::default()).is_err());
    }

    #[test]
    fn cc_switch_proxy_port_round_trips_and_rejects_privileged_ports() {
        let file = json!({"dandelion.agent.ccSwitchProxyPort": 15900});
        let settings = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default()).unwrap();
        assert_eq!(settings.cc_switch_proxy_port, 15900);
        assert_eq!(settings.to_json_value()["dandelion.agent.ccSwitchProxyPort"], 15900);
        let invalid = json!({"dandelion.agent.ccSwitchProxyPort": 80});
        assert!(Settings::merge(Some(&invalid), &HashMap::new(), &SettingsOverlay::default()).is_err());
    }

    #[test]
    fn mlx_memory_limits_round_trip_and_validate() {
        let file = json!({
            "dandelion.sidecar.mlxPromptCacheBytes": 1_073_741_824,
            "dandelion.sidecar.mlxKvBits": 4,
        });
        let settings = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default()).unwrap();
        assert_eq!(settings.sidecar_mlx_prompt_cache_bytes, Some(1_073_741_824));
        assert_eq!(settings.sidecar_mlx_kv_bits, Some(4));
        assert_eq!(settings.to_json_value()["dandelion.sidecar.mlxKvBits"], 4);
        assert!(Settings::merge(Some(&json!({"dandelion.sidecar.mlxKvBits": 3})), &HashMap::new(), &SettingsOverlay::default()).is_err());
    }


    #[test]
    fn existing_settings_use_longer_local_model_stream_timeout() {
        let old_file = json!({"dandelion.sseIdleTimeoutMs": 120_000});
        let settings = Settings::merge(
            Some(&old_file),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap();
        assert_eq!(settings.sse_idle_timeout_ms, 120_000);
        assert_eq!(settings.sidecar_sse_idle_timeout_ms, 900_000);
        assert_eq!(
            settings.to_json_value()["dandelion.sidecar.sseIdleTimeoutMs"],
            json!(900_000)
        );
    }

    #[test]
    fn patch_file_keeps_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        std::fs::write(
            settings_path(path),
            serde_json::to_string_pretty(&json!({
                "dandelion.baseUrl": "http://old.example/v1",
                "dandelion.customFlag": true
            }))
            .unwrap(),
        )
        .unwrap();
        let mut patch = Map::new();
        patch.insert("dandelion.baseUrl".into(), json!("http://new.example/v1"));
        Settings::patch_file(path, &patch).unwrap();
        let file = read_settings_file(path).unwrap().unwrap();
        assert_eq!(file["dandelion.baseUrl"], "http://new.example/v1");
        assert_eq!(file["dandelion.customFlag"], true);
    }

    #[test]
    fn service_profile_survives_switch_to_local_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = Map::new();
        service.insert(KEY_BASE_URL.into(), json!("https://api.deepseek.com"));
        service.insert(KEY_SERVICE_BACKEND.into(), json!("openai_compat"));
        service.insert(KEY_SERVICE_MODEL.into(), json!("deepseek-flash"));
        service.insert(KEY_DEFAULT_BACKEND.into(), json!("openai_compat"));
        service.insert(KEY_DEFAULT_MODEL.into(), json!("deepseek-flash"));
        Settings::patch_file(dir.path(), &service).unwrap();

        let mut local = Map::new();
        local.insert(KEY_DEFAULT_BACKEND.into(), json!("sidecar"));
        local.insert(KEY_DEFAULT_MODEL.into(), json!("local-mlx-id"));
        Settings::patch_file(dir.path(), &local).unwrap();

        let loaded = Settings::load(dir.path(), &SettingsOverlay::default()).unwrap();
        assert_eq!(loaded.default_backend, "sidecar");
        assert_eq!(loaded.default_model.as_deref(), Some("local-mlx-id"));
        assert_eq!(loaded.service_backend.as_deref(), Some("openai_compat"));
        assert_eq!(loaded.service_model.as_deref(), Some("deepseek-flash"));
        assert_eq!(loaded.base_url.as_deref(), Some("https://api.deepseek.com"));
    }

    #[test]
    fn invalid_patch_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        std::fs::write(
            settings_path(path),
            serde_json::to_string_pretty(&json!({
                "dandelion.temperature": 0.2
            }))
            .unwrap(),
        )
        .unwrap();
        let mut patch = Map::new();
        patch.insert("dandelion.temperature".into(), json!("hot"));
        let err =
            Settings::merge_patch_and_write(path, &patch, &SettingsOverlay::default()).unwrap_err();
        assert!(err.to_string().contains("dandelion.temperature"), "{err}");
        let file = read_settings_file(path).unwrap().unwrap();
        assert_eq!(file["dandelion.temperature"], 0.2);
    }

    #[test]
    fn unknown_keys_preserved() {
        let file = json!({
            "dandelion.futureCompatibilityFlag": "preserve-me",
            "dandelion.temperature": 0.4
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert_eq!(s.temperature, 0.4);
        assert_eq!(
            s.extra()["dandelion.futureCompatibilityFlag"],
            json!("preserve-me")
        );
        let out = s.to_json_value();
        assert_eq!(
            out["dandelion.futureCompatibilityFlag"],
            json!("preserve-me")
        );
        assert_eq!(out["dandelion.maxLocalConcurrency"], json!(2));
        assert_eq!(out["dandelion.killSidecarOnExit"], json!(true));
    }

    #[test]
    fn context_pack_keys_are_validated_and_roundtrip() {
        let settings = Settings::merge(
            Some(&json!({
                "dandelion.contextPack.enabled": false,
                "dandelion.contextPack.tokenBudget": 64,
                "dandelion.contextPack.maxItems": 100,
            })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap();
        assert!(!settings.context_pack_enabled);
        assert_eq!(settings.context_pack_token_budget, 64);
        assert_eq!(settings.context_pack_max_items, 100);
        assert_eq!(
            settings.to_json_value()["dandelion.contextPack.tokenBudget"],
            json!(64)
        );
        let err = Settings::merge(
            Some(&json!({ "dandelion.contextPack.maxItems": 101 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("contextPack.maxItems"), "{err}");
    }

    #[test]
    fn json_timeout_is_used() {
        let file = json!({ "dandelion.sseIdleTimeoutMs": 1111 });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert_eq!(s.sse_idle_timeout_ms, 1111);
    }

    #[test]
    fn invalid_key_type_names_the_key() {
        let file = json!({ "dandelion.temperature": "hot" });
        let err =
            Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("dandelion.temperature"), "{msg}");
    }

    #[test]
    fn thinking_intensity_default_xhigh() {
        let s = Settings::defaults();
        assert_eq!(s.thinking_intensity, ThinkingIntensity::Xhigh);
        assert_eq!(s.thinking_intensity.as_str(), "xhigh");
        assert_eq!(
            ThinkingIntensity::parse("off").unwrap().reasoning_effort(),
            None
        );
    }

    #[test]
    fn tools_parallel_keys_json_and_env() {
        let file = json!({
            "dandelion.tools.parallelIndependent": false,
            "dandelion.tools.parallelMax": 3,
            "dandelion.tools.startOnReady": false
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert!(!s.tools_parallel_independent);
        assert_eq!(s.tools_parallel_max, 3);
        assert!(!s.tools_start_on_ready);
        let mut env = HashMap::new();
        env.insert("DANDELION_TOOLS_PARALLEL_INDEPENDENT".into(), "true".into());
        env.insert("DANDELION_TOOLS_PARALLEL_MAX".into(), "4".into());
        env.insert("DANDELION_TOOLS_START_ON_READY".into(), "true".into());
        let s = Settings::merge(Some(&file), &env, &SettingsOverlay::default()).expect("env");
        assert!(s.tools_parallel_independent);
        assert_eq!(s.tools_parallel_max, 4);
        assert!(s.tools_start_on_ready);
    }

    #[test]
    fn compact_keys_json_and_env() {
        let file = json!({
            "dandelion.compact.enabled": false,
            "dandelion.compact.maxPerPrompt": 3,
            "dandelion.compact.keepTurns": 2,
            "dandelion.compact.sourceMaxBytes": 2048
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert!(!s.compact_enabled);
        assert_eq!(s.compact_max_per_prompt, 3);
        assert_eq!(s.compact_keep_turns, 2);
        assert_eq!(s.compact_source_max_bytes, 2048);
        let mut env = HashMap::new();
        env.insert("DANDELION_COMPACT_ENABLED".into(), "true".into());
        env.insert("DANDELION_COMPACT_MAX_PER_PROMPT".into(), "1".into());
        env.insert("DANDELION_COMPACT_KEEP_TURNS".into(), "6".into());
        env.insert("DANDELION_COMPACT_SOURCE_MAX_BYTES".into(), "4096".into());
        env.insert("DANDELION_COMPACT_PREFLIGHT_ENABLED".into(), "true".into());
        env.insert(
            "DANDELION_COMPACT_PREFLIGHT_RESERVE_TOKENS".into(),
            "512".into(),
        );
        let s = Settings::merge(Some(&file), &env, &SettingsOverlay::default()).expect("env");
        assert!(s.compact_enabled);
        assert_eq!(s.compact_max_per_prompt, 1);
        assert_eq!(s.compact_keep_turns, 6);
        assert_eq!(s.compact_source_max_bytes, 4096);
        assert!(s.compact_preflight_enabled);
        assert_eq!(s.compact_preflight_reserve_tokens, 512);
    }

    #[test]
    fn compact_preflight_keys_json_and_floor() {
        assert!(!Settings::defaults().compact_preflight_enabled);
        assert_eq!(Settings::defaults().compact_preflight_reserve_tokens, 2048);
        let file = json!({
            "dandelion.compact.preflightEnabled": true,
            "dandelion.compact.preflightReserveTokens": 256
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert!(s.compact_preflight_enabled);
        assert_eq!(s.compact_preflight_reserve_tokens, 256);
        let err = Settings::merge(
            Some(&json!({ "dandelion.compact.preflightReserveTokens": 8 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("preflightReserveTokens"), "{err}");
    }

    #[test]
    fn spec_keys_json_and_floor() {
        assert_eq!(Settings::defaults().spec_generate_max_iterations, 8);
        assert_eq!(Settings::defaults().spec_adapt_max_iterations, 6);
        assert_eq!(Settings::defaults().spec_max_file_bytes, 65_536);
        assert_eq!(Settings::defaults().spec_execute_max_iterations, 0);
        let file = json!({
            "dandelion.spec.generateMaxIterations": 4,
            "dandelion.spec.adaptMaxIterations": 3,
            "dandelion.spec.maxFileBytes": 2048,
            "dandelion.spec.executeMaxIterations": 12
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert_eq!(s.spec_generate_max_iterations, 4);
        assert_eq!(s.spec_adapt_max_iterations, 3);
        assert_eq!(s.spec_max_file_bytes, 2048);
        assert_eq!(s.spec_execute_max_iterations, 12);
        let err = Settings::merge(
            Some(&json!({ "dandelion.spec.generateMaxIterations": 0 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("generateMaxIterations"), "{err}");
        let mut env = HashMap::new();
        env.insert("DANDELION_SPEC_MAX_FILE_BYTES".into(), "4096".into());
        let s = Settings::merge(Some(&file), &env, &SettingsOverlay::default()).expect("env");
        assert_eq!(s.spec_max_file_bytes, 4096);
    }

    #[test]
    fn compact_keep_turns_zero_rejected() {
        let file = json!({ "dandelion.compact.keepTurns": 0 });
        let err =
            Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default()).unwrap_err();
        assert!(err.to_string().contains("keepTurns"), "{err}");
    }

    #[test]
    fn zero_parallel_max_rejected() {
        let file = json!({ "dandelion.tools.parallelMax": 0 });
        let err =
            Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default()).unwrap_err();
        assert!(err.to_string().contains("parallelMax"), "{err}");
    }

    #[test]
    fn strips_models_suffix() {
        assert_eq!(
            Settings::normalize_base_url("http://127.0.0.1:11434/v1/models"),
            "http://127.0.0.1:11434/v1"
        );
    }

    #[test]
    fn zero_timeout_rejected() {
        let file = json!({ "dandelion.httpConnectTimeoutMs": 0 });
        let err =
            Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default()).unwrap_err();
        assert!(err.to_string().contains("httpConnectTimeoutMs"));
    }

    #[test]
    fn g2_keys_json_and_env() {
        let file = json!({
            "dandelion.hooks.timeoutMs": 1234,
            "dandelion.hooks.enabled": ["deny_named_tool"],
            "dandelion.skills.maxInjectBytes": 2048,
            "dandelion.mcp.spawnTimeoutMs": 2000,
            "dandelion.mcp.maxToolSchemaBytes": 2048,
            "dandelion.mcp.descriptionMaxChars": 128,
            "dandelion.mcp.maxSchemaBytes": 4096,
            "dandelion.context.enabled": false,
            "dandelion.context.maxInjectBytes": 2048,
            "dandelion.context.maxFileBytes": 4096,
            "dandelion.mcp.servers": [{
                "id": "echo",
                "enabled": true,
                "command": "npx",
                "args": ["-y", "x"],
                "env": { "FOO": "bar" }
            }]
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert_eq!(s.hooks_timeout_ms, 1234);
        assert_eq!(s.hooks_enabled, vec!["deny_named_tool"]);
        assert_eq!(s.skills_max_inject_bytes, 2048);
        assert!(!s.context_enabled);
        assert_eq!(s.mcp_servers.len(), 1);
        assert_eq!(s.mcp_servers[0].id, "echo");
        assert_eq!(
            s.mcp_servers[0].env.get("FOO").map(String::as_str),
            Some("bar")
        );
        let mut env = HashMap::new();
        env.insert("DANDELION_HOOKS_TIMEOUT_MS".into(), "5000".into());
        env.insert("DANDELION_CONTEXT_ENABLED".into(), "true".into());
        let s = Settings::merge(Some(&file), &env, &SettingsOverlay::default()).expect("env");
        assert_eq!(s.hooks_timeout_ms, 5000);
        assert!(s.context_enabled);
        assert_eq!(s.hooks_enabled, vec!["deny_named_tool"]);
    }

    #[test]
    fn mcp_max_schema_zero_ok() {
        let file = json!({ "dandelion.mcp.maxSchemaBytes": 0 });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert_eq!(s.mcp_max_schema_bytes, 0);
    }

    #[test]
    fn g2_validation_rejects_bad_caps() {
        let err = Settings::merge(
            Some(&json!({ "dandelion.hooks.timeoutMs": 0 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("timeoutMs"), "{err}");

        let err = Settings::merge(
            Some(&json!({ "dandelion.mcp.descriptionMaxChars": 8 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("descriptionMaxChars"), "{err}");

        let err = Settings::merge(
            Some(&json!({
                "dandelion.mcp.maxToolSchemaBytes": 2048,
                "dandelion.mcp.maxSchemaBytes": 1024
            })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("maxSchemaBytes"), "{err}");

        let err = Settings::merge(
            Some(&json!({
                "dandelion.mcp.servers": [
                    {"id": "echo", "command": "npx"},
                    {"id": "echo", "command": "npx"}
                ]
            })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");

        let err = Settings::merge(
            Some(&json!({ "dandelion.skills.maxInjectBytes": 512 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("maxInjectBytes"), "{err}");
    }

    #[test]
    fn hooks_enabled_not_overridden_by_env() {
        let file = json!({ "dandelion.hooks.enabled": ["deny_named_tool"] });
        let mut env = HashMap::new();
        env.insert("DANDELION_HOOKS_ENABLED".into(), "should-ignore".into());
        let s = Settings::merge(Some(&file), &env, &SettingsOverlay::default()).expect("merge");
        assert_eq!(s.hooks_enabled, vec!["deny_named_tool"]);
    }

    #[test]
    fn skills_disabled_and_sandbox_roots_roundtrip() {
        let file = json!({
            "dandelion.skills.disabledIds": ["demo-skill"],
            "dandelion.sandbox.allowRoots": ["E:/ws/extra"]
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert_eq!(s.skills_disabled_ids, vec!["demo-skill"]);
        assert_eq!(
            s.sandbox_allow_roots,
            vec![std::path::PathBuf::from("E:/ws/extra")]
        );
        let out = s.to_json_value();
        assert_eq!(out["dandelion.skills.disabledIds"], json!(["demo-skill"]));
        assert_eq!(out["dandelion.sandbox.allowRoots"], json!(["E:/ws/extra"]));
    }

    #[test]
    fn mcp_remote_url_parsed() {
        let file = json!({
            "dandelion.mcp.servers": [{
                "id": "jira",
                "url": "https://mcp.example.com/mcp",
                "headers": { "X-Token": "abc" },
                "bearerTokenEnvVar": "JIRA_MCP_TOKEN",
                "transport": "sse"
            }]
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert!(s.mcp_servers[0].is_remote());
        assert_eq!(
            s.mcp_servers[0].url.as_deref(),
            Some("https://mcp.example.com/mcp")
        );
        assert_eq!(
            s.mcp_servers[0].headers.get("X-Token").map(String::as_str),
            Some("abc")
        );
        assert_eq!(
            s.mcp_servers[0].bearer_token_env_var.as_deref(),
            Some("JIRA_MCP_TOKEN")
        );
        assert_eq!(s.mcp_servers[0].transport, "sse");
        assert!(s.mcp_servers[0].command.is_empty());
    }

    #[test]
    fn mcp_requires_command_or_url() {
        let err = Settings::merge(
            Some(&json!({
                "dandelion.mcp.servers": [{ "id": "x" }]
            })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("command"), "{err}");

        let err = Settings::merge(
            Some(&json!({
                "dandelion.mcp.servers": [{
                    "id": "x",
                    "command": "npx",
                    "url": "https://example.com/mcp"
                }]
            })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("exactly one"), "{err}");
    }

    #[test]
    fn mcp_http_headers_alias_parsed() {
        let file = json!({
            "dandelion.mcp.servers": [{
                "id": "remote",
                "url": "https://mcp.example.com/mcp",
                "http_headers": { "X-Api-Key": "k" },
                "transport": "streamable-http"
            }]
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert_eq!(
            s.mcp_servers[0]
                .headers
                .get("X-Api-Key")
                .map(String::as_str),
            Some("k")
        );
        assert_eq!(s.mcp_servers[0].transport, "streamable-http");
    }

    #[test]
    fn g3_keys_json_and_env() {
        let file = json!({
            "dandelion.memory.enabled": false,
            "dandelion.memory.maxInjectBytes": 5120,
            "dandelion.memory.soulMaxBytes": 1024,
            "dandelion.memory.userMaxBytes": 512,
            "dandelion.memory.memoryMaxBytes": 1024,
            "dandelion.corpus.maxHits": 0,
            "dandelion.corpus.maxInjectBytes": 2048,
            "dandelion.permission.rules": {
                "deny": ["write_file"],
                "allow": ["read_file"]
            },
            "dandelion.delegation.maxDepth": 2,
            "dandelion.delegation.maxConcurrent": 4,
            "dandelion.delegation.childMaxIterations": 10,
            "dandelion.delegation.childTimeoutSec": 30,
            "dandelion.delegation.inheritShell": true,
            "dandelion.delegation.model": "child-model",
            "dandelion.delegation.summaryMaxChars": 1024,
            "dandelion.delegation.staleIdleSec": 10,
            "dandelion.delegation.staleInToolSec": 20,
            "dandelion.cron.tickSec": 5,
            "dandelion.cron.maxConcurrent": 2,
            "dandelion.session.listLimit": 20
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert!(!s.memory_enabled);
        assert_eq!(s.memory_max_inject_bytes, 5120);
        assert_eq!(s.corpus_max_hits, 0);
        assert_eq!(s.permission_rules.deny, vec!["write_file"]);
        assert_eq!(s.permission_rules.allow, vec!["read_file"]);
        assert_eq!(s.delegation_max_depth, 2);
        assert!(s.delegation_inherit_shell);
        assert_eq!(s.delegation_model.as_deref(), Some("child-model"));
        assert_eq!(s.cron_tick_sec, 5);
        assert_eq!(s.session_list_limit, 20);
        let out = s.to_json_value();
        assert_eq!(out["dandelion.permission.rules"]["deny"][0], "write_file");
        let mut env = HashMap::new();
        env.insert("DANDELION_MEMORY_ENABLED".into(), "true".into());
        env.insert("DANDELION_DELEGATION_MAX_DEPTH".into(), "9".into());
        env.insert("DANDELION_SESSION_LIST_LIMIT".into(), "80".into());
        let s = Settings::merge(Some(&file), &env, &SettingsOverlay::default()).expect("env");
        assert!(s.memory_enabled);
        assert_eq!(s.delegation_max_depth, 2, "depth >2 clamps to 2");
        assert_eq!(s.session_list_limit, 80);
        assert_eq!(s.permission_rules.deny, vec!["write_file"]);
    }

    #[test]
    fn todo_keys_json_and_env() {
        let file = json!({
            "dandelion.todo.maxItems": 12,
            "dandelion.todo.maxContentChars": 128
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert_eq!(s.todo_max_items, 12);
        assert_eq!(s.todo_max_content_chars, 128);
        let mut env = HashMap::new();
        env.insert("DANDELION_TODO_MAX_ITEMS".into(), "20".into());
        env.insert("DANDELION_TODO_MAX_CONTENT_CHARS".into(), "200".into());
        let s = Settings::merge(Some(&file), &env, &SettingsOverlay::default()).expect("env");
        assert_eq!(s.todo_max_items, 20);
        assert_eq!(s.todo_max_content_chars, 200);
        let err = Settings::merge(
            Some(&json!({ "dandelion.todo.maxItems": 0 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("maxItems"), "{err}");
    }

    #[test]
    fn web_keys_json_and_env() {
        let file = json!({
            "dandelion.web.timeoutMs": 2,
            "dandelion.web.maxResults": 3,
            "dandelion.web.maxExtractBytes": 2048,
            "dandelion.web.maxExtractUrls": 2,
            "dandelion.web.searchUrl": "http://127.0.0.1/s?q={query}",
            "dandelion.web.userAgent": "TestAgent/1"
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert_eq!(s.web_timeout_ms, 2);
        assert_eq!(s.web_max_results, 3);
        assert_eq!(s.web_max_extract_bytes, 2048);
        assert_eq!(s.web_max_extract_urls, 2);
        assert!(s.web_search_url.contains("127.0.0.1"));
        assert_eq!(s.web_user_agent, "TestAgent/1");
        let mut env = HashMap::new();
        env.insert("DANDELION_WEB_TIMEOUT_MS".into(), "9000".into());
        env.insert("DANDELION_WEB_MAX_RESULTS".into(), "8".into());
        env.insert("DANDELION_WEB_USER_AGENT".into(), "EnvAgent/2".into());
        let s = Settings::merge(Some(&file), &env, &SettingsOverlay::default()).expect("env");
        assert_eq!(s.web_timeout_ms, 9000);
        assert_eq!(s.web_max_results, 8);
        assert_eq!(s.web_user_agent, "EnvAgent/2");
        let err = Settings::merge(
            Some(&json!({ "dandelion.web.maxResults": 0 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("maxResults"), "{err}");
    }

    #[test]
    fn terminal_live_keys_json_and_env() {
        let file = json!({
            "dandelion.terminal.liveChunkBytes": 128,
            "dandelion.terminal.liveFlushMs": 40
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert_eq!(s.terminal_live_chunk_bytes, 128);
        assert_eq!(s.terminal_live_flush_ms, 40);
        let mut env = HashMap::new();
        env.insert("DANDELION_TERMINAL_LIVE_CHUNK_BYTES".into(), "256".into());
        env.insert("DANDELION_TERMINAL_LIVE_FLUSH_MS".into(), "15".into());
        let s = Settings::merge(Some(&file), &env, &SettingsOverlay::default()).expect("env");
        assert_eq!(s.terminal_live_chunk_bytes, 256);
        assert_eq!(s.terminal_live_flush_ms, 15);
        let err = Settings::merge(
            Some(&json!({ "dandelion.terminal.liveChunkBytes": 8 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("liveChunkBytes"), "{err}");
    }

    #[test]
    fn g3_validation_rejects_bad_caps() {
        let err = Settings::merge(
            Some(&json!({ "dandelion.memory.soulMaxBytes": 8 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("soulMaxBytes"), "{err}");

        let err = Settings::merge(
            Some(&json!({ "dandelion.session.listLimit": 0 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("listLimit"), "{err}");

        let err = Settings::merge(
            Some(&json!({
                "dandelion.delegation.staleIdleSec": 50,
                "dandelion.delegation.staleInToolSec": 10
            })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("staleInToolSec"), "{err}");
    }

    #[test]
    fn child_max_iterations_zero_uses_global() {
        let mut s = Settings::defaults();
        s.max_iterations = 40;
        s.delegation_child_max_iterations = 0;
        assert_eq!(s.child_max_iterations(), 40);
        s.delegation_child_max_iterations = 7;
        assert_eq!(s.child_max_iterations(), 7);
    }

    #[test]
    fn encrypt_and_window_keys_json_and_env() {
        assert!(Settings::defaults().encrypt_api_key);
        assert_eq!(Settings::defaults().ui_window_width, 1120);
        assert_eq!(Settings::defaults().ui_window_height, 720);
        assert!(!Settings::defaults().ui_window_maximized);
        let file = json!({
            "dandelion.encryptApiKey": false,
            "dandelion.ui.windowWidth": 1400,
            "dandelion.ui.windowHeight": 900,
            "dandelion.ui.windowMaximized": true
        });
        let s = Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default())
            .expect("merge");
        assert!(!s.encrypt_api_key);
        assert_eq!(s.ui_window_width, 1400);
        assert_eq!(s.ui_window_height, 900);
        assert!(s.ui_window_maximized);
        assert!(s.extra().get("dandelion.encryptApiKey").is_none());
        let mut env = HashMap::new();
        env.insert("DANDELION_ENCRYPT_API_KEY".into(), "true".into());
        env.insert("DANDELION_UI_WINDOW_WIDTH".into(), "1600".into());
        env.insert("DANDELION_UI_WINDOW_MAXIMIZED".into(), "false".into());
        let s = Settings::merge(Some(&file), &env, &SettingsOverlay::default()).expect("env");
        assert!(s.encrypt_api_key);
        assert_eq!(s.ui_window_width, 1600);
        assert!(!s.ui_window_maximized);
        let err = Settings::merge(
            Some(&json!({ "dandelion.ui.windowWidth": 100 })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("windowWidth"), "{err}");
        // 越界值（< min 或 > max）由 merge 入口 clamp 到合法范围，避免启动失败。
        let s = Settings::merge(
            Some(&json!({
                "dandelion.ui.windowWidth": 4096,
                "dandelion.ui.windowHeight": 4096,
            })),
            &HashMap::new(),
            &SettingsOverlay::default(),
        )
        .expect("merge clamps oversized values");
        assert_eq!(s.ui_window_width, UI_WINDOW_MAX_W);
        assert_eq!(s.ui_window_height, UI_WINDOW_MAX_H);
    }
}
