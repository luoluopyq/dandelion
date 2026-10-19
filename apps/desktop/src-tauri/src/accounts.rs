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
    *host.lease.lock().unwrap() = access
        .filter(|a| a.allowed && a.valid_for_ms > 0 && !host.installing.load(Ordering::SeqCst))
        .map(|a| requested_at + Duration::from_millis(a.valid_for_ms.min(maximum_ms)));
}

pub fn lease_valid(lease: &std::sync::Mutex<Option<std::time::Instant>>) -> bool {
    lease
        .lock()
        .map(|until| until.is_some_and(|until| until > std::time::Instant::now()))
        .unwrap_or(false)
}

pub async fn require_access(app: &AppHandle) -> Result<(), String> {
    let host = app.state::<AccountHost>();
    let _lock = host.auth_operation.lock().await;
    let state = app.state::<AppState>();
    if host.installing.load(Ordering::SeqCst) {
        return Err("正在安装更新，请稍候".into());
    }
    let settings = state.runtime.read().await.settings();
    let result = async {
        let c = Client::new(&settings, &state.host_data_dir())?;
        let requested_at = std::time::Instant::now();
        let value = c
            .request(reqwest::Method::GET, "api/v1/access", None, true)
            .await?;
        let access: Access = serde_json::from_value(value).map_err(|_| "授权响应格式不正确")?;
        update_lease(
            &host,
            Some(&access),
            settings.account_recheck_ms,
            requested_at,
        );
        if access.allowed && lease_valid(&host.lease) {
            Ok(())
        } else {
            Err(if access.allowed {
                "授权验证超时，请重试".into()
            } else {
                access.message
            })
        }
    }
    .await;
    if result.is_err() {
        *host.lease.lock().unwrap() = None;
        state.runtime.read().await.cancel_host_work().await;
        let _ = app.emit("account-access-denied", result.as_ref().err());
    }
    result
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
                let mut period = state.runtime.read().await.settings().account_recheck_ms;
                let status = tokio::time::timeout(
                    Duration::from_millis(period / 2),
                    load_status(&state, &host),
                )
                .await;
                match status {
                    Ok(Ok(status)) => {
                        if let Some(access) = status.access.as_ref().filter(|a| a.allowed) {
                            period = period.min(access.valid_for_ms.max(1));
                        }
                        if !status.access.as_ref().is_some_and(|a| a.allowed) {
                            *host.lease.lock().unwrap() = None;
                            state.runtime.read().await.cancel_host_work().await;
                        }
                        let _ = app.emit("account-status", status);
                    }
                    _ => {
                        *host.lease.lock().unwrap() = None;
                        state.runtime.read().await.cancel_host_work().await;
                        let _ = app.emit("account-access-denied", "无法验证授权，请检查网络后重试");
                    }
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
    ) -> Result<Value, String> {
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
            .map_err(|_| "无法连接账号服务，请检查服务地址或网络")?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED && authenticated {
            self.store.delete(&self.token_key).map_err(map_err)?;
            return Err("登录已失效，请重新登录".into());
        }
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let value: Value = response
            .json()
            .await
            .map_err(|_| "账号服务返回了无法识别的响应")?;
        if !status.is_success() {
            return Err(value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("请求失败，请稍后重试")
                .into());
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
) -> Result<Status, String> {
    let _lock = host.auth_operation.lock().await;
    load_status(&state, &host).await
}

async fn load_status(state: &AppState, host: &AccountHost) -> Result<Status, String> {
    let settings = state.runtime.read().await.settings();
    let configured = Client::new(&settings, &state.host_data_dir());
    let Ok(c) = configured else {
        *host.lease.lock().unwrap() = None;
        return Ok(Status {
            access: None,
            configured: false,
            server_url: String::new(),
            profile: None,
            message: configured.err(),
            version: env!("CARGO_PKG_VERSION").into(),
            channel: "stable".into(),
            update_key_configured: false,
            can_install: false,
        });
    };
    let mut message = None;
    let profile = if c.token()?.is_some() {
        match c
            .request(reqwest::Method::GET, "api/v1/account", None, true)
            .await
        {
            Ok(v) => Some(v),
            Err(e) => {
                message = Some(e);
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
                    message = Some("授权响应格式不正确".into());
                    None
                }
            },
            Err(e) => {
                message = Some(e);
                None
            }
        }
    } else {
        None
    };
    update_lease(
        host,
        access.as_ref(),
        settings.account_recheck_ms,
        requested_at,
    );
    if !host.installing.load(Ordering::SeqCst) && !lease_valid(&host.lease) {
        if let Some(a) = access.as_mut().filter(|a| a.allowed) {
            a.allowed = false;
            a.code = "verification_timeout".into();
            a.message = "授权验证超时，请重试".into();
        }
    }
    Ok(Status {
        access,
        configured: true,
        server_url: c.base.to_string(),
        profile,
        message,
        version: env!("CARGO_PKG_VERSION").into(),
        channel: c.channel,
        update_key_configured: !c.pubkey.is_empty(),
        can_install: !cfg!(debug_assertions) && !c.pubkey.is_empty(),
    })
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
}
#[tauri::command]
pub async fn account_orders(state: State<'_, AppState>) -> Result<Value, String> {
    client(&state)
        .await?
        .request(reqwest::Method::GET, "api/v1/orders", None, true)
        .await
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
