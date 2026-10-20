//! A private CC Switch CLI instance for a Dandelion-managed Claude Code.
//! It never changes the user's normal CC Switch or Claude configuration.
use crate::{Error, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

const VERSION: &str = "v5.11.0";
const PROVIDER_ID: &str = "dandelion-managed";
const MAX_ARCHIVE_BYTES: usize = 32 * 1024 * 1024;

// SHA-256 values published alongside the official v5.11.0 release assets.
// https://github.com/SaladDay/cc-switch-cli/releases/expanded_assets/v5.11.0
fn release_asset() -> Result<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok((
            "darwin-arm64.tar.gz",
            "b4dac300c3147222f6a698262dcb820e915c89f946956ccfe6cb514fa3fcc8fb",
        )),
        ("macos", "x86_64") => Ok((
            "darwin-x64.tar.gz",
            "97c0b808b9fa4288080929ac763fa5788888ff40580ddfeaa6331d272dceede2",
        )),
        ("linux", "aarch64") => Ok((
            "linux-arm64-musl.tar.gz",
            "c84c563f4ab7594113221028893e0e834318216beb1d8b0c0671fd38b328492e",
        )),
        ("linux", "x86_64") => Ok((
            "linux-x64-musl.tar.gz",
            "272a4d5414a1bc815752227d305c24d894b6c2f5d59bb5e39853041ff752a7af",
        )),
        ("windows", "x86_64") => Ok((
            "windows-x64.zip",
            "2493764062063f2b0e5a3a1eda009f4e62451a0c113dc3f294beb10267deb6ff",
        )),
        _ => Err(Error::NotConfigured(
            "CC Switch CLI 暂不支持当前系统架构".into(),
        )),
    }
}

pub fn binary(data_dir: &Path) -> PathBuf {
    data_dir
        .join("agent-engines")
        .join("cc_switch_cli")
        .join(VERSION)
        .join(if cfg!(windows) {
            "cc-switch.exe"
        } else {
            "cc-switch"
        })
}

fn config_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("cc-switch-managed")
}
pub fn claude_dir(data_dir: &Path) -> PathBuf {
    config_dir(data_dir).join("claude")
}

fn command(data_dir: &Path) -> Command {
    let mut cmd = Command::new(binary(data_dir));
    apply_env(&mut cmd, data_dir);
    // 静默托管的辅助 CLI（--version 探测、provider 切换）：GUI 宿主下不得
    // 弹出控制台窗口，见 agent_plugins::hide_console_windows。
    crate::agent_plugins::hide_console_windows(&mut cmd);
    cmd
}

fn apply_env(cmd: &mut Command, data_dir: &Path) {
    let root = config_dir(data_dir);
    cmd.env("CC_SWITCH_CONFIG_DIR", &root)
        .env("CLAUDE_CONFIG_DIR", claude_dir(data_dir))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"));
}

pub async fn install(
    data_dir: &Path,
    cancel: CancellationToken,
    mut progress: impl FnMut(&'static str, String),
) -> Result<()> {
    let target = binary(data_dir);
    if target.is_file() {
        if command(data_dir)
            .arg("--version")
            .output()
            .await
            .is_ok_and(|out| out.status.success())
        {
            return Ok(());
        }
        std::fs::remove_file(&target)?;
    }
    let (asset, expected_sha) = release_asset()?;
    let file_name = format!("cc-switch-cli-{VERSION}-{asset}");
    let url = format!(
        "https://github.com/SaladDay/cc-switch-cli/releases/download/{VERSION}/{file_name}"
    );
    progress("downloading", "正在下载 CC Switch CLI".into());
    let response = tokio::select! {
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        result = reqwest::Client::new().get(url).send() => result.map_err(|e| Error::Provider(format!("下载 CC Switch CLI 失败：{e}")))?,
    };
    let response = response
        .error_for_status()
        .map_err(|e| Error::Provider(format!("下载 CC Switch CLI 失败：{e}")))?;
    if response
        .content_length()
        .is_some_and(|size| size as usize > MAX_ARCHIVE_BYTES)
    {
        return Err(Error::Invalid("CC Switch CLI 下载包超过大小限制".into()));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = tokio::select! {
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        result = stream.next() => result,
    } {
        let chunk =
            chunk.map_err(|e| Error::Provider(format!("读取 CC Switch CLI 下载包失败：{e}")))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_ARCHIVE_BYTES {
            return Err(Error::Invalid("CC Switch CLI 下载包超过大小限制".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    let actual_sha = format!("{:x}", Sha256::digest(&bytes));
    if actual_sha != expected_sha {
        return Err(Error::Invalid(
            "CC Switch CLI 下载包 SHA-256 校验失败".into(),
        ));
    }
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    progress("installing", "正在安装已校验的 CC Switch CLI".into());
    let parent = target
        .parent()
        .ok_or_else(|| Error::Invalid("CC Switch CLI 安装路径无效".into()))?;
    std::fs::create_dir_all(parent)?;
    let archive = parent.join(&file_name);
    std::fs::write(&archive, bytes)?;
    let mut untar = Command::new("tar");
    untar.arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(parent)
        .arg(if cfg!(windows) {
            "cc-switch.exe"
        } else {
            "cc-switch"
        })
        .kill_on_drop(true);
    crate::agent_plugins::hide_console_windows(&mut untar);
    let output = untar.output().await;
    let _ = std::fs::remove_file(&archive);
    let output = output.map_err(|e| Error::Provider(format!("解压 CC Switch CLI 失败：{e}")))?;
    if !output.status.success() || !target.is_file() {
        let _ = std::fs::remove_file(&target);
        return Err(Error::Provider("CC Switch CLI 下载包解压失败".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700))?;
    }
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let output = command(data_dir).arg("--version").output().await?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&target);
        return Err(Error::Provider("CC Switch CLI 安装后无法启动".into()));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub upstream: String,
    pub model: String,
    pub key_fingerprint: String,
}

impl Route {
    pub fn new(upstream: String, model: String, key: &str) -> Self {
        Self {
            upstream,
            model,
            key_fingerprint: format!("{:x}", Sha256::digest(key.as_bytes())),
        }
    }
}

// The owner keeps the write end of stdin open. On Unix the supervisor gives
// the proxy /dev/null for stdin, and an independent watcher kills their process
// group on EOF. This also works after SIGKILL/crash, when Rust Drop never runs.
// Arguments are passed as argv ("$@"), never interpolated into shell source.
#[cfg(unix)]
const PROXY_SUPERVISOR: &str = r#"
exec 3<&0
"$@" </dev/null 3<&- &
proxy=$!
owner=$$
( IFS= read -r ignored <&3; /bin/kill -KILL -"$owner" ) &
watcher=$!
exec 3<&-
wait "$proxy"
status=$?
kill "$watcher" 2>/dev/null
wait "$watcher" 2>/dev/null
exit "$status"
"#;

struct ProxyLifetime {
    #[cfg(unix)]
    pipe: StdMutex<Option<tokio::process::ChildStdin>>,
    #[cfg(windows)]
    job: crate::jobs::winjob::Job,
}

impl ProxyLifetime {
    fn close(&self) {
        #[cfg(unix)]
        self.pipe.lock().unwrap_or_else(|e| e.into_inner()).take();
        #[cfg(windows)]
        self.job.terminate();
    }
}

struct ProxyProcess {
    child: Child,
    lifetime: Arc<ProxyLifetime>,
    port: u16,
    preferred_port: u16,
    route: Route,
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        self.lifetime.close();
    }
}

pub struct ManagedProxy {
    child: Mutex<Option<ProxyProcess>>,
    // Independent of the async lock: desktop Exit must also stop a proxy while
    // ensure() is awaiting readiness or configuration.
    lifetime: StdMutex<Option<Arc<ProxyLifetime>>>,
    shutdown: CancellationToken,
}
impl Default for ManagedProxy {
    fn default() -> Self {
        Self {
            child: Mutex::new(None),
            lifetime: StdMutex::new(None),
            shutdown: CancellationToken::new(),
        }
    }
}

impl Drop for ManagedProxy {
    fn drop(&mut self) {
        self.kill_sync();
    }
}

impl ManagedProxy {
    pub async fn ensure(
        &self,
        data_dir: &Path,
        route: &Route,
        key: &str,
        port: u16,
        timeout_ms: u64,
    ) -> Result<String> {
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(
                Duration::from_millis(timeout_ms),
                self.ensure_inner(data_dir, route, key, port),
            ) => result.unwrap_or_else(|_| Err(Error::Provider(
                "Claude Code 代理启动超时，请重试；若持续失败，请检查本地模型和 Agent 配置".into()
            ))),
        }
    }

    async fn ensure_inner(
        &self,
        data_dir: &Path,
        route: &Route,
        key: &str,
        preferred_port: u16,
    ) -> Result<String> {
        if !binary(data_dir).is_file() {
            return Err(Error::NotConfigured(
                "请先在 Agent 页面安装 Claude Code 和 CC Switch CLI".into(),
            ));
        }
        let mut child = self.child.lock().await;
        // Take ownership across the health await: cancellation/timeout drops
        // the guard and closes its owner pipe instead of caching a hung proxy.
        if let Some(mut live) = child.take() {
            if live.preferred_port == preferred_port
                && live.route == *route
                && live.child.try_wait()?.is_none()
                && proxy_ready(live.port).await
                && live.child.try_wait()?.is_none()
            {
                let url = format!("http://127.0.0.1:{}", live.port);
                *child = Some(live);
                return Ok(url);
            }
            live.lifetime.close();
            let _ = live.child.wait().await;
        }
        self.lifetime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let root = config_dir(data_dir);
        let route_file = root.join("route.json");
        let saved = std::fs::read(&route_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Route>(&bytes).ok());
        if saved.as_ref() != Some(route) {
            configure(data_dir, route, key).await?;
            std::fs::write(&route_file, serde_json::to_vec(route)?)?;
        }
        // A stale proxy or unrelated application must never block chat or be
        // killed/adopted based on a port number. Use a fresh local port instead.
        let reservation = reserve_port(preferred_port)?;
        let port = reservation.local_addr()?.port();
        if port != preferred_port {
            tracing::warn!(
                preferred_port,
                port,
                "Claude proxy preferred port busy; using an available loopback port"
            );
        }
        #[cfg(unix)]
        let mut process = {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", PROXY_SUPERVISOR, "dandelion-proxy"])
                .arg(binary(data_dir))
                .process_group(0);
            apply_env(&mut cmd, data_dir);
            cmd
        };
        #[cfg(not(unix))]
        let mut process = command(data_dir);
        process
            .args([
                "proxy",
                "serve",
                "--listen-address",
                "127.0.0.1",
                "--listen-port",
                &port.to_string(),
                "--takeover",
                "claude",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        drop(reservation);
        let mut process = process.spawn()?;
        #[cfg(unix)]
        let lifetime = Arc::new(ProxyLifetime {
            pipe: StdMutex::new(process.stdin.take()),
        });
        #[cfg(windows)]
        let lifetime = Arc::new(ProxyLifetime {
            job: crate::jobs::winjob::Job::from_child(&process)?,
        });
        #[cfg(not(any(unix, windows)))]
        compile_error!("Managed Claude proxy requires Unix or Windows process ownership");
        *self.lifetime.lock().unwrap_or_else(|e| e.into_inner()) = Some(lifetime.clone());
        let mut live = ProxyProcess {
            child: process,
            lifetime,
            port,
            preferred_port,
            route: route.clone(),
        };
        loop {
            if let Some(status) = live.child.try_wait()? {
                return Err(Error::Provider(format!(
                    "Claude Code 代理启动失败（退出码 {:?}），请检查 Agent 配置后重试",
                    status.code()
                )));
            }
            if proxy_ready(port).await && live.child.try_wait()?.is_none() {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
        *child = Some(live);
        Ok(format!("http://127.0.0.1:{port}"))
    }

    pub async fn stop(&self) {
        let mut child = self.child.lock().await;
        if let Some(mut process) = child.take() {
            process.lifetime.close();
            let _ = process.child.wait().await;
        }
        self.lifetime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }

    pub fn kill_sync(&self) {
        self.shutdown.cancel();
        if let Some(lifetime) = self
            .lifetime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            lifetime.close();
        }
    }
}

fn reserve_port(preferred: u16) -> std::io::Result<std::net::TcpListener> {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, preferred)).or_else(|error| {
        if error.kind() == std::io::ErrorKind::AddrInUse {
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        } else {
            Err(error)
        }
    })
}

async fn proxy_ready(port: u16) -> bool {
    // A listening socket alone can belong to a foreign service. Require the
    // proxy's HTTP health endpoint and keep checking our child's exit status.
    let Ok(client) = reqwest::Client::builder().no_proxy().build() else {
        return false;
    };
    let Ok(response) = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
    else {
        return false;
    };
    response.status().is_success()
        && response
            .json::<serde_json::Value>()
            .await
            .is_ok_and(|body| body.get("ok").and_then(|value| value.as_bool()) == Some(true))
}

struct PendingConfig(PathBuf);
impl Drop for PendingConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn configure(data_dir: &Path, route: &Route, key: &str) -> Result<()> {
    let root = config_dir(data_dir);
    std::fs::create_dir_all(claude_dir(data_dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(claude_dir(data_dir), std::fs::Permissions::from_mode(0o700))?;
    }
    let config = serde_json::json!({"env": {
        "ANTHROPIC_BASE_URL": route.upstream,
        "ANTHROPIC_AUTH_TOKEN": key,
        "ANTHROPIC_MODEL": route.model,
        "ANTHROPIC_DEFAULT_HAIKU_MODEL": route.model,
        "ANTHROPIC_DEFAULT_SONNET_MODEL": route.model,
        "ANTHROPIC_DEFAULT_OPUS_MODEL": route.model
    }});
    let config_file = root.join("provider.pending.json");
    let _pending = PendingConfig(config_file.clone());
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&config_file)?
            .write_all(&serde_json::to_vec(&config)?)?;
    }
    #[cfg(not(unix))]
    std::fs::write(&config_file, serde_json::to_vec(&config)?)?;
    // Delete only the provider owned by Dandelion; never touch user providers.
    let _ = command(data_dir)
        .args(["provider", "delete", PROVIDER_ID, "--app", "claude"])
        .kill_on_drop(true)
        .output()
        .await;
    let added = command(data_dir)
        .args([
            "provider",
            "add",
            "--app",
            "claude",
            "--id",
            PROVIDER_ID,
            "--name",
            "Dandelion",
            "--config-file",
        ])
        .arg(&config_file)
        .args(["--api-format", "openai_chat"])
        .kill_on_drop(true)
        .output()
        .await;
    let _ = std::fs::remove_file(&config_file);
    let added = added?;
    if !added.status.success() {
        return Err(Error::Provider(format!(
            "配置 CC Switch 提供商失败：{}",
            String::from_utf8_lossy(&added.stderr)
        )));
    }
    let switched = command(data_dir)
        .args(["provider", "switch", PROVIDER_ID, "--app", "claude"])
        .kill_on_drop(true)
        .output()
        .await?;
    if !switched.status.success() {
        return Err(Error::Provider(format!(
            "启用 CC Switch 提供商失败：{}",
            String::from_utf8_lossy(&switched.stderr)
        )));
    }
    Ok(())
}

pub fn remove_managed(data_dir: &Path) -> Result<()> {
    let target = binary(data_dir);
    let Some(parent) = target.parent() else {
        return Ok(());
    };
    if parent.is_dir() {
        if parent.is_symlink() {
            return Err(Error::Invalid("CC Switch 安装目录不能是符号链接".into()));
        }
        std::fs::remove_dir_all(parent)?;
    }
    // Keep provider settings and logs so a later reinstall can recover them.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_asset_is_pinned_for_supported_hosts() {
        let (name, sha) = release_asset().unwrap();
        assert!(name.ends_with(".tar.gz") || name.ends_with(".zip"));
        assert_eq!(sha.len(), 64);
    }
    #[test]
    fn route_does_not_persist_the_key() {
        let route = Route::new("http://127.0.0.1:8001/v1".into(), "model".into(), "secret");
        assert!(!serde_json::to_string(&route).unwrap().contains("secret"));
    }
    // The test executable doubles as a foreground proxy. The script only maps
    // CC Switch argv to this fixture; no Python/server package is required.
    #[test]
    fn proxy_fixture() {
        use std::io::{Read, Write};
        let Ok(port) = std::env::var("DANDELION_TEST_PROXY_PORT") else {
            return;
        };
        let listener = std::net::TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
        if let Ok(root) = std::env::var("CC_SWITCH_CONFIG_DIR") {
            std::fs::write(Path::new(&root).join("test-ready-port"), &port).unwrap();
        }
        for connection in listener.incoming() {
            let mut connection = connection.unwrap();
            let mut request = [0; 4096];
            if connection.read(&mut request).unwrap_or(0) == 0 {
                continue;
            }
            let body = r#"{"ok":true,"content":[{"type":"text","text":"OK"}]}"#;
            let status = if std::env::var_os("DANDELION_TEST_PROXY_UNHEALTHY").is_some() {
                "503 Unavailable"
            } else {
                "200 OK"
            };
            write!(
                connection,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
    }

    #[cfg(unix)]
    fn fixture(dir: &Path, serve: &str) -> Route {
        use std::os::unix::fs::PermissionsExt;
        let executable = binary(dir);
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        let test_exe = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .replace('\'', "'\\''");
        let source = format!(
            r#"#!/bin/sh
if [ "$1" != proxy ]; then exit 0; fi
while [ "$#" -gt 0 ]; do
  if [ "$1" = --listen-port ]; then shift; port=$1; fi
  shift
done
export DANDELION_TEST_PROXY_PORT="$port"
{serve}
exec '{test_exe}' --exact cc_switch::tests::proxy_fixture --nocapture
"#
        );
        std::fs::write(&executable, source).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        Route::new("http://127.0.0.1:8001/v1".into(), "model".into(), "local")
    }

    #[cfg(unix)]
    async fn assert_released(port: u16) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned proxy/descendants retained the listener");
    }

    #[cfg(unix)]
    fn port(url: &str) -> u16 {
        url.rsplit(':').next().unwrap().parse().unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn occupied_port_falls_back_and_reuses_only_owned_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let route = fixture(dir.path(), "");
        let foreign = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let preferred = foreign.local_addr().unwrap().port();
        let proxy = ManagedProxy::default();
        let url = proxy
            .ensure(dir.path(), &route, "local", preferred, 5000)
            .await
            .unwrap();
        assert_ne!(port(&url), preferred);
        let pid = proxy.child.lock().await.as_ref().unwrap().child.id();
        assert_eq!(
            proxy
                .ensure(dir.path(), &route, "local", preferred, 5000)
                .await
                .unwrap(),
            url
        );
        assert_eq!(proxy.child.lock().await.as_ref().unwrap().child.id(), pid);
        let reply: serde_json::Value = reqwest::Client::new()
            .post(format!("{url}/v1/messages"))
            .body("test")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(reply["content"][0]["text"], "OK");
        proxy.stop().await;
        assert_released(port(&url)).await;
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", preferred)).is_err(),
            "foreign service must remain untouched"
        );
        let restarted = proxy
            .ensure(dir.path(), &route, "local", preferred, 5000)
            .await
            .unwrap();
        proxy.kill_sync();
        assert_released(port(&restarted)).await;
        assert!(matches!(
            proxy
                .ensure(dir.path(), &route, "local", preferred, 5000)
                .await,
            Err(Error::Cancelled)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn crashed_proxy_restarts_and_route_change_is_applied() {
        let dir = tempfile::tempdir().unwrap();
        let mut route = fixture(dir.path(), "");
        let proxy = ManagedProxy::default();
        let first = proxy
            .ensure(dir.path(), &route, "local", 0, 5000)
            .await
            .unwrap();
        {
            let mut lock = proxy.child.lock().await;
            let live = lock.as_mut().unwrap();
            live.lifetime.close();
            live.child.wait().await.unwrap();
        }
        assert_released(port(&first)).await;
        let second = proxy
            .ensure(dir.path(), &route, "local", 0, 5000)
            .await
            .unwrap();
        route.model = "changed-model".into();
        let third = proxy
            .ensure(dir.path(), &route, "local", 0, 5000)
            .await
            .unwrap();
        let persisted: Route = serde_json::from_slice(
            &std::fs::read(config_dir(dir.path()).join("route.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted, route);
        proxy.stop().await;
        assert_released(port(&second)).await;
        assert_released(port(&third)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_owner_releases_proxy_listener() {
        let dir = tempfile::tempdir().unwrap();
        let route = fixture(dir.path(), "");
        let proxy = ManagedProxy::default();
        let url = proxy
            .ensure(dir.path(), &route, "local", 0, 5000)
            .await
            .unwrap();
        drop(proxy);
        assert_released(port(&url)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_failure_and_timeout_can_retry() {
        let dir = tempfile::tempdir().unwrap();
        let route = fixture(dir.path(), "exit 23");
        let proxy = ManagedProxy::default();
        let error = proxy
            .ensure(dir.path(), &route, "local", 0, 5000)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("23"), "{error}");
        fixture(dir.path(), "exec sleep 60");
        let error = proxy
            .ensure(dir.path(), &route, "local", 0, 200)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("超时"), "{error}");
        fixture(dir.path(), "");
        let url = proxy
            .ensure(dir.path(), &route, "local", 0, 5000)
            .await
            .unwrap();
        proxy.stop().await;
        assert_released(port(&url)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn proxy_owner_fixture() {
        let Ok(root) = std::env::var("DANDELION_TEST_PROXY_OWNER") else {
            return;
        };
        let proxy = ManagedProxy::default();
        let route = Route::new("http://127.0.0.1:8001/v1".into(), "model".into(), "local");
        let url = proxy
            .ensure(Path::new(&root), &route, "local", 0, 5000)
            .await
            .unwrap();
        std::fs::write(Path::new(&root).join("ready-url"), url).unwrap();
        std::future::pending::<()>().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sigkill_owner_releases_proxy_and_restart_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let route = fixture(dir.path(), "");
        let mut owner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cc_switch::tests::proxy_owner_fixture",
                "--nocapture",
            ])
            .env("DANDELION_TEST_PROXY_OWNER", dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let url = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(url) = std::fs::read_to_string(dir.path().join("ready-url")) {
                    break url;
                }
                if let Some(status) = owner.try_wait().unwrap() {
                    panic!("owner failed: {status}");
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owner did not start");
        owner.kill().await.unwrap(); // SIGKILL, no Rust Drop/exit hook
        assert_released(port(&url)).await;
        let proxy = ManagedProxy::default();
        let restarted = proxy
            .ensure(dir.path(), &route, "local", port(&url), 5000)
            .await
            .unwrap();
        assert_eq!(restarted, url);
        proxy.stop().await;
        assert_released(port(&url)).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires installed Claude Code/CC Switch and a running local model; uses isolated data"]
    async fn live_claude_runtime_roundtrip_after_restart_and_port_conflict() {
        use crate::agent_plugins::{self, Engine};
        use crate::{CliHost, PromptRequest, Runtime, SessionPrefs, Settings};
        let source = PathBuf::from(
            std::env::var("DANDELION_TEST_INSTALLED_DATA").expect("installed data directory"),
        );
        let dir = tempfile::tempdir().unwrap();
        let model_owner = if std::env::var_os("DANDELION_TEST_START_MODEL").is_some() {
            let model_data = dir.path().join("model-data");
            std::fs::create_dir_all(&model_data).unwrap();
            std::fs::copy(
                source.join("local-models.json"),
                model_data.join("local-models.json"),
            )
            .unwrap();
            std::os::unix::fs::symlink(source.join("bin"), model_data.join("bin")).unwrap();
            let mut settings = Settings::defaults_configured();
            settings.default_backend = "sidecar".into();
            let rt = Runtime::new_with_provider(
                Arc::new(CliHost::with_data_dir(model_data)),
                settings,
                Arc::new(crate::provider::ScriptedProvider::new(vec![])),
            )
            .unwrap();
            let url = rt
                .sidecar_start()
                .await
                .expect("isolated local model failed to start");
            println!("Isolated model ready: {url}");
            Some((rt, url))
        } else {
            None
        };
        let upstream = model_owner
            .as_ref()
            .map(|(_, url)| url.clone())
            .unwrap_or_else(|| std::env::var("DANDELION_TEST_MODEL_URL").expect("local model URL"));
        let model = std::env::var("DANDELION_TEST_MODEL_ID").expect("local model ID");
        assert!(
            upstream.starts_with("http://127.0.0.1:"),
            "live test must use a local model"
        );
        let health = reqwest::Client::new()
            .get(format!("{}/models", upstream.trim_end_matches('/')))
            .send()
            .await
            .expect("local model must be listening before testing Claude");
        assert!(health.status().is_success());
        let data = dir.path().join("data");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let target = binary(&data);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(binary(&source), &target).unwrap();
        let claude = agent_plugins::executable(&source, Engine::ClaudeCode, &[]).unwrap();
        std::fs::write(
            data.join("agent-plugins.json"),
            serde_json::to_vec(&serde_json::json!({
                "plugins":{"claude_code":{"enabled":true,"managed":true,"executable":claude}}
            }))
            .unwrap(),
        )
        .unwrap();
        let foreign = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut settings = Settings::defaults_configured();
        settings.base_url = Some(upstream);
        settings.default_model = Some(model);
        settings.workspace = Some(workspace);
        settings.cc_switch_proxy_port = foreign.local_addr().unwrap().port();
        settings.permission_mode = crate::PermissionMode::Deny;
        settings.wiki_proposal_enabled = false;
        let make_runtime = || {
            Runtime::new_with_provider(
                Arc::new(CliHost::with_data_dir(data.clone())),
                settings.clone(),
                Arc::new(crate::provider::ScriptedProvider::new(vec![])),
            )
            .unwrap()
        };
        let mut sid = None;
        for iteration in 0..2 {
            let rt = make_runtime();
            let session = sid
                .clone()
                .unwrap_or_else(|| rt.create_session(None).unwrap());
            rt.session_set_prefs(
                &session,
                SessionPrefs {
                    agent_engine: Some("claude_code".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            let request =
                PromptRequest::new(session.clone(), "连接测试，请不要调用任何工具，只回复 OK。");
            tokio::time::timeout(Duration::from_secs(120), rt.prompt(request))
                .await
                .expect("actual Claude runtime timed out")
                .unwrap();
            let exported = rt.session_export(&session).unwrap();
            let answers = exported["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|message| message["role"] == "assistant")
                .filter_map(|message| message["content"].as_str())
                .collect::<Vec<_>>();
            assert!(
                answers.len() > iteration && answers.last().unwrap().contains("OK"),
                "{answers:?}"
            );
            println!(
                "Actual Runtime -> Claude Code -> managed proxy -> local model: OK (round {})",
                iteration + 1
            );
            rt.sidecar_kill_on_exit();
            rt.agent_plugin_stop("claude_code").await.unwrap();
            drop(rt);
            sid = Some(session);
        }
        assert!(std::net::TcpListener::bind(foreign.local_addr().unwrap()).is_err());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_during_startup_cleans_up_and_does_not_wait_for_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let route = fixture(dir.path(), "export DANDELION_TEST_PROXY_UNHEALTHY=1");
        let proxy = Arc::new(ManagedProxy::default());
        let task_proxy = proxy.clone();
        let path = dir.path().to_path_buf();
        let pending =
            tokio::spawn(async move { task_proxy.ensure(&path, &route, "local", 0, 30000).await });
        let port_file = config_dir(dir.path()).join("test-ready-port");
        let listening = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&port_file) {
                    break text.parse::<u16>().unwrap();
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        proxy.kill_sync();
        let outcome = tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(outcome, Err(Error::Cancelled)));
        assert_released(listening).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn configuration_timeout_removes_pending_credentials_and_can_retry() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let route = fixture(dir.path(), "");
        let executable = binary(dir.path());
        std::fs::write(&executable, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let proxy = ManagedProxy::default();
        assert!(proxy
            .ensure(dir.path(), &route, "local", 0, 200)
            .await
            .unwrap_err()
            .to_string()
            .contains("超时"));
        assert!(!config_dir(dir.path())
            .join("provider.pending.json")
            .exists());
        assert!(
            !config_dir(dir.path()).join("route.json").exists(),
            "failed configuration must not be marked usable"
        );
        fixture(dir.path(), "");
        let url = proxy
            .ensure(dir.path(), &route, "local", 0, 5000)
            .await
            .unwrap();
        proxy.stop().await;
        assert_released(port(&url)).await;
    }
}
