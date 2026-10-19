//! Process table + kill-tree on cancel/timeout.
//!
//! Same contract on every OS: Stop must kill the spawned command **and** its
//! descendants. Adapters: Windows Job Object; POSIX process group (`killpg`).

use crate::acp::{
    encode_base64, SessionUpdate, TerminalExitStatus, ToolCallContent, ToolCallUpdate,
};
use crate::config::Settings;
use crate::ids::{SessionId, ToolCallId};
use serde_json::json;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::time::{interval, sleep, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// ACP live-pane options. Chunk size / flush interval come from Settings.
#[derive(Clone)]
pub struct LiveOpts {
    pub emit: Arc<dyn Fn(SessionUpdate) + Send + Sync>,
    pub tool_call_id: ToolCallId,
    /// Agent-initiated terminal tools are represented in the chat transcript.
    /// Manually opened desktop terminals are not Agent tool calls and must only
    /// update the terminal pane.
    pub publish_tool_call: bool,
    pub chunk_bytes: usize,
    pub flush_ms: u64,
}

#[derive(Clone)]
struct LiveSink {
    terminal_id: String,
    emit: Arc<dyn Fn(SessionUpdate) + Send + Sync>,
    chunk_bytes: usize,
    flush_ms: u64,
    tool_call_id: ToolCallId,
    publish_tool_call: bool,
    exited: Arc<AtomicBool>,
}

impl LiveSink {
    fn bind(proc_id: String, opts: &LiveOpts) -> Self {
        Self {
            terminal_id: proc_id,
            emit: opts.emit.clone(),
            chunk_bytes: opts.chunk_bytes.max(64),
            flush_ms: opts.flush_ms.max(1),
            tool_call_id: opts.tool_call_id.clone(),
            publish_tool_call: opts.publish_tool_call,
            exited: Arc::new(AtomicBool::new(false)),
        }
    }

    fn emit_start(&self, command: &str, cwd: &std::path::Path, background: bool) {
        (self.emit)(SessionUpdate::TerminalUpdate {
            terminal_id: self.terminal_id.clone(),
            command: Some(command.to_string()),
            cwd: Some(cwd.display().to_string()),
            background: Some(background),
            output: None,
            exit_status: None,
        });
        if self.publish_tool_call {
            (self.emit)(SessionUpdate::tool_call(ToolCallUpdate {
                tool_call_id: self.tool_call_id.clone(),
                title: None,
                kind: None,
                status: None,
                raw_input: None,
                content: Some(vec![ToolCallContent::Terminal {
                    terminal_id: self.terminal_id.clone(),
                }]),
            }));
        }
    }

    fn emit_chunk(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        (self.emit)(SessionUpdate::TerminalOutputChunk {
            terminal_id: self.terminal_id.clone(),
            data: encode_base64(bytes),
        });
    }

    fn emit_exit(&self, code: Option<i32>) {
        if self.exited.swap(true, Ordering::SeqCst) {
            return;
        }
        (self.emit)(SessionUpdate::TerminalUpdate {
            terminal_id: self.terminal_id.clone(),
            command: None,
            cwd: None,
            background: None,
            output: None,
            exit_status: Some(TerminalExitStatus {
                exit_code: code.and_then(|c| u32::try_from(c).ok()),
                signal: None,
            }),
        });
    }
}

struct Chunker {
    sink: LiveSink,
    acc: Vec<u8>,
}

impl Chunker {
    fn push(&mut self, bytes: &[u8]) {
        self.acc.extend_from_slice(bytes);
        let n = self.sink.chunk_bytes;
        while self.acc.len() >= n {
            let rest = self.acc.split_off(n);
            self.sink.emit_chunk(&self.acc);
            self.acc = rest;
        }
    }

    fn flush(&mut self) {
        if self.acc.is_empty() {
            return;
        }
        self.sink.emit_chunk(&self.acc);
        self.acc.clear();
    }
}

pub fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n...[truncated {} bytes]",
        &s[..end],
        s.len().saturating_sub(end)
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcStatus {
    Running,
    Exited,
    Killed,
    Timeout,
}

impl ProcStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Killed => "killed",
            Self::Timeout => "timeout",
        }
    }
}

struct ProcRec {
    proc_id: String,
    acp_session: SessionId,
    command: String,
    cwd: String,
    started_at: Instant,
    pid: u32,
    kill_tree: Option<KillTree>,
    output: Arc<Mutex<String>>,
    status: ProcStatus,
    exit_code: Option<i32>,
    last_poll: usize,
    background: bool,
    live: Option<LiveSink>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundProcess {
    pub id: String,
    pub command: String,
    pub cwd: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub output: String,
}

#[derive(Clone)]
pub struct ProcessTable {
    inner: Arc<Mutex<HashMap<String, ProcRec>>>,
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl ProcessTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run_foreground(
        &self,
        acp_session: &SessionId,
        command: &str,
        workdir: &std::path::Path,
        timeout_sec: u64,
        settings: &Settings,
        cancel: CancellationToken,
        live: Option<LiveOpts>,
    ) -> String {
        match spawn_shell(command, workdir) {
            Ok((mut child, kill_tree)) => {
                let pid = child.id().unwrap_or(0);
                let proc_id = new_proc_id();
                let output = Arc::new(Mutex::new(String::new()));
                let sink = live
                    .as_ref()
                    .map(|opts| LiveSink::bind(proc_id.clone(), opts));
                if let Some(s) = &sink {
                    s.emit_start(command, workdir, false);
                }
                {
                    let mut map = self.inner.lock().expect("lock");
                    map.insert(
                        proc_id.clone(),
                        ProcRec {
                            proc_id: proc_id.clone(),
                            acp_session: acp_session.clone(),
                            command: command.to_string(),
                            cwd: workdir.display().to_string(),
                            started_at: Instant::now(),
                            pid,
                            kill_tree,
                            output: output.clone(),
                            status: ProcStatus::Running,
                            exit_code: None,
                            last_poll: 0,
                            background: false,
                            live: sink.clone(),
                        },
                    );
                }
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let reader = tokio::spawn(read_pipes(stdout, stderr, output.clone(), sink.clone()));
                let outcome = tokio::select! {
                    _ = cancel.cancelled() => ForegroundOutcome::Cancelled,
                    _ = sleep(Duration::from_secs(timeout_sec)) => ForegroundOutcome::Timeout,
                    status = child.wait() => ForegroundOutcome::Exited(status.ok().and_then(|s| s.code())),
                };
                match outcome {
                    ForegroundOutcome::Exited(code) => {
                        let _ = reader.await;
                        self.finish_foreground(&proc_id, ProcStatus::Exited, code);
                        let text = take_output(&output, settings.terminal_output_max_bytes);
                        format_exit(code.unwrap_or(-1), &text)
                    }
                    ForegroundOutcome::Timeout => {
                        self.kill_proc(&proc_id);
                        let _ = child.kill().await;
                        let _ = reader.await;
                        self.finish_foreground(&proc_id, ProcStatus::Timeout, None);
                        let text = take_output(&output, settings.terminal_output_max_bytes);
                        format!(
                            "Error: timeout after {timeout_sec}s\n{}",
                            if text.is_empty() {
                                "(no output)"
                            } else {
                                &text
                            }
                        )
                    }
                    ForegroundOutcome::Cancelled => {
                        self.kill_proc(&proc_id);
                        let _ = child.kill().await;
                        let _ = reader.await;
                        self.finish_foreground(&proc_id, ProcStatus::Killed, None);
                        let text = take_output(&output, settings.terminal_output_max_bytes);
                        format!(
                            "Error: cancelled\n{}",
                            if text.is_empty() {
                                "(no output)"
                            } else {
                                &text
                            }
                        )
                    }
                }
            }
            Err(err) => format!("Error: failed to spawn: {err}"),
        }
    }

    pub fn spawn_background(
        &self,
        acp_session: &SessionId,
        command: &str,
        workdir: &std::path::Path,
        live: Option<LiveOpts>,
    ) -> String {
        match spawn_shell(command, workdir) {
            Ok((mut child, kill_tree)) => {
                let pid = child.id().unwrap_or(0);
                let proc_id = new_proc_id();
                let output = Arc::new(Mutex::new(String::new()));
                let sink = live
                    .as_ref()
                    .map(|opts| LiveSink::bind(proc_id.clone(), opts));
                if let Some(s) = &sink {
                    s.emit_start(command, workdir, true);
                }
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                {
                    let mut map = self.inner.lock().expect("lock");
                    map.insert(
                        proc_id.clone(),
                        ProcRec {
                            proc_id: proc_id.clone(),
                            acp_session: acp_session.clone(),
                            command: command.to_string(),
                            cwd: workdir.display().to_string(),
                            started_at: Instant::now(),
                            pid,
                            kill_tree,
                            output: output.clone(),
                            status: ProcStatus::Running,
                            exit_code: None,
                            last_poll: 0,
                            background: true,
                            live: sink.clone(),
                        },
                    );
                }
                let table = self.clone();
                let watch_id = proc_id.clone();
                tokio::spawn(async move {
                    let reader = tokio::spawn(read_pipes(stdout, stderr, output, sink.clone()));
                    let code = child.wait().await.ok().and_then(|s| s.code());
                    let _ = reader.await;
                    table.mark_exited(&watch_id, code);
                });
                json!({
                    "session_id": proc_id,
                    "pid": pid,
                    "status": "running"
                })
                .to_string()
            }
            Err(err) => format!("Error: failed to spawn: {err}"),
        }
    }

    pub fn list(&self, acp_session: &SessionId) -> String {
        let map = self.inner.lock().expect("lock");
        let items: Vec<_> = map
            .values()
            .filter(|p| p.acp_session == *acp_session && p.background)
            .map(|p| {
                json!({
                    "id": p.proc_id,
                    "pid": p.pid,
                    "status": p.status.as_str(),
                    "command": preview(&p.command, 80),
                })
            })
            .collect();
        serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into())
    }

    /// Snapshot for a desktop window opened after the live start event.
    pub fn background_processes(&self, acp_session: &SessionId, max_output_bytes: usize) -> Vec<BackgroundProcess> {
        let map = self.inner.lock().expect("lock");
        map.values()
            .filter(|p| p.acp_session == *acp_session && p.background)
            .map(|p| BackgroundProcess {
                id: p.proc_id.clone(),
                command: p.command.clone(),
                cwd: p.cwd.clone(),
                status: p.status.as_str().to_string(),
                exit_code: p.exit_code,
                elapsed_ms: p.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64,
                output: {
                    let output = p.output.lock().expect("lock");
                    let mut start = output.len().saturating_sub(max_output_bytes);
                    while !output.is_char_boundary(start) { start += 1; }
                    output[start..].to_string()
                },
            })
            .collect()
    }

    pub fn poll(&self, proc_id: &str, max_bytes: usize) -> String {
        let mut map = self.inner.lock().expect("lock");
        let Some(rec) = map.get_mut(proc_id) else {
            return format!("Error: unknown process session_id {proc_id}");
        };
        let buf = rec.output.lock().expect("lock").clone();
        let new = if rec.last_poll >= buf.len() {
            String::new()
        } else {
            buf[rec.last_poll..].to_string()
        };
        rec.last_poll = buf.len();
        let clipped = truncate_bytes(&new, max_bytes);
        json!({
            "status": rec.status.as_str(),
            "exit_code": rec.exit_code,
            "output": clipped,
        })
        .to_string()
    }

    pub fn log(&self, proc_id: &str, offset: Option<i64>, limit: u32, max_bytes: usize) -> String {
        let map = self.inner.lock().expect("lock");
        let Some(rec) = map.get(proc_id) else {
            return format!("Error: unknown process session_id {proc_id}");
        };
        let buf = rec.output.lock().expect("lock").clone();
        let lines: Vec<&str> = buf.lines().collect();
        let n = lines.len();
        let take = limit as usize;
        let start = match offset {
            Some(o) if o >= 0 => (o as usize).min(n),
            _ => n.saturating_sub(take),
        };
        let slice = lines
            .get(start..)
            .unwrap_or(&[])
            .iter()
            .take(take)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        truncate_bytes(&slice, max_bytes)
    }

    pub fn kill(&self, proc_id: &str) -> String {
        if self.kill_live(proc_id) {
            json!({"status":"killed","session_id": proc_id}).to_string()
        } else {
            format!("Error: unknown process session_id {proc_id}")
        }
    }

    /// Stop exactly one live terminal/process tree without cancelling the
    /// agent session that owns it.
    pub fn kill_live(&self, proc_id: &str) -> bool {
        if !self.kill_proc(proc_id) {
            return false;
        }
        let mut map = self.inner.lock().expect("lock");
        if let Some(rec) = map.get_mut(proc_id) {
            rec.status = ProcStatus::Killed;
        }
        true
    }

    pub fn kill_session(&self, acp_session: &SessionId) {
        let ids: Vec<String> = {
            let map = self.inner.lock().expect("lock");
            map.values()
                .filter(|p| p.acp_session == *acp_session)
                .map(|p| p.proc_id.clone())
                .collect()
        };
        for id in ids {
            self.kill_proc(&id);
            let mut map = self.inner.lock().expect("lock");
            if let Some(rec) = map.get_mut(&id) {
                if rec.status == ProcStatus::Running {
                    rec.status = ProcStatus::Killed;
                }
            }
        }
    }

    pub fn session_has_running(&self, acp_session: &SessionId) -> bool {
        let map = self.inner.lock().expect("lock");
        map.values()
            .any(|p| p.acp_session == *acp_session && p.status == ProcStatus::Running)
    }

    fn kill_proc(&self, proc_id: &str) -> bool {
        let mut map = self.inner.lock().expect("lock");
        let Some(rec) = map.get_mut(proc_id) else {
            return false;
        };
        if let Some(tree) = rec.kill_tree.take() {
            tree.terminate();
        }
        true
    }

    fn finish_foreground(&self, proc_id: &str, status: ProcStatus, code: Option<i32>) {
        let live = {
            let mut map = self.inner.lock().expect("lock");
            let live = map.get(proc_id).and_then(|r| r.live.clone());
            if let Some(rec) = map.get_mut(proc_id) {
                rec.status = status;
                rec.exit_code = code;
                rec.kill_tree = None;
            }
            map.remove(proc_id);
            live
        };
        if let Some(live) = live {
            live.emit_exit(code);
        }
    }

    pub fn mark_exited(&self, proc_id: &str, code: Option<i32>) {
        let live = {
            let mut map = self.inner.lock().expect("lock");
            let Some(rec) = map.get_mut(proc_id) else {
                return;
            };
            if rec.status == ProcStatus::Running {
                rec.status = ProcStatus::Exited;
                rec.exit_code = code;
            }
            rec.live.clone()
        };
        if let Some(live) = live {
            live.emit_exit(code);
        }
    }
}

enum ForegroundOutcome {
    Exited(Option<i32>),
    Timeout,
    Cancelled,
}

fn new_proc_id() -> String {
    format!("proc_{}", Uuid::new_v4().simple())
}

fn preview(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

fn take_output(buf: &Arc<Mutex<String>>, max: usize) -> String {
    let s = buf.lock().expect("lock").clone();
    truncate_bytes(&s, max)
}

fn format_exit(code: i32, text: &str) -> String {
    if text.trim().is_empty() {
        format!("exit_code={code}\n(no output)")
    } else {
        format!("exit_code={code}\n{text}")
    }
}

async fn read_pipes(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    buf: Arc<Mutex<String>>,
    live: Option<LiveSink>,
) {
    let chunker = live.map(|sink| {
        let flush_ms = sink.flush_ms;
        let c = Arc::new(Mutex::new(Chunker {
            sink,
            acc: Vec::new(),
        }));
        let ticker_c = c.clone();
        let ticker = tokio::spawn(async move {
            let mut tick = interval(Duration::from_millis(flush_ms));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if let Ok(mut g) = ticker_c.lock() {
                    g.flush();
                }
            }
        });
        (c, ticker)
    });
    let shared = chunker.as_ref().map(|(c, _)| c.clone());
    let mut out_task = None;
    let mut err_task = None;
    if let Some(so) = stdout {
        let b = buf.clone();
        let c = shared.clone();
        out_task = Some(tokio::spawn(async move {
            pump_reader(so, b, c).await;
        }));
    }
    if let Some(se) = stderr {
        let b = buf.clone();
        let c = shared.clone();
        err_task = Some(tokio::spawn(async move {
            pump_reader(se, b, c).await;
        }));
    }
    if let Some(t) = out_task {
        let _ = t.await;
    }
    if let Some(t) = err_task {
        let _ = t.await;
    }
    if let Some((c, ticker)) = chunker {
        ticker.abort();
        c.lock().expect("lock").flush();
    }
}

async fn pump_reader<R: tokio::io::AsyncRead + Unpin>(
    mut r: R,
    buf: Arc<Mutex<String>>,
    chunker: Option<Arc<Mutex<Chunker>>>,
) {
    let mut tmp = [0u8; 4096];
    loop {
        match r.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                append_lossy(&buf, &tmp[..n]);
                if let Some(c) = &chunker {
                    c.lock().expect("lock").push(&tmp[..n]);
                }
            }
            Err(_) => break,
        }
    }
}

fn append_lossy(buf: &Arc<Mutex<String>>, bytes: &[u8]) {
    let s = String::from_utf8_lossy(bytes);
    buf.lock().expect("lock").push_str(&s);
}

fn spawn_shell(
    command: &str,
    workdir: &std::path::Path,
) -> std::io::Result<(Child, Option<KillTree>)> {
    use std::process::Stdio;
    let mut cmd = shell_command(command);
    cmd.current_dir(workdir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false)
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        // Child is group leader (pgid == pid). Cancel uses killpg, same idea as
        // Hermes/Codex on POSIX — not "kill the parent and hope".
        cmd.process_group(0);
    }
    let child = cmd.spawn()?;
    let tree = KillTree::from_child(&child);
    Ok((child, tree))
}

fn shell_command(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            command,
        ]);
        cmd
    }
    #[cfg(unix)]
    {
        let shell = if std::path::Path::new("/bin/bash").exists() {
            "/bin/bash"
        } else {
            "/bin/sh"
        };
        let mut cmd = tokio::process::Command::new(shell);
        cmd.args(["-c", command]);
        cmd
    }
    #[cfg(not(any(windows, unix)))]
    {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    }
}

/// OS adapter for "kill this command and every descendant".
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
            // SIGKILL the whole group. pgid 0/1 would hit our own session or init.
            if self.pgid > 1 {
                unsafe {
                    let _ = libc::killpg(self.pgid as i32, libc::SIGKILL);
                }
            }
        }
    }
}

#[cfg(windows)]
pub(crate) mod winjob {
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
    use std::time::{Duration, Instant};

    #[cfg(any(windows, unix))]
    #[tokio::test]
    async fn background_instant_exit_reaches_exited() {
        let table = ProcessTable::new();
        let sid = SessionId::new();
        let dir = tempfile::tempdir().unwrap();
        let updates = Arc::new(Mutex::new(Vec::<SessionUpdate>::new()));
        let captured = updates.clone();
        let live = LiveOpts {
            emit: Arc::new(move |update| captured.lock().unwrap().push(update)),
            tool_call_id: ToolCallId::new(),
            publish_tool_call: false,
            chunk_bytes: 64,
            flush_ms: 1,
        };
        let json = table.spawn_background(&sid, "echo ready", dir.path(), Some(live));
        let v: serde_json::Value = serde_json::from_str(&json).expect("json");
        let id = v["session_id"].as_str().expect("id").to_string();
        let initial = table.background_processes(&sid, 1024);
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].id, id);
        assert_eq!(initial[0].command, "echo ready");
        assert_eq!(initial[0].cwd, dir.path().display().to_string());
        assert!(updates.lock().unwrap().iter().any(|update| matches!(update,
            SessionUpdate::TerminalUpdate { command: Some(_), background: Some(true), .. }
        )));
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let poll: serde_json::Value =
                serde_json::from_str(&table.poll(&id, 1024)).expect("poll");
            if poll["status"] == "exited" {
                let finished = table.background_processes(&sid, 1024);
                assert_eq!(finished[0].status, "exited");
                assert_eq!(finished[0].exit_code, Some(0));
                assert!(finished[0].output.contains("ready"));
                break;
            }
            if Instant::now() > deadline {
                panic!("stuck as {poll}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
