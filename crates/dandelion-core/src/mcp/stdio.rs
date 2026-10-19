//! One MCP manager: local stdio (`TokioChildProcess`) and remote Streamable HTTP.
//! Hosted `url` entries use the same `mcp__{server}__{tool}` registry and dual gate.
//!
//! H3 failure governance (Hermes parity: per-server jittered-backoff
//! respawn; Codex parity: per-server `Failed` status that never aborts the
//! others): a dead mid-session server is evicted after
//! `mcp_failure_threshold` consecutive transport errors, and the next
//! snapshot lazily respawns it once the backoff window has passed. All
//! error text that can reach the model, `mcp_list`, or trace is passed
//! through [`sanitize_error_text`] so tokens never leak.

use super::schema::{apply_schema_budget, candidate_from_mcp, SchemaCandidate};
use crate::acp::ToolKind;
use crate::config::{McpServerConfig, Settings};
use crate::provider::types::{ToolSource, ToolSpec};
use crate::secret::{CredentialStore, FileCredentialStore, MCP_SECRET_REF};
use crate::tools::{Tool, ToolCtx};
use crate::{Error, Result};
use async_trait::async_trait;
use http::{HeaderName, HeaderValue};
use rmcp::model::{CallToolRequestParams, ContentBlock};
use rmcp::service::{RoleClient, RunningService, ServiceExt};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::Mutex;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpListItem {
    pub id: String,
    pub enabled: bool,
    pub ready: bool,
    pub error: Option<String>,
    pub listed: usize,
    pub in_snapshot: usize,
    pub skipped_by_budget: Vec<String>,
    pub tool_names: Vec<String>,
}

pub struct McpSnapshot {
    /// Directly registered MCP tools. Empty when `lazy` — the fleet is then
    /// reached through `find_tools` + activation instead.
    pub tools: Vec<Arc<dyn Tool>>,
    pub parallel_unscoped: Vec<String>,
    pub skipped: Vec<String>,
    /// Every usable listed candidate (both modes). In lazy mode this is the
    /// searchable catalog; schemas reach the model only once activated.
    pub catalog: super::catalog::McpCatalog,
    /// True when the fleet was too wide to inject: only `find_tools` is
    /// registered and the model promotes tools on demand (H3 exit condition:
    /// 500 candidates ⇒ first turn stays lean).
    pub lazy: bool,
}

enum Slot {
    Ready(ReadySlot),
    /// Failed with the time of the failure; `ensure_server` respawns lazily
    /// once the jittered backoff window has passed (Hermes-style).
    Failed {
        error: String,
        since: Instant,
    },
}

struct Inner {
    slots: Mutex<HashMap<String, Slot>>,
    failures: Mutex<HashMap<String, u32>>,
}

struct ReadySlot {
    peer: rmcp::service::Peer<RoleClient>,
    service: Option<RunningService<RoleClient, ()>>,
    listed: Vec<SchemaCandidate>,
    skipped: Vec<String>,
    listed_count: usize,
}

pub struct McpManager {
    inner: Arc<Inner>,
    /// `mcp_stop` until `mcp_start`. Snapshot and list must not auto-respawn these.
    /// `Arc` so `Clone` handles (e.g. `find_tools`) observe later stops.
    stopped: Arc<Mutex<HashSet<String>>>,
}

impl Clone for McpManager {
    /// A handle to the same live state: slots, failures and the stopped set
    /// stay shared, so `find_tools` respects `mcp_stop` made from any host.
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            stopped: self.stopped.clone(),
        }
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                slots: Mutex::new(HashMap::new()),
                failures: Mutex::new(HashMap::new()),
            }),
            stopped: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub async fn mcp_list(&self, settings: &Settings) -> Result<Vec<McpListItem>> {
        let mut out = Vec::new();
        for cfg in &settings.mcp_servers {
            if !cfg.enabled {
                out.push(McpListItem {
                    id: cfg.id.clone(),
                    enabled: false,
                    ready: false,
                    error: None,
                    listed: 0,
                    in_snapshot: 0,
                    skipped_by_budget: Vec::new(),
                    tool_names: Vec::new(),
                });
                continue;
            }
            let slots = self.inner.slots.lock().await;
            match slots.get(&cfg.id) {
                Some(Slot::Ready(r)) => out.push(McpListItem {
                    id: cfg.id.clone(),
                    enabled: true,
                    ready: true,
                    error: None,
                    listed: r.listed_count,
                    in_snapshot: r.listed.len(),
                    skipped_by_budget: r.skipped.clone(),
                    tool_names: r.listed.iter().map(|c| c.name.clone()).collect(),
                }),
                Some(Slot::Failed { error, .. }) => out.push(McpListItem {
                    id: cfg.id.clone(),
                    enabled: true,
                    ready: false,
                    error: Some(error.clone()),
                    listed: 0,
                    in_snapshot: 0,
                    skipped_by_budget: Vec::new(),
                    tool_names: Vec::new(),
                }),
                None => out.push(McpListItem {
                    id: cfg.id.clone(),
                    enabled: true,
                    ready: false,
                    error: None,
                    listed: 0,
                    in_snapshot: 0,
                    skipped_by_budget: Vec::new(),
                    tool_names: Vec::new(),
                }),
            }
        }
        Ok(out)
    }

    pub async fn mcp_start(&self, id: &str, settings: &Settings, data_dir: &Path) -> Result<()> {
        let cfg = settings
            .mcp_servers
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| Error::Invalid(format!("unknown mcp server: {id}")))?;
        if !cfg.enabled {
            return Err(Error::Invalid(format!("mcp server disabled: {id}")));
        }
        {
            let mut stopped = self.stopped.lock().await;
            stopped.remove(id);
        }
        self.drop_slot(id).await;
        self.ensure_server(cfg, settings, data_dir).await;
        Ok(())
    }

    pub async fn mcp_stop(&self, id: &str) -> Result<()> {
        {
            let mut stopped = self.stopped.lock().await;
            stopped.insert(id.to_string());
        }
        self.drop_slot(id).await;
        Ok(())
    }

    async fn drop_slot(&self, id: &str) {
        self.inner.failures.lock().await.remove(id);
        let mut slots = self.inner.slots.lock().await;
        match slots.remove(id) {
            Some(Slot::Ready(mut r)) => {
                if let Some(svc) = r.service.take() {
                    let _ = svc.cancel().await;
                }
            }
            Some(Slot::Failed { .. }) | None => {}
        }
    }

    pub async fn apply_servers(&self, old: &[McpServerConfig], new: &[McpServerConfig]) {
        let new_ids: HashSet<&str> = new.iter().map(|c| c.id.as_str()).collect();
        let new_fp: HashMap<&str, String> = new
            .iter()
            .map(|c| (c.id.as_str(), server_fingerprint(c)))
            .collect();
        let mut drop_ids = Vec::new();
        for cfg in old {
            let gone = !new_ids.contains(cfg.id.as_str());
            let changed = new_fp
                .get(cfg.id.as_str())
                .map(|fp| fp != &server_fingerprint(cfg))
                .unwrap_or(false);
            if gone || changed {
                drop_ids.push(cfg.id.clone());
            }
        }
        for id in drop_ids {
            self.drop_slot(&id).await;
            if !new_ids.contains(id.as_str()) {
                self.stopped.lock().await.remove(&id);
            }
        }
    }

    pub async fn snapshot_for(
        &self,
        server_ids: &[String],
        settings: &Settings,
        data_dir: &Path,
        scopes: &super::scopes::ScopeDecision,
    ) -> McpSnapshot {
        let stopped = self.stopped.lock().await.clone();
        let mut ordered = Vec::new();
        for cfg in &settings.mcp_servers {
            if !cfg.enabled {
                continue;
            }
            if !server_ids.iter().any(|id| id == &cfg.id) {
                continue;
            }
            if stopped.contains(&cfg.id) {
                continue;
            }
            self.ensure_server(cfg, settings, data_dir).await;
            ordered.push(cfg.id.clone());
        }
        let slots = self.inner.slots.lock().await;
        let mut candidates = Vec::new();
        for id in &ordered {
            if let Some(Slot::Ready(r)) = slots.get(id) {
                candidates.extend(r.listed.clone());
            }
        }
        drop(slots);
        // H3 scopes: denied tools (user floor ∪ project tightening) vanish
        // before the catalog exists, so neither direct injection nor a later
        // `find_tools` search can ever surface them.
        let before = candidates.len();
        candidates.retain(|c| !scopes.is_denied(&c.server_id, &c.remote_name));
        let mut skipped: Vec<String> = scopes
            .notes
            .iter()
            .map(|n| format!("mcp scope: {n}"))
            .collect();
        if before != candidates.len() {
            skipped.push(format!(
                "mcp scope: {} tool(s) removed by deny lists",
                before - candidates.len()
            ));
        }
        // Catalog drops only per-tool hard-cap violators; the direct/lazy
        // decision then compares the fleet against the same budgets an
        // activated set must obey.
        let (catalog, budget_skipped) = super::catalog::prepare_catalog(candidates, settings);
        skipped.extend(budget_skipped);
        let lazy = !super::catalog::plan_direct_injection(&catalog, settings);
        let (tools, parallel_unscoped) = if lazy {
            (Vec::new(), Vec::new())
        } else {
            let budget = apply_schema_budget(catalog.entries().to_vec(), settings);
            skipped.extend(budget.skipped);
            let slots = self.inner.slots.lock().await;
            let mut tools = Vec::new();
            let mut parallel_unscoped = Vec::new();
            for c in &budget.kept {
                let Some(Slot::Ready(r)) = slots.get(&c.server_id) else {
                    continue;
                };
                if c.read_only {
                    parallel_unscoped.push(c.name.clone());
                }
                tools.push(build_proxy(
                    c,
                    &r.peer,
                    &self.inner,
                    settings.mcp_failure_threshold,
                ));
            }
            (tools, parallel_unscoped)
        };
        McpSnapshot {
            tools,
            parallel_unscoped,
            skipped,
            catalog,
            lazy,
        }
    }

    /// Build one dispatching proxy for a catalog candidate. Lazy activation
    /// uses this; `None` when the server is no longer `Ready` (a search whose
    /// server died between listing and activation reports it, never half-binds).
    pub async fn proxy_for_candidate(
        &self,
        candidate: &super::schema::SchemaCandidate,
        failure_threshold: u32,
    ) -> Option<Arc<dyn Tool>> {
        let slots = self.inner.slots.lock().await;
        let peer = match slots.get(&candidate.server_id) {
            Some(Slot::Ready(r)) => r.peer.clone(),
            _ => return None,
        };
        drop(slots);
        Some(build_proxy(
            candidate,
            &peer,
            &self.inner,
            failure_threshold,
        ))
    }

    async fn ensure_server(&self, cfg: &McpServerConfig, settings: &Settings, data_dir: &Path) {
        {
            let stopped = self.stopped.lock().await;
            if stopped.contains(&cfg.id) {
                return;
            }
        }
        {
            let slots = self.inner.slots.lock().await;
            match slots.get(&cfg.id) {
                Some(Slot::Ready(_)) => return,
                Some(Slot::Failed { since, .. }) => {
                    // Jittered backoff before the next respawn attempt so a
                    // broken parked server cannot hot-loop spawn attempts.
                    if since.elapsed() < reconnect_backoff(settings, &cfg.id) {
                        return;
                    }
                }
                None => {}
            }
        }
        // Retire the failed slot before respawning; a concurrent Ready wins.
        self.inner.slots.lock().await.remove(&cfg.id);
        let timeout = Duration::from_millis(settings.mcp_spawn_timeout_ms);
        let cfg_clone = cfg.clone();
        let data = data_dir.to_path_buf();
        let result =
            tokio::time::timeout(timeout, spawn_and_list(&cfg_clone, settings, &data)).await;
        let mut slots = self.inner.slots.lock().await;
        if matches!(slots.get(&cfg.id), Some(Slot::Ready(_))) {
            drop(slots);
            if let Ok(Ok(ready)) = result {
                if let Some(r) = ready.service {
                    let _ = r.cancel().await;
                }
            }
            return;
        }
        match result {
            Ok(Ok(ready)) => {
                slots.insert(cfg.id.clone(), Slot::Ready(ready));
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    server = %cfg.id,
                    error = %sanitize_error_text(&err.to_string()),
                    "mcp server failed"
                );
                slots.insert(
                    cfg.id.clone(),
                    Slot::Failed {
                        error: sanitize_error_text(&err.to_string()),
                        since: Instant::now(),
                    },
                );
            }
            Err(_) => {
                tracing::warn!(server = %cfg.id, "mcp spawn timeout");
                slots.insert(
                    cfg.id.clone(),
                    Slot::Failed {
                        error: "timeout".into(),
                        since: Instant::now(),
                    },
                );
            }
        }
    }
}

async fn spawn_and_list(
    cfg: &McpServerConfig,
    settings: &Settings,
    data_dir: &Path,
) -> Result<ReadySlot> {
    if cfg.is_remote() {
        connect_remote(cfg, settings, data_dir).await
    } else {
        connect_stdio(cfg, settings, data_dir).await
    }
}

async fn connect_stdio(
    cfg: &McpServerConfig,
    settings: &Settings,
    data_dir: &Path,
) -> Result<ReadySlot> {
    let mut cmd = Command::new(&cfg.command);
    for arg in &cfg.args {
        cmd.arg(expand_config_value(arg, &cfg.id, data_dir)?);
    }
    for (k, v) in &cfg.env {
        cmd.env(k, expand_config_value(v, &cfg.id, data_dir)?);
    }
    if let Some(cwd) = &cfg.cwd {
        cmd.current_dir(cwd);
    }
    cmd.kill_on_drop(true);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    // MCP children can print their environment or startup request on stderr;
    // credentials are supplied only for the process lifetime, but inherited
    // stderr would bypass Dandelion's redaction boundary.
    cmd.stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    let transport = TokioChildProcess::new(cmd)
        .map_err(|err| Error::Invalid(format!("mcp spawn {}: {err}", cfg.id)))?;
    let running = ()
        .serve(transport)
        .await
        .map_err(|err| Error::Invalid(format!("mcp init {}: {err}", cfg.id)))?;
    finish_list(cfg, settings, running).await
}

async fn connect_remote(
    cfg: &McpServerConfig,
    settings: &Settings,
    data_dir: &Path,
) -> Result<ReadySlot> {
    let url = expand_config_value(cfg.url.as_deref().unwrap_or("").trim(), &cfg.id, data_dir)?;
    if url.is_empty() {
        return Err(Error::Invalid(format!("mcp url empty: {}", cfg.id)));
    }
    let t = cfg.transport.trim().to_ascii_lowercase();
    if t == "sse" {
        tracing::warn!(
            server = %cfg.id,
            "transport=sse: connecting with Streamable HTTP (MCP 2025-03-26); standalone 2024-11-05 SSE-only servers need a gateway"
        );
    }
    let mut http_cfg = StreamableHttpClientTransportConfig::with_uri(url);
    if let Some(var) = cfg
        .bearer_token_env_var
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        match bearer_token_from_env(var) {
            Some(token) => {
                http_cfg = http_cfg.auth_header(token);
            }
            None => {
                tracing::warn!(
                    server = %cfg.id,
                    env = var,
                    "mcp bearer env var missing or empty"
                );
            }
        }
    }
    let mut custom = HashMap::new();
    for (k, v) in &cfg.headers {
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|err| Error::Invalid(format!("mcp {}: bad header name {k}: {err}", cfg.id)))?;
        let resolved = expand_config_value(v, &cfg.id, data_dir)?;
        let value = HeaderValue::from_str(&resolved)
            .map_err(|err| Error::Invalid(format!("mcp {}: bad header {k}: {err}", cfg.id)))?;
        custom.insert(name, value);
    }
    if !custom.is_empty() {
        http_cfg = http_cfg.custom_headers(custom);
    }
    let transport = StreamableHttpClientTransport::from_config(http_cfg);
    let running = ()
        .serve(transport)
        .await
        .map_err(|err| Error::Invalid(format!("mcp init {}: {err}", cfg.id)))?;
    finish_list(cfg, settings, running).await
}

async fn finish_list(
    cfg: &McpServerConfig,
    settings: &Settings,
    running: RunningService<RoleClient, ()>,
) -> Result<ReadySlot> {
    let listed_tools = running
        .list_tools(Default::default())
        .await
        .map_err(|err| Error::Invalid(format!("mcp list {}: {err}", cfg.id)))?;
    let mut raw = Vec::new();
    for t in listed_tools.tools {
        let params = Value::Object((*t.input_schema).clone());
        let desc = t.description.as_deref().unwrap_or("").to_string();
        let read_only = t
            .annotations
            .as_ref()
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false);
        if let Some(c) = candidate_from_mcp(&cfg.id, t.name.as_ref(), desc, params, read_only) {
            raw.push(c);
        }
    }
    let listed_count = raw.len();
    let budget = apply_schema_budget(raw, settings);
    let peer = running.peer().clone();
    Ok(ReadySlot {
        peer,
        service: Some(running),
        listed: budget.kept,
        skipped: budget.skipped,
        listed_count,
    })
}

/// Resolve one MCP config value: a `secret:<name>` value reads the
/// credential store (never settings.json or project files);
/// `${env:VAR}`/`${VAR}` read the process environment; everything else is a
/// literal.
fn expand_config_value(raw: &str, server_id: &str, data_dir: &Path) -> Result<String> {
    if let Some(name) = raw.trim().strip_prefix(MCP_SECRET_REF) {
        let name = name.trim();
        let full = crate::secret::mcp_secret_name(server_id, name)
            .map_err(|err| Error::Invalid(format!("mcp {server_id}: {err}")))?;
        let store = FileCredentialStore::new(data_dir);
        return store.get(&full)?.ok_or_else(|| {
            Error::Invalid(format!(
                "mcp {server_id}: secret reference has no stored credential for '{name}'"
            ))
        });
    }
    Ok(expand_env_refs(raw))
}

fn expand_env_refs(raw: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\$\{(?:env:)?([A-Za-z_][A-Za-z0-9_]*)\}").expect("env ref")
    });
    re.replace_all(raw, |caps: &regex::Captures| {
        std::env::var(&caps[1]).unwrap_or_default()
    })
    .into_owned()
}

fn bearer_token_from_env(var: &str) -> Option<String> {
    let raw = std::env::var(var).ok()?;
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    let token = t
        .strip_prefix("Bearer ")
        .or_else(|| t.strip_prefix("bearer "))
        .unwrap_or(t)
        .trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

struct McpProxy {
    spec: ToolSpec,
    server_id: String,
    remote_name: String,
    peer: rmcp::service::Peer<RoleClient>,
    inner: Arc<Inner>,
    failure_threshold: u32,
    read_only: bool,
}

/// Shared by direct injection (`snapshot_for`) and lazy activation
/// (`proxy_for_candidate`) so a promoted tool dispatches exactly like one
/// registered from the start.
fn build_proxy(
    c: &super::schema::SchemaCandidate,
    peer: &rmcp::service::Peer<RoleClient>,
    inner: &Arc<Inner>,
    failure_threshold: u32,
) -> Arc<dyn Tool> {
    Arc::new(McpProxy {
        spec: ToolSpec {
            name: c.name.clone(),
            description: c.description.clone(),
            parameters: c.parameters.clone(),
            source: ToolSource::Mcp {
                server_id: c.server_id.clone(),
            },
        },
        server_id: c.server_id.clone(),
        remote_name: c.remote_name.clone(),
        peer: peer.clone(),
        inner: inner.clone(),
        failure_threshold,
        read_only: c.read_only,
    })
}

impl McpProxy {
    /// Transport-level failure accounting. After `mcp_failure_threshold`
    /// consecutive transport errors the slot is evicted so the next
    /// `snapshot_for` respawns the server (backoff-gated); the failing tool
    /// call itself still returns a sanitized error to the model only.
    async fn record_transport_error(&self) {
        let mut failures = self.inner.failures.lock().await;
        let count = failures.entry(self.server_id.clone()).or_insert(0);
        *count += 1;
        if *count < self.failure_threshold.max(1) {
            return;
        }
        failures.remove(&self.server_id);
        drop(failures);
        let removed = {
            let mut slots = self.inner.slots.lock().await;
            matches!(slots.remove(&self.server_id), Some(Slot::Ready(_)))
        };
        if removed {
            tracing::warn!(
                server = %self.server_id,
                threshold = self.failure_threshold,
                "mcp server evicted after consecutive transport errors; will respawn with backoff"
            );
        }
    }
}

#[async_trait]
impl Tool for McpProxy {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    fn read_only_annotation(&self) -> bool {
        self.read_only
    }

    async fn call(&self, args: Value, _ctx: ToolCtx) -> String {
        let obj: Map<String, Value> = match args {
            Value::Object(m) => m,
            other => {
                return format!(
                    "Error: mcp: {}: arguments must be an object (got {other})",
                    self.server_id
                );
            }
        };
        let params = CallToolRequestParams::new(self.remote_name.clone()).with_arguments(obj);
        match self.peer.call_tool(params).await {
            Ok(result) => {
                self.inner.failures.lock().await.remove(&self.server_id);
                format_mcp_result(&self.server_id, result)
            }
            Err(err) => {
                self.record_transport_error().await;
                format!(
                    "Error: mcp: {}: {} (the server may be restarting)",
                    self.server_id,
                    sanitize_error_text(&err.to_string())
                )
            }
        }
    }
}

/// Jittered respawn backoff for a failed server. The ±20 % jitter is
/// derived from the server id so restart storms from several servers cannot
/// synchronize (Hermes `mcp_tool.py` reconnect pacing).
fn reconnect_backoff(settings: &Settings, server_id: &str) -> Duration {
    let base = settings.mcp_reconnect_backoff_sec.max(1);
    let hash: u64 = server_id
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    let jitter_pct = (hash % 41) as i64 - 20;
    let secs = (base as i64 * (100 + jitter_pct) / 100).max(1) as u64;
    Duration::from_secs(secs)
}

/// Scrub credential-shaped material from anything that can reach the model,
/// `mcp_list`, trace or logs: URL query strings, bearer values, and
/// key/token/secret/password/authorization/cookie assignments
/// (Hermes `_sanitize_error` parity).
pub fn sanitize_error_text(text: &str) -> String {
    static URL_QUERY: OnceLock<regex::Regex> = OnceLock::new();
    static BEARER: OnceLock<regex::Regex> = OnceLock::new();
    static ASSIGN: OnceLock<regex::Regex> = OnceLock::new();
    let out = URL_QUERY
        .get_or_init(|| {
            regex::Regex::new(r#"https?://[^\s"'<>]+\?[^\s"'<>]+"#).expect("url query re")
        })
        .replace_all(text, |caps: &regex::Captures| {
            let url = &caps[0];
            let cut = url.find('?').unwrap_or(url.len());
            format!("{}?<redacted>", &url[..cut])
        })
        .into_owned();
    let out = BEARER
        .get_or_init(|| {
            regex::Regex::new(r"(?i)bearer\s+[A-Za-z0-9._~+/=-]{6,}").expect("bearer re")
        })
        .replace_all(&out, "Bearer [REDACTED]")
        .into_owned();
    ASSIGN
        .get_or_init(|| {
            regex::Regex::new(
                r#"(?i)(api[_-]?key|token|secret|password|authorization|cookie)(\s*[=:]\s*)[^\s,;&"']+"#,
            )
            .expect("assign re")
        })
        .replace_all(&out, |caps: &regex::Captures| {
            format!("{}{}[REDACTED]", &caps[1], &caps[2])
        })
        .into_owned()
}

fn server_fingerprint(cfg: &McpServerConfig) -> String {
    serde_json::to_string(cfg).unwrap_or_default()
}

fn format_mcp_result(server_id: &str, result: rmcp::model::CallToolResult) -> String {
    let mut texts = Vec::new();
    for block in result.content {
        if let ContentBlock::Text(t) = block {
            texts.push(t.text);
        }
    }
    let body = if !texts.is_empty() {
        texts.join("\n")
    } else if let Some(v) = result.structured_content {
        v.to_string()
    } else {
        String::new()
    };
    let body = redact_mcp_body(&body);
    if result.is_error == Some(true) {
        format!("Error: mcp: {server_id}: {body}")
    } else {
        body
    }
}

fn redact_mcp_body(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        return crate::trace::redact_value(&value).to_string();
    }
    sanitize_error_text(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpServerConfig;

    #[tokio::test]
    async fn disabled_server_listed_without_spawn() {
        let mgr = McpManager::new();
        let mut settings = Settings::defaults();
        settings.mcp_servers = vec![McpServerConfig {
            id: "echo".into(),
            enabled: false,
            command: "definitely-missing-binary".into(),
            ..Default::default()
        }];
        let list = mgr.mcp_list(&settings).await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].enabled);
        assert!(!list[0].ready);
        assert!(list[0].error.is_none());
    }

    #[tokio::test]
    async fn snapshot_without_ids_is_empty() {
        let mgr = McpManager::new();
        let data_dir = tempfile::tempdir().unwrap();
        let snap = mgr
            .snapshot_for(
                &[],
                &Settings::defaults(),
                data_dir.path(),
                &Default::default(),
            )
            .await;
        assert!(snap.tools.is_empty());
        assert!(snap.parallel_unscoped.is_empty());
    }

    #[tokio::test]
    async fn stop_unknown_id_ok() {
        McpManager::new().mcp_stop("nope").await.unwrap();
    }

    #[tokio::test]
    async fn snapshot_unknown_id_is_empty() {
        let data_dir = tempfile::tempdir().unwrap();
        let snap = McpManager::new()
            .snapshot_for(
                &["nope".into()],
                &Settings::defaults(),
                data_dir.path(),
                &Default::default(),
            )
            .await;
        assert!(snap.tools.is_empty());
        assert!(snap.parallel_unscoped.is_empty());
    }

    #[test]
    fn error_result_prefixed() {
        let mut err = rmcp::model::CallToolResult::default();
        err.content = vec![ContentBlock::text("boom")];
        err.is_error = Some(true);
        let s = format_mcp_result("echo", err);
        assert_eq!(s, "Error: mcp: echo: boom");
    }

    #[test]
    fn ok_text_joined() {
        let mut ok = rmcp::model::CallToolResult::default();
        ok.content = vec![ContentBlock::text("a"), ContentBlock::text("b")];
        assert_eq!(format_mcp_result("echo", ok), "a\nb");
    }

    #[test]
    fn sanitizes_credentials_from_successful_tool_bodies_too() {
        let mut ok = rmcp::model::CallToolResult::default();
        ok.content = vec![ContentBlock::text("token=do-not-return-this")];
        assert_eq!(format_mcp_result("echo", ok), "token=[REDACTED]");

        let mut structured = rmcp::model::CallToolResult::default();
        structured.structured_content = Some(serde_json::json!({
            "data": {"apiKey": "do-not-return-this", "count": 2}
        }));
        let rendered = format_mcp_result("echo", structured);
        assert!(!rendered.contains("do-not-return-this"), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");
        assert!(rendered.contains("count"), "{rendered}");
    }

    #[test]
    fn structured_content_when_no_text() {
        let mut ok = rmcp::model::CallToolResult::default();
        ok.structured_content = Some(serde_json::json!({"k": 1}));
        assert_eq!(format_mcp_result("echo", ok), "{\"k\":1}");
    }

    #[test]
    fn sanitizes_credentials_from_provider_error_shapes() {
        let raw = "request https://mcp.example/tools?api_key=top-secret&tenant=abc Bearer bearer-secret token=also-secret";
        let safe = sanitize_error_text(raw);
        assert!(!safe.contains("top-secret"), "{safe}");
        assert!(!safe.contains("bearer-secret"), "{safe}");
        assert!(!safe.contains("also-secret"), "{safe}");
        assert!(
            safe.contains("https://mcp.example/tools?<redacted>"),
            "{safe}"
        );
        assert!(safe.contains("Bearer [REDACTED]"), "{safe}");
    }

    #[test]
    fn expand_env_refs_cursor_and_plain() {
        std::env::set_var("DANDELION_TEST_MCP_TOKEN", "secret-token");
        assert_eq!(
            expand_env_refs("Bearer ${env:DANDELION_TEST_MCP_TOKEN}"),
            "Bearer secret-token"
        );
        assert_eq!(
            expand_env_refs("https://x/${DANDELION_TEST_MCP_TOKEN}/mcp"),
            "https://x/secret-token/mcp"
        );
        std::env::remove_var("DANDELION_TEST_MCP_TOKEN");
    }

    #[test]
    fn bearer_token_strips_prefix() {
        std::env::set_var("DANDELION_TEST_MCP_BEARER", "Bearer abc");
        assert_eq!(
            bearer_token_from_env("DANDELION_TEST_MCP_BEARER").as_deref(),
            Some("abc")
        );
        std::env::remove_var("DANDELION_TEST_MCP_BEARER");
        assert!(bearer_token_from_env("DANDELION_TEST_MCP_BEARER_MISSING").is_none());
    }

    #[tokio::test]
    async fn remote_bad_url_listed_failed() {
        let mgr = McpManager::new();
        let mut settings = Settings::defaults();
        settings.mcp_spawn_timeout_ms = 2_000;
        settings.mcp_servers = vec![McpServerConfig {
            id: "remote".into(),
            enabled: true,
            url: Some("http://127.0.0.1:1/mcp".into()),
            ..Default::default()
        }];
        let data_dir = tempfile::tempdir().unwrap();
        mgr.mcp_start("remote", &settings, data_dir.path())
            .await
            .unwrap();
        let list = mgr.mcp_list(&settings).await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].ready);
        assert!(list[0].error.is_some());
    }

    #[tokio::test]
    async fn list_does_not_auto_start() {
        let mgr = McpManager::new();
        let mut settings = Settings::defaults();
        settings.mcp_servers = vec![McpServerConfig {
            id: "echo".into(),
            enabled: true,
            command: "definitely-missing-binary".into(),
            ..Default::default()
        }];
        let list = mgr.mcp_list(&settings).await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].enabled);
        assert!(!list[0].ready);
        assert!(list[0].error.is_none());
    }

    #[test]
    fn reconnect_backoff_stays_in_jitter_band_and_is_deterministic() {
        let mut settings = Settings::defaults();
        settings.mcp_reconnect_backoff_sec = 10;
        for id in ["a", "jira", "io.github.owner.server", "x7"] {
            let d = reconnect_backoff(&settings, id);
            assert!(d.as_secs() >= 8 && d.as_secs() <= 12, "{id}: {d:?}");
            assert_eq!(
                d,
                reconnect_backoff(&settings, id),
                "{id} jitter must be stable"
            );
        }
        // Floor of 1 s even with a degenerate base, so backoff never becomes a hot loop.
        settings.mcp_reconnect_backoff_sec = 1;
        assert!(reconnect_backoff(&settings, "srv").as_secs() >= 1);
    }
}
