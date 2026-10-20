//! Account and release APIs are host services, separate from the Agent loop.
//! Passwords are never persisted; session tokens stay in CredentialStore.
use crate::{map_err, AppState};
use dandelion_core::{
    secret::{CredentialStore, FileCredentialStore},
    Settings,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Mutex;

#[derive(Default)]
pub struct AccountHost {
    pub operation: Mutex<()>,
    auth_operation: Mutex<()>,
    installing: AtomicBool,
    active_prompts: AtomicUsize,
    pub lease: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    /// 断网宽限到期时刻。授权曾经授予过之后，临时性校验失败（超时/断网/坏响应）
    /// 不再立即等同撤销——见 `note_transient_failure`。明确撤销会把两者一起清零，
    /// 之后不可能再进入宽限。
    pub grace: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    /// 当前这份授权已经用掉的宽限所对应的租约到期时刻：一份授权最多宽限一次，
    /// 防止长时间断网被无限续期。新的成功授权会重置它。
    grace_armed_for: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    /// 是否已经因「工作不可用」取消过任务并通知过前端。宽限耗尽后的每个 tick
    /// 只切换状态，不再反复 cancel / 弹提示。
    blocked: AtomicBool,
    /// account_status 结果缓存。启动时监控循环、App、TitleBar 会背靠背查同一份
    /// 授权，v0.0.1 每次都排队 auth_operation 锁 + 各发两次 HTTP；现在 TTL 内
    /// 共用同一份校验结果。
    status_cache: std::sync::Mutex<Option<(std::time::Instant, (Status, Probe))>>,
}
pub struct PromptGuard<'a>(&'a AtomicUsize);
impl Drop for PromptGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl AccountHost {
    pub async fn begin_prompt(&self) -> Result<PromptGuard<'_>, String> {
        let _lock = self.operation.lock().await;
        if self.installing.load(Ordering::SeqCst) {
            return Err("正在安装软件更新，请完成后再发送任务".into());
        }
        self.active_prompts.fetch_add(1, Ordering::SeqCst);
        Ok(PromptGuard(&self.active_prompts))
    }
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    access: Option<Access>,
    configured: bool,
    server_url: String,
    profile: Option<Value>,
    message: Option<String>,
    version: String,
    channel: String,
    update_key_configured: bool,
    can_install: bool,
    /// 断网宽限期内为 true：授权没有被撤销，只是暂时无法复核。前端据此保持
    /// 界面可用；明确撤销时恒为 false。
    grace: bool,
}

/// 校验失败的区分放在类型里，语义对齐 ref/codex 的
/// `RefreshTokenError::{Permanent, Transient}` 和 ref/Hermes 的
/// `_TERMINAL_AUTH_REASONS` 白名单：`revocation` 只在服务端明确说当前授权
/// 不能继续时置位（401 且令牌已销毁、或复核后拿不到任何可用授权时长）。
/// 连接失败、超时、响应格式错误、一般业务错误都只是「暂时无法复核」，
/// 绝不能按撤销处理。
#[derive(Debug, Clone)]
pub struct ClientError {
    message: String,
    revocation: bool,
}

impl ClientError {
    fn transient(message: impl Into<String>) -> Self {
        Self { message: message.into(), revocation: false }
    }
}

impl From<ClientError> for String {
    fn from(error: ClientError) -> Self {
        error.message
    }
}

impl From<String> for ClientError {
    fn from(message: String) -> Self {
        Self { message, revocation: false }
    }
}

impl From<&str> for ClientError {
    fn from(message: &str) -> Self {
        Self { message: message.into(), revocation: false }
    }
}

/// 一次账号状态探测的整体结论。`Transient` 之外的值都代表拿到了明确答案。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Probe {
    /// 服务端明确应答（授权、拒绝、或未登录——都是确定状态）。
    Definitive,
    /// 超时、断网、响应格式错误：保留现有租约，进入宽限评估。
    Transient,
    /// 服务端明确说登录已失效（401，令牌已销毁）。
    Revoked,
    /// 服务地址未配置，无从授权。
    NotConfigured,
}

struct Client {
    base: reqwest::Url,
    http: reqwest::Client,
    store: FileCredentialStore,
    token_key: String,
    channel: String,
    pubkey: String,
    timeout: Duration,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Access {
    pub allowed: bool,
    pub code: String,
    pub message: String,
    pub valid_for_ms: u64,
}

fn update_lease(
    host: &AccountHost,
    access: Option<&Access>,
    maximum_ms: u64,
    requested_at: std::time::Instant,
) {
    let granted = access
        .filter(|a| a.allowed && a.valid_for_ms > 0 && !host.installing.load(Ordering::SeqCst))
        .map(|a| requested_at + Duration::from_millis(a.valid_for_ms.min(maximum_ms)));
    *host.lease.lock().unwrap() = granted;
    if granted.is_some() {
        // 成功核验到新授权：旧的宽限与「已用掉」标记一并作废，新授权重新
        // 获得一份独立的宽限预算。
        *host.grace.lock().unwrap() = None;
        *host.grace_armed_for.lock().unwrap() = None;
        host.blocked.store(false, Ordering::SeqCst);
    }
}

pub fn lease_valid(lease: &std::sync::Mutex<Option<std::time::Instant>>) -> bool {
    lease
        .lock()
        .map(|until| until.is_some_and(|until| until > std::time::Instant::now()))
        .unwrap_or(false)
}

pub fn grace_valid(grace: &std::sync::Mutex<Option<std::time::Instant>>) -> bool {
    lease_valid(grace)
}

impl AccountHost {
    /// 已授予过授权的工作是否仍可继续：租约未过期，或仍在断网宽限期内。
    /// 明确撤销会同时清零租约与宽限，因此撤销后的限制永远不会被放宽。
    pub fn work_allowed(&self) -> bool {
        lease_valid(&self.lease) || grace_valid(&self.grace)
    }
}

/// 临时性校验失败（超时/断网/坏响应）后的一次宽限评估。
/// - 从未拿到过授权（lease=None）：维持 fail-closed，宽限不救济未登录用户。
/// - 曾成功核验、租约已过期（Some(expired) 仍留在槽里作「曾有授权」标记）：
///   允许一次有界宽限，覆盖一次重启窗口或一段短暂断网。
/// - 同一份授权已用过宽限（armed 与当前租约到期值相同）：不再续期。
/// 参照 ref/codex `auth()` 短暂失败时返回旧凭据、ref/Hermes 的有界冷却；
/// 与两者一致，撤销永远不走这条路。
fn note_transient_failure(host: &AccountHost, grace_ms: u64) {
    if grace_ms == 0 || grace_valid(&host.grace) {
        return;
    }
    let Ok(lease) = host.lease.lock() else { return };
    let Some(expiry) = *lease else { return };
    let Ok(mut armed) = host.grace_armed_for.lock() else { return };
    if *armed == Some(expiry) {
        return;
    }
    *armed = Some(expiry);
    drop(lease);
    *host.grace.lock().unwrap() =
        Some(std::time::Instant::now() + Duration::from_millis(grace_ms));
    tracing::warn!(grace_ms, "账号服务暂时不可达，授权与运行中任务在宽限期内保留");
}

/// 明确撤销（服务端 allowed=false、401、服务未配置、复核不出可用授权时长）：
/// 授权与宽限一起清零并停止运行中的任务。之后 `note_transient_failure` 因
/// lease=None 直接返回，撤销后的限制不可能被放宽。
async fn revoke_access(state: &AppState, host: &AccountHost) {
    *host.lease.lock().unwrap() = None;
    *host.grace.lock().unwrap() = None;
    *host.grace_armed_for.lock().unwrap() = None;
    host.blocked.store(true, Ordering::SeqCst);
    state.runtime.read().await.cancel_host_work().await;
}

/// 非撤销原因导致的工作不可用（宽限耗尽、从未授权）：只在第一次发生时
/// 取消任务并通知前端，后续 tick 静默，避免 v0.0.1 每 tick 弹一次「无法验证」。
async fn block_work_once(app: &AppHandle, state: &AppState, host: &AccountHost, reason: &str) {
    if host.work_allowed() {
        host.blocked.store(false, Ordering::SeqCst);
        return;
    }
    if !host.blocked.swap(true, Ordering::SeqCst) {
        state.runtime.read().await.cancel_host_work().await;
        let _ = app.emit("account-access-denied", reason);
    }
}

pub async fn require_access(app: &AppHandle) -> Result<(), String> {
    let host = app.state::<AccountHost>();
    if host.installing.load(Ordering::SeqCst) {
        return Err("正在安装更新，请稍候".into());
    }
    // 快速路径：租约或宽限仍有效就不再排队 auth_operation、不再发网络请求。
    // v0.0.1 在每个受控 invoke 上先拿锁再做一次 HTTP 探测，启动阶段的会话
    // 加载 IPC 全部排在账号服务延迟之后——这是启动慢的主因之一。
    if host.work_allowed() {
        return Ok(());
    }
    let _lock = host.auth_operation.lock().await;
    // single-flight：等锁期间可能已有另一个调用完成了同一份核验。
    if host.work_allowed() {
        return Ok(());
    }
    let state = app.state::<AppState>();
    let settings = state.runtime.read().await.settings();
    let grace_ms = settings.account_grace_ms;
    let result = async {
        let c = Client::new(&settings, &state.host_data_dir())?;
        let requested_at = std::time::Instant::now();
        let value = c
            .request(reqwest::Method::GET, "api/v1/access", None, true)
            .await?;
        let access: Access =
            serde_json::from_value(value).map_err(|_| ClientError::transient("授权响应格式不正确"))?;
        update_lease(
            &host,
            Some(&access),
            settings.account_recheck_ms,
            requested_at,
        );
        if access.allowed && lease_valid(&host.lease) {
            Ok(())
        } else if access.allowed {
            // 服务端说允许、但拿不到任何可用授权时长：维持 v0.0.1 的拒绝语义
            // （撤销式，不进宽限），不让坏数据变相放行。
            Err(ClientError { message: "授权验证超时，请重试".into(), revocation: true })
        } else {
            Err(ClientError { message: access.message, revocation: true })
        }
    }
    .await;
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.revocation => {
            // 明确撤销：立即回收授权、停止任务并通知，绝不进入宽限。
            revoke_access(&state, &host).await;
            let _ = app.emit("account-access-denied", error.message.clone());
            Err(error.message)
        }
        Err(error) => {
            // 临时失败：曾授予授权的按有界宽限放行；宽限耗尽或从未授权的
            // 仍然拒绝，但取消与通知只在第一次发生（block_work_once）。
            note_transient_failure(&host, grace_ms);
            if host.work_allowed() {
                return Ok(());
            }
            block_work_once(app, &state, &host, &error.message).await;
            Err(error.message)
        }
    }
}

pub fn needs_access(command: &str) -> bool {
    // Recovery operations stay reachable even if authentication or the service fails.
    !(command.starts_with("account_")
        || matches!(
            command,
            "boot_marks"
                | "session_cancel"
                | "host_permission_respond"
                | "agent_plugin_stop"
                | "agent_plugin_install_cancel"
                | "terminal_kill"
                | "space_build_cancel"
                | "wiki_build_cancel"
                | "wiki_cancel_draft"
                | "cron_stop_current"
                | "sidecar_stop"
                | "browser_embed_hide"
        ))
}

#[tauri::command]
pub async fn account_connection(
    state: State<'_, AppState>,
    server_url: String,
) -> Result<(), String> {
    if !cfg!(debug_assertions) {
        return Err("发行版的账号服务由软件发行配置指定".into());
    }
    validated_base(&server_url)?;
    state
        .runtime
        .read()
        .await
        .settings_patch(
            json!({"dandelion.account.serverUrl":server_url})
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        .map_err(map_err)?;
    Ok(())
}

pub fn start_access_monitor(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let period = {
                let host = app.state::<AccountHost>();
                let _lock = host.auth_operation.lock().await;
                let state = app.state::<AppState>();
                let settings = state.runtime.read().await.settings();
                let mut period = settings.account_recheck_ms;
                let grace_ms = settings.account_grace_ms;
                // 与 account_status 命令共用同一份缓存：启动时监控、App、
                // TitleBar 背靠背查询只会打一次网络。
                let outcome = tokio::time::timeout(
                    Duration::from_millis(period / 2),
                    refresh_status(&state, &host, false, period.max(2_000) / 2),
                )
                .await;
                match outcome {
                    Ok(Ok((status, probe))) => {
                        if let Some(access) = status.access.as_ref().filter(|a| a.allowed) {
                            period = period.min(access.valid_for_ms.max(1));
                        }
                        match probe {
                            // 临时失败：不动授权，只评估宽限；撤销与明确拒绝才回收。
                            Probe::Transient => note_transient_failure(&host, grace_ms),
                            Probe::Revoked | Probe::NotConfigured => {
                                revoke_access(&state, &host).await
                            }
                            Probe::Definitive
                                if !status.access.as_ref().is_some_and(|a| a.allowed) =>
                            {
                                revoke_access(&state, &host).await
                            }
                            Probe::Definitive => {}
                        }
                        let _ = app.emit("account-status", status);
                    }
                    // 核验自身超时（Ok(Err) 是设置/凭据读取失败）：同样按临时
                    // 失败处理，不再像 v0.0.1 那样直接撤销 + 取消任务。
                    Ok(Err(_)) | Err(_) => note_transient_failure(&host, grace_ms),
                }
                // 统一对账：恢复可工作则清除标记；不可工作则只取消 + 通知一次。
                if host.work_allowed() {
                    host.blocked.store(false, Ordering::SeqCst);
                } else if !host.blocked.swap(true, Ordering::SeqCst) {
                    state.runtime.read().await.cancel_host_work().await;
                    let _ = app.emit("account-access-denied", "无法验证授权，请检查网络后重试");
                }
                period
            };
            tokio::time::sleep(Duration::from_millis((period / 2).max(1))).await;
        }
    });
}

impl Client {
    fn new(settings: &Settings, data: &std::path::Path) -> Result<Self, String> {
        let string = |key: &str| {
            settings
                .extra()
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned()
        };
        let mut base = string("dandelion.account.serverUrl");
        if base.is_empty() {
            base = option_env!("DLN_OPERATIONS_URL")
                .unwrap_or(if cfg!(debug_assertions) {
                    "http://127.0.0.1:8090"
                } else {
                    ""
                })
                .into()
        }
        if !cfg!(debug_assertions) {
            base = option_env!("DLN_OPERATIONS_URL").unwrap_or("").into();
        }
        let base = validated_base(&base)?;
        let mut pubkey = string("dandelion.updates.publicKey");
        if pubkey.is_empty() {
            pubkey = option_env!("DLN_UPDATE_PUBLIC_KEY").unwrap_or("").into()
        }
        if !cfg!(debug_assertions) {
            pubkey = option_env!("DLN_UPDATE_PUBLIC_KEY").unwrap_or("").into();
        }
        let channel = string("dandelion.updates.channel");
        let channel = if channel.is_empty() {
            "stable".into()
        } else {
            channel
        };
        if !["stable", "beta"].contains(&channel.as_str()) {
            return Err("更新通道不合法".into());
        }
        let timeout_ms = settings
            .extra()
            .get("dandelion.updates.timeoutMs")
            .and_then(Value::as_u64)
            .unwrap_or(settings.sse_idle_timeout_ms);
        if timeout_ms == 0 {
            return Err("更新下载超时必须大于 0".into());
        }
        let timeout = Duration::from_millis(timeout_ms);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(settings.http_connect_timeout_ms))
            .timeout(Duration::from_millis(settings.registry_timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "无法建立账号连接")?;
        let scope = format!("{:x}", Sha256::digest(base.as_str().as_bytes()));
        Ok(Self {
            base,
            http,
            store: FileCredentialStore::new(data),
            token_key: format!("account-session-{scope}"),
            channel,
            pubkey,
            timeout,
        })
    }
    fn url(&self, path: &str) -> Result<reqwest::Url, String> {
        self.base.join(path).map_err(|_| "服务地址不合法".into())
    }
    fn token(&self) -> Result<Option<String>, String> {
        self.store.get(&self.token_key).map_err(map_err)
    }
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        authenticated: bool,
    ) -> Result<Value, ClientError> {
        let mut request = self.http.request(method, self.url(path)?);
        if authenticated {
            request = request.bearer_auth(self.token()?.ok_or("请先登录")?)
        }
        if let Some(body) = body {
            request = request.json(&body)
        }
        let response = request
            .send()
            .await
            .map_err(|_| ClientError::transient("无法连接账号服务，请检查服务地址或网络"))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED && authenticated {
            self.store.delete(&self.token_key).map_err(map_err)?;
            // 全流程唯一的撤销标记点：服务端明确 401 且本地令牌已销毁。
            // 其余一切失败都留在 transient，撤销判定不放宽。
            return Err(ClientError { message: "登录已失效，请重新登录".into(), revocation: true });
        }
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let value: Value = response
            .json()
            .await
            .map_err(|_| ClientError::transient("账号服务返回了无法识别的响应"))?;
        if !status.is_success() {
            return Err(ClientError::transient(
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("请求失败，请稍后重试"),
            ));
        }
        Ok(value)
    }
    fn install_id(&self) -> Result<String, String> {
        if let Some(id) = self.store.get("account-install-id").map_err(map_err)? {
            return Ok(id);
        };
        let id = uuid::Uuid::new_v4().to_string();
        self.store.put("account-install-id", &id).map_err(map_err)?;
        Ok(id)
    }
}
fn validated_base(raw: &str) -> Result<reqwest::Url, String> {
    if raw.is_empty() {
        return Err("请先配置账号服务地址".into());
    }
    let mut url = reqwest::Url::parse(raw).map_err(|_| "账号服务地址不合法")?;
    let local = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.scheme() != "https" && !(url.scheme() == "http" && local))
    {
        return Err("账号服务需使用 HTTPS；本地调试可使用 localhost 或 127.0.0.1".into());
    }
    let path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}
async fn client(state: &State<'_, AppState>) -> Result<Client, String> {
    let rt = state.runtime.read().await;
    Client::new(&rt.settings(), &state.host_data_dir())
}
impl AppState {
    fn host_data_dir(&self) -> PathBuf {
        self.host.data_dir()
    }
}
#[tauri::command]
pub async fn account_status(
    state: State<'_, AppState>,
    host: State<'_, AccountHost>,
    force: Option<bool>,
) -> Result<Status, String> {
    let ttl_ms = state.runtime.read().await.settings().account_recheck_ms.max(2_000) / 2;
    // 缓存命中不碰 auth_operation：App、TitleBar、监控循环启动时背靠背查询
    // 同一份授权，去重后只会打一次网络。force=true 只绕过缓存，仍进锁互斥。
    if force != Some(true) {
        if let Some((status, _)) = cached_status(&host, ttl_ms) {
            return Ok(status);
        }
    }
    let _lock = host.auth_operation.lock().await;
    if force != Some(true) {
        // double-check：等锁期间另一个调用可能刚写好缓存。
        if let Some((status, _)) = cached_status(&host, ttl_ms) {
            return Ok(status);
        }
    }
    let (status, _) = refresh_status(&state, &host, false, ttl_ms).await?;
    Ok(status)
}

/// 缓存的 account_status 结论（TTL 内）。
fn cached_status(host: &AccountHost, ttl_ms: u64) -> Option<(Status, Probe)> {
    let cache = host.status_cache.lock().ok()?;
    let (at, result) = cache.as_ref()?;
    (at.elapsed() < Duration::from_millis(ttl_ms)).then(|| result.clone())
}

/// 读缓存或重新探测，并把结论写回缓存。调用方须持有 auth_operation，
/// 保证同一时刻至多一次网络核验（重复启动请求去重的关键）。
async fn refresh_status(
    state: &AppState,
    host: &AccountHost,
    force: bool,
    ttl_ms: u64,
) -> Result<(Status, Probe), String> {
    if !force {
        if let Some(hit) = cached_status(host, ttl_ms) {
            return Ok(hit);
        }
    }
    let result = probe_status(state, host).await?;
    if let Ok(mut cache) = host.status_cache.lock() {
        *cache = Some((std::time::Instant::now(), result.clone()));
    }
    Ok(result)
}

async fn probe_status(state: &AppState, host: &AccountHost) -> Result<(Status, Probe), String> {
    let settings = state.runtime.read().await.settings();
    let configured = Client::new(&settings, &state.host_data_dir());
    let Ok(c) = configured else {
        // 服务未配置无从授权：与 v0.0.1 一致清零租约，这不属于临时故障。
        *host.lease.lock().unwrap() = None;
        return Ok((
            Status {
                access: None,
                configured: false,
                server_url: String::new(),
                profile: None,
                message: configured.err(),
                version: env!("CARGO_PKG_VERSION").into(),
                channel: "stable".into(),
                update_key_configured: false,
                can_install: false,
                grace: false,
            },
            Probe::NotConfigured,
        ));
    };
    let mut message = None;
    let mut transient = false;
    let mut revoked = false;
    let profile = if c.token()?.is_some() {
        match c
            .request(reqwest::Method::GET, "api/v1/account", None, true)
            .await
        {
            Ok(v) => Some(v),
            Err(e) => {
                transient |= !e.revocation;
                revoked |= e.revocation;
                message = Some(e.message);
                None
            }
        }
    } else {
        None
    };
    let requested_at = std::time::Instant::now();
    let mut access = if profile.is_some() {
        match c
            .request(reqwest::Method::GET, "api/v1/access", None, true)
            .await
        {
            Ok(value) => match serde_json::from_value::<Access>(value) {
                Ok(a) => Some(a),
                Err(_) => {
                    // 应答了但格式不对：是坏响应，不是撤销证据。
                    transient = true;
                    message = Some("授权响应格式不正确".into());
                    None
                }
            },
            Err(e) => {
                transient |= !e.revocation;
                revoked |= e.revocation;
                message = Some(e.message);
                None
            }
        }
    } else {
        None
    };
    // 与工作单第 2 条对应的主要差异点：临时失败时完全不碰现有租约——
    // v0.0.1 这里无条件 update_lease(None)，断网即清授权、取消运行中任务。
    // 只有服务端明确应答（授权 / 拒绝 / 未登录）才写入租约。
    if !transient {
        update_lease(
            host,
            access.as_ref(),
            settings.account_recheck_ms,
            requested_at,
        );
    }
    if !host.installing.load(Ordering::SeqCst) && !lease_valid(&host.lease) {
        if let Some(a) = access.as_mut().filter(|a| a.allowed) {
            a.allowed = false;
            a.code = "verification_timeout".into();
            a.message = "授权验证超时，请重试".into();
        }
    }
    let probe = if revoked {
        Probe::Revoked
    } else if transient {
        Probe::Transient
    } else {
        Probe::Definitive
    };
    // grace 只在「确实没复核成功但工作仍被允许」时为 true；明确应答路径恒为 false。
    let grace = transient && host.work_allowed();
    Ok((
        Status {
            access,
            configured: true,
            server_url: c.base.to_string(),
            profile,
            message,
            version: env!("CARGO_PKG_VERSION").into(),
            channel: c.channel,
            update_key_configured: !c.pubkey.is_empty(),
            can_install: !cfg!(debug_assertions) && !c.pubkey.is_empty(),
            grace,
        },
        probe,
    ))
}
#[tauri::command]
pub async fn account_register(
    state: State<'_, AppState>,
    email: String,
    name: String,
    password: String,
) -> Result<(), String> {
    client(&state)
        .await?
        .request(
            reqwest::Method::POST,
            "api/v1/auth/register",
            Some(json!({"email":email,"name":name,"password":password})),
            false,
        )
        .await?;
    Ok(())
}
#[tauri::command]
pub async fn account_login(
    state: State<'_, AppState>,
    host: State<'_, AccountHost>,
    email: String,
    password: String,
) -> Result<Value, String> {
    let _lock = host.auth_operation.lock().await;
    *host.lease.lock().unwrap() = None;
    let c = client(&state).await?;
    let value=c.request(reqwest::Method::POST,"api/v1/auth/login",Some(json!({"email":email,"password":password,"installID":c.install_id()?,"deviceName":format!("蒲公英 {}",std::env::consts::OS)})),false).await?;
    let token = value
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| t.len() == 48)
        .ok_or("登录响应不完整")?;
    c.store.put(&c.token_key, token).map_err(map_err)?;
    let mut profile = value.get("account").cloned().unwrap_or(Value::Null);
    if let Some(object) = profile.as_object_mut() {
        object.insert(
            "deviceWarning".into(),
            value.get("deviceWarning").cloned().unwrap_or(Value::Null),
        );
    }
    Ok(profile)
}
#[tauri::command]
pub async fn account_logout(
    state: State<'_, AppState>,
    host: State<'_, AccountHost>,
) -> Result<(), String> {
    let _lock = host.auth_operation.lock().await;
    *host.lease.lock().unwrap() = None;
    state.runtime.read().await.cancel_host_work().await;
    let c = client(&state).await?;
    let _ = c
        .request(reqwest::Method::POST, "api/v1/auth/logout", None, true)
        .await;
    c.store.delete(&c.token_key).map_err(map_err)
}
#[tauri::command]
pub async fn account_plans(state: State<'_, AppState>) -> Result<Value, String> {
    client(&state)
        .await?
        .request(reqwest::Method::GET, "api/v1/plans", None, false)
        .await
        .map_err(String::from)
}
#[tauri::command]
pub async fn account_create_order(
    state: State<'_, AppState>,
    plan: String,
) -> Result<Value, String> {
    client(&state)
        .await?
        .request(
            reqwest::Method::POST,
            "api/v1/orders",
            Some(json!({"plan":plan})),
            true,
        )
        .await
        .map_err(String::from)
}
#[tauri::command]
pub async fn account_orders(state: State<'_, AppState>) -> Result<Value, String> {
    client(&state)
        .await?
        .request(reqwest::Method::GET, "api/v1/orders", None, true)
        .await
        .map_err(String::from)
}
#[tauri::command]
pub async fn account_unbind_device(state: State<'_, AppState>, id: u64) -> Result<(), String> {
    client(&state)
        .await?
        .request(
            reqwest::Method::DELETE,
            &format!("api/v1/devices/{id}"),
            None,
            true,
        )
        .await?;
    Ok(())
}
fn target() -> String {
    format!(
        "{}-{}",
        if cfg!(target_os = "macos") {
            "darwin"
        } else {
            std::env::consts::OS
        },
        std::env::consts::ARCH
    )
}
#[tauri::command]
pub async fn account_check_update(state: State<'_, AppState>) -> Result<Value, String> {
    let c = client(&state).await?;
    c.request(
        reqwest::Method::GET,
        &format!(
            "api/v1/updates/{}/{}/{}",
            c.channel,
            target(),
            env!("CARGO_PKG_VERSION")
        ),
        None,
        false,
    )
    .await
    .map_err(String::from)
}
struct Installing<'a>(&'a AtomicBool);
impl Drop for Installing<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst)
    }
}
#[tauri::command]
pub async fn account_install_update(
    app: AppHandle,
    state: State<'_, AppState>,
    host: State<'_, AccountHost>,
    expected_version: String,
) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Err("开发环境只检查更新，请使用安装版验证升级安装".into());
    }
    let operation = host.operation.lock().await;
    if host.active_prompts.load(Ordering::SeqCst) > 0
        || state.runtime.read().await.has_active_turns()
    {
        return Err("请先结束所有窗口中的运行任务，再安装更新".into());
    }
    if host.installing.swap(true, Ordering::SeqCst) {
        return Err("正在安装更新".into());
    }
    let _guard = Installing(&host.installing);
    *host.lease.lock().unwrap() = None;
    drop(operation);
    let c = client(&state).await?;
    if c.pubkey.is_empty() {
        return Err("尚未配置更新签名公钥".into());
    }
    // A fresh check avoids installing a release withdrawn since the UI preview.
    let url = c.url(&format!(
        "api/v1/updates/{}/{}/{}",
        c.channel,
        target(),
        env!("CARGO_PKG_VERSION")
    ))?;
    let exit_app = app.clone();
    let update = app
        .updater_builder()
        .on_before_exit(move || {
            if let Some(state) = exit_app.try_state::<AppState>() {
                if let Ok(rt) = state.runtime.try_read() {
                    rt.sidecar_kill_on_exit();
                }
            }
        })
        .pubkey(c.pubkey)
        .endpoints(vec![url])
        .map_err(|e| e.to_string())?
        .timeout(c.timeout)
        .build()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| format!("检查更新失败：{e}"))?
        .ok_or("暂无可安装更新")?;
    if update.version != expected_version {
        return Err("可用版本发生变化，请重新检查更新".into());
    }
    let mut downloaded = 0u64;
    let emit = app.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = emit.emit(
                    "account-update-progress",
                    json!({"downloaded":downloaded,"total":total}),
                );
            },
            || {},
        )
        .await
        .map_err(|e| format!("更新下载或签名验证失败：{e}"))?;

    update
        .install(bytes)
        .map_err(|e| format!("安装更新失败：{e}"))?;
    {
        let rt = state.runtime.read().await;
        rt.sidecar_kill_on_exit();
    }
    app.restart()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_lease_fails_closed_and_recovery_remains_available() {
        let host = AccountHost::default();
        assert!(!lease_valid(&host.lease));
        let access = Access {
            allowed: true,
            code: "authorized".into(),
            message: "ok".into(),
            valid_for_ms: 1000,
        };
        update_lease(&host, Some(&access), 1, std::time::Instant::now());
        std::thread::sleep(Duration::from_millis(5));
        assert!(!lease_valid(&host.lease));
        update_lease(&host, Some(&access), 1000, std::time::Instant::now());
        assert!(lease_valid(&host.lease));
        update_lease(&host, None, 1000, std::time::Instant::now());
        assert!(!lease_valid(&host.lease));
        update_lease(
            &host,
            Some(&access),
            1000,
            std::time::Instant::now() - Duration::from_secs(2),
        );
        assert!(
            !lease_valid(&host.lease),
            "response delay must not extend server authorization"
        );
        host.installing.store(true, Ordering::SeqCst);
        update_lease(&host, Some(&access), 1000, std::time::Instant::now());
        assert!(
            !lease_valid(&host.lease),
            "updates must prevent cron admission"
        );
        for command in [
            "session_prompt",
            "cron_run_now",
            "settings_patch",
            "terminal_run_user",
            "wiki_compile",
            "knowledge_import_files",
            "future_sensitive_command",
        ] {
            assert!(needs_access(command), "{command}");
        }
        for command in [
            "account_login",
            "account_register",
            "account_install_update",
            "account_status",
            "session_cancel",
            "terminal_kill",
        ] {
            assert!(!needs_access(command), "{command}");
        }
    }
    #[test]
    fn transient_failure_gets_bounded_grace_and_revocation_is_final() {
        let host = AccountHost::default();
        // 从未授权：临时故障不得凭空造出宽限（fail-closed 维持 v0.0.1）。
        note_transient_failure(&host, 10_000);
        assert!(!host.work_allowed(), "never-granted account must stay blocked");
        // 曾授予授权、租约刚过期：允许一次有界宽限，运行中任务不被断网误杀。
        let access = Access {
            allowed: true,
            code: "authorized".into(),
            message: "ok".into(),
            valid_for_ms: 1,
        };
        update_lease(&host, Some(&access), 1000, std::time::Instant::now());
        std::thread::sleep(Duration::from_millis(5));
        assert!(!lease_valid(&host.lease));
        note_transient_failure(&host, 10_000);
        assert!(host.work_allowed(), "brief outage must not purge a once-valid grant");
        // 一份授权只给一次宽限：宽限用完后反复超时不能无限续期。
        *host.grace.lock().unwrap() = None;
        note_transient_failure(&host, 10_000);
        assert!(!host.work_allowed(), "grace must be bounded, not stretched by repeated timeouts");
        // 明确撤销：授权与宽限一起清零；之后的临时失败不得再放宽限制。
        update_lease(&host, Some(&access), 1000, std::time::Instant::now() + Duration::from_secs(30));
        assert!(host.work_allowed());
        *host.lease.lock().unwrap() = None;
        *host.grace.lock().unwrap() = None;
        *host.grace_armed_for.lock().unwrap() = None;
        note_transient_failure(&host, 10_000);
        assert!(!host.work_allowed(), "grace must never loosen limits after explicit revocation");
    }

    #[test]
    fn fresh_grant_reopens_the_grace_budget() {
        let host = AccountHost::default();
        let access = Access {
            allowed: true,
            code: "authorized".into(),
            message: "ok".into(),
            valid_for_ms: 1,
        };
        update_lease(&host, Some(&access), 1000, std::time::Instant::now());
        std::thread::sleep(Duration::from_millis(5));
        note_transient_failure(&host, 5_000);
        *host.grace.lock().unwrap() = None; // 模拟宽限到期
        note_transient_failure(&host, 5_000);
        assert!(!host.work_allowed(), "spent grant must stay spent");
        // 一次成功的重新核验重新打开预算：之后的断网又可宽限一次。
        update_lease(&host, Some(&access), 1000, std::time::Instant::now() + Duration::from_secs(30));
        assert!(host.work_allowed());
        *host.lease.lock().unwrap() = Some(std::time::Instant::now()); // 租约到期、留「曾有授权」标记
        note_transient_failure(&host, 5_000);
        assert!(host.work_allowed(), "a new grant must carry a fresh grace budget");
    }

    #[tokio::test]
    async fn request_errors_distinguish_transient_failure_from_revocation() {
        // 连接被拒：只是暂时无法复核，绝不能被当作撤销。
        let directory = tempfile::tempdir().unwrap();
        let write_url = |url: String| {
            std::fs::write(
                directory.path().join("settings.json"),
                serde_json::to_vec(&json!({"dandelion.account.serverUrl": url})).unwrap(),
            )
            .unwrap();
            Settings::load(directory.path(), &Default::default()).unwrap()
        };
        let settings = write_url("http://127.0.0.1:1".into());
        let c = Client::new(&settings, directory.path()).unwrap();
        let err = c
            .request(reqwest::Method::GET, "api/v1/access", None, false)
            .await
            .unwrap_err();
        assert!(!err.revocation, "connection failure must stay transient");
        // 401：明确撤销，且本地令牌必须已被销毁。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&mut socket);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            write!(socket, "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let settings = write_url(format!("http://{addr}"));
        let c = Client::new(&settings, directory.path()).unwrap();
        c.store.put(&c.token_key, "expired-token").unwrap();
        let err = c
            .request(reqwest::Method::GET, "api/v1/access", None, true)
            .await
            .unwrap_err();
        assert!(err.revocation, "401 must be classified as explicit revocation");
        assert!(c.token().unwrap().is_none(), "revoked token must be removed locally");
        server.join().unwrap();
    }

    #[test]
    fn status_cache_dedups_back_to_back_boot_probes() {
        let host = AccountHost::default();
        let status = Status {
            access: None,
            configured: true,
            server_url: "http://127.0.0.1:9/".into(),
            profile: None,
            message: None,
            version: "0.0.0".into(),
            channel: "stable".into(),
            update_key_configured: false,
            can_install: false,
            grace: false,
        };
        assert!(cached_status(&host, 60_000).is_none());
        *host.status_cache.lock().unwrap() =
            Some((std::time::Instant::now(), (status.clone(), Probe::Definitive)));
        // 启动时 App / TitleBar / 监控循环背靠背查询：第二次必须命中缓存，
        // 既不排队 auth_operation 也不再发 HTTP。
        let (hit, probe) = cached_status(&host, 60_000).expect("fresh entry must be reused");
        assert_eq!(hit.version, status.version);
        assert_eq!(probe, Probe::Definitive);
        assert!(cached_status(&host, 0).is_none(), "expired entry must not be reused");
    }

    #[test]
    fn service_urls_must_be_secure_and_credential_free() {
        for url in [
            "http://example.com",
            "https://user:secret@example.com",
            "file:///tmp/a",
            "https://a.test/?token=abc",
        ] {
            assert!(validated_base(url).is_err())
        }
        assert_eq!(
            validated_base("http://127.0.0.1:8090").unwrap().as_str(),
            "http://127.0.0.1:8090/"
        );
        assert!(validated_base("https://accounts.example.com").is_ok())
    }
    #[tokio::test]
    async fn update_installation_and_prompt_start_are_mutually_exclusive() {
        let host = AccountHost::default();
        let prompt = host.begin_prompt().await.unwrap();
        assert_eq!(host.active_prompts.load(Ordering::SeqCst), 1);
        drop(prompt);
        assert_eq!(host.active_prompts.load(Ordering::SeqCst), 0);
        host.installing.store(true, Ordering::SeqCst);
        assert!(host.begin_prompt().await.is_err());
        drop(Installing(&host.installing));
        assert!(host.begin_prompt().await.is_ok());
    }

    #[tokio::test]
    async fn official_updater_verifies_signed_bytes_and_rejects_modified_payloads() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/update", listener.local_addr().unwrap());
        let metadata = json!({"version":"99.0.0","url":"https://example.invalid/update.tar.gz",
            "signature":include_str!("../tests/fixtures/updater.sig").trim()})
        .to_string();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",metadata.len(),metadata).unwrap();
        });
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context
            .config_mut()
            .plugins
            .0
            .insert("updater".into(), json!({"pubkey":"","endpoints":[]}));
        let app = tauri::test::mock_builder().build(context).unwrap();
        app.handle()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .unwrap();
        let updater = app
            .updater_builder()
            .pubkey(include_str!("../tests/fixtures/updater.pub").trim())
            .target("regression-target")
            .executable_path("fixture-path/Dandelion")
            .endpoints(vec![endpoint.parse().unwrap()])
            .unwrap()
            .build()
            .unwrap();
        let update = updater.check().await.unwrap().unwrap();
        server.join().unwrap();
        let checked = update.verify(include_bytes!("../tests/fixtures/updater-payload.txt"));
        assert!(checked.is_ok(), "{checked:?}");
        assert!(update.verify(b"modified update bytes").is_err());
    }

    #[tokio::test]
    #[ignore = "requires an isolated local operations test service"]
    async fn live_registration_login_private_credentials_logout_and_update_check() {
        let base = std::env::var("DLN_TEST_OPERATIONS_URL").expect("isolated service URL");
        assert!(base.starts_with("http://127.0.0.1:"));
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("settings.json"),
            serde_json::to_vec(&json!({
                "dandelion.account.serverUrl":base
            }))
            .unwrap(),
        )
        .unwrap();
        let settings = Settings::load(directory.path(), &Default::default()).unwrap();
        let c = Client::new(&settings, directory.path()).unwrap();
        let email = format!("account-{}@example.test", uuid::Uuid::new_v4());
        let password = "Dandelion-regression-password";
        c.request(
            reqwest::Method::POST,
            "api/v1/auth/register",
            Some(json!({"email":email,"password":password,"name":"验收账号"})),
            false,
        )
        .await
        .unwrap();
        let login=c.request(reqwest::Method::POST,"api/v1/auth/login",Some(json!({"email":email,"password":password,"installID":c.install_id().unwrap(),"deviceName":"隔离验收设备"})),false).await.unwrap();
        c.store
            .put(&c.token_key, login["token"].as_str().unwrap())
            .unwrap();
        let profile = c
            .request(reqwest::Method::GET, "api/v1/account", None, true)
            .await
            .unwrap();
        assert_eq!(profile["user"]["email"], email);
        assert_eq!(profile["devices"].as_array().unwrap().len(), 1);
        assert_eq!(
            c.request(reqwest::Method::GET, "api/v1/access", None, true)
                .await
                .unwrap()["allowed"],
            false
        );
        let admin_email = std::env::var("DLN_TEST_ADMIN_EMAIL").expect("test admin email");
        let admin_password = std::env::var("DLN_TEST_ADMIN_PASSWORD").expect("test admin password");
        let admin: Value = reqwest::Client::new()
            .post(c.url("api/v1/auth/login").unwrap())
            .json(&json!({"email":admin_email,"password":admin_password}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let user_id = profile["user"]["id"].as_str().unwrap();
        for granted in [true, false] {
            reqwest::Client::new()
                .patch(c.url(&format!("api/v1/admin/users/{user_id}")).unwrap())
                .bearer_auth(admin["token"].as_str().unwrap())
                .json(&json!({"status":"active","plan":"free","accessGranted":granted}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            let access = c
                .request(reqwest::Method::GET, "api/v1/access", None, true)
                .await
                .unwrap();
            assert_eq!(access["allowed"], granted);
        }

        assert!(c
            .request(
                reqwest::Method::GET,
                "api/v1/updates/stable/darwin-aarch64/0.1.0",
                None,
                false
            )
            .await
            .unwrap()
            .is_null());
        c.request(reqwest::Method::POST, "api/v1/auth/logout", None, true)
            .await
            .unwrap();
        assert!(c
            .request(reqwest::Method::GET, "api/v1/account", None, true)
            .await
            .is_err());
        assert!(
            c.token().unwrap().is_none(),
            "revoked token must be removed locally"
        );
        println!(
            "Rust account client -> Go backend registration/login/device/logout/update: passed"
        );
    }
}
