//! Local GGUF/MLX catalog + loopback inference sidecar + GPU slot queue.
//!
//! Sidecar is an OpenAI-compatible HTTP child, not a second agent loop. Vue never
//! talks to it; `Runtime::prompt` still goes through `OpenAiSse`.

use crate::ids::SessionId;
use crate::{Error, Result};
use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{header, Request, Response, StatusCode};
use axum::Router;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_RANGE, RANGE};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const CATALOG_FILE: &str = "local-models.json";
const LOG_LINES: usize = 400;
const GGUF_MAGIC: &[u8] = b"GGUF";
// macOS does not kill a child when its parent crashes. Keep its stdin pipe
// open while Dandelion owns it; EOF makes the MLX process release its model.
#[cfg(target_os = "macos")]
const MLX_PARENT_WATCHDOG: &str = r#"
import os
import threading

def watch_parent():
    try:
        while os.read(0, 1):
            pass
    finally:
        os._exit(0)

threading.Thread(target=watch_parent, daemon=True).start()
from mlx_lm.server import main
main()
"#;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareProbe {
    pub gpu_name: Option<String>,
    pub vram_mb: Option<u64>,
    pub ram_mb: Option<u64>,
    pub disk_free_mb: Option<u64>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModel {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub source: String,
    pub quant: String,
    pub bytes: u64,
    pub sha256: String,
    pub sha256_ok: bool,
    pub active: bool,
    #[serde(default = "default_model_format")]
    pub format: String,
}

fn default_model_format() -> String {
    "gguf".into()
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDownloadStatus {
    pub running: bool,
    pub repo_id: String,
    pub file_path: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSnapshot {
    pub running: usize,
    pub waiting: usize,
    pub max: u32,
    pub waiting_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SidecarStatus {
    pub kind: SidecarStatusKind,
    pub bind: Option<String>,
    pub pid: Option<u32>,
    pub active_model: Option<String>,
    pub last_error: Option<String>,
    pub error_kind: Option<String>,
    pub queue: QueueSnapshot,
    pub binary: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SidecarStatusKind {
    Stopped,
    Starting,
    Ready,
    Failed,
}

#[derive(Clone, Debug)]
pub struct VramCheck {
    pub required_mb: u64,
    pub available_mb: Option<u64>,
    pub recommend_quant: String,
    pub ok: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct CatalogFile {
    models: Vec<LocalModel>,
}

#[derive(Clone)]
pub struct LocalQueue {
    inner: Arc<Mutex<QueueInner>>,
    notify: Arc<Notify>,
}

struct QueueInner {
    running: HashSet<SessionId>,
    waiting: VecDeque<SessionId>,
}

pub struct SlotLease {
    queue: LocalQueue,
    id: SessionId,
}

impl LocalQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(QueueInner {
                running: HashSet::new(),
                waiting: VecDeque::new(),
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn snapshot(&self, max: u32) -> QueueSnapshot {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        QueueSnapshot {
            running: g.running.len(),
            waiting: g.waiting.len(),
            max,
            waiting_ids: g.waiting.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub async fn acquire(
        &self,
        id: SessionId,
        max: usize,
        cancel: CancellationToken,
    ) -> Result<SlotLease> {
        let max = max.max(1);
        {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if g.running.contains(&id) {
                return Ok(SlotLease {
                    queue: self.clone(),
                    id,
                });
            }
            if !g.waiting.iter().any(|s| s == &id) {
                g.waiting.push_back(id.clone());
            }
        }
        loop {
            if cancel.is_cancelled() {
                self.remove_waiting(&id);
                return Err(Error::Cancelled);
            }
            {
                let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                let is_head = g.waiting.front() == Some(&id);
                if is_head && g.running.len() < max {
                    g.waiting.pop_front();
                    g.running.insert(id.clone());
                    return Ok(SlotLease {
                        queue: self.clone(),
                        id,
                    });
                }
            }
            tokio::select! {
                _ = cancel.cancelled() => {
                    self.remove_waiting(&id);
                    return Err(Error::Cancelled);
                }
                _ = self.notify.notified() => {}
            }
        }
    }

    fn remove_waiting(&self, id: &SessionId) {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.waiting.retain(|s| s != id);
        self.notify.notify_waiters();
    }

    pub fn waiting_contains(&self, id: &SessionId) -> bool {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.waiting.iter().any(|s| s == id)
    }
}

impl Drop for SlotLease {
    fn drop(&mut self) {
        let mut g = self.queue.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.running.remove(&self.id);
        g.waiting.retain(|s| s != &self.id);
        self.queue.notify.notify_waiters();
    }
}

pub struct SidecarManager {
    data_dir: PathBuf,
    inner: Arc<Mutex<SidecarInner>>,
}

struct SidecarInner {
    live: Option<LiveSidecar>,
    logs: VecDeque<String>,
    last_error: Option<String>,
    error_kind: Option<String>,
    starting: bool,
    download: ModelDownloadStatus,
}

struct DownloadLease(Arc<Mutex<SidecarInner>>);

impl Drop for DownloadLease {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .download
            .running = false;
    }
}

struct LiveSidecar {
    pid: u32,
    port: u16,
    public_port: Option<u16>,
    proxy: Option<tokio::task::JoinHandle<()>>,
    kill: Option<KillTree>,
    _parent_pipe: Option<ChildStdin>,
}

impl LiveSidecar {
    fn exposed_port(&self) -> u16 {
        self.public_port.unwrap_or(self.port)
    }
}

impl SidecarManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            inner: Arc::new(Mutex::new(SidecarInner {
                live: None,
                logs: VecDeque::new(),
                last_error: None,
                error_kind: None,
                starting: false,
                download: ModelDownloadStatus::default(),
            })),
        }
    }

    pub fn model_dir(&self, configured: Option<&Path>) -> PathBuf {
        configured
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.data_dir.join("models"))
    }

    fn catalog_path(&self) -> PathBuf {
        self.data_dir.join(CATALOG_FILE)
    }

    pub fn load_catalog(&self) -> Vec<LocalModel> {
        read_catalog(&self.catalog_path())
    }

    pub fn save_catalog(&self, models: &[LocalModel]) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        let json = serde_json::to_string_pretty(&CatalogFile {
            models: models.to_vec(),
        })?;
        std::fs::write(self.catalog_path(), json)?;
        Ok(())
    }

    pub fn active_model(&self) -> Option<LocalModel> {
        self.load_catalog().into_iter().find(|m| m.active)
    }

    pub fn import_gguf(&self, src: &Path, model_dir: &Path) -> Result<LocalModel> {
        self.import_gguf_with_source(src, model_dir, "import", None)
    }

    fn import_gguf_with_source(
        &self,
        src: &Path,
        model_dir: &Path,
        source: &str,
        name_override: Option<&str>,
    ) -> Result<LocalModel> {
        if !src.is_file() {
            return Err(Error::Sidecar(format!(
                "sidecar start: not a file: {}",
                src.display()
            )));
        }
        let mut magic = [0u8; 4];
        File::open(src)?.read_exact(&mut magic)?;
        if magic != *GGUF_MAGIC {
            return Err(Error::Sidecar(
                "sidecar start: file is not GGUF (missing magic)".into(),
            ));
        }
        let meta = std::fs::metadata(src)?;
        let need = meta.len().saturating_add(64 * 1024 * 1024);
        if let Some(free) = disk_free_bytes(model_dir).or_else(|| disk_free_bytes(&self.data_dir)) {
            if free < need {
                return Err(Error::Sidecar(format!(
                    "sidecar start: disk full (need {} MiB)",
                    need / (1024 * 1024)
                )));
            }
        }
        std::fs::create_dir_all(model_dir)?;
        let name = name_override
            .or_else(|| src.file_name().and_then(|s| s.to_str()))
            .ok_or_else(|| Error::Invalid("gguf file name".into()))?;
        let dest = model_dir.join(name);
        std::fs::copy(src, &dest)?;
        let sha = sha256_file(&dest)?;
        let bytes = std::fs::metadata(&dest)?.len();
        let quant = parse_quant(name);
        let mut models = self.load_catalog();
        models.retain(|m| m.path != dest);
        let become_active = models.iter().all(|m| !m.active);
        for m in &mut models {
            if become_active {
                m.active = false;
            }
        }
        let row = LocalModel {
            id: unique_id(&models, stem(name)),
            name: name.to_string(),
            path: dest,
            source: source.into(),
            quant,
            bytes,
            sha256: sha,
            sha256_ok: true,
            active: become_active,
            format: default_model_format(),
        };
        models.push(row.clone());
        self.save_catalog(&models)?;
        Ok(row)
    }

    /// Register an existing MLX directory in place, without copying its weights.
    pub fn import_mlx(&self, src: &Path) -> Result<LocalModel> {
        if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            return Err(Error::Sidecar("MLX 模型仅支持 Apple Silicon Mac".into()));
        }
        let path = src.canonicalize()?;
        let (sha256, bytes) = mlx_manifest(&path)?;
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| Error::Invalid("MLX 模型目录名无效".into()))?;
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.join("config.json"))?)?;
        let quant = config
            .get("quantization")
            .or_else(|| config.get("quantization_config"))
            .and_then(|value| value.get("bits"))
            .and_then(|value| value.as_u64())
            .map(|bits| format!("{bits}-bit"))
            .unwrap_or_else(|| "MLX".into());
        let mut models = self.load_catalog();
        if let Some(existing) = models.iter_mut().find(|model| model.path == path) {
            existing.sha256 = sha256;
            existing.sha256_ok = true;
            existing.bytes = bytes;
            let out = existing.clone();
            self.save_catalog(&models)?;
            return Ok(out);
        }
        let active = models.iter().all(|model| !model.active);
        let row = LocalModel {
            id: unique_id(&models, name.to_string()),
            name: name.into(),
            path,
            source: "linked-local".into(),
            quant,
            bytes,
            sha256,
            sha256_ok: true,
            active,
            format: "mlx".into(),
        };
        models.push(row.clone());
        self.save_catalog(&models)?;
        Ok(row)
    }

    pub fn download_status(&self) -> ModelDownloadStatus {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .download
            .clone()
    }

    /// Download one GGUF from ModelScope's official single-file endpoint.
    /// A partial file is retained on network failure so the next attempt can resume.
    pub async fn download_modelscope(
        &self,
        repo_id: &str,
        file_path: &str,
        model_dir: &Path,
    ) -> Result<LocalModel> {
        let (url, name) = modelscope_file_url(repo_id, file_path)?;
        let source = format!("modelscope:{repo_id}");
        {
            let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if guard.download.running {
                return Err(Error::Sidecar("魔搭下载正在进行中".into()));
            }
            guard.download = ModelDownloadStatus {
                running: true,
                repo_id: repo_id.into(),
                file_path: file_path.into(),
                downloaded_bytes: 0,
                total_bytes: None,
            };
        }
        let _lease = DownloadLease(self.inner.clone());
        self.download_modelscope_inner(&url, &name, &source, model_dir)
            .await
    }

    async fn download_modelscope_inner(
        &self,
        url: &reqwest::Url,
        name: &str,
        source: &str,
        model_dir: &Path,
    ) -> Result<LocalModel> {
        tokio::fs::create_dir_all(model_dir).await?;
        let dest = model_dir.join(name);
        if dest.exists() {
            return Err(Error::Sidecar(format!("模型文件已存在：{name}")));
        }
        let hash = Sha256::digest(url.as_str().as_bytes());
        let partial = model_dir.join(format!(
            ".{name}.{:02x}{:02x}{:02x}{:02x}.modelscope.part",
            hash[0], hash[1], hash[2], hash[3]
        ));
        let offset = tokio::fs::metadata(&partial)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10 || attempt.url().scheme() != "https" {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|e| Error::Sidecar(format!("魔搭下载客户端：{e}")))?;
        let mut request = client.get(url.clone());
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let mut response = request
            .send()
            .await
            .map_err(|e| Error::Sidecar(format!("魔搭下载请求：{e}")))?;
        if offset > 0 && response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            tokio::fs::remove_file(&partial).await?;
            response = client
                .get(url.clone())
                .send()
                .await
                .map_err(|e| Error::Sidecar(format!("魔搭下载请求：{e}")))?;
        }
        if !response.status().is_success() {
            return Err(Error::Sidecar(format!(
                "魔搭下载失败：HTTP {}",
                response.status()
            )));
        }
        // Some mirrors ignore Range; in that case restart rather than append duplicate bytes.
        let resumed = offset > 0
            && response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with(&format!("bytes {offset}-")));
        if response.status() == reqwest::StatusCode::PARTIAL_CONTENT && !resumed {
            return Err(Error::Sidecar("魔搭返回了无效的续传范围，请重试".into()));
        }
        let start = if resumed { offset } else { 0 };
        let total = response.content_length().map(|n| n.saturating_add(start));
        if let (Some(free), Some(remaining)) =
            (disk_free_bytes(model_dir), response.content_length())
        {
            if free < remaining.saturating_add(64 * 1024 * 1024) {
                return Err(Error::Sidecar("魔搭下载失败：磁盘空间不足".into()));
            }
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&partial)
            .await?;
        {
            let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            guard.download.downloaded_bytes = start;
            guard.download.total_bytes = total;
        }
        let mut stream = response.bytes_stream();
        let mut received = start;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Error::Sidecar(format!("魔搭下载中断：{e}")))?;
            file.write_all(&chunk).await?;
            received += chunk.len() as u64;
            self.inner
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .download
                .downloaded_bytes = received;
        }
        file.flush().await?;
        drop(file);
        if total.is_some_and(|expected| expected != received) {
            return Err(Error::Sidecar("魔搭下载不完整，可再次点击继续下载".into()));
        }
        let row = match self.import_gguf_with_source(&partial, model_dir, source, Some(name)) {
            Ok(row) => row,
            Err(err) => {
                // A completed non-GGUF response cannot be fixed by resuming it.
                if matches!(&err, Error::Sidecar(msg) if msg.contains("not GGUF")) {
                    let _ = tokio::fs::remove_file(&partial).await;
                }
                return Err(err);
            }
        };
        let _ = tokio::fs::remove_file(&partial).await;
        Ok(row)
    }

    pub fn delete_model(&self, id: &str) -> Result<()> {
        let mut models = self.load_catalog();
        let Some(idx) = models.iter().position(|m| m.id == id) else {
            return Err(Error::Invalid(format!("unknown local model {id}")));
        };
        let removed = models.remove(idx);
        let under_data = removed.path.starts_with(&self.data_dir);
        if under_data && removed.format == "gguf" {
            let _ = std::fs::remove_file(&removed.path);
        }
        if removed.active {
            if let Some(next) = models.last_mut() {
                next.active = true;
            }
        }
        self.save_catalog(&models)
    }

    pub fn verify_model(&self, id: &str) -> Result<LocalModel> {
        let mut models = self.load_catalog();
        let Some(row) = models.iter_mut().find(|m| m.id == id) else {
            return Err(Error::Invalid(format!("unknown local model {id}")));
        };
        let digest = if row.format == "mlx" {
            mlx_manifest(&row.path)?.0
        } else {
            sha256_file(&row.path)?
        };
        row.sha256_ok = digest.eq_ignore_ascii_case(&row.sha256);
        if row.sha256_ok {
            row.sha256 = digest;
        }
        let out = row.clone();
        self.save_catalog(&models)?;
        if !out.sha256_ok {
            return Err(Error::Sidecar(format!(
                "sidecar start: sha256 mismatch for {}",
                out.name
            )));
        }
        Ok(out)
    }

    pub fn activate_model(&self, id: &str) -> Result<LocalModel> {
        let mut models = self.load_catalog();
        if !models.iter().any(|m| m.id == id) {
            return Err(Error::Invalid(format!("unknown local model {id}")));
        }
        for m in &mut models {
            m.active = m.id == id;
        }
        self.save_catalog(&models)?;
        Ok(models.into_iter().find(|m| m.id == id).expect("active"))
    }

    pub fn logs(&self) -> String {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.logs.iter().cloned().collect::<Vec<_>>().join("")
    }

    pub fn openai_base_url(&self) -> Option<String> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.live
            .as_ref()
            .map(|l| format!("http://127.0.0.1:{}/v1", l.exposed_port()))
    }

    pub fn status(&self, queue: QueueSnapshot, binary: Option<String>) -> SidecarStatus {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let (kind, bind, pid) = if g.starting {
            (SidecarStatusKind::Starting, None, None)
        } else if let Some(live) = &g.live {
            (
                SidecarStatusKind::Ready,
                Some(format!("127.0.0.1:{}", live.exposed_port())),
                Some(live.pid),
            )
        } else if g.last_error.is_some() {
            (SidecarStatusKind::Failed, None, None)
        } else {
            (SidecarStatusKind::Stopped, None, None)
        };
        SidecarStatus {
            kind,
            bind,
            pid,
            active_model: self.active_model().map(|m| m.name),
            last_error: g.last_error.clone(),
            error_kind: g.error_kind.clone(),
            queue,
            binary,
        }
    }

    pub fn kill_sync(&self) {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(live) = g.live.take() {
            if let Some(proxy) = live.proxy {
                proxy.abort();
            }
            if let Some(kill) = live.kill {
                kill.terminate();
            }
        }
        g.starting = false;
    }

    pub fn push_log(&self, line: &str) {
        push_log_line(&self.inner, line);
    }

    fn set_error(&self, kind: &str, msg: String) {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.last_error = Some(msg);
        g.error_kind = Some(kind.into());
        g.starting = false;
        if let Some(live) = g.live.take() {
            if let Some(proxy) = live.proxy {
                proxy.abort();
            }
            if let Some(kill) = live.kill {
                kill.terminate();
            }
        }
    }

    pub fn check_vram(
        &self,
        model: &LocalModel,
        hw: &HardwareProbe,
        recommend: Option<&str>,
    ) -> VramCheck {
        let required_mb = estimate_vram_mb(model.bytes, &model.quant);
        let available_mb = hw.vram_mb;
        let recommend_quant = recommend
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| recommend_quant_for(available_mb));
        let ok = match available_mb {
            Some(v) => v >= required_mb,
            None => true,
        };
        VramCheck {
            required_mb,
            available_mb,
            recommend_quant,
            ok,
        }
    }

    pub async fn start(
        &self,
        model: &LocalModel,
        ready_timeout: Duration,
        n_gpu_layers: i32,
        bin: &Path,
        listen_port: Option<u16>,
        mlx_prompt_cache_bytes: Option<u64>,
        mlx_kv_bits: Option<u8>,
    ) -> Result<String> {
        self.kill_sync();
        {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.starting = true;
            g.last_error = None;
            g.error_kind = None;
            g.logs.clear();
        }
        let use_proxy = model.format == "mlx" && listen_port.is_some();
        let proxy_listener = if use_proxy {
            let public_port = listen_port.expect("proxy requires a configured port");
            match tokio::net::TcpListener::bind(("127.0.0.1", public_port)).await {
                Ok(listener) => Some(listener),
                Err(err) => {
                    let msg = format!("sidecar start: 127.0.0.1:{public_port} unavailable: {err}");
                    self.set_error("start", msg.clone());
                    return Err(Error::Sidecar(msg));
                }
            }
        } else {
            None
        };
        let requested_internal_port = if use_proxy { None } else { listen_port };
        let port = match requested_internal_port.map_or_else(free_loopback_port, |port| {
            let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|err| {
                Error::Sidecar(format!(
                    "sidecar start: 127.0.0.1:{port} unavailable: {err}"
                ))
            })?;
            drop(listener);
            Ok(port)
        }) {
            Ok(p) => p,
            Err(err) => {
                self.set_error("start", err.to_string());
                return Err(err);
            }
        };
        let args = if model.format == "mlx" {
            mlx_args(port, &model.path, mlx_prompt_cache_bytes, mlx_kv_bits)
        } else {
            llama_args(port, &model.path, n_gpu_layers)
        };
        #[cfg(target_os = "macos")]
        let watchdog_python = if model.format == "mlx" {
            bin.parent()
                .map(|dir| dir.join("python3"))
                .filter(|path| path.is_file())
        } else {
            None
        };
        #[cfg(not(target_os = "macos"))]
        let watchdog_python: Option<PathBuf> = None;
        let mut cmd = tokio::process::Command::new(watchdog_python.as_deref().unwrap_or(bin));
        #[cfg(target_os = "macos")]
        if watchdog_python.is_some() {
            cmd.arg("-c").arg(MLX_PARENT_WATCHDOG);
        }
        if model.format == "mlx" {
            let cache = self.data_dir.join("mlx-cache").join("hub");
            std::fs::create_dir_all(&cache)?;
            cmd.env("HF_HUB_CACHE", &cache)
                .env("HF_HUB_OFFLINE", "1")
                .env("TRANSFORMERS_OFFLINE", "1");
        }
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if watchdog_python.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(err) => {
                let msg = format!("sidecar start: spawn {}: {err}", bin.display());
                self.set_error("start", msg.clone());
                return Err(Error::Sidecar(msg));
            }
        };
        let pid = child.id().unwrap_or(0);
        let kill = KillTree::from_child(&child);
        let parent_pipe = child.stdin.take();
        pipe_logs(child.stdout.take(), child.stderr.take(), self.inner.clone());
        {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            g.live = Some(LiveSidecar {
                pid,
                port,
                public_port: if use_proxy { listen_port } else { None },
                proxy: None,
                kill,
                _parent_pipe: parent_pipe,
            });
        }
        let url = format!("http://127.0.0.1:{port}/v1");
        let deadline = Instant::now() + ready_timeout;
        loop {
            if child_exited(&mut child) {
                let tail = self.logs();
                let msg = format!("sidecar start: process exited before ready\n{tail}");
                self.set_error("start", msg.clone());
                return Err(Error::Sidecar(msg));
            }
            if Instant::now() >= deadline {
                self.kill_sync();
                let tail = self.logs();
                let msg = format!("sidecar start: health timeout\n{tail}");
                self.set_error("start", msg.clone());
                return Err(Error::Sidecar(msg));
            }
            if health_ok(&url).await {
                if model.format == "mlx" {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if let Err(err) = warmup_mlx(&url, remaining).await {
                        self.kill_sync();
                        let msg =
                            format!("sidecar start: MLX 模型加载失败：{err}\n{}", self.logs());
                        self.set_error("start", msg.clone());
                        return Err(Error::Sidecar(msg));
                    }
                }
                if let Some(listener) = proxy_listener {
                    let router = Router::new().fallback(mlx_proxy).with_state(MlxProxyState {
                        upstream_port: port,
                        client: reqwest::Client::new(),
                    });
                    let task = tokio::spawn(async move {
                        if let Err(err) = axum::serve(listener, router).await {
                            tracing::error!("MLX compatibility proxy stopped: {err}");
                        }
                    });
                    let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(live) = g.live.as_mut() {
                        live.proxy = Some(task);
                    }
                }
                let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                g.starting = false;
                drop(g);
                reap_child(child);
                return Ok(format!(
                    "http://127.0.0.1:{}/v1",
                    listen_port.unwrap_or(port)
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

fn push_log_line(inner: &Arc<Mutex<SidecarInner>>, line: &str) {
    let mut g = inner.lock().unwrap_or_else(|p| p.into_inner());
    g.logs.push_back(line.to_string());
    while g.logs.len() > LOG_LINES {
        g.logs.pop_front();
    }
}

#[derive(Clone)]
struct MlxProxyState {
    upstream_port: u16,
    client: reqwest::Client,
}

async fn mlx_proxy(State(state): State<MlxProxyState>, request: Request<Body>) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map_or("/", |value| value.as_str());
    let bytes = match to_bytes(body, 32 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(err) => return proxy_error(StatusCode::PAYLOAD_TOO_LARGE, err.to_string()),
    };
    let body = if parts.uri.path() == "/v1/chat/completions" {
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(mut payload) => {
                normalize_mlx_messages(&mut payload);
                match serde_json::to_vec(&payload) {
                    Ok(value) => value,
                    Err(err) => return proxy_error(StatusCode::BAD_REQUEST, err.to_string()),
                }
            }
            Err(err) => return proxy_error(StatusCode::BAD_REQUEST, err.to_string()),
        }
    } else {
        bytes.to_vec()
    };
    let url = format!("http://127.0.0.1:{}{path}", state.upstream_port);
    let mut forward = state.client.request(parts.method, url).body(body);
    for (name, value) in &parts.headers {
        if name != header::HOST
            && name != header::CONTENT_LENGTH
            && name != header::CONNECTION
            && name != header::TRANSFER_ENCODING
        {
            forward = forward.header(name, value);
        }
    }
    let upstream = match forward.send().await {
        Ok(response) => response,
        Err(err) => return proxy_error(StatusCode::BAD_GATEWAY, err.to_string()),
    };
    let mut response = Response::builder().status(upstream.status());
    for (name, value) in upstream.headers() {
        if name != header::CONTENT_LENGTH
            && name != header::CONNECTION
            && name != header::TRANSFER_ENCODING
        {
            response = response.header(name, value);
        }
    }
    response
        .body(Body::from_stream(upstream.bytes_stream()))
        .unwrap_or_else(|err| proxy_error(StatusCode::BAD_GATEWAY, err.to_string()))
}

fn proxy_error(status: StatusCode, message: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Body::from(message))
        .expect("valid proxy error response")
}

fn normalize_mlx_messages(payload: &mut serde_json::Value) {
    let Some(messages) = payload
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    let mut system_parts = Vec::new();
    let mut rest = Vec::new();
    for message in std::mem::take(messages) {
        if message.get("role").and_then(serde_json::Value::as_str) == Some("system") {
            if let Some(content) = message.get("content").and_then(serde_json::Value::as_str) {
                system_parts.push(content.to_owned());
            } else if let Some(blocks) =
                message.get("content").and_then(serde_json::Value::as_array)
            {
                for block in blocks {
                    if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
                        system_parts.push(text.to_owned());
                    }
                }
            }
        } else {
            rest.push(message);
        }
    }
    if !system_parts.is_empty() {
        messages.push(serde_json::json!({"role":"system","content":system_parts.join("\n\n")}));
    }
    messages.extend(rest);
}

fn pipe_logs(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    inner: Arc<Mutex<SidecarInner>>,
) {
    if let Some(out) = stdout {
        let inner = inner.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                push_log_line(&inner, &format!("{line}\n"));
            }
        });
    }
    if let Some(err) = stderr {
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                push_log_line(&inner, &format!("{line}\n"));
            }
        });
    }
}

fn reap_child(child: Child) {
    tokio::spawn(async move {
        let mut child = child;
        let _ = child.wait().await;
    });
}

fn child_exited(child: &mut Child) -> bool {
    match child.try_wait() {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(_) => true,
    }
}

async fn health_ok(base_url: &str) -> bool {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let Ok(client) = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(800))
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => resp.json::<serde_json::Value>().await.is_ok(),
        Err(_) => false,
        _ => false,
    }
}

async fn warmup_mlx(base_url: &str, timeout: Duration) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| Error::Sidecar(format!("MLX 客户端：{e}")))?;
    let response = client
        .post(format!(
            "{}/chat/completions",
            base_url.trim_end_matches('/')
        ))
        .json(&serde_json::json!({
            "model": "default_model",
            "messages": [{"role": "user", "content": "你好"}],
            "max_tokens": 1,
            "stream": false
        }))
        .send()
        .await
        .map_err(|e| Error::Sidecar(format!("MLX 预热请求：{e}")))?;
    let response = response
        .error_for_status()
        .map_err(|e| Error::Sidecar(format!("MLX 预热状态：{e}")))?;
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| Error::Sidecar(format!("MLX 预热响应：{e}")))?;
    if body["choices"]
        .as_array()
        .is_some_and(|choices| !choices.is_empty())
    {
        Ok(())
    } else {
        Err(Error::Sidecar(format!("MLX 预热未返回结果：{body}")))
    }
}

pub fn llama_args(port: u16, model: &Path, n_gpu_layers: i32) -> Vec<String> {
    vec![
        "-m".into(),
        model.display().to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "-ngl".into(),
        n_gpu_layers.to_string(),
    ]
}

pub fn find_llama_server(data_dir: &Path) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DANDELION_LLAMA_SERVER") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let local = if cfg!(windows) {
        data_dir.join("bin").join("llama-server.exe")
    } else {
        data_dir.join("bin").join("llama-server")
    };
    if local.is_file() {
        return Some(local);
    }
    which("llama-server")
}

/// Locate the Python MLX runner owned by Dandelion. The environment override is
/// useful for development; packaged builds can place it in data_dir/bin.
pub fn find_mlx_server(data_dir: &Path) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("DANDELION_MLX_SERVER") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let local = data_dir.join("bin").join("mlx_lm.server");
    if local.is_file() {
        return Some(local);
    }
    let managed = data_dir
        .join("bin")
        .join("mlx-venv")
        .join("bin")
        .join("mlx_lm.server");
    if managed.is_file() {
        return Some(managed);
    }
    which("mlx_lm.server")
}

fn mlx_args(
    port: u16,
    model: &Path,
    prompt_cache_bytes: Option<u64>,
    kv_bits: Option<u8>,
) -> Vec<String> {
    let mut args = vec![
        "--model".into(),
        model.display().to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--allowed-origins".into(),
        "http://127.0.0.1".into(),
    ];
    if let Some(bytes) = prompt_cache_bytes {
        args.extend(["--prompt-cache-bytes".into(), bytes.to_string()]);
    }
    if let Some(bits) = kv_bits {
        args.extend(["--kv-bits".into(), bits.to_string()]);
    }
    args
}

/// A quick structural fingerprint. Reading 15+ GB of weights when linking a
/// directory would make import unusable; the runner validates their contents.
fn mlx_manifest(path: &Path) -> Result<(String, u64)> {
    if !path.is_dir() {
        return Err(Error::Sidecar("MLX 模型路径不是文件夹".into()));
    }
    let config = std::fs::read(path.join("config.json"))
        .map_err(|_| Error::Sidecar("MLX 模型缺少 config.json".into()))?;
    let _: serde_json::Value = serde_json::from_slice(&config)?;
    if !path.join("tokenizer.json").is_file() && !path.join("tokenizer.model").is_file() {
        return Err(Error::Sidecar(
            "MLX 模型缺少 tokenizer.json 或 tokenizer.model".into(),
        ));
    }
    let mut files = std::fs::read_dir(path)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "safetensors"))
        .collect::<Vec<_>>();
    files.sort();
    if files.is_empty() {
        return Err(Error::Sidecar("MLX 模型缺少 .safetensors 权重".into()));
    }
    if path.join("model.safetensors.index.json").is_file() {
        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.join("model.safetensors.index.json"))?)?;
        if let Some(map) = index.get("weight_map").and_then(|v| v.as_object()) {
            for shard in map.values().filter_map(|v| v.as_str()) {
                if !path.join(shard).is_file() {
                    return Err(Error::Sidecar(format!("MLX 模型缺少权重分片：{shard}")));
                }
            }
        }
    }
    let mut hash = Sha256::new();
    hash.update(config);
    let mut bytes = 0u64;
    for file in files {
        let meta = std::fs::metadata(&file)?;
        if !meta.is_file() || meta.len() == 0 {
            return Err(Error::Sidecar("MLX 权重分片为空".into()));
        }
        bytes = bytes.saturating_add(meta.len());
        hash.update(file.file_name().unwrap().as_encoded_bytes());
        hash.update(meta.len().to_le_bytes());
        if let Ok(time) = meta.modified().and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map_err(std::io::Error::other)
        }) {
            hash.update(time.as_nanos().to_le_bytes());
        }
    }
    Ok((format!("{:x}", hash.finalize()), bytes))
}

fn modelscope_file_url(repo_id: &str, file_path: &str) -> Result<(reqwest::Url, String)> {
    fn valid_segment(s: &str) -> bool {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    }
    let repo_parts: Vec<_> = repo_id.split('/').collect();
    if repo_parts.len() != 2 || !repo_parts.iter().all(|part| valid_segment(part)) {
        return Err(Error::Invalid("魔搭仓库 ID 应为 所有者/模型名".into()));
    }
    let file_parts: Vec<_> = file_path.split('/').collect();
    if !file_parts.iter().all(|part| valid_segment(part)) {
        return Err(Error::Invalid("请输入仓库中的 GGUF 文件相对路径".into()));
    }
    let name = file_parts.last().unwrap().to_string();
    if !name.to_ascii_lowercase().ends_with(".gguf") {
        return Err(Error::Invalid("只能下载 .gguf 模型文件".into()));
    }
    let mut url = reqwest::Url::parse(&format!(
        "https://modelscope.cn/api/v1/models/{repo_id}/repo"
    ))
    .map_err(|e| Error::Invalid(format!("魔搭地址无效：{e}")))?;
    url.query_pairs_mut()
        .append_pair("Revision", "master")
        .append_pair("FilePath", file_path);
    Ok((url, name))
}

fn which(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let cand = dir.join(&exe);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

pub fn free_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|err| Error::Sidecar(format!("sidecar start: cannot bind 127.0.0.1: {err}")))?;
    let addr = listener
        .local_addr()
        .map_err(|err| Error::Sidecar(format!("sidecar start: no local addr: {err}")))?;
    if !addr.ip().is_loopback() {
        return Err(Error::Sidecar(
            "sidecar start: refused non-loopback bind".into(),
        ));
    }
    Ok(addr.port())
}

pub fn parse_quant(name: &str) -> String {
    let upper = name.to_ascii_uppercase();
    const PATS: &[&str] = &[
        "IQ4_XS", "IQ3_XXS", "Q5_K_M", "Q5_K_S", "Q4_K_M", "Q4_K_S", "Q3_K_M", "Q6_K", "Q8_0",
        "Q4_0", "BF16", "F16", "F32",
    ];
    for p in PATS {
        if upper.contains(p) {
            return (*p).to_string();
        }
    }
    "unknown".into()
}

pub fn estimate_vram_mb(bytes: u64, _quant: &str) -> u64 {
    let file_mb = bytes.div_ceil(1024 * 1024);
    file_mb.saturating_add(768)
}

fn recommend_quant_for(vram_mb: Option<u64>) -> String {
    let Some(vram) = vram_mb else {
        return "Q4_K_M".into();
    };
    if vram >= 16_000 {
        "Q8_0".into()
    } else if vram >= 8_000 {
        "Q5_K_M".into()
    } else {
        "Q4_K_M".into()
    }
}

fn stem(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name)
        .to_string()
}

fn unique_id(models: &[LocalModel], base: String) -> String {
    if !models.iter().any(|m| m.id == base) {
        return base;
    }
    for i in 2..1000 {
        let id = format!("{base}-{i}");
        if !models.iter().any(|m| m.id == id) {
            return id;
        }
    }
    format!("{base}-x")
}

fn read_catalog(path: &Path) -> Vec<LocalModel> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<CatalogFile>(&text)
        .map(|c| c.models)
        .unwrap_or_default()
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn probe_hardware(disk_path: &Path) -> HardwareProbe {
    let mut notes = Vec::new();
    let (gpu_name, vram_mb) = match nvidia_smi() {
        Some((name, vram)) => (Some(name), Some(vram)),
        None => {
            notes.push("non-NVIDIA / nvidia-smi missing — VRAM unknown, 11b skipped".into());
            (None, None)
        }
    };
    HardwareProbe {
        gpu_name,
        vram_mb,
        ram_mb: system_ram_mb(),
        disk_free_mb: disk_free_bytes(disk_path).map(|b| b / (1024 * 1024)),
        notes,
    }
}

fn nvidia_smi() -> Option<(String, u64)> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    let mut parts = line.split(',');
    let name = parts.next()?.trim().to_string();
    let vram: u64 = parts.next()?.trim().parse().ok()?;
    if name.is_empty() {
        return None;
    }
    Some((name, vram))
}

fn system_ram_mb() -> Option<u64> {
    #[cfg(windows)]
    {
        return windows_ram_mb();
    }
    #[cfg(unix)]
    {
        return unix_ram_mb();
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

#[cfg(windows)]
fn windows_ram_mb() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    unsafe {
        let mut info = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        GlobalMemoryStatusEx(&mut info).ok()?;
        Some(info.ullTotalPhys / (1024 * 1024))
    }
}

#[cfg(unix)]
fn unix_ram_mb() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb / 1024);
        }
    }
    None
}

pub(crate) fn disk_free_bytes(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        return windows_disk_free(path);
    }
    #[cfg(unix)]
    {
        return unix_disk_free(path);
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = path;
        None
    }
}

#[cfg(windows)]
fn windows_disk_free(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let target = path.ancestors().find(|p| p.exists()).unwrap_or(path);
    let wide: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free = 0u64;
    unsafe {
        GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), None, None, Some(&mut free)).ok()?;
    }
    Some(free)
}

#[cfg(unix)]
fn unix_disk_free(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    let c = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut s = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(c.as_ptr(), s.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let s = unsafe { s.assume_init() };
    Some(s.f_bavail as u64 * s.f_frsize as u64)
}

struct KillTree {
    #[cfg(windows)]
    job: winjob::Job,
    #[cfg(unix)]
    pgid: u32,
}

impl KillTree {
    fn from_child(child: &Child) -> Option<Self> {
        #[cfg(windows)]
        {
            return winjob::Job::from_child(child).ok().map(|job| Self { job });
        }
        #[cfg(unix)]
        {
            return child
                .id()
                .filter(|pgid| *pgid > 1)
                .map(|pgid| Self { pgid });
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = child;
            None
        }
    }

    fn terminate(self) {
        #[cfg(windows)]
        {
            self.job.terminate();
        }
        #[cfg(unix)]
        {
            if self.pgid > 1 {
                unsafe {
                    let _ = libc::killpg(self.pgid as i32, libc::SIGKILL);
                }
            }
        }
    }
}

#[cfg(windows)]
mod winjob {
    use tokio::process::Child;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct Job {
        handle: HANDLE,
    }

    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn from_child(child: &Child) -> std::io::Result<Self> {
            unsafe {
                let handle =
                    CreateJobObjectW(None, windows::core::PCWSTR::null()).map_err(|err| {
                        std::io::Error::new(std::io::ErrorKind::Other, err.to_string())
                    })?;
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
                .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err.to_string()))?;
                let raw = child.raw_handle().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::Other, "child has no process handle")
                })?;
                let proc = HANDLE(raw);
                if let Err(err) = AssignProcessToJobObject(handle, proc) {
                    let _ = CloseHandle(handle);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        err.to_string(),
                    ));
                }
                Ok(Self { handle })
            }
        }

        pub fn terminate(&self) {
            unsafe {
                let _ = TerminateJobObject(self.handle, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn mlx_watchdog_exits_when_owner_pipe_closes() {
        let code = MLX_PARENT_WATCHDOG.replace(
            "from mlx_lm.server import main\nmain()",
            "import time\ntime.sleep(30)",
        );
        let mut child = tokio::process::Command::new("python3")
            .args(["-c", &code])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("start watchdog probe");
        let parent_pipe = child.stdin.take().expect("stdin pipe");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(child.try_wait().expect("probe status").is_none());
        drop(parent_pipe);
        let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .expect("watchdog did not exit")
            .expect("wait for watchdog");
        assert!(status.success());
    }

    #[test]
    fn llama_args_are_loopback_only() {
        let args = llama_args(12345, Path::new("m.gguf"), 99);
        assert!(args.windows(2).any(|w| w == ["--host", "127.0.0.1"]));
        assert!(!args.iter().any(|a| a.contains("0.0.0.0")));
        assert!(args.windows(2).any(|w| w == ["--port", "12345"]));
    }

    #[test]
    fn parse_quant_from_filename() {
        assert_eq!(parse_quant("phi-Q4_K_M.gguf"), "Q4_K_M");
        assert_eq!(parse_quant("x-f16.gguf"), "F16");
    }

    #[test]
    fn modelscope_url_accepts_only_gguf_repo_paths() {
        let (url, name) =
            modelscope_file_url("Qwen/Qwen3-0.6B-GGUF", "gguf/model-Q4_K_M.gguf").unwrap();
        assert_eq!(name, "model-Q4_K_M.gguf");
        assert_eq!(url.host_str(), Some("modelscope.cn"));
        assert!(url.as_str().contains("FilePath=gguf%2Fmodel-Q4_K_M.gguf"));
        assert!(modelscope_file_url("../evil", "model.gguf").is_err());
        assert!(modelscope_file_url("Qwen/model", "../model.gguf").is_err());
        assert!(modelscope_file_url("Qwen/model", "model.bin").is_err());
    }

    #[test]
    fn mlx_directory_is_linked_and_deletion_preserves_weights() {
        if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let model_dir = dir.path().join(".hidden-models").join("toy.8-mlx");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(
            model_dir.join("config.json"),
            r#"{"model_type":"qwen3","quantization":{"bits":4}}"#,
        )
        .unwrap();
        std::fs::write(model_dir.join("tokenizer.json"), "{}").unwrap();
        std::fs::write(model_dir.join("model.safetensors"), b"weights").unwrap();
        let manager = SidecarManager::new(dir.path().join("data"));
        let row = manager.import_mlx(&model_dir).unwrap();
        assert_eq!(row.format, "mlx");
        assert_eq!(row.id, "toy.8-mlx");
        assert_eq!(row.quant, "4-bit");
        assert!(row.active);
        assert_eq!(manager.import_mlx(&model_dir).unwrap().id, row.id);
        manager.verify_model(&row.id).unwrap();
        manager.delete_model(&row.id).unwrap();
        assert!(model_dir.join("model.safetensors").exists());
    }

    #[test]
    fn mlx_manifest_detects_missing_shard() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        std::fs::write(dir.path().join("tokenizer.json"), "{}").unwrap();
        std::fs::write(
            dir.path().join("model-00001-of-00002.safetensors"),
            b"weights",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("model.safetensors.index.json"),
            r#"{"weight_map":{"x":"model-00002-of-00002.safetensors"}}"#,
        )
        .unwrap();
        assert!(mlx_manifest(dir.path()).is_err());
    }

    #[tokio::test]
    async fn modelscope_download_registers_verified_gguf() {
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let mut body = b"GGUF".to_vec();
        body.extend_from_slice(&[0; 64]);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let manager = SidecarManager::new(dir.path().join("data"));
        let url = reqwest::Url::parse(&server.uri()).unwrap();
        let model_dir = dir.path().join("models");
        let row = manager
            .download_modelscope_inner(&url, "demo-Q4_K_M.gguf", "modelscope:Qwen/demo", &model_dir)
            .await
            .unwrap();
        assert_eq!(row.source, "modelscope:Qwen/demo");
        assert!(row.sha256_ok);
        assert_eq!(manager.load_catalog().len(), 1);
        assert!(row.path.is_file());
    }

    #[test]
    fn free_port_is_loopback() {
        let p = free_loopback_port().expect("port");
        assert!(p > 0);
    }

    #[test]
    fn mlx_proxy_moves_and_merges_system_messages() {
        let mut payload = serde_json::json!({"messages": [
            {"role":"user","content":"hello"},
            {"role":"system","content":"first"},
            {"role":"assistant","content":"hi"},
            {"role":"system","content":"second"}
        ]});
        normalize_mlx_messages(&mut payload);
        assert_eq!(
            payload["messages"][0],
            serde_json::json!({"role":"system","content":"first\n\nsecond"})
        );
        assert_eq!(payload["messages"][1]["role"], "user");
        assert_eq!(payload["messages"][2]["role"], "assistant");
    }

    #[test]
    fn mlx_memory_limits_are_passed_to_runner() {
        let args = mlx_args(8001, Path::new("/tmp/model"), Some(1_073_741_824), Some(4));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--prompt-cache-bytes", "1073741824"]));
        assert!(args.windows(2).any(|pair| pair == ["--kv-bits", "4"]));
    }

    #[tokio::test]
    async fn queue_second_waits_until_first_drops() {
        let q = LocalQueue::new();
        let a = SessionId::parse("tsk_a").unwrap();
        let b = SessionId::parse("tsk_b").unwrap();
        let lease_a = q
            .acquire(a.clone(), 1, CancellationToken::new())
            .await
            .unwrap();
        let cancel_b = CancellationToken::new();
        let q2 = q.clone();
        let b2 = b.clone();
        let handle = tokio::spawn(async move { q2.acquire(b2, 1, cancel_b).await });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(q.waiting_contains(&b));
        drop(lease_a);
        let lease_b = handle.await.unwrap().unwrap();
        assert!(!q.waiting_contains(&b));
        drop(lease_b);
    }

    #[tokio::test]
    async fn queue_cancel_dequeues() {
        let q = LocalQueue::new();
        let a = SessionId::parse("tsk_a").unwrap();
        let b = SessionId::parse("tsk_b").unwrap();
        let _lease_a = q.acquire(a, 1, CancellationToken::new()).await.unwrap();
        let cancel_b = CancellationToken::new();
        let c2 = cancel_b.clone();
        let q2 = q.clone();
        let handle = tokio::spawn(async move { q2.acquire(b.clone(), 1, cancel_b).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        c2.cancel();
        let res = handle.await.unwrap();
        assert!(matches!(res, Err(Error::Cancelled)));
    }
}
