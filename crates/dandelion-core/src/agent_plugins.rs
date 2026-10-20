//! Opt-in connectors for external Agent CLIs. The native harness stays built in.
//! This registry never executes a model or an Agent loop in a host process.
use crate::acp::{RequestPermissionOutcome, RequestPermissionRequest};
use crate::host::AgentHost;
use crate::ids::{SessionId, ToolCallId};
use crate::{Error, PermissionMode, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Native,
    ClaudeCode,
    OpenCode,
    Pi,
}

impl Engine {
    pub fn parse(id: &str) -> Result<Self> {
        match id {
            "native" => Ok(Self::Native),
            "claude_code" => Ok(Self::ClaudeCode),
            "opencode" => Ok(Self::OpenCode),
            "pi" => Ok(Self::Pi),
            _ => Err(Error::Invalid(format!("unknown Agent engine: {id}"))),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::ClaudeCode => "claude_code",
            Self::OpenCode => "opencode",
            Self::Pi => "pi",
        }
    }

    fn binary(self) -> &'static str {
        match self {
            Self::Native => "",
            Self::ClaudeCode => "claude",
            Self::OpenCode => "opencode",
            Self::Pi => "pi",
        }
    }

    fn package(self) -> &'static str {
        match self {
            Self::Native => "",
            Self::ClaudeCode => "@anthropic-ai/claude-code",
            Self::OpenCode => "opencode-ai",
            Self::Pi => "@earendil-works/pi-coding-agent",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PluginConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    executable: Option<PathBuf>,
    #[serde(default)]
    managed: bool,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    plugins: BTreeMap<String, PluginConfig>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub ready: bool,
    pub discovered: bool,
    pub executable: Option<String>,
    pub managed: bool,
    pub package: Option<String>,
    pub install_directory: Option<String>,
    pub source_url: Option<String>,
    pub cc_switch_executable: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallProgress {
    pub phase: &'static str,
    pub detail: String,
}

pub struct ExternalToolEvent {
    pub id: String,
    pub name: String,
    pub finished: bool,
    pub failed: bool,
    pub detail: Option<String>,
}

static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq)]
struct ClaudeProcessConfig {
    executable: PathBuf,
    model: Option<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    permission_mode: PermissionMode,
    /// Whitelisted directories passed as `--add-dir` (settings
    /// sandbox.allowRoots). Changing them invalidates the reused process via
    /// the config equality check — a changed trust boundary must respawn.
    extra_dirs: Vec<PathBuf>,
    /// Dandelion wiki-bridge config file for THIS session+project grant:
    /// Claude Code gets it as `--mcp-config`, Pi as `--extension`. Part of
    /// the process identity on purpose — a revoked or re-minted grant must
    /// never keep an old process pointed at a dead token.
    wiki_bridge_file: Option<PathBuf>,
}

type ProcessConfig = ClaudeProcessConfig;

struct PiProcess {
    config: ProcessConfig,
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    stderr_task: tokio::task::JoinHandle<String>,
    pid: Option<u32>,
    #[cfg(unix)]
    _group_guard: ProcessGroupGuard,
}

/// OpenCode 的每轮直启状态。历史上这里常驻一个 `serve` 进程、每轮
/// `run --attach` 连上去；真机证实（v1.18.35 与 v1.19.3 均复现）attach
/// 客户端只转发 step_start 就退出，文本与工具事件全被吞掉（服务端会话
/// API 里回答其实已生成）。现改为每轮直接 `run --session <id>`，run 自起
/// 自退，因此只需记住上轮配置（一致才敢续接）与 OpenCode 原生会话 ID。
struct OpenCodeTurnState {
    config: ProcessConfig,
    native_session: Option<String>,
}

/// A conversation owns one external Agent session. The transport varies by
/// engine, but lifecycle, configuration invalidation, and cleanup are shared.
#[derive(Default)]
pub struct AgentSessions {
    claude: ClaudeSessions,
    pi: Mutex<HashMap<String, Arc<tokio::sync::Mutex<PiProcess>>>>,
    opencode: Mutex<HashMap<String, Arc<tokio::sync::Mutex<OpenCodeTurnState>>>>,
}

impl AgentSessions {
    pub fn session_ids(&self, engine: Engine) -> Vec<String> {
        match engine {
            Engine::ClaudeCode => self.claude.processes.lock().ok()
                .map(|map| map.keys().cloned().collect()).unwrap_or_default(),
            Engine::Pi => self.pi.lock().ok()
                .map(|map| map.keys().cloned().collect()).unwrap_or_default(),
            Engine::OpenCode => self.opencode.lock().ok()
                .map(|map| map.keys().cloned().collect()).unwrap_or_default(),
            Engine::Native => Vec::new(),
        }
    }

    pub async fn shutdown_engine(&self, engine: Engine) {
        match engine {
            Engine::ClaudeCode => {
                if let Ok(mut allow) = self.claude.allow_all.lock() {
                    allow.clear();
                }
                let processes: Vec<_> = self.claude.processes.lock().ok()
                    .map(|mut map| map.drain().map(|(_, process)| process).collect())
                    .unwrap_or_default();
                for process in processes { process.lock().await.kill().await; }
            }
            Engine::Pi => {
                let processes: Vec<_> = self.pi.lock().ok()
                    .map(|mut map| map.drain().map(|(_, process)| process).collect())
                    .unwrap_or_default();
                for process in processes { process.lock().await.kill().await; }
            }
            // run 逐轮自起自退，无常驻进程可杀；清掉续接凭据即可。
            Engine::OpenCode => { if let Ok(mut map) = self.opencode.lock() { map.clear(); } }
            Engine::Native => {}
        }
    }

    pub fn close_engine(&self, engine: Engine) {
        let ids: Vec<String> = match engine {
            Engine::ClaudeCode => self.claude.processes.lock().ok()
                .map(|map| map.keys().cloned().collect()).unwrap_or_default(),
            Engine::Pi => self.pi.lock().ok()
                .map(|map| map.keys().cloned().collect()).unwrap_or_default(),
            Engine::OpenCode => self.opencode.lock().ok()
                .map(|map| map.keys().cloned().collect()).unwrap_or_default(),
            Engine::Native => Vec::new(),
        };
        for id in ids {
            match engine {
                Engine::ClaudeCode => self.claude.close(&id),
                Engine::Pi => {
                    if let Some(process) = self.pi.lock().ok().and_then(|mut map| map.remove(&id)) {
                        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                            runtime.spawn(async move { process.lock().await.kill().await; });
                        }
                    }
                }
                Engine::OpenCode => { if let Ok(mut map) = self.opencode.lock() { map.remove(&id); } }
                Engine::Native => {}
            }
        }
    }

    fn close_other_engines(&self, engine: Engine, session_id: &str) {
        if engine != Engine::ClaudeCode { self.claude.close(session_id); }
        if engine != Engine::Pi {
            if let Some(process) = self.pi.lock().ok().and_then(|mut map| map.remove(session_id)) {
                if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                    runtime.spawn(async move { process.lock().await.kill().await; });
                }
            }
        }
        if engine != Engine::OpenCode {
            if let Ok(mut map) = self.opencode.lock() { map.remove(session_id); }
        }
    }

    pub fn close(&self, session_id: &str) {
        self.close_other_engines(Engine::Native, session_id);
    }

    /// 宿主侧「本次会话不再询问」放行表（见 `ClaudeApproval::allow_all`）。
    /// 引擎无关的一份：runtime 构造 Claude 审批上下文时把借用交给
    /// `can_use_tool` 应答；原生 prompt 路径把同一份 Arc 交给本轮
    /// `ToolRegistry`。弹窗前先查表，用户选「不再询问」时写表；表与 CLI
    /// 进程解耦，进程重启、循环换轮 prompt 都不丢会话级放行。
    pub fn allow_all(&self) -> Arc<std::sync::Mutex<std::collections::HashSet<String>>> {
        self.claude.allow_all.clone()
    }

    /// `images` (data URLs) reach engines that accept structured visual input
    /// natively — today Claude Code, as Anthropic content blocks. Engines
    /// without a verified structured channel receive the images as files on
    /// disk referenced from the prompt text (built by the caller), so the
    /// turn never hard-fails on an attachment (C-rules: honest degradation).
    pub async fn execute(
        &self, engine: Engine, session_id: &str, executable: &Path, model: Option<&str>,
        cwd: &Path, prompt: &str, fresh_prompt: &str, env: &[(String, String)],
        permission_mode: PermissionMode,
        claude_extra_dirs: &[PathBuf], claude_approval: Option<&ClaudeApproval<'_>>,
        claude_images: &[String], wiki_bridge_file: Option<&Path>,
        cancel: CancellationToken,
        mut on_text: impl FnMut(&str), mut on_tool: impl FnMut(ExternalToolEvent),
    ) -> Result<String> {
        self.close_other_engines(engine, session_id);
        match engine {
            Engine::ClaudeCode => self.claude.execute(session_id, executable, model, cwd,
                prompt, fresh_prompt, env, permission_mode, claude_extra_dirs, wiki_bridge_file,
                claude_approval, claude_images, cancel, on_text, on_tool).await,
            Engine::Pi => self.execute_pi(session_id, executable, model, cwd, prompt,
                fresh_prompt, env, permission_mode, wiki_bridge_file, cancel, &mut on_text, &mut on_tool).await,
            Engine::OpenCode => self.execute_opencode(session_id, executable, model, cwd, prompt,
                fresh_prompt, env, permission_mode, cancel, &mut on_text, &mut on_tool).await,
            Engine::Native => Err(Error::Invalid("native engine has no CLI adapter".into())),
        }
    }
}

struct ClaudeProcess {
    config: ClaudeProcessConfig,
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    stderr_task: tokio::task::JoinHandle<String>,
    pid: Option<u32>,
    #[cfg(unix)]
    _group_guard: ProcessGroupGuard,
}

/// One Claude transport per Dandelion conversation. This only hosts the
/// official CLI; Claude Code still owns its own agent loop and tool protocol.
#[derive(Default)]
pub struct ClaudeSessions {
    processes: Mutex<HashMap<String, Arc<tokio::sync::Mutex<ClaudeProcess>>>>,
    /// 见 `AgentSessions::allow_all`：按会话记「本会话全量放行」。与进程无关 ——
    /// CLI 因 config 漂移 respawn 后、原生循环换轮 prompt 后，会话级放行依然有效。
    /// Arc：原生 prompt 的 `ToolRegistry` 经 `allow_all()` 拿同一份查表/写表。
    allow_all: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl ClaudeSessions {
    pub fn close(&self, session_id: &str) {
        if let Ok(mut allow) = self.allow_all.lock() {
            allow.remove(session_id);
        }
        let process = self.processes.lock().ok().and_then(|mut map| map.remove(session_id));
        if let Some(process) = process {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move { process.lock().await.kill().await; });
            }
            // Outside a Tokio runtime, Child::kill_on_drop still stops the CLI.
        }
    }

    fn remove_if_same(&self, session_id: &str, process: &Arc<tokio::sync::Mutex<ClaudeProcess>>) {
        if let Ok(mut map) = self.processes.lock() {
            if map.get(session_id).is_some_and(|current| Arc::ptr_eq(current, process)) {
                map.remove(session_id);
            }
        }
    }
}

/// Windows：凡是被我们静默托管（stdio 全部走管道、由 harness 消化输出）的
/// 子进程都必须设置 CREATE_NO_WINDOW。桌面宿主是 GUI 进程（无控制台），而
/// 引擎 CLI 都是控制台子系统程序（claude.exe PE Subsystem=3；npm.cmd 经
/// cmd.exe；taskkill 同），不设置标志时 CreateProcess 会为每个子进程新分配
/// 一个黑色控制台窗口 —— 这就是安装版使用 Claude Code 时弹出 "claude"
/// 窗口的原因，Pi / OpenCode / 安装与清理辅助进程同理。
/// 选 CREATE_NO_WINDOW 而非 DETACHED_PROCESS：后者让孩子完全无控制台，它
/// 再派生的控制台孙进程（git/rg/sh 等）会各自闪现新窗口；前者让孩子持有一个
/// 无窗口的控制台，孙进程继承之，整棵进程树无弹窗。与 Hermes
/// `_subprocess_compat.windows_detach_flags`、Codex windows-sandbox 同一
/// 策略，也与本仓库 jobs.rs / sidecar.rs 的既有写法一致。
/// 注意：本函数只用于静默托管的子进程；用户主动打开的窗口不走这里。
pub(crate) fn hide_console_windows(command: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// std::process::Command 版本的 [`hide_console_windows`]。
pub(crate) fn hide_console_windows_std(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

fn external_command(config: &ProcessConfig, engine: Engine) -> Result<tokio::process::Command> {
    let mut command = tokio::process::Command::new(&config.executable);
    command.env_clear();
    for key in ["PATH", "HOME", "USER", "LOGNAME", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL",
        "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "SHELL", "USERPROFILE",
        "APPDATA", "LOCALAPPDATA", "SystemRoot", "ComSpec", "HTTP_PROXY", "HTTPS_PROXY",
        "NO_PROXY", "http_proxy", "https_proxy", "no_proxy", "OPENAI_API_KEY",
        "OPENAI_BASE_URL", "ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL"] {
        if let Some(value) = std::env::var_os(key) { command.env(key, value); }
    }
    command.envs(config.env.iter().map(|(key, value)| (key, value)));
    if engine == Engine::OpenCode && config.permission_mode == PermissionMode::Deny {
        command.env("OPENCODE_CONFIG_CONTENT", restricted_opencode_config(&config.env)?);
    }
    command.current_dir(&config.cwd).stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    hide_console_windows(&mut command);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    Ok(command)
}

fn stderr_tail(mut stderr: tokio::process::ChildStderr) -> tokio::task::JoinHandle<String> {
    tokio::spawn(async move {
        let mut tail = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(count) = stderr.read(&mut chunk).await {
            if count == 0 { break; }
            tail.extend_from_slice(&chunk[..count]);
            if tail.len() > 32 * 1024 { tail.drain(..tail.len() - 32 * 1024); }
        }
        String::from_utf8_lossy(&tail).into_owned()
    })
}

impl PiProcess {
    fn spawn(config: ProcessConfig) -> Result<Self> {
        let mut command = external_command(&config, Engine::Pi)?;
        command.args(["--mode", "rpc", "--no-session", "--no-context-files"]);
        // Thin Dandelion wiki extension (registers the same tool semantics
        // and calls back into the host bridge). Absent = no wiki tools.
        if let Some(file) = &config.wiki_bridge_file {
            command.args(["--extension", &file.to_string_lossy()]);
        }
        if config.permission_mode == PermissionMode::Deny { command.arg("--no-tools"); }
        if let Some(model) = &config.model { command.args(["--model", model]); }
        let mut child = command.spawn().map_err(|err| Error::NotConfigured(format!("Pi 启动失败：{err}")))?;
        let pid = child.id();
        let stdin = child.stdin.take().ok_or_else(|| Error::Provider("Pi stdin unavailable".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Provider("Pi stdout unavailable".into()))?;
        let stderr_task = stderr_tail(child.stderr.take().ok_or_else(|| Error::Provider("Pi stderr unavailable".into()))?);
        Ok(Self { config, child, stdin, lines: BufReader::new(stdout).lines(), stderr_task, pid,
            #[cfg(unix)] _group_guard: ProcessGroupGuard(pid) })
    }

    async fn kill(&mut self) {
        kill_process_tree(self.pid, &mut self.child).await;
        self.stderr_task.abort();
    }

    async fn turn(&mut self, prompt: &str, cancel: &CancellationToken,
        on_text: &mut impl FnMut(&str), on_tool: &mut impl FnMut(ExternalToolEvent)) -> Result<String> {
        // A conversation serializes turns, so this process has one outstanding
        // RPC command at a time. A stable ID also makes stray responses easy
        // to distinguish from extension events.
        let request_id = "dandelion-prompt";
        let mut input = serde_json::to_vec(&serde_json::json!({
            "id": request_id, "type": "prompt", "message": prompt
        }))?;
        input.push(b'\n');
        self.stdin.write_all(&input).await?;
        self.stdin.flush().await?;
        let mut answer = String::new();
        let mut accepted = false;
        let mut settled = false;
        let mut provider_error = None;
        loop {
            let line = tokio::select! {
                _ = cancel.cancelled() => return Err(Error::Cancelled),
                line = self.lines.next_line() => line?,
            };
            let Some(line) = line else {
                return Err(Error::Provider("Pi RPC 会话意外退出".into()));
            };
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else { continue; };
            if event["type"] == "response" && event["id"] == request_id {
                if event["success"] != true {
                    return Err(Error::Provider(format!("Pi 拒绝本轮输入：{}", event["error"])));
                }
                accepted = true;
                if event["data"]["disposition"] == "handled" { return Ok(answer); }
            }
            if event["type"] == "message_end" && event["message"]["role"] == "assistant" {
                if let Some(reason) = event["message"]["stopReason"].as_str() {
                    if matches!(reason, "error" | "aborted") {
                        provider_error = Some(event["message"]["errorMessage"].as_str()
                            .unwrap_or(reason).to_owned());
                    }
                }
            }
            if let Some(tool) = event_tool(Engine::Pi, &event) { on_tool(tool); }
            if let Some(delta) = event_text_delta(Engine::Pi, &event, true) {
                answer.push_str(delta);
                on_text(delta);
            }
            // Current Pi RPC ends a prompt with agent_end; older releases used
            // agent_settled. Accept both so a completed reply never hangs.
            if event["type"] == "agent_end" || event["type"] == "agent_settled" {
                if event["aborted"] == true { return Err(Error::Cancelled); }
                settled = true;
            }
            if settled && accepted {
                if let Some(error) = provider_error { return Err(Error::Provider(format!("Pi 执行失败：{error}"))); }
                if answer.trim().is_empty() { return Err(Error::Provider("Pi 未返回可见回答".into())); }
                return Ok(answer);
            }
        }
    }
}

impl AgentSessions {
    async fn execute_pi(&self, id: &str, executable: &Path, model: Option<&str>, cwd: &Path,
        prompt: &str, fresh_prompt: &str, env: &[(String, String)], permission_mode: PermissionMode,
        wiki_bridge_file: Option<&Path>,
        cancel: CancellationToken, on_text: &mut impl FnMut(&str),
        on_tool: &mut impl FnMut(ExternalToolEvent)) -> Result<String> {
        let config = ProcessConfig { executable: executable.to_path_buf(), model: model.map(str::to_owned),
            cwd: cwd.to_path_buf(), env: env.to_vec(), permission_mode, extra_dirs: Vec::new(),
            wiki_bridge_file: wiki_bridge_file.map(Path::to_path_buf) };
        let existing = self.pi.lock().map_err(|_| Error::Provider("Pi 会话锁已损坏".into()))?.get(id).cloned();
        if let Some(process) = &existing {
            let mut guard = process.lock().await;
            if guard.config == config && guard.child.try_wait()?.is_none() {
                let result = guard.turn(prompt, &cancel, on_text, on_tool).await;
                if result.is_err() { guard.kill().await; self.pi.lock().unwrap().remove(id); }
                return result;
            }
            guard.kill().await;
            self.pi.lock().unwrap().remove(id);
        }
        if cancel.is_cancelled() { return Err(Error::Cancelled); }
        let process = Arc::new(tokio::sync::Mutex::new(PiProcess::spawn(config)?));
        self.pi.lock().map_err(|_| Error::Provider("Pi 会话锁已损坏".into()))?.insert(id.to_owned(), process.clone());
        let mut guard = process.lock().await;
        let result = guard.turn(fresh_prompt, &cancel, on_text, on_tool).await;
        if result.is_err() { guard.kill().await; self.pi.lock().unwrap().remove(id); }
        result
    }
}

impl AgentSessions {
    /// 每轮直启一次 `opencode run`（真机证据见 `OpenCodeTurnState` 注释：
    /// serve+attach 形态会把回答整个吞掉）。会话续接靠回传 OpenCode 自己的
    /// `--session <原生 ID>`——跨进程续接已实测正确（新一轮回想起上一轮
    /// 内容）。与 Pi 同构：配置一致才续接；配置变了或本轮出错就删条目，
    /// 下一轮自动回到 fresh_prompt 开新会话（开新/续接由调用方通过
    /// fresh_prompt/prompt 区分）。
    async fn execute_opencode(&self, id: &str, executable: &Path, model: Option<&str>, cwd: &Path,
        prompt: &str, fresh_prompt: &str, env: &[(String, String)], permission_mode: PermissionMode,
        cancel: CancellationToken, on_text: &mut impl FnMut(&str),
        on_tool: &mut impl FnMut(ExternalToolEvent)) -> Result<String> {
        // OpenCode gets the bridge through OPENCODE_CONFIG_CONTENT (merged
        // into its `mcp` key by the caller), so no extra file flag here.
        let config = ProcessConfig { executable: executable.to_path_buf(), model: model.map(str::to_owned),
            cwd: cwd.to_path_buf(), env: env.to_vec(), permission_mode, extra_dirs: Vec::new(),
            wiki_bridge_file: None };
        let existing = self.opencode.lock().map_err(|_| Error::Provider("OpenCode 会话锁已损坏".into()))?.get(id).cloned();
        if let Some(state) = &existing {
            // 这把锁仍有用：串行化同一会话的两轮，防止同一原生会话被并发续接两次。
            let mut guard = state.lock().await;
            if guard.config == config && guard.native_session.is_some() {
                let session = guard.native_session.clone();
                let mut observed = None;
                let result = execute_with_connection(Engine::OpenCode, &config.executable,
                    config.model.as_deref(), &config.cwd, prompt, &config.env,
                    config.permission_mode, cancel,
                    Some(OpenCodeRun { session: session.as_deref(), observed_session: &mut observed }),
                    on_text, on_tool).await;
                let answer = match result {
                    Ok(answer) => answer,
                    Err(err) => { drop(guard); self.opencode.lock().unwrap().remove(id); return Err(err); }
                };
                // 本轮没观测到 sessionID 就保留旧值（与旧实现一致）。
                if observed.is_some() { guard.native_session = observed; }
                return Ok(answer);
            }
            drop(guard);
            self.opencode.lock().unwrap().remove(id);
        }
        if cancel.is_cancelled() { return Err(Error::Cancelled); }
        let mut observed = None;
        let answer = execute_with_connection(Engine::OpenCode, &config.executable,
            config.model.as_deref(), &config.cwd, fresh_prompt, &config.env,
            config.permission_mode, cancel,
            Some(OpenCodeRun { session: None, observed_session: &mut observed }),
            on_text, on_tool).await?;
        // 首轮成功却没拿到会话 ID：回答已交付，但没有可续接的凭据，
        // 如实报错且不落条目（下一轮仍开新会话，不做半吊子续接）。
        let native_session = match observed {
            Some(session) => session,
            None => return Err(Error::Provider("OpenCode 未提供会话 ID，无法安全续接".into())),
        };
        let state = Arc::new(tokio::sync::Mutex::new(OpenCodeTurnState { config, native_session: Some(native_session) }));
        self.opencode.lock().map_err(|_| Error::Provider("OpenCode 会话锁已损坏".into()))?.insert(id.to_owned(), state);
        Ok(answer)
    }
}

fn lock_registry() -> Result<std::sync::MutexGuard<'static, ()>> {
    REGISTRY_LOCK.lock().map_err(|_| Error::Settings("Agent 插件状态锁已损坏".into()))
}

fn registry_path(data_dir: &Path) -> PathBuf {
    data_dir.join("agent-plugins.json")
}

fn read_registry(data_dir: &Path) -> Result<Registry> {
    match std::fs::read(registry_path(data_dir)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(err) => Err(err.into()),
    }
}

fn write_registry(data_dir: &Path, registry: &Registry) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let target = registry_path(data_dir);
    let temp = data_dir.join(format!(".agent-plugins-{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temp, serde_json::to_vec_pretty(registry)?)?;
    #[cfg(windows)]
    if target.exists() { std::fs::remove_file(&target)?; }
    std::fs::rename(temp, target)?;
    Ok(())
}

fn discovery_dirs(workspaces: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    for workspace in workspaces {
        for relative in ["node_modules/.bin", "tools/node_modules/.bin", ".benchmarks/tools/node_modules/.bin"] {
            dirs.push(workspace.join(relative));
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for path in [".local/bin", ".claude/local", ".bun/bin", ".npm-global/bin"] {
            dirs.push(home.join(path));
        }
        let versions = home.join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(versions) {
            for entry in entries.flatten().take(40) {
                dirs.push(entry.path().join("bin"));
            }
        }
    }
    dirs.extend([PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]);
    let mut seen = HashSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()));
    dirs
}

fn discover_executable(name: &str, workspaces: &[PathBuf]) -> Option<PathBuf> {
    #[cfg(windows)]
    let names = [format!("{name}.exe"), format!("{name}.cmd"), format!("{name}.bat")];
    #[cfg(not(windows))]
    let names = [name.to_string()];
    for dir in discovery_dirs(workspaces) {
        for candidate in &names {
            let path = dir.join(candidate);
            if path.is_file() { return Some(path); }
        }
    }
    None
}

fn resolved_executable(engine: Engine, config: &PluginConfig, workspaces: &[PathBuf]) -> Option<PathBuf> {
    let found = config
        .executable
        .as_ref()
        .filter(|path| path.is_file())
        .cloned()
        .or_else(|| discover_executable(engine.binary(), workspaces));
    // 展示用尽力替换：解析得到原生 exe 就展示原生路径，解析不到仍展示
    // 原路径（就绪状态不因此翻转，运行时的硬性检查在 executable()）。
    #[cfg(windows)]
    if engine == Engine::OpenCode {
        if let Some(path) = &found {
            if is_batch_launcher(path) {
                if let Some(native) = native_batch_target(path) {
                    return Some(native);
                }
            }
        }
    }
    found
}

#[cfg(windows)]
fn is_batch_launcher(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
}

/// Windows：npm 生成的 `.cmd` 垫片实际由 cmd.exe 承载，cmd.exe 会把包含
/// 换行、引号、前导 `-` 等特殊字符的位置参数以
/// “batch file arguments are invalid” 直接拒掉，进程在 spawn 阶段就失败；
/// 而 `opencode run` 恰恰必须把用户消息作为位置参数传入。因此 OpenCode
/// 一旦落在批处理垫片上（托管安装或用户手动指定皆然），必须改为直接
/// 启动垫片指向的原生 exe，绝不退回“用批处理传消息”。
#[cfg(windows)]
fn opencode_native_executable(path: PathBuf) -> Result<PathBuf> {
    if !is_batch_launcher(&path) {
        return Ok(path);
    }
    native_batch_target(&path).ok_or_else(|| Error::Provider(format!(
        "OpenCode 程序启动失败：批处理启动器 {} 之后未找到可安全启动的原生 opencode.exe。\
         npm 安装的 opencode-ai 自带原生可执行文件；若这是你手动指定的程序路径，请改为直接指向 opencode.exe。",
        path.display()
    )))
}

/// npm/yarn 风格批处理垫片真正执行的原生 exe：先按垫片文本里写的
/// `.exe` 路径（`%dp0%`/`%~dp0%` 展开为垫片所在目录）精确跟随，再回退到
/// opencode-ai 官方包的常见布局。
#[cfg(windows)]
fn native_batch_target(shim: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(shim).ok()?;
    let dir = shim.parent()?;
    let lower = text.to_lowercase();
    let mut cursor = 0usize;
    while let Some(found) = lower[cursor..].find(".exe") {
        let end = cursor + found + 4;
        cursor = end;
        let start = lower[..end].rfind(['"', '\'', ' ', '\t']).map_or(0, |i| i + 1);
        let token = text[start..end].trim();
        if token.is_empty() {
            continue;
        }
        let expanded = expand_batch_vars(token, dir);
        if expanded.contains('%') || expanded.contains('$') {
            continue;
        }
        let candidate = PathBuf::from(&expanded);
        let candidate = if candidate.is_absolute() { candidate } else { dir.join(candidate) };
        if candidate.is_file() {
            return Some(native_path(candidate));
        }
    }
    let stem = shim.file_stem()?.to_string_lossy().into_owned();
    let node_modules = dir.parent()?;
    for candidate in [
        dir.join(format!("{stem}.exe")),
        node_modules.join(format!("{stem}-ai")).join("bin").join(format!("{stem}.exe")),
        node_modules.join(&stem).join("bin").join(format!("{stem}.exe")),
        node_modules.join(format!("{stem}-windows-x64")).join("bin").join(format!("{stem}.exe")),
    ] {
        if candidate.is_file() {
            return Some(native_path(candidate));
        }
    }
    None
}

/// 塌掉 `%dp0%\..` 这类未归一化的中间组件，展示与报错都拿到干净路径。
#[cfg(windows)]
fn native_path(candidate: PathBuf) -> PathBuf {
    dunce::canonicalize(&candidate).unwrap_or(candidate)
}

#[cfg(windows)]
fn expand_batch_vars(token: &str, dir: &Path) -> String {
    let dir = dir.to_string_lossy().into_owned();
    let mut out = token.to_string();
    // `%~dp0` 展开自带尾随反斜杠，`%dp0%`（垫片自设变量）没有。
    for (var, value) in [
        ("%~dp0%", &format!("{dir}\\")),
        ("%~dp0", &format!("{dir}\\")),
        ("%dp0%", &dir),
        ("%dp0", &dir),
    ] {
        out = replace_case_insensitive(&out, var, value);
    }
    out
}

#[cfg(windows)]
fn replace_case_insensitive(hay: &str, needle: &str, with: &str) -> String {
    let lower = hay.to_lowercase();
    let needle = needle.to_lowercase();
    let mut out = String::new();
    let mut i = 0usize;
    while let Some(pos) = lower[i..].find(&needle) {
        out.push_str(&hay[i..i + pos]);
        out.push_str(with);
        i += pos + needle.len();
    }
    out.push_str(&hay[i..]);
    out
}

pub fn list(data_dir: &Path, workspaces: &[PathBuf]) -> Result<Vec<PluginInfo>> {
    let _guard = lock_registry()?;
    let registry = read_registry(data_dir)?;
    let mut result = vec![PluginInfo {
        id: "native".into(), name: "蒲公英原生".into(), installed: true,
        ready: true, discovered: false, executable: None, managed: false, package: None,
        install_directory: None, source_url: None, cc_switch_executable: None, model: None,
    }];
    for (engine, name) in [
        (Engine::ClaudeCode, "Claude Code"),
        (Engine::OpenCode, "OpenCode"),
        (Engine::Pi, "Pi"),
    ] {
        let explicit = registry.plugins.get(engine.id());
        let config = explicit.cloned().unwrap_or_default();
        let enabled = explicit.map(|item| item.enabled).unwrap_or(true);
        let executable = resolved_executable(engine, &config, workspaces);
        let cc_switch_executable = if engine == Engine::ClaudeCode && config.managed {
            let path = crate::cc_switch::binary(data_dir);
            path.is_file().then(|| path.to_string_lossy().into_owned())
        } else { None };
        let ready = enabled && executable.is_some() &&
            (engine != Engine::ClaudeCode || !config.managed || cc_switch_executable.is_some());
        result.push(PluginInfo {
            id: engine.id().into(), name: name.into(), installed: enabled && executable.is_some(),
            ready, discovered: explicit.is_none() && executable.is_some(),
            executable: executable.map(|path| path.to_string_lossy().into_owned()),
            managed: config.managed, package: Some(engine.package().into()),
            install_directory: Some(data_dir.join("agent-engines").join(engine.id()).to_string_lossy().into_owned()),
            source_url: Some(match engine {
                Engine::ClaudeCode => "https://www.npmjs.com/package/@anthropic-ai/claude-code",
                Engine::OpenCode => "https://www.npmjs.com/package/opencode-ai",
                Engine::Pi => "https://www.npmjs.com/package/@earendil-works/pi-coding-agent",
                Engine::Native => unreachable!(),
            }.into()),
            cc_switch_executable,
            model: config.model,
        });
    }
    Ok(result)
}

pub fn executable(data_dir: &Path, engine: Engine, workspaces: &[PathBuf]) -> Result<PathBuf> {
    let _guard = lock_registry()?;
    if engine == Engine::Native {
        return Err(Error::Invalid("native Agent is not an external plugin".into()));
    }
    let registry = read_registry(data_dir)?;
    let explicit = registry.plugins.get(engine.id());
    let config = explicit.cloned().unwrap_or_default();
    if explicit.is_some_and(|item| !item.enabled) {
        return Err(Error::NotConfigured(format!("{} plugin not installed", engine.id())));
    }
    let path = resolved_executable(engine, &config, workspaces)
        .ok_or_else(|| Error::NotConfigured(format!("{} executable not found", engine.id())))?;
    // 运行时硬闸门（resolved_executable 只做尽力替换）：OpenCode 绝不能以
    // 批处理垫片身份进入 spawn —— cmd.exe 会拒收含特殊字符的位置参数消息。
    // 解析不到原生 exe 时给出明确错误，而不是带着垫片去撞
    // “batch file arguments are invalid”。
    #[cfg(windows)]
    if engine == Engine::OpenCode {
        return opencode_native_executable(path);
    }
    Ok(path)
}

pub fn is_managed(data_dir: &Path, engine: Engine) -> Result<bool> {
    let _guard = lock_registry()?;
    Ok(read_registry(data_dir)?.plugins.get(engine.id()).is_some_and(|entry| entry.managed && entry.enabled))
}

/// Connect an already installed official CLI without altering its files or auth.
pub fn connect_existing(data_dir: &Path, id: &str, path: Option<PathBuf>, workspaces: &[PathBuf]) -> Result<()> {
    let _guard = lock_registry()?;
    let engine = Engine::parse(id)?;
    if engine == Engine::Native { return Err(Error::Invalid("native Agent is built in".into())); }
    if path.as_ref().is_some_and(|value| !value.is_file()) {
        return Err(Error::Invalid("selected executable does not exist".into()));
    }
    let mut registry = read_registry(data_dir)?;
    let old = registry.plugins.get(id);
    let model = old.and_then(|item| item.model.clone());
    let managed = old.is_some_and(|item| item.managed) && path.is_none();
    let executable = path
        .or_else(|| old.and_then(|item| item.executable.as_ref()).filter(|path| path.is_file()).cloned())
        .or_else(|| discover_executable(engine.binary(), workspaces))
        .ok_or_else(|| Error::NotConfigured(format!("{} 未在常见安装目录或当前工作区找到", engine.binary())))?;
    registry.plugins.insert(id.into(), PluginConfig { enabled: true, executable: Some(executable), managed, model });
    write_registry(data_dir, &registry)
}

/// Install an official package only after the customer explicitly clicks Install.
pub async fn install_managed(data_dir: &Path, id: &str) -> Result<()> {
    install_managed_with_progress(data_dir, id, CancellationToken::new(), |_| {}).await
}

fn remove_partial_release(prefix: &Path) {
    if !prefix.exists() { return; }
    if std::fs::symlink_metadata(prefix).is_ok_and(|meta| meta.file_type().is_symlink()) { return; }
    let parent = prefix.parent().and_then(|path| std::fs::canonicalize(path).ok());
    let target = std::fs::canonicalize(prefix).ok();
    if let (Some(parent), Some(target)) = (parent, target) {
        if target.parent() == Some(parent.as_path()) && target != parent {
            let _ = std::fs::remove_dir_all(prefix);
        }
    }
}

pub async fn install_managed_with_progress(
    data_dir: &Path, id: &str, cancel: CancellationToken,
    mut on_progress: impl FnMut(InstallProgress),
) -> Result<()> {
    let engine = Engine::parse(id)?;
    if engine == Engine::Native { return Err(Error::Invalid("native Agent is built in".into())); }
    if cancel.is_cancelled() { return Err(Error::Cancelled); }
    on_progress(InstallProgress { phase: "preparing", detail: "正在准备安装目录".into() });
    // Versioned installation leaves the executable of an active run intact.
    let prefix = data_dir.join("agent-engines").join(id).join("releases")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&prefix)?;
    #[cfg(windows)]
    let npm = "npm.cmd";
    #[cfg(not(windows))]
    let npm = "npm";
    let mut install = tokio::process::Command::new(npm);
    install.args(["install", "--prefix"]).arg(&prefix)
        .args(["--no-audit", "--no-fund", "--save-exact", "--loglevel", "info"]);
    // Claude Code's npm distribution links its native binary in postinstall.
    // Pi explicitly supports installing without package scripts.
    if engine == Engine::Pi { install.arg("--ignore-scripts"); }
    install.arg(engine.package()).kill_on_drop(true)
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    hide_console_windows(&mut install);
    #[cfg(unix)] {
        use std::os::unix::process::CommandExt;
        install.as_std_mut().process_group(0);
    }
    let mut child = install.spawn().map_err(|err| {
        remove_partial_release(&prefix);
        Error::NotConfigured(format!("无法运行 npm 安装 {}：{err}。可先安装 Node.js，或使用已有 Agent 程序。", id))
    })?;
    let pid = child.id();
    #[cfg(unix)]
    let _group_guard = ProcessGroupGuard(pid);
    let mut stdout = BufReader::new(child.stdout.take().ok_or_else(|| Error::Provider("npm stdout unavailable".into()))?).lines();
    let mut stderr = BufReader::new(child.stderr.take().ok_or_else(|| Error::Provider("npm stderr unavailable".into()))?).lines();
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut status = None;
    let mut diagnostic = String::new();
    let mut activity = 0usize;
    on_progress(InstallProgress { phase: "downloading", detail: format!("正在获取 {}", engine.package()) });
    while status.is_none() || stdout_open || stderr_open {
        tokio::select! {
            _ = cancel.cancelled() => {
                kill_process_tree(pid, &mut child).await;
                remove_partial_release(&prefix);
                return Err(Error::Cancelled);
            }
            result = stdout.next_line(), if stdout_open => {
                match result? {
                    Some(line) => {
                        activity += 1;
                        if line.contains("added ") || line.contains("changed ") {
                            on_progress(InstallProgress { phase: "installing", detail: "正在写入程序文件".into() });
                        } else if activity % 10 == 0 {
                            on_progress(InstallProgress { phase: "downloading", detail: format!("已处理 {activity} 条安装信息") });
                        }
                    }
                    None => stdout_open = false,
                }
            }
            result = stderr.next_line(), if stderr_open => {
                match result? {
                    Some(line) => {
                        if diagnostic.len() > 16_000 { diagnostic.drain(..diagnostic.len() - 8_000); }
                        diagnostic.push_str(&line);
                        diagnostic.push('\n');
                        activity += 1;
                        if line.contains("http fetch") || line.contains("tarball") {
                            if activity % 5 == 0 {
                                on_progress(InstallProgress { phase: "downloading", detail: format!("正在下载依赖 · 已处理 {activity} 条信息") });
                            }
                        } else if line.contains("reify") || line.contains("postinstall") {
                            on_progress(InstallProgress { phase: "installing", detail: "正在安装和配置依赖".into() });
                        }
                    }
                    None => stderr_open = false,
                }
            }
            result = child.wait(), if status.is_none() => status = Some(result?),
        }
    }
    if !status.expect("npm status must be collected").success() {
        remove_partial_release(&prefix);
        return Err(Error::Invalid(format!("安装 {} 失败：{}", id, diagnostic.trim())));
    }
    on_progress(InstallProgress { phase: "verifying", detail: "正在校验启动程序".into() });
    let binary = prefix.join("node_modules").join(".bin").join(engine.binary());
    #[cfg(windows)]
    let binary = binary.with_extension("cmd");
    if !binary.is_file() {
        remove_partial_release(&prefix);
        return Err(Error::Invalid(format!("{} 安装完成，但找不到启动程序", id)));
    }
    let _guard = lock_registry()?;
    let mut registry = read_registry(data_dir)?;
    let model = registry.plugins.get(id).and_then(|old| old.model.clone());
    registry.plugins.insert(id.into(), PluginConfig {
        enabled: true, executable: Some(binary), managed: true, model,
    });
    write_registry(data_dir, &registry)?;
    on_progress(InstallProgress { phase: "completed", detail: "安装完成".into() });
    Ok(())
}

pub fn uninstall(data_dir: &Path, id: &str) -> Result<()> {
    let _guard = lock_registry()?;
    let engine = Engine::parse(id)?;
    if engine == Engine::Native { return Err(Error::Invalid("native Agent cannot be removed".into())); }
    let mut registry = read_registry(data_dir)?;
    // An explicit disabled entry prevents a discovered CLI from reconnecting
    // immediately after the user disconnects it.
    registry.plugins.entry(id.into()).or_default().enabled = false;
    // Leave managed binaries on disk until no running task can be using them.
    write_registry(data_dir, &registry)
}

/// Disconnect an external CLI. Managed releases are removed only after the
/// runtime has stopped every session that might be using their executable.
pub fn remove(data_dir: &Path, id: &str) -> Result<()> {
    let _guard = lock_registry()?;
    let engine = Engine::parse(id)?;
    if engine == Engine::Native { return Err(Error::Invalid("native Agent cannot be removed".into())); }
    let mut registry = read_registry(data_dir)?;
    let entry = registry.plugins.entry(id.into()).or_default();
    if entry.managed {
        let base = data_dir.join("agent-engines");
        let target = base.join(id);
        if target.exists() {
            if std::fs::symlink_metadata(&target)?.file_type().is_symlink() {
                return Err(Error::Invalid("Agent 安装目录不能是符号链接".into()));
            }
            // Resolve both paths before a recursive removal. A user-edited
            // registry or symlink must never redirect deletion outside data_dir.
            let base = std::fs::canonicalize(base)?;
            let resolved = std::fs::canonicalize(&target)?;
            if resolved.parent() != Some(base.as_path()) || resolved == base {
                return Err(Error::Invalid("Agent 安装目录不在蒲公英数据目录内".into()));
            }
            std::fs::remove_dir_all(&target)?;
        }
    }
    entry.enabled = false;
    entry.executable = None;
    entry.managed = false;
    write_registry(data_dir, &registry)
}

pub fn set_model(data_dir: &Path, id: &str, model: Option<String>) -> Result<()> {
    let _guard = lock_registry()?;
    let engine = Engine::parse(id)?;
    if engine == Engine::Native { return Err(Error::Invalid("native model is configured in Settings".into())); }
    let mut registry = read_registry(data_dir)?;
    let plugin = registry.plugins.entry(id.into()).or_insert_with(|| PluginConfig {
        enabled: true, ..PluginConfig::default()
    });
    if !plugin.enabled { return Err(Error::NotConfigured(format!("{} plugin not installed", id))); }
    plugin.model = model.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    write_registry(data_dir, &registry)
}

pub fn configured_model(data_dir: &Path, engine: Engine) -> Result<Option<String>> {
    let _guard = lock_registry()?;
    Ok(read_registry(data_dir)?.plugins.get(engine.id()).and_then(|p| p.model.clone()))
}

/// Stream structured output from an official CLI. The CLI owns its own Agent
/// loop; Dandelion only hosts it and translates its events into the common bus.
pub async fn execute(
    engine: Engine,
    executable: &Path,
    model: Option<&str>,
    cwd: &Path,
    prompt: &str,
    env: &[(String, String)],
    permission_mode: PermissionMode,
    cancel: CancellationToken,
    mut on_text: impl FnMut(&str),
    mut on_tool: impl FnMut(ExternalToolEvent),
) -> Result<String> {
    execute_with_connection(engine, executable, model, cwd, prompt, env, permission_mode,
        cancel, None, &mut on_text, &mut on_tool).await
}

/// 单轮 OpenCode 直启 `run` 的会话参数：`session` 是上轮观测到的原生
/// 会话 ID（None = 开新会话），`observed_session` 收本轮事件里的
/// sessionID，成功轮由 `execute_opencode` 回写状态供下轮续接。
struct OpenCodeRun<'a> {
    session: Option<&'a str>,
    observed_session: &'a mut Option<String>,
}

async fn execute_with_connection(
    engine: Engine, executable: &Path, model: Option<&str>, cwd: &Path,
    prompt: &str, env: &[(String, String)], permission_mode: PermissionMode,
    cancel: CancellationToken, mut opencode_run: Option<OpenCodeRun<'_>>,
    mut on_text: impl FnMut(&str), mut on_tool: impl FnMut(ExternalToolEvent),
) -> Result<String> {
    let mut command = tokio::process::Command::new(executable);
    command.env_clear();
    for key in [
        "PATH", "HOME", "USER", "LOGNAME", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL",
        "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "SHELL",
        "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy",
        "USERPROFILE", "APPDATA", "LOCALAPPDATA", "SystemRoot", "ComSpec",
    ] {
        if let Some(value) = std::env::var_os(key) { command.env(key, value); }
    }
    let provider_keys: &[&str] = match engine {
        Engine::ClaudeCode => &[
            "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL",
            "ANTHROPIC_MODEL", "CLAUDE_CODE_OAUTH_TOKEN",
        ],
        Engine::Pi | Engine::OpenCode => &[
            "OPENAI_API_KEY", "OPENAI_BASE_URL", "ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL",
        ],
        Engine::Native => &[],
    };
    for key in provider_keys {
        if let Some(value) = std::env::var_os(key) { command.env(key, value); }
    }
    command.current_dir(cwd).stdin(if engine == Engine::OpenCode {
        std::process::Stdio::null()
    } else {
        std::process::Stdio::piped()
    })
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    hide_console_windows(&mut command);
    command.envs(env.iter().map(|(key, value)| (key, value)));
    if engine == Engine::OpenCode && permission_mode == PermissionMode::Deny {
        command.env("OPENCODE_CONFIG_CONTENT", restricted_opencode_config(env)?);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    match engine {
        Engine::Native => return Err(Error::Invalid("native engine has no CLI adapter".into())),
        Engine::Pi => {
            command.args(["--mode", "json", "--print", "--no-session", "--no-context-files"]);
            if permission_mode == PermissionMode::Deny { command.arg("--no-tools"); }
            if let Some(model) = model { command.args(["--model", model]); }
        }
        Engine::OpenCode => {
            command.args(["run", "--format", "json"]);
            if let Some(run) = &opencode_run {
                if let Some(session) = run.session { command.args(["--session", session]); }
            }
            if permission_mode == PermissionMode::Auto { command.arg("--auto"); }
            if permission_mode == PermissionMode::Deny { command.args(["--agent", "plan", "--pure"]); }
            // `--model` 必须拼在 `--` 之前：`--` 之后全部成为消息正文。
            // 真机反证——曾在 `--` 之后追加时，服务器收到的用户消息是
            // 「你好 --model dandelion/Qwen3.8-Flash-Next」，--model 从未生效。
            if let Some(model) = model { command.args(["--model", model]); }
            // `opencode run` takes the message as a positional argument. Its
            // current CLI ignores a prompt written only to stdin and exits 0
            // without a text event. `--` also protects prompts starting `-`.
            command.args(["--", prompt]);
        }
        Engine::ClaudeCode => {
            command.args(["--print", "--verbose", "--output-format", "stream-json", "--include-partial-messages"]);
            match permission_mode {
                PermissionMode::Auto => { command.args(["--permission-mode", "acceptEdits"]); }
                PermissionMode::Deny => { command.args(["--permission-mode", "dontAsk"]); }
                PermissionMode::Ask => {}
            }
            if let Some(model) = model { command.args(["--model", model]); }
        }
    }
    // 同上：可执行文件已解析成功，spawn 失败属于运行期启动故障
    // （Provider），与“模型未配置”（NotConfigured）严格区分。
    let mut child = command
        .spawn()
        .map_err(|err| Error::Provider(format!("{} 程序启动失败：{err}", engine.id())))?;
    let pid = child.id();
    #[cfg(unix)]
    let _group_guard = ProcessGroupGuard(pid);
    let stdin_task = if engine == Engine::OpenCode {
        None
    } else {
        let mut stdin = child.stdin.take().ok_or_else(|| Error::Provider("Agent stdin unavailable".into()))?;
        let input = prompt.as_bytes().to_vec();
        Some(tokio::spawn(async move {
            stdin.write_all(&input).await?;
            stdin.shutdown().await
        }))
    };
    let stdout = child.stdout.take().ok_or_else(|| Error::Provider("Agent stdout unavailable".into()))?;
    let stderr = child.stderr.take().ok_or_else(|| Error::Provider("Agent stderr unavailable".into()))?;
    let stderr_task = tokio::spawn(async move {
        let mut input = stderr;
        let mut tail = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(count) = input.read(&mut chunk).await {
            if count == 0 { break; }
            tail.extend_from_slice(&chunk[..count]);
            if tail.len() > 32 * 1024 { tail.drain(..tail.len() - 32 * 1024); }
        }
        String::from_utf8_lossy(&tail).into_owned()
    });
    let mut lines = BufReader::new(stdout).lines();
    let mut answer = String::new();
    let mut saw_delta = false;
    let mut result_error = None;
    loop {
        let line = tokio::select! {
            _ = cancel.cancelled() => {
                kill_process_tree(pid, &mut child).await;
                let _ = stderr_task.await;
                return Err(Error::Cancelled);
            }
            line = lines.next_line() => line?,
        };
        let Some(line) = line else { break; };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else { continue; };
        if let Some(run) = &mut opencode_run {
            if let Some(native_id) = event["sessionID"].as_str() {
                *run.observed_session = Some(native_id.to_owned());
            }
        }
        if engine == Engine::ClaudeCode && event["type"] == "result"
            && event["is_error"].as_bool().unwrap_or(false) {
            result_error = Some(event["result"].as_str().unwrap_or("Claude Code 执行失败").to_string());
        }
        if let Some(tool) = event_tool(engine, &event) { on_tool(tool); }
        if engine == Engine::ClaudeCode && event["type"] == "assistant" && !event["error"].is_null() {
            continue;
        }
        if engine == Engine::OpenCode && event["type"] == "error" {
            result_error = Some(event["error"].to_string());
        }
        if let Some(delta) = event_text_delta(engine, &event, saw_delta) {
            if !delta.is_empty() {
                answer.push_str(delta);
                on_text(delta);
                saw_delta = true;
            }
        }
    }
    let status = tokio::select! {
        _ = cancel.cancelled() => {
            kill_process_tree(pid, &mut child).await;
            let _ = stderr_task.await;
            return Err(Error::Cancelled);
        }
        status = child.wait() => status?,
    };
    let errors = stderr_task.await.unwrap_or_default();
    if let Some(error) = result_error {
        return Err(Error::Provider(format!("{} 执行失败：{error}", engine.id())));
    }
    if !status.success() {
        let detail = if errors.trim().is_empty() {
            format!("进程退出码 {}，请检查模型服务与 CC Switch 日志", status.code().map_or_else(|| "未知".into(), |code| code.to_string()))
        } else {
            errors.trim().to_owned()
        };
        return Err(Error::Provider(format!("{} 退出：{detail}", engine.id())));
    }
    if let Some(stdin_task) = stdin_task {
        stdin_task.await.map_err(|err| Error::Provider(format!("Agent input task failed: {err}")))??;
    }
    if answer.trim().is_empty() {
        return Err(Error::Provider(format!("{} 未返回可见回答：{}", engine.id(), errors.trim())));
    }
    Ok(answer)
}

/// Persistent-session stream-json args. `--permission-prompt-tool stdio` is
/// what actually opens the approval channel: it makes Claude Code delegate
/// permission prompts to us as `control_request` (subtype `can_use_tool`)
/// frames on stdout; `turn` below must answer every one of them. Without it
/// the CLI has no prompt target and fail-closes every ask locally, never
/// consulting us. (`--permission-prompts` is not a Claude Code option and
/// makes the CLI exit with code 1.) `--add-dir` mirrors
/// Dandelion's sandbox whitelist into the hosted CLI so pre-approved
/// directories need no second approval.
fn claude_persistent_args(
    permission_mode: PermissionMode,
    model: Option<&str>,
    extra_dirs: &[PathBuf],
    wiki_bridge_file: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "--print".into(),
        "--output-format".to_string(), "stream-json".into(),
        "--verbose".into(),
        "--input-format".to_string(), "stream-json".into(),
        "--include-partial-messages".into(),
        "--permission-prompt-tool".into(), "stdio".into(),
    ];
    match permission_mode {
        PermissionMode::Auto => args.extend(["--permission-mode".into(), "acceptEdits".into()]),
        PermissionMode::Deny => args.extend(["--permission-mode".into(), "dontAsk".into()]),
        PermissionMode::Ask => {}
    }
    for dir in extra_dirs {
        args.extend(["--add-dir".into(), dir.to_string_lossy().into_owned()]);
    }
    if let Some(file) = wiki_bridge_file {
        args.extend(["--mcp-config".into(), file.to_string_lossy().into_owned()]);
        // Only the host-gated read-only knowledge tool is preauthorized.
        // Keep all mutation and unrelated MCP permissions unchanged.
        args.extend(["--allowedTools".into(), "mcp__dandelion__dandelion_knowledge_search".into()]);
    }
    if let Some(model) = model {
        args.extend(["--model".into(), model.to_string()]);
    }
    args
}

/// Host-side approval context threaded into one Claude Code turn so the
/// bridge can raise Dandelion's own permission dialog for hosted tool calls.
pub struct ClaudeApproval<'a> {
    pub host: &'a dyn AgentHost,
    pub session_id: &'a str,
    pub permission_timeout_sec: u64,
    /// 宿主侧「本次会话不再询问」的权威记录（key = 蒲公英根会话 id）。
    ///
    /// 「不再询问」不能只翻译给 CLI：`updatedPermissions` 规则活在子进程里，
    /// CLI 有多条静默丢弃宿主规则的路径（malformed/catch 吞掉/过滤器），进程
    /// 因 config 漂移 respawn 时也会整体丢失。过去 Dandelion 自己不记账，
    /// 每个 `can_use_tool` 都无条件弹窗 —— 用户按了"不再询问"仍被逐条追问。
    /// 现在放行以这份表为准，CLI 规则只作免弹加速。这份表同时交给原生循环的
    /// `ToolRegistry`（`AgentSessions::allow_all`），两个引擎语义一致。
    pub allow_all: &'a std::sync::Mutex<std::collections::HashSet<String>>,
}

fn control_response(
    request_id: serde_json::Value,
    response: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response }
    })
}

fn tool_input_summary(input: &serde_json::Value) -> String {
    for key in ["file_path", "path", "command", "url", "pattern", "query", "prompt"] {
        if let Some(value) = input.get(key).and_then(|v| v.as_str()) {
            return value.chars().take(160).collect();
        }
    }
    String::new()
}

/// Answer one `can_use_tool` control request. Three-tier mapping of the
/// Dandelion dialog: allow → once; allow_turn → allow plus a session-scoped
/// rule (the reused process stops asking for that tool kind this task);
/// deny / timeout / missing approval channel → deny with an actionable reason.
async fn can_use_tool_response(
    request: &serde_json::Value,
    approval: Option<&ClaudeApproval<'_>>,
    cancel: &CancellationToken,
) -> serde_json::Value {
    let tool_name = request["tool_name"].as_str().unwrap_or("tool");
    let input = request["input"].clone();
    let input = if input.is_object() { input } else { serde_json::json!({}) };
    let deny = |message: String| serde_json::json!({ "behavior": "deny", "message": message });
    let Some(approval) = approval.filter(|a| a.host.is_interactive()) else {
        return deny(format!(
            "{tool_name} 已被拒绝：当前宿主没有审批通道，无法请求用户批准。请在桌面端任务窗口中批准后重试，\
             或将相关目录加入蒲公英设置 dandelion.sandbox.allowRoots。"
        ));
    };
    let Ok(session_id) = SessionId::parse(approval.session_id.to_string()) else {
        return deny(format!("{tool_name} 已被拒绝：会话标识无效，无法登记审批。"));
    };
    // 弹窗前先查宿主放行表：这条任务里用户已经说过「不再询问」，一切工具请求
    // 直接放行。这是权威判定 —— 不依赖 CLI 是否采纳了我们回传的 session 规则。
    if approval
        .allow_all
        .lock()
        .is_ok_and(|allowed| allowed.contains(approval.session_id))
    {
        return serde_json::json!({ "behavior": "allow", "updatedInput": input });
    }
    let summary = tool_input_summary(&input);
    let title = if summary.is_empty() {
        format!("Claude Code 请求执行 {tool_name}，是否允许？")
    } else {
        format!("Claude Code 请求执行 {tool_name}：{summary}")
    };
    let request = RequestPermissionRequest::tool_call(session_id, title, ToolCallId::new());
    let wait = Duration::from_secs(approval.permission_timeout_sec);
    let outcome = tokio::select! {
        _ = cancel.cancelled() => RequestPermissionOutcome::Cancelled,
        res = timeout(wait, approval.host.request_permission(request, cancel.clone())) => {
            match res {
                Ok(Ok(outcome)) => outcome,
                _ => RequestPermissionOutcome::Denied,
            }
        }
    };
    match outcome {
        RequestPermissionOutcome::Selected { option_id } if option_id == "allow" => {
            serde_json::json!({ "behavior": "allow", "updatedInput": input })
        }
        RequestPermissionOutcome::Selected { option_id } if option_id == "allow_turn" => {
            // 宿主记档：此后该会话（＝这条任务）内的所有工具请求都免弹窗直接放行。
            if let Ok(mut allowed) = approval.allow_all.lock() {
                allowed.insert(approval.session_id.to_string());
            }
            // updatedPermissions 仍然回传：CLI 若采纳可在它的侧少发请求（加速），
            // 但即使它丢弃，下一次也会在上面查表命中，不再打扰用户。
            serde_json::json!({
                "behavior": "allow",
                "updatedInput": input,
                "updatedPermissions": [{
                    "type": "addRules",
                    "rules": [{ "toolName": tool_name }],
                    "behavior": "allow",
                    "destination": "session",
                }],
            })
        }
        _ => deny(format!(
            "{tool_name} 已被拒绝或审批超时。重试该操作并在蒲公英弹窗中批准即可；\
             希望本会话不再弹窗请在弹窗选择“本次会话不再询问”，或将目录加入 \
             dandelion.sandbox.allowRoots。"
        )),
    }
}

fn spawn_claude_process(config: ClaudeProcessConfig) -> Result<ClaudeProcess> {
    let mut command = tokio::process::Command::new(&config.executable);
    command.env_clear();
    let isolated_config = config.env.iter().any(|(key, _)| key == "CLAUDE_CONFIG_DIR");
    for key in [
        "PATH", "HOME", "USER", "LOGNAME", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL",
        "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "SHELL",
        "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy",
        "USERPROFILE", "APPDATA", "LOCALAPPDATA", "SystemRoot", "ComSpec",
        "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL",
        "ANTHROPIC_MODEL", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CONFIG_DIR",
    ] {
        if isolated_config && key == "CLAUDE_CODE_OAUTH_TOKEN" { continue; }
        if let Some(value) = std::env::var_os(key) { command.env(key, value); }
    }
    command.envs(config.env.iter().map(|(key, value)| (key, value)));
    command.current_dir(&config.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    hide_console_windows(&mut command);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    // Claude Code requires --print for stream-json input/output and hosted
    // permission prompts. Keeping stdin open allows subsequent turns.
    command.args(claude_persistent_args(
        config.permission_mode,
        config.model.as_deref(),
        &config.extra_dirs,
        config.wiki_bridge_file.as_deref(),
    ));
    let mut child = command.spawn().map_err(|err| Error::NotConfigured(format!("Claude Code 启动失败：{err}")))?;
    let pid = child.id();
    let stdin = child.stdin.take().ok_or_else(|| Error::Provider("Claude Code stdin unavailable".into()))?;
    let stdout = child.stdout.take().ok_or_else(|| Error::Provider("Claude Code stdout unavailable".into()))?;
    let stderr = child.stderr.take().ok_or_else(|| Error::Provider("Claude Code stderr unavailable".into()))?;
    let stderr_task = tokio::spawn(async move {
        let mut input = stderr;
        let mut tail = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(count) = input.read(&mut chunk).await {
            if count == 0 { break; }
            tail.extend_from_slice(&chunk[..count]);
            if tail.len() > 32 * 1024 { tail.drain(..tail.len() - 32 * 1024); }
        }
        String::from_utf8_lossy(&tail).into_owned()
    });
    Ok(ClaudeProcess {
        config, child, stdin, lines: BufReader::new(stdout).lines(), stderr_task, pid,
        #[cfg(unix)]
        _group_guard: ProcessGroupGuard(pid),
    })
}

/// Build the stream-json `content` for one user turn. Text-only turns keep
/// the plain-string form; a turn with images becomes an Anthropic content
/// block array so the CLI forwards them to a multimodal model. Unsupported
/// data URLs are dropped rather than failing the turn.
fn claude_content(prompt: &str, images: &[String]) -> serde_json::Value {
    let blocks = images.iter().filter_map(|url| {
        let (meta, data) = url.split_once(',')?;
        let media_type = meta
            .strip_prefix("data:")?
            .strip_suffix(";base64")?;
        if !matches!(media_type, "image/png" | "image/jpeg" | "image/webp" | "image/gif")
            || data.is_empty()
        {
            return None;
        }
        Some(serde_json::json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data},
        }))
    });
    if images.is_empty() {
        return serde_json::Value::String(prompt.to_owned());
    }
    let blocks: Vec<_> = blocks.collect();
    if blocks.is_empty() {
        return serde_json::Value::String(prompt.to_owned());
    }
    let mut content = vec![serde_json::json!({"type": "text", "text": prompt})];
    content.extend(blocks);
    serde_json::Value::Array(content)
}

impl ClaudeProcess {
    async fn kill(&mut self) {
        kill_process_tree(self.pid, &mut self.child).await;
        self.stderr_task.abort();
    }

    async fn turn(
        &mut self,
        prompt: &str,
        images: &[String],
        cancel: &CancellationToken,
        approval: Option<&ClaudeApproval<'_>>,
        on_text: &mut impl FnMut(&str),
        on_tool: &mut impl FnMut(ExternalToolEvent),
    ) -> Result<String> {
        // stream-json accepts `content` as a plain string or as Anthropic
        // content blocks. With images we send a block array so a multimodal
        // model actually sees them; text-only turns keep the string form.
        let content = claude_content(prompt, images);
        let message = serde_json::json!({
            "type": "user", "message": {"role": "user", "content": content},
            "parent_tool_use_id": null, "session_id": "default"
        });
        let mut input = serde_json::to_vec(&message)?;
        input.push(b'\n');
        self.stdin.write_all(&input).await?;
        self.stdin.flush().await?;
        let mut answer = String::new();
        let mut saw_delta = false;
        loop {
            let line = tokio::select! {
                _ = cancel.cancelled() => return Err(Error::Cancelled),
                line = self.lines.next_line() => line?,
            };
            let Some(line) = line else {
                let status = self.child.wait().await?;
                return Err(Error::Provider(format!("Claude Code 会话意外退出（{}）", status.code().map_or_else(|| "未知".into(), |code| code.to_string()))));
            };
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else { continue; };
            if event["type"] == "control_request" {
                // `--permission-prompt-tool stdio` routes every would-be
                // prompt here. Every request must be answered or the CLI
                // waits.
                let request_id = event["request_id"].clone();
                let response = match event["request"]["subtype"].as_str() {
                    Some("can_use_tool") => can_use_tool_response(&event["request"], approval, cancel).await,
                    _ => serde_json::json!({
                        "behavior": "deny",
                        "message": "蒲公英无法应答该控制类型",
                    }),
                };
                let mut payload = serde_json::to_vec(&control_response(request_id, response))?;
                payload.push(b'\n');
                self.stdin.write_all(&payload).await?;
                self.stdin.flush().await?;
                continue;
            }
            if event["type"] == "result" {
                if event["is_error"].as_bool().unwrap_or(false) {
                    return Err(Error::Provider(format!("Claude Code 执行失败：{}", event["result"].as_str().unwrap_or("未知错误"))));
                }
                if answer.trim().is_empty() {
                    return Err(Error::Provider("Claude Code 未返回可见回答".into()));
                }
                return Ok(answer);
            }
            if let Some(tool) = event_tool(Engine::ClaudeCode, &event) { on_tool(tool); }
            if event["type"] == "assistant" && !event["error"].is_null() { continue; }
            if let Some(delta) = event_text_delta(Engine::ClaudeCode, &event, saw_delta) {
                if !delta.is_empty() {
                    answer.push_str(delta);
                    on_text(delta);
                    saw_delta = true;
                }
            }
        }
    }
}

impl ClaudeSessions {
    pub async fn execute(
        &self,
        session_id: &str,
        executable: &Path,
        model: Option<&str>,
        cwd: &Path,
        prompt: &str,
        fresh_prompt: &str,
        env: &[(String, String)],
        permission_mode: PermissionMode,
        extra_dirs: &[PathBuf],
        wiki_bridge_file: Option<&Path>,
        approval: Option<&ClaudeApproval<'_>>,
        images: &[String],
        cancel: CancellationToken,
        mut on_text: impl FnMut(&str),
        mut on_tool: impl FnMut(ExternalToolEvent),
    ) -> Result<String> {
        let config = ClaudeProcessConfig {
            executable: executable.to_path_buf(), model: model.map(str::to_owned),
            cwd: cwd.to_path_buf(), env: env.to_vec(), permission_mode,
            extra_dirs: extra_dirs.to_vec(),
            wiki_bridge_file: wiki_bridge_file.map(Path::to_path_buf),
        };
        let existing = self.processes.lock().map_err(|_| Error::Provider("Claude Code 会话锁已损坏".into()))?
            .get(session_id).cloned();
        if let Some(process) = &existing {
            let mut guard = process.lock().await;
            if guard.config == config && guard.child.try_wait()?.is_none() {
                let result = guard.turn(prompt, images, &cancel, approval, &mut on_text, &mut on_tool).await;
                if result.is_err() {
                    guard.kill().await;
                    self.remove_if_same(session_id, process);
                }
                return result;
            }
            guard.kill().await;
            self.remove_if_same(session_id, process);
        }
        if cancel.is_cancelled() { return Err(Error::Cancelled); }
        let process = Arc::new(tokio::sync::Mutex::new(spawn_claude_process(config)?));
        self.processes.lock().map_err(|_| Error::Provider("Claude Code 会话锁已损坏".into()))?
            .insert(session_id.to_owned(), process.clone());
        let mut guard = process.lock().await;
        let result = guard.turn(fresh_prompt, images, &cancel, approval, &mut on_text, &mut on_tool).await;
        if result.is_err() {
            guard.kill().await;
            self.remove_if_same(session_id, &process);
        }
        result
    }
}

fn restricted_opencode_config(env: &[(String, String)]) -> Result<String> {
    let mut config = env.iter()
        .find(|(key, _)| key == "OPENCODE_CONFIG_CONTENT")
        .map(|(_, value)| serde_json::from_str::<serde_json::Value>(value))
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({}));
    if !config.is_object() {
        return Err(Error::Invalid("OpenCode 配置必须是 JSON 对象".into()));
    }
    let limited = serde_json::json!({
        "*": "deny", "read": "allow", "glob": "allow", "grep": "allow", "list": "allow",
        "edit": "deny", "bash": "deny", "task": "deny", "external_directory": "deny",
        "dandelion_dandelion_knowledge_search": "allow"
    });
    config["permission"] = limited.clone();
    if !config["agent"].is_object() { config["agent"] = serde_json::json!({}); }
    if !config["agent"]["plan"].is_object() { config["agent"]["plan"] = serde_json::json!({}); }
    config["agent"]["plan"]["permission"] = limited;
    Ok(serde_json::to_string(&config)?)
}

fn event_tool(engine: Engine, event: &serde_json::Value) -> Option<ExternalToolEvent> {
    let (id, name, finished, failed, detail) = match engine {
        Engine::Pi if event["type"] == "tool_execution_start" => (
            event["toolCallId"].as_str()?, event["toolName"].as_str().unwrap_or("tool"),
            false, false, None,
        ),
        Engine::Pi if event["type"] == "tool_execution_end" => (
            event["toolCallId"].as_str()?, event["toolName"].as_str().unwrap_or("tool"),
            true, event["isError"].as_bool().unwrap_or(false),
            event.get("result").map(|value| value.to_string()),
        ),
        Engine::OpenCode if event["type"] == "tool_use" => {
            let part = &event["part"];
            let state = part["state"]["status"].as_str().unwrap_or("");
            (
                part["callID"].as_str().or_else(|| part["id"].as_str())?,
                part["tool"].as_str().unwrap_or("tool"),
                matches!(state, "completed" | "error"), state == "error",
                part["state"].get("output").map(|value| value.to_string()),
            )
        }
        Engine::ClaudeCode if event["type"] == "stream_event"
            && event["event"]["type"] == "content_block_start"
            && event["event"]["content_block"]["type"] == "tool_use" => (
                event["event"]["content_block"]["id"].as_str()?,
                event["event"]["content_block"]["name"].as_str().unwrap_or("tool"),
                false, false, None,
            ),
        Engine::ClaudeCode if event["type"] == "user" => {
            let part = event["message"]["content"].as_array()?.iter()
                .find(|part| part["type"] == "tool_result")?;
            (part["tool_use_id"].as_str()?, "tool", true,
                part["is_error"].as_bool().unwrap_or(false),
                part.get("content").map(|value| value.to_string()))
        }
        _ => return None,
    };
    Some(ExternalToolEvent {
        id: id.to_string(), name: name.to_string(), finished, failed,
        detail: detail.map(|value| value.chars().take(4_000).collect()),
    })
}

fn event_text_delta<'a>(engine: Engine, event: &'a serde_json::Value, saw_delta: bool) -> Option<&'a str> {
    match engine {
        Engine::Pi => {
            if event["type"] == "message_update" && event["assistantMessageEvent"]["type"] == "text_delta" {
                event["assistantMessageEvent"]["delta"].as_str()
            } else { None }
        }
        Engine::OpenCode => {
            if event["type"] == "text" { event["part"]["text"].as_str() } else { None }
        }
        Engine::ClaudeCode => {
            if event["type"] == "stream_event" && event["event"]["type"] == "content_block_delta" {
                event["event"]["delta"]["text"].as_str()
            } else if !saw_delta && event["type"] == "assistant" {
                event["message"]["content"].as_array()?.iter()
                    .find(|part| part["type"] == "text")?.get("text")?.as_str()
            } else { None }
        }
        Engine::Native => None,
    }
}

async fn kill_process_tree(pid: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // The subprocess started in its own group. Kill descendants as well as
        // the CLI process, so a cancelled run cannot keep modifying files.
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
    }
    #[cfg(windows)]
    if let Some(pid) = pid {
        let mut kill = tokio::process::Command::new("taskkill");
        kill.args(["/PID", &pid.to_string(), "/T", "/F"]);
        hide_console_windows(&mut kill);
        let _ = kill.output().await;
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(unix)]
struct ProcessGroupGuard(Option<u32>);

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // Also covers a task future dropped during desktop shutdown.
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
        }
    }
}

#[cfg(test)]
mod tests {
    /// 生产段（`#[cfg(test)]` 之前）里每个外部子进程构造后都必须调用
    /// hide_console_windows(_std)。日后新增 spawn 漏配时本测试必须红 ——
    /// 静默托管的子进程不允许有漏网的；GUI 宿主下漏一个就多一种弹窗。
    #[test]
    fn every_spawn_site_in_agent_plugins_hides_its_console() {
        let source = include_str!("agent_plugins.rs");
        let production = source.split("#[cfg(test)]").next().expect("tests marker");
        let mut remaining = production;
        let mut sites = 0usize;
        while let Some(at) = remaining.find("Command::new(") {
            let after = &remaining[at + "Command::new(".len()..];
            let next_site = after.find("Command::new(").unwrap_or(after.len());
            let span = &after[..next_site];
            assert!(
                span.contains("hide_console_windows"),
                "agent_plugins.rs 中一处子进程构造缺少 hide_console_windows：{})",
                span.chars().take(160).collect::<String>(),
            );
            sites += 1;
            remaining = &after[next_site..];
        }
        assert!(sites >= 5, "子进程 spawn 点数量与审计预期不符：{sites}");
    }

    /// 行为验证：在 GUI 宿主同形（父进程无控制台）的真实场景下，
    /// CREATE_NO_WINDOW 让被管道托管的控制台子进程不再获得带窗口的控制台。
    /// 探针源码见 tests/fixtures/win_no_console_probe.rs。
    /// （对照组不带标志时，交互式桌面上会短暂闪出一个 powershell 控制台窗口
    /// —— 那正是被修复的缺陷本身；无窗口站的会话中对照组结果仅供参考。）
    #[cfg(windows)]
    #[test]
    fn hosted_children_get_no_console_window() {
        let dir = std::env::temp_dir().join(format!("dln-no-console-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp probe dir");
        let source = dir.join("win_no_console_probe.rs");
        let exe = dir.join("win_no_console_probe.exe");
        std::fs::write(&source, include_str!("../tests/fixtures/win_no_console_probe.rs"))
            .expect("write probe source");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let built = std::process::Command::new(rustc)
            .args(["--edition", "2021", "--crate-name", "dln_no_console_probe"])
            .arg("-o").arg(&exe).arg(&source)
            .output()
            .expect("rustc 可用");
        assert!(built.status.success(), "探针编译失败：{}", String::from_utf8_lossy(&built.stderr));
        let run = |mode: &str| -> String {
            let out = std::process::Command::new(&exe).arg(mode).output().expect("探针可运行");
            assert!(out.status.success(), "探针 {mode} 运行失败：{}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).split('|').next().unwrap_or("").trim().to_string()
        };
        assert_eq!(run("hide").as_str(), "0", "CREATE_NO_WINDOW 下的子进程不应持有带窗口的控制台");
        let shown = run("show");
        if shown == "0" {
            eprintln!("提示：当前会话无法为对照组分配可见控制台（无窗口站？），负向对照仅记录不判定");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_content_wraps_only_valid_images_as_blocks() {
        // 无图：纯字符串，与旧行为逐字节一致。
        assert_eq!(claude_content("hi", &[]), serde_json::json!("hi"));
        let v = claude_content(
            "look",
            &[
                "data:image/png;base64,AAAA".to_string(),
                "data:image/svg+xml;base64,AAAA".to_string(),
            ],
        );
        let blocks = v.as_array().expect("text + image blocks");
        assert_eq!(blocks.len(), 2, "白名单外的 mime 应被丢弃");
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "look");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "base64");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert_eq!(blocks[1]["source"]["data"], "AAAA");
        // 全部无效时退回纯文本，不产生空数组。
        assert_eq!(
            claude_content("look", &["data:image/svg+xml;base64,AAAA".to_string()]),
            serde_json::json!("look")
        );
    }

    use super::*;

    #[test]
    fn managed_claude_requires_cc_switch_cli_to_be_ready() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("agent-engines/claude_code/releases/current/claude");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, "fixture").unwrap();
        let mut registry = Registry::default();
        registry.plugins.insert("claude_code".into(), PluginConfig {
            enabled: true, executable: Some(executable), managed: true, model: None,
        });
        write_registry(dir.path(), &registry).unwrap();
        let claude = list(dir.path(), &[]).unwrap().into_iter().find(|item| item.id == "claude_code").unwrap();
        assert!(!claude.ready);
        assert!(claude.cc_switch_executable.is_none());
        let cc = crate::cc_switch::binary(dir.path());
        std::fs::create_dir_all(cc.parent().unwrap()).unwrap();
        std::fs::write(&cc, "fixture").unwrap();
        let claude = list(dir.path(), &[]).unwrap().into_iter().find(|item| item.id == "claude_code").unwrap();
        assert!(claude.ready);
        assert!(claude.cc_switch_executable.is_some());
    }

    #[test]
    fn remove_deletes_only_managed_agent_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("agent-engines").join("pi");
        let executable = root.join("releases").join("current").join("pi.cmd");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, "fixture").unwrap();
        let external = dir.path().join("external-agent.cmd");
        std::fs::write(&external, "fixture").unwrap();
        let mut registry = Registry::default();
        registry.plugins.insert("pi".into(), PluginConfig {
            enabled: true, executable: Some(executable), managed: true, model: None,
        });
        registry.plugins.insert("opencode".into(), PluginConfig {
            enabled: true, executable: Some(external.clone()), managed: false, model: None,
        });
        write_registry(dir.path(), &registry).unwrap();
        remove(dir.path(), "pi").unwrap();
        remove(dir.path(), "opencode").unwrap();
        assert!(!root.exists());
        assert!(external.exists());
        let saved = read_registry(dir.path()).unwrap();
        assert!(!saved.plugins["pi"].enabled);
        assert!(!saved.plugins["opencode"].enabled);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn pi_rpc_reuses_process_and_context_across_turns() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("fake-pi.cmd");
        std::fs::write(&cli, "@echo off\r\nsetlocal EnableDelayedExpansion\r\nset count=0\r\n:next\r\nset /p line=\r\nif errorlevel 1 goto done\r\nset /a count+=1 >nul\r\necho {\"id\":\"dandelion-prompt\",\"type\":\"response\",\"command\":\"prompt\",\"success\":true}\r\necho {\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"delta\":\"turn-!count!\"}}\r\necho {\"type\":\"agent_end\"}\r\ngoto next\r\n:done\r\n").unwrap();
        let sessions = AgentSessions::default();
        for (prompt, expected) in [("first", "turn-1"), ("second", "turn-2")] {
            let answer = sessions.execute(Engine::Pi, "conversation", &cli, None,
                dir.path(), prompt, prompt, &[], PermissionMode::Deny,
                &[], None, &[], None, CancellationToken::new(), |_| {}, |_| {}).await.unwrap();
            assert_eq!(answer, expected);
        }
        sessions.close("conversation");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn pi_receives_wiki_extension_as_spawn_argument() {
        // The generated extension file must reach the Pi process on the
        // command line; without it the engine silently has no wiki tool.
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("fake-pi.cmd");
        std::fs::write(&cli, "@echo off\r\necho %* >\"%~dp0args.txt\"\r\nset /p line=\r\necho {\"id\":\"dandelion-prompt\",\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{\"disposition\":\"started\"}}\r\necho {\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"delta\":\"ok\"}}\r\necho {\"type\":\"agent_settled\",\"aborted\":false}\r\n").unwrap();
        let bridge_file = dir.path().join("pi-wiki-tok.js");
        std::fs::write(&bridge_file, "// extension").unwrap();
        let sessions = AgentSessions::default();
        let answer = sessions.execute(Engine::Pi, "conversation", &cli, None,
            dir.path(), "hello", "hello", &[], PermissionMode::Deny,
            &[], None, &[], Some(&bridge_file), CancellationToken::new(), |_| {}, |_| {}).await.unwrap();
        assert_eq!(answer, "ok");
        sessions.close("conversation");
        let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert!(args.contains("--extension"), "{args}");
        assert!(args.contains("pi-wiki-tok.js"), "{args}");
    }

    /// 直启 run 形态的续接回归：带 `--session <原生 ID>`、绝不出现
    /// `--attach`（attach 会把 text/tool 事件全部吞掉，真机 v1.18.35 与
    /// v1.19.3 均复现）；`--model` 必须排在 `--` 之前，否则会被并进消息
    /// 正文（真机曾把用户消息发成「你好 --model …」）。
    #[cfg(windows)]
    #[tokio::test]
    async fn opencode_run_continues_session_and_puts_model_first() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("fake-opencode.cmd");
        std::fs::write(&cli, "@echo off\r\necho %* >\"%~dp0args.txt\"\r\necho {\"type\":\"text\",\"sessionID\":\"native-123\",\"part\":{\"text\":\"ok\"}}\r\n").unwrap();
        let mut observed = None;
        let answer = execute_with_connection(Engine::OpenCode, &cli, Some("m1"), dir.path(),
            "hello", &[], PermissionMode::Auto, CancellationToken::new(),
            Some(OpenCodeRun { session: Some("native-previous"), observed_session: &mut observed }),
            |_| {}, |_| {}).await.unwrap();
        assert_eq!(answer, "ok");
        assert_eq!(observed.as_deref(), Some("native-123"));
        let raw = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert!(!raw.contains("--attach"), "不得回到丢事件的 attach 形态：{raw}");
        let tokens: Vec<&str> = raw.split_whitespace().collect();
        let dash = tokens.iter().rposition(|a| *a == "--").expect("`--` 分隔必须在场");
        let session = tokens.iter().position(|a| *a == "--session")
            .expect("续接必须带 --session");
        assert_eq!(tokens[session + 1], "native-previous");
        assert!(session < dash, "--session 必须在 `--` 之前：{tokens:?}");
        let model = tokens.iter().position(|a| *a == "--model").expect("--model 必须在场");
        assert!(model < dash, "--model 必须在 `--` 之前，否则被并进消息正文：{tokens:?}");
        assert_eq!(tokens[model + 1], "m1");
        assert_eq!(tokens[dash + 1], "hello", "OpenCode prompt must be positional: {raw}");
    }

    /// `execute_opencode` 层的状态机：首轮捕获 sessionID 落条目；次轮
    /// 配置一致 → argv 带 `--session`；改配置（换 model）→ 回退新会话，
    /// argv 不带 `--session`。
    #[cfg(windows)]
    #[tokio::test]
    async fn opencode_execute_continues_only_with_unchanged_config() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("fake-opencode.cmd");
        // 每次调用覆盖写 args.txt，断言的总是"最后这一轮实际收到了什么"。
        std::fs::write(&cli, "@echo off\r\necho %* >\"%~dp0args.txt\"\r\necho {\"type\":\"text\",\"sessionID\":\"native-123\",\"part\":{\"text\":\"ok\"}}\r\n").unwrap();
        let args_file = dir.path().join("args.txt");
        let sessions = AgentSessions::default();
        async fn run(sessions: &AgentSessions, cli: &Path, cwd: &Path,
            model: Option<&str>, prompt: &str) -> String {
            sessions.execute(Engine::OpenCode, "conversation", cli, model, cwd,
                prompt, prompt, &[], PermissionMode::Auto,
                &[], None, &[], None, CancellationToken::new(), |_| {}, |_| {}).await.unwrap()
        }
        // 首轮：新会话，不带 --session；sessionID 回写会话状态。
        assert_eq!(run(&sessions, &cli, dir.path(), Some("m1"), "first").await, "ok");
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(!args.contains("--session"), "首轮必须开新会话：{args}");
        assert_eq!(sessions.session_ids(Engine::OpenCode), vec!["conversation".to_string()]);
        // 次轮：配置一致 → 续接原生会话。
        assert_eq!(run(&sessions, &cli, dir.path(), Some("m1"), "second").await, "ok");
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(args.contains("--session native-123"), "配置一致必须续接：{args}");
        // 换 model：配置漂移 → 回退新会话（调用方据此换用 fresh_prompt）。
        assert_eq!(run(&sessions, &cli, dir.path(), Some("m2"), "third").await, "ok");
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(!args.contains("--session"), "配置漂移必须回退新会话：{args}");
        assert!(args.contains("--model m2"), "新配置的 --model 必须生效：{args}");
        sessions.close("conversation");
    }

    /// 与托管安装同形的真实 npm 布局：`.bin\opencode.cmd` 使用 npm 实际
    /// 生成的垫片文本，原生 exe 位置放现场编译的探针（它会记录收到的
    /// argv 并按 OpenCode 协议回显消息，不是回显固定答案的假 `.cmd`）。
    #[cfg(windows)]
    const NPM_SHIM: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\"%dp0%\\..\\opencode-ai\\bin\\opencode.exe\"   %*\r\n";

    /// 把探针编译成 npm 布局里的原生 opencode.exe，返回 (shim, exe)。
    #[cfg(windows)]
    fn opencode_npm_layout(dir: &Path) -> (PathBuf, PathBuf) {
        let bin = dir.join("node_modules").join(".bin");
        let native_dir = dir.join("node_modules").join("opencode-ai").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&native_dir).unwrap();
        let shim = bin.join("opencode.cmd");
        std::fs::write(&shim, NPM_SHIM).unwrap();
        let exe = native_dir.join("opencode.exe");
        let source = native_dir.join("opencode_probe.rs");
        std::fs::write(&source, include_str!("../tests/fixtures/win_opencode_probe.rs")).unwrap();
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let built = std::process::Command::new(rustc)
            .args(["--edition", "2021", "--crate-name", "dln_opencode_probe"])
            .arg("-o").arg(&exe).arg(&source)
            .output()
            .expect("rustc 可用");
        assert!(built.status.success(), "OpenCode 探针编译失败：{}", String::from_utf8_lossy(&built.stderr));
        (shim, exe)
    }

    /// 选择逻辑：无论路径来自蒲公英托管安装还是用户手动指定（两者都
    /// 经 config.executable → executable()），OpenCode 都必须落在原生
    /// exe 上；直接指向 exe 时原样通过；垫片背后找不到原生 exe 时给出
    /// 明确的“程序启动失败”错误，绝不把消息交给批处理垫片。
    #[cfg(windows)]
    #[test]
    fn opencode_resolves_native_exe_not_the_batch_shim() {
        let dir = tempfile::tempdir().unwrap();
        let (shim, exe) = opencode_npm_layout(dir.path());

        let registry_of = |path: PathBuf, managed: bool| {
            let mut registry = Registry::default();
            registry.plugins.insert("opencode".into(), PluginConfig {
                enabled: true, executable: Some(path), managed, model: None,
            });
            write_registry(dir.path(), &registry).unwrap();
        };

        let same_file = |a: &Path, b: &Path| dunce::canonicalize(a).unwrap() == dunce::canonicalize(b).unwrap();

        // 托管安装（agent-engines 下 .bin\opencode.cmd 的同一形态）。
        registry_of(shim.clone(), true);
        let chosen = executable(dir.path(), Engine::OpenCode, &[]).unwrap();
        assert!(same_file(&chosen, &exe), "托管 shim 必须被解析为原生 opencode.exe，实际 {chosen:?}");
        assert!(!is_batch_launcher(&chosen));

        // 用户手动指定原生 exe：原样直通，不做任何改写。
        registry_of(exe.clone(), false);
        assert_eq!(executable(dir.path(), Engine::OpenCode, &[]).unwrap(), exe);

        // 手动指定 .cmd 且背后确有原生 exe：同样换到原生 exe。
        registry_of(shim.clone(), false);
        assert!(same_file(&executable(dir.path(), Engine::OpenCode, &[]).unwrap(), &exe));

        // 垫片背后无原生 exe：明确报程序启动失败并告知原因；
        // 不得伪装成 not configured，也不得放行 .cmd。
        let ghost_dir = dir.path().join("elsewhere");
        std::fs::create_dir_all(&ghost_dir).unwrap();
        let ghost = ghost_dir.join("opencode.cmd");
        std::fs::write(&ghost, "@ECHO off\r\n\"%dp0%\\missing\\opencode.exe\" %*\r\n").unwrap();
        registry_of(ghost, true);
        let err = executable(dir.path(), Engine::OpenCode, &[]).expect_err("无原生 exe 的垫片必须被拒绝");
        let text = err.to_string();
        assert!(matches!(err, Error::Provider(_)), "应为运行期启动故障而非配置缺失：{text}");
        assert!(text.contains("opencode.exe") && text.contains("启动失败"), "错误必须指明可执行的补救方向：{text}");
        assert!(!text.starts_with("not configured"), "不得再出现误导性配置缺失文案：{text}");
    }

    /// 端到端消息传递：以 executable() 实际选中的原生 exe 运行
    /// `execute_with_connection`，核验中文、引号、换行、前导 `-` 四类
    /// 消息逐字到达被选中的程序（探针回写的 argv 文件），并且 `--`
    /// 分隔在场；探针按协议回显的 answer 必须与 prompt 逐字相等。
    #[cfg(windows)]
    #[tokio::test]
    async fn opencode_run_delivers_prompts_via_native_exe() {
        let dir = tempfile::tempdir().unwrap();
        let (shim, exe) = opencode_npm_layout(dir.path());
        let mut registry = Registry::default();
        registry.plugins.insert("opencode".into(), PluginConfig {
            enabled: true, executable: Some(shim), managed: true, model: None,
        });
        write_registry(dir.path(), &registry).unwrap();
        let chosen = executable(dir.path(), Engine::OpenCode, &[]).unwrap();
        assert!(dunce::canonicalize(&chosen).unwrap() == dunce::canonicalize(&exe).unwrap());
        let args_file = exe.parent().unwrap().join("last_args.txt");

        for prompt in [
            "你好",
            "say \"hi\" and 'go' now",
            "-dash-leading message",
            "line one\nline two\nend",
        ] {
            let answer = execute_with_connection(Engine::OpenCode, &chosen, Some("probe/model"), dir.path(),
                prompt, &[], PermissionMode::Auto, CancellationToken::new(), None,
                |_| {}, |_| {}).await
                .unwrap_or_else(|err| panic!("prompt {prompt:?} 应成功送达原生 exe：{err}"));
            assert_eq!(answer, prompt, "消息必须经原生 exe 原样往返");
            let recorded = std::fs::read_to_string(&args_file).expect("探针应已启动并记录 argv");
            let argv: Vec<&str> = recorded.split('\u{1f}').collect();
            for flag in ["run", "--format", "json", "--auto"] {
                assert!(argv.contains(&flag), "缺少 run 参数 {flag}：{argv:?}");
            }
            let dash = argv.iter().rposition(|a| *a == "--").expect("`--` 分隔必须在场");
            assert_eq!(argv[dash + 1], prompt, "消息必须逐字作为 `--` 后的位置参数：{argv:?}");
            // `--model` 若落在 `--` 之后就会被并入消息正文（真机反证），必须在分隔符前。
            let model = argv.iter().position(|a| *a == "--model").expect("--model 必须在场");
            assert!(model < dash, "--model 必须在 `--` 之前：{argv:?}");
            assert_eq!(argv[model + 1], "probe/model");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn claude_stream_keeps_a_single_process_for_two_turns() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("fake-claude.cmd");
        std::fs::write(&cli, "@echo off\r\nsetlocal EnableDelayedExpansion\r\nset count=0\r\n:next\r\nset /p line=\r\nif errorlevel 1 goto done\r\nset /a count+=1 >nul\r\necho {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"turn-!count!\"}]}}\r\necho {\"type\":\"result\",\"is_error\":false}\r\ngoto next\r\n:done\r\n").unwrap();
        let sessions = ClaudeSessions::default();
        let mut chunks = Vec::new();
        for (prompt, expected) in [("first", "turn-1"), ("second", "turn-2")] {
            let result = sessions.execute(
                "conversation", &cli, None, dir.path(), prompt, prompt, &[],
                PermissionMode::Deny, &[], None, None, &[], CancellationToken::new(),
                |chunk| chunks.push(chunk.to_owned()), |_| {},
            ).await.unwrap();
            assert_eq!(result, expected);
        }
        assert_eq!(chunks, ["turn-1", "turn-2"]);
        sessions.close("conversation");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancelling_claude_stream_releases_the_process() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("waiting-claude.cmd");
        let marker = dir.path().join("ready");
        std::fs::write(&cli, "@echo off\r\nset /p line=\r\necho ready>\"%~dp0ready\"\r\nping -n 30 127.0.0.1 >nul\r\n").unwrap();
        let sessions = Arc::new(ClaudeSessions::default());
        let cancel = CancellationToken::new();
        let worker = {
            let sessions = sessions.clone();
            let cancel = cancel.clone();
            let cli = cli.clone();
            let cwd = dir.path().to_path_buf();
            tokio::spawn(async move {
                sessions.execute("conversation", &cli, None, &cwd, "hello", "hello", &[],
                    PermissionMode::Deny, &[], None, None, &[], cancel, |_| {}, |_| {}).await
            })
        };
        for _ in 0..100 {
            if marker.is_file() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(marker.is_file(), "fixture never received the prompt");
        cancel.cancel();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(8), worker).await
            .expect("cancellation should stop Claude promptly").unwrap();
        assert!(matches!(outcome, Err(Error::Cancelled)));
        assert!(!sessions.processes.lock().unwrap().contains_key("conversation"));
    }

    #[test]
    fn structured_text_only() {
        let pi = serde_json::json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"你好"}});
        assert_eq!(event_text_delta(Engine::Pi, &pi, false), Some("你好"));
        let oc = serde_json::json!({"type":"text","part":{"text":"done"}});
        assert_eq!(event_text_delta(Engine::OpenCode, &oc, false), Some("done"));
        let cc = serde_json::json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"text":"ok"}}});
        assert_eq!(event_text_delta(Engine::ClaudeCode, &cc, false), Some("ok"));
        let tool = serde_json::json!({"type":"tool_execution_start","toolCallId":"call_1","toolName":"read"});
        let parsed = event_tool(Engine::Pi, &tool).unwrap();
        assert_eq!(parsed.id, "call_1");
        assert!(!parsed.finished);
        let opencode_tool = serde_json::json!({"type":"tool_use","part":{
            "id":"part_1","tool":"bash","state":{"status":"completed","output":"ok"}
        }});
        let parsed = event_tool(Engine::OpenCode, &opencode_tool).unwrap();
        assert!(parsed.finished);
        assert_eq!(parsed.name, "bash");
    }

    #[test]
    fn opencode_restricted_config_keeps_model_and_denies_mutations() {
        let source = serde_json::json!({"provider":{"dandelion":{"name":"Dandelion"}}});
        let config = restricted_opencode_config(&[("OPENCODE_CONFIG_CONTENT".into(), source.to_string())]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&config).unwrap();
        assert_eq!(parsed["provider"]["dandelion"]["name"], "Dandelion");
        assert_eq!(parsed["permission"]["edit"], "deny");
        assert_eq!(parsed["permission"]["bash"], "deny");
        assert_eq!(parsed["permission"]["dandelion_dandelion_knowledge_search"], "allow");
        assert_eq!(parsed["permission"]["*"], "deny");
        assert!(parsed["permission"].get("dandelion_*").is_none());
        assert_eq!(parsed["agent"]["plan"]["permission"]["task"], "deny");
        assert_eq!(parsed["agent"]["plan"]["permission"]["read"], "allow");
    }

    #[test]
    fn discovers_workspace_cli_and_respects_disconnect() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join(".benchmarks/tools/node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let fixture = bin_dir.join(if cfg!(windows) { "agent-fixture.exe" } else { "agent-fixture" });
        std::fs::write(&fixture, "fixture").unwrap();
        assert_eq!(discover_executable("agent-fixture", &[dir.path().to_path_buf()]), Some(fixture));

        let claude = bin_dir.join(if cfg!(windows) { "claude.exe" } else { "claude" });
        std::fs::write(&claude, "fixture").unwrap();
        let roots = [dir.path().to_path_buf()];
        assert!(list(dir.path(), &roots).unwrap().iter().any(|item| item.id == "claude_code" && item.ready));
        uninstall(dir.path(), "claude_code").unwrap();
        assert!(list(dir.path(), &roots).unwrap().iter().any(|item| item.id == "claude_code" && !item.ready));
        connect_existing(dir.path(), "claude_code", None, &roots).unwrap();
        assert!(list(dir.path(), &roots).unwrap().iter().any(|item| item.id == "claude_code" && item.ready));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn connected_plugin_streams_and_stays_opt_in() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("fake-pi");
        std::fs::write(&cli, "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *handoff*) ;; *) exit 7;; esac\nprintf '%s\\n' '{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"delta\":\"你好\"}}'\nprintf '%s\\n' '{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"delta\":\"世界\"}}'\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        connect_existing(dir.path(), "pi", Some(cli.clone()), &[]).unwrap();
        assert!(list(dir.path(), &[]).unwrap().iter().any(|p| p.id == "pi" && p.ready));
        let mut chunks = Vec::new();
        let result = execute(Engine::Pi, &cli, None, dir.path(), "handoff", &[], PermissionMode::Auto, CancellationToken::new(), |chunk| chunks.push(chunk.to_string()), |_| {}).await.unwrap();
        assert_eq!(result, "你好世界");
        assert_eq!(chunks, ["你好", "世界"]);
        uninstall(dir.path(), "pi").unwrap();
        assert!(executable(dir.path(), Engine::Pi, &[]).is_err());
        assert!(cli.is_file());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_kills_descendants() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("child-agent");
        let marker = dir.path().join("survived");
        std::fs::write(&cli, format!(
            "#!/bin/sh\ncat >/dev/null\n(sleep 1; touch '{}') &\nsleep 30\n",
            marker.display()
        )).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            trigger.cancel();
        });
        let result = execute(Engine::Pi, &cli, None, dir.path(), "task", &[], PermissionMode::Auto, cancel, |_| {}, |_| {}).await;
        assert!(matches!(result, Err(Error::Cancelled)));
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
        assert!(!marker.exists());
    }

    #[test]
    fn claude_persistent_args_bridge_approvals_and_mirror_allowlist() {
        let args = claude_persistent_args(
            PermissionMode::Ask,
            Some("some-model"),
            &[PathBuf::from("C:/trusted/logs")],
            None,
        );
        let joined = args.join(" ");
        assert!(args.iter().any(|arg| arg == "--print"), "{joined}");
        assert!(!joined.contains("--permission-prompts"), "{joined}: flag is unknown to Claude Code CLI");
        assert!(joined.contains("--permission-prompt-tool stdio"),
            "{joined}: stdio is the flag that actually opens the can_use_tool channel");
        assert!(!joined.contains("none"), "{joined}");
        assert!(joined.contains("--add-dir C:/trusted/logs"), "{joined}");
        assert!(joined.contains("--model some-model"), "{joined}");
        assert!(!joined.contains("--permission-mode"), "Ask keeps the CLI default mode");
        let deny = claude_persistent_args(PermissionMode::Deny, None, &[], None);
        assert!(deny.windows(2).any(|w| w == ["--permission-mode", "dontAsk"]));
        // The wiki bridge rides in as a session-scoped --mcp-config file; it
        // is part of the process identity, so a rotated grant respawns.
        let bridged = claude_persistent_args(PermissionMode::Ask, None, &[],
            Some(Path::new("C:/data/wiki/bridge/claude-mcp-tok.json")));
        assert!(bridged.windows(2).any(|w| w == ["--allowedTools", "mcp__dandelion__dandelion_knowledge_search"]));
        assert!(bridged.windows(2).any(|w| w
            == ["--mcp-config", "C:/data/wiki/bridge/claude-mcp-tok.json"]), "{}", bridged.join(" "));
    }

    #[tokio::test]
    async fn can_use_tool_maps_three_approval_tiers() {
        use crate::host::RecordingHost;
        let dir = tempfile::tempdir().unwrap();
        let sid = SessionId::new().to_string();
        let cancel = CancellationToken::new();
        let request = serde_json::json!({
            "subtype": "can_use_tool",
            "tool_name": "Read",
            "input": { "file_path": "C:/outside/a.log" },
        });

        let allow_all = std::sync::Mutex::new(std::collections::HashSet::new());

        let once = RecordingHost::new(dir.path().to_path_buf());
        let approval = ClaudeApproval { host: &once, session_id: &sid, permission_timeout_sec: 5, allow_all: &allow_all };
        let response = can_use_tool_response(&request, Some(&approval), &cancel).await;
        assert_eq!(response["behavior"], "allow");
        assert_eq!(response["updatedInput"]["file_path"], "C:/outside/a.log");
        assert!(response["updatedPermissions"].is_null(), "allow-once must not add rules");
        assert!(once.call_titles()[0].contains("C:/outside/a.log"), "title shows the target");
        // allow-once 不写宿主放行表：下一条请求照常弹窗。
        assert!(!allow_all.lock().unwrap().contains(&sid));
        let response = can_use_tool_response(&request, Some(&approval), &cancel).await;
        assert_eq!(response["behavior"], "allow");
        assert_eq!(once.call_titles().len(), 2, "allow-once prompts again next time");

        let turn = RecordingHost::new(dir.path().to_path_buf()).with_option("allow_turn");
        let approval = ClaudeApproval { host: &turn, session_id: &sid, permission_timeout_sec: 5, allow_all: &allow_all };
        let response = can_use_tool_response(&request, Some(&approval), &cancel).await;
        assert_eq!(response["behavior"], "allow");
        assert_eq!(response["updatedPermissions"][0]["type"], "addRules");
        assert_eq!(response["updatedPermissions"][0]["rules"][0]["toolName"], "Read");
        assert_eq!(response["updatedPermissions"][0]["destination"], "session");
        // 宿主权威记档：同一条任务里的后续请求不再弹窗，直接放行 —— 哪怕换了
        // 工具（CLI 可能丢弃我们回传的 session 规则，放行不能依赖它）。
        let other_tool = serde_json::json!({ "tool_name": "Bash", "input": { "command": "git status" } });
        let response = can_use_tool_response(&other_tool, Some(&approval), &cancel).await;
        assert_eq!(response["behavior"], "allow");
        assert!(response["updatedPermissions"].is_null());
        assert_eq!(turn.call_titles().len(), 1, "allow_turn must not prompt again in this task");

        let denied = RecordingHost::new(dir.path().to_path_buf()).deny();
        let other_session = SessionId::new().to_string();
        let approval = ClaudeApproval { host: &denied, session_id: &other_session, permission_timeout_sec: 5, allow_all: &allow_all };
        let response = can_use_tool_response(&request, Some(&approval), &cancel).await;
        assert_eq!(response["behavior"], "deny");
        assert!(response["message"].as_str().is_some_and(|m| m.contains("allowRoots")),
            "放行表按会话隔离：别的会话不受上一条 allow_turn 影响");

        let headless = RecordingHost::new(dir.path().to_path_buf()).non_interactive();
        // 用未放行的会话：非交互宿主必须在「查表未命中」后才走无审批通道拒绝。
        let approval = ClaudeApproval { host: &headless, session_id: &other_session, permission_timeout_sec: 5, allow_all: &allow_all };
        let response = can_use_tool_response(&request, Some(&approval), &cancel).await;
        assert_eq!(response["behavior"], "deny");
        assert!(headless.call_titles().is_empty(), "non-interactive host never prompts");

        let response = can_use_tool_response(&request, None, &cancel).await;
        assert_eq!(response["behavior"], "deny");
    }
}
