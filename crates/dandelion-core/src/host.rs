use crate::acp::{RequestPermissionOutcome, RequestPermissionRequest};
use crate::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Same folder CLI (`dln`) and the desktop host use for `settings.json` / SQLite.
///
/// `DANDELION_DATA_DIR` overrides the location when set to a non-blank
/// value. Used by perf/acceptance tooling to run against an isolated
/// first-install directory without touching real user data (the
/// `directories` crate reads the shell known folder on Windows, so plain
/// `APPDATA` redirection does not work).
pub fn product_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var("DANDELION_DATA_DIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("com", "dandelion", "dandelion")
        .map(|p| p.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".dandelion"))
}

/// Process that embeds [`crate::Runtime`].
///
/// CLI and Tauri must each provide one implementation. The runtime never
/// assumes stdout, a webview, or JSON-RPC transport.
#[async_trait]
pub trait AgentHost: Send + Sync + 'static {
    /// Host admission policy, independent of provider/tool permission policy.
    fn check_execution(&self) -> Result<()> {
        Ok(())
    }
    fn kind(&self) -> HostKind;
    fn data_dir(&self) -> PathBuf;
    fn is_interactive(&self) -> bool;
    async fn request_permission(
        &self,
        req: RequestPermissionRequest,
        cancel: CancellationToken,
    ) -> Result<RequestPermissionOutcome>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostKind {
    Cli,
    /// Reserved for `apps/desktop` (Tauri). Same Runtime, different I/O.
    Desktop,
}

/// Default host for `dln`. Data lives under the platform app-data directory.
#[derive(Clone, Debug)]
pub struct CliHost {
    data_dir: PathBuf,
    interactive: bool,
}

impl CliHost {
    pub fn new() -> Self {
        Self {
            data_dir: product_data_dir(),
            interactive: io::stdin().is_terminal(),
        }
    }

    pub fn with_data_dir(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            interactive: false,
        }
    }

    pub fn with_data_dir_interactive(data_dir: PathBuf, interactive: bool) -> Self {
        Self {
            data_dir,
            interactive,
        }
    }

    pub fn data_dir(&self) -> PathBuf {
        self.data_dir.clone()
    }
}

impl Default for CliHost {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentHost for CliHost {
    fn kind(&self) -> HostKind {
        HostKind::Cli
    }

    fn data_dir(&self) -> PathBuf {
        self.data_dir.clone()
    }

    fn is_interactive(&self) -> bool {
        self.interactive
    }

    async fn request_permission(
        &self,
        req: RequestPermissionRequest,
        cancel: CancellationToken,
    ) -> Result<RequestPermissionOutcome> {
        if !self.interactive {
            return Ok(RequestPermissionOutcome::Denied);
        }
        {
            let mut err = io::stderr().lock();
            let _ = writeln!(err, "{}", req.title);
            let _ = write!(err, "Allow? [y/N] ");
            let _ = err.flush();
        }
        let read = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            let _ = io::stdin().read_line(&mut line);
            line
        });
        tokio::select! {
            _ = cancel.cancelled() => Ok(RequestPermissionOutcome::Cancelled),
            res = read => {
                let line = res.unwrap_or_default();
                let t = line.trim();
                if t.eq_ignore_ascii_case("y") || t.eq_ignore_ascii_case("yes") {
                    Ok(RequestPermissionOutcome::Selected {
                        option_id: "allow".into(),
                    })
                } else {
                    Ok(RequestPermissionOutcome::Selected {
                        option_id: "deny".into(),
                    })
                }
            }
        }
    }
}

/// Tauri host. Permission is request-response through [`DesktopHost::respond`],
/// not a bus event. Attach a sink (webview emit) before the first `ask` tool.
#[derive(Clone)]
pub struct DesktopHost {
    inner: Arc<DesktopHostInner>,
}

struct DesktopHostInner {
    execution_guard: Mutex<Option<ExecutionGuard>>,
    data_dir: PathBuf,
    sink: Mutex<Option<PermissionSink>>,
    /// request_id → (payload, responder). The payload rides along so a
    /// window that missed the one-shot event can replay the dialog later.
    pending: Mutex<
        HashMap<
            String,
            (
                RequestPermissionRequest,
                oneshot::Sender<RequestPermissionOutcome>,
            ),
        >,
    >,
}

/// Remove unanswered requests when the waiting future is cancelled or times out.
struct PendingPermissionGuard {
    inner: Arc<DesktopHostInner>,
    request_id: String,
}

impl Drop for PendingPermissionGuard {
    fn drop(&mut self) {
        self.inner
            .pending
            .lock()
            .expect("permission pending")
            .remove(&self.request_id);
    }
}

/// `request_id` plus the ACP permission payload. Host adapter emits this to Vue.
pub type PermissionSink = Arc<dyn Fn(String, RequestPermissionRequest) + Send + Sync>;
pub type ExecutionGuard = Arc<dyn Fn() -> Result<()> + Send + Sync>;

impl DesktopHost {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(DesktopHostInner {
                execution_guard: Mutex::new(None),
                data_dir,
                sink: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn new_default() -> Self {
        Self::new(product_data_dir())
    }

    pub fn data_dir(&self) -> PathBuf {
        self.inner.data_dir.clone()
    }

    pub fn set_permission_sink(&self, sink: PermissionSink) {
        *self.inner.sink.lock().expect("permission sink") = Some(sink);
    }

    pub fn set_execution_guard(&self, guard: ExecutionGuard) {
        *self.inner.execution_guard.lock().expect("execution guard") = Some(guard);
    }

    /// Answer a pending [`AgentHost::request_permission`]. Unknown ids are ignored.
    pub fn respond(&self, request_id: &str, outcome: RequestPermissionOutcome) {
        if let Some((_req, tx)) = self
            .inner
            .pending
            .lock()
            .expect("permission pending")
            .remove(request_id)
        {
            let _ = tx.send(outcome);
        }
    }

    /// Snapshot of still-unanswered requests (request_id, payload). A window
    /// that was not visible when the one-shot event fired can pull these on
    /// focus and render the dialog before the core-side timeout elapses.
    pub fn pending_requests(&self) -> Vec<(String, RequestPermissionRequest)> {
        self.inner
            .pending
            .lock()
            .expect("permission pending")
            .iter()
            .map(|(id, (req, _))| (id.clone(), req.clone()))
            .collect()
    }
}

#[async_trait]
impl AgentHost for DesktopHost {
    fn check_execution(&self) -> Result<()> {
        let guard = self
            .inner
            .execution_guard
            .lock()
            .map_err(|_| crate::Error::Invalid("execution guard".into()))?
            .clone();
        if let Some(guard) = guard {
            guard()?;
        }
        Ok(())
    }
    fn kind(&self) -> HostKind {
        HostKind::Desktop
    }

    fn data_dir(&self) -> PathBuf {
        self.inner.data_dir.clone()
    }

    fn is_interactive(&self) -> bool {
        true
    }

    async fn request_permission(
        &self,
        req: RequestPermissionRequest,
        cancel: CancellationToken,
    ) -> Result<RequestPermissionOutcome> {
        if cancel.is_cancelled() {
            return Ok(RequestPermissionOutcome::Cancelled);
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .expect("permission pending")
            .insert(request_id.clone(), (req.clone(), tx));
        let _pending_guard = PendingPermissionGuard {
            inner: self.inner.clone(),
            request_id: request_id.clone(),
        };
        let sink = self.inner.sink.lock().expect("permission sink").clone();
        let Some(sink) = sink else {
            return Ok(RequestPermissionOutcome::Denied);
        };
        sink(request_id.clone(), req);
        tokio::select! {
            _ = cancel.cancelled() => Ok(RequestPermissionOutcome::Cancelled),
            res = rx => Ok(res.unwrap_or(RequestPermissionOutcome::Denied)),
        }
    }
}

/// Test/CI host. Records permission titles; never prints.
#[derive(Clone)]
pub struct RecordingHost {
    data_dir: PathBuf,
    interactive: bool,
    pub calls: Arc<Mutex<Vec<String>>>,
    allow: bool,
    hang_on_permission: bool,
    selected_option: String,
}

impl RecordingHost {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            interactive: true,
            calls: Arc::new(Mutex::new(Vec::new())),
            allow: true,
            hang_on_permission: false,
            selected_option: "allow".into(),
        }
    }

    pub fn deny(mut self) -> Self {
        self.allow = false;
        self
    }

    /// Answer permission prompts with an explicit option id ("allow_turn", …).
    pub fn with_option(mut self, option_id: &str) -> Self {
        self.selected_option = option_id.into();
        self.allow = true;
        self
    }

    pub fn non_interactive(mut self) -> Self {
        self.interactive = false;
        self
    }

    /// Block in `request_permission` until the session cancel token fires.
    pub fn hang_on_permission(mut self) -> Self {
        self.hang_on_permission = true;
        self.interactive = true;
        self
    }

    pub fn call_titles(&self) -> Vec<String> {
        self.calls.lock().expect("lock").clone()
    }
}

#[async_trait]
impl AgentHost for RecordingHost {
    fn kind(&self) -> HostKind {
        HostKind::Cli
    }

    fn data_dir(&self) -> PathBuf {
        self.data_dir.clone()
    }

    fn is_interactive(&self) -> bool {
        self.interactive
    }

    async fn request_permission(
        &self,
        req: RequestPermissionRequest,
        cancel: CancellationToken,
    ) -> Result<RequestPermissionOutcome> {
        self.calls.lock().expect("lock").push(req.title);
        if self.hang_on_permission {
            cancel.cancelled().await;
            return Ok(RequestPermissionOutcome::Cancelled);
        }
        if cancel.is_cancelled() {
            return Ok(RequestPermissionOutcome::Cancelled);
        }
        if !self.interactive {
            return Ok(RequestPermissionOutcome::Denied);
        }
        if self.allow {
            Ok(RequestPermissionOutcome::Selected {
                option_id: self.selected_option.clone(),
            })
        } else {
            Ok(RequestPermissionOutcome::Selected {
                option_id: "deny".into(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{SessionId, ToolCallId};
    use std::time::Duration;

    fn sample_req() -> RequestPermissionRequest {
        RequestPermissionRequest::tool_call(
            SessionId::parse("tsk_parent").unwrap(),
            "Allow write_file to a.txt?".into(),
            ToolCallId::parse("call_1"),
        )
    }

    #[tokio::test]
    async fn desktop_host_without_sink_denies() {
        let host = DesktopHost::new(std::env::temp_dir());
        let out = host
            .request_permission(sample_req(), CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(out, RequestPermissionOutcome::Denied));
    }

    #[tokio::test]
    async fn desktop_host_respond_allow() {
        let host = DesktopHost::new(std::env::temp_dir());
        let reply = host.clone();
        host.set_permission_sink(Arc::new(move |id, _req| {
            reply.respond(
                &id,
                RequestPermissionOutcome::Selected {
                    option_id: "allow".into(),
                },
            );
        }));
        let out = host
            .request_permission(sample_req(), CancellationToken::new())
            .await
            .unwrap();
        assert!(out.is_allow());
    }

    #[tokio::test]
    async fn desktop_host_session_cancel_aborts_prompt() {
        let host = DesktopHost::new(std::env::temp_dir());
        host.set_permission_sink(Arc::new(|_id, _req| {}));
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            cancel2.cancel();
        });
        let out = host.request_permission(sample_req(), cancel).await.unwrap();
        assert!(matches!(out, RequestPermissionOutcome::Cancelled));
    }

    #[tokio::test]
    async fn desktop_host_dropped_request_clears_pending() {
        let host = DesktopHost::new(std::env::temp_dir());
        let (tx, rx) = oneshot::channel::<String>();
        let sender = Arc::new(Mutex::new(Some(tx)));
        host.set_permission_sink(Arc::new(move |id, _req| {
            if let Some(tx) = sender.lock().expect("sender").take() {
                let _ = tx.send(id);
            }
        }));
        let waiting_host = host.clone();
        let waiter = tokio::spawn(async move {
            waiting_host
                .request_permission(sample_req(), CancellationToken::new())
                .await
        });
        let request_id = rx.await.expect("request emitted");
        assert!(host
            .inner
            .pending
            .lock()
            .expect("pending")
            .contains_key(&request_id));
        waiter.abort();
        let _ = waiter.await;
        assert!(!host
            .inner
            .pending
            .lock()
            .expect("pending")
            .contains_key(&request_id));
    }
}
