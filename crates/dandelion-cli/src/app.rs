use crate::cli::{
    Cli, Command, CorpusCommand, CronCommand, EvalCommand, McpCommand, McpSecretCommand,
    MemoryCommand, OutputFormat, SessionCommand, SkillsCommand, SpecCommand, WikiCommand,
    WikiEntryKind,
};
use clap::Parser;
use dandelion_core::acp::{SessionState, SessionUpdate};
use dandelion_core::host::CliHost;
use dandelion_core::secret::{CredentialStore, FileCredentialStore};
use dandelion_core::{
    PermissionMode, PromptRequest, Runtime, SessionId, SessionPrefs, Settings, SettingsOverlay,
    SpecMode, ThinkingIntensity, CLI_BIN, PRODUCT_NAME,
};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

type CmdResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// `--data-dir` / `DANDELION_DATA_DIR`, stored once by `run`. Every command
/// builds its host through `cli_host`, so one flag relocates skills, wiki,
/// sessions and settings together — an isolated smoke run cannot touch the
/// real product data dir. Unset keeps the platform default.
static CLI_DATA_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// The `CliHost` for this invocation. Interactivity matches stdin either
/// way: only the data directory is redirected, never the TTY behaviour.
pub(crate) fn cli_host() -> CliHost {
    match CLI_DATA_DIR.get() {
        Some(dir) => {
            use std::io::IsTerminal;
            CliHost::with_data_dir_interactive(dir.clone(), std::io::stdin().is_terminal())
        }
        None => CliHost::new(),
    }
}

pub async fn run() -> CmdResult<()> {
    let cli = Cli::parse();
    if let Some(dir) = cli.data_dir.clone() {
        let _ = CLI_DATA_DIR.set(dir);
    }
    init_tracing(cli.log.as_deref());

    match cli.command {
        Command::Version => {
            println!(
                "{PRODUCT_NAME} {v} ({CLI_BIN})",
                v = dandelion_core::VERSION
            );
        }
        Command::Doctor => doctor().await?,
        Command::Skills { command } => match command {
            SkillsCommand::List => skills_list()?,
            SkillsCommand::Import { path } => skills_import(&path)?,
            SkillsCommand::Install { url, subdir, git } => {
                skills_install(&url, subdir.as_deref(), git).await?
            }
            SkillsCommand::Remove { id } => skills_remove(&id)?,
        },
        Command::Mcp { command } => match command {
            McpCommand::List => mcp_list().await?,
            McpCommand::Start { id } => mcp_start(&id).await?,
            McpCommand::Stop { id } => mcp_stop(&id).await?,
            McpCommand::Search { query, cursor } => mcp_search(&query, cursor.as_deref()).await?,
            McpCommand::Install { name, version } => mcp_install(&name, version.as_deref()).await?,
            McpCommand::Remove { id } => mcp_remove(&id).await?,
            McpCommand::Secret { command } => match command {
                McpSecretCommand::Put { server, name } => mcp_secret_put(&server, &name)?,
                McpSecretCommand::List => mcp_secret_list()?,
                McpSecretCommand::Remove { server, name } => mcp_secret_remove(&server, &name)?,
            },
        },
        Command::Session { command } => session_cmd(command).await?,
        Command::Cron { command } => cron_cmd(command).await?,
        Command::Corpus { command } => corpus_cmd(command)?,
        Command::Memory { command } => memory_cmd(command)?,
        Command::Compact { session } => compact_cmd(&session).await?,
        Command::Spec { command } => spec_cmd(command).await?,
        Command::Wiki { command } => wiki_cmd(command).await?,
        Command::Eval { command } => eval_cmd(command).await?,
        Command::Acp => crate::acp_stdio::run().await?,
        Command::Serve { bind } => crate::serve::run(bind).await?,
        Command::Chat {
            prompt,
            session,
            format,
            workspace,
            permission_mode,
            base_url,
            model,
            api_key,
            temperature,
            max_tokens,
            max_iterations,
            thinking_intensity,
            skill,
            mcp,
            corpus,
        } => {
            chat(ChatOpts {
                prompt,
                session: session.clone(),
                format,
                overlay: overlay(
                    workspace,
                    permission_mode,
                    base_url,
                    model,
                    api_key,
                    temperature,
                    max_tokens,
                    max_iterations,
                    thinking_intensity,
                )?,
                skill_ids: skill,
                mcp_server_ids: mcp,
                corpus_ids: corpus,
                persist_bindings: session.is_some(),
            })
            .await?;
        }
    }
    Ok(())
}

/// Usage / could-not-run evaluation failures map to exit code 2, so CI can
/// distinguish "the suite ran and something failed" (1) from "nothing ran"
/// (bad suite name, unreadable report, missing git…).
#[derive(Debug)]
pub struct EvalUsageError(String);
impl std::fmt::Display for EvalUsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for EvalUsageError {}

fn usage_err(msg: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(EvalUsageError(msg.into()))
}

/// Read + strict version-check a JSONL report file; unreadable or malformed
/// reports are exit-2 usage errors, never a silent empty comparison.
fn load_eval_report(
    path: &std::path::Path,
) -> CmdResult<Vec<dandelion_core::eval::report::EvalEvent>> {
    use dandelion_core::eval::report::EvalEvent;
    let raw = std::fs::read_to_string(path)
        .map_err(|err| usage_err(format!("cannot read {}: {err}", path.display())))?;
    EvalEvent::parse_report(&raw).map_err(|err| usage_err(format!("{}: {err}", path.display())))
}

/// Pure compare path (load → parse → gate), sharing the loader with the CLI
/// arm. Keeps the exit-code contract unit-testable without spawning `dln`.
#[cfg(test)]
pub fn eval_compare_for_test(baseline: &str, candidate: &str) -> CmdResult<()> {
    use dandelion_core::eval::report::Comparison;
    let baseline = load_eval_report(std::path::Path::new(baseline))?;
    let candidate = load_eval_report(std::path::Path::new(candidate))?;
    let comparison =
        Comparison::between(&baseline, &candidate).map_err(|err| usage_err(err.to_string()))?;
    if !comparison.is_acceptable() {
        return Err("eval compare: regressions or removed cases".into());
    }
    Ok(())
}

async fn eval_cmd(command: EvalCommand) -> CmdResult<()> {
    use dandelion_core::eval::report::{Comparison, EvalEvent};
    match command {
        EvalCommand::Smoke { report } => {
            let root = std::env::temp_dir()
                .join(format!("dandelion-eval-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&root)?;
            let suite = dandelion_core::eval::run_scripted_suite(
                &root,
                dandelion_core::eval::smoke_suite(),
            )
            .await?;
            dandelion_core::eval::write_suite_report(&report, &suite)?;
            println!(
                "{} {}",
                if suite.passed { "PASS" } else { "FAIL" },
                report.display()
            );
            if !suite.passed {
                return Err("evaluation smoke suite failed".into());
            }
        }
        EvalCommand::Run { suite, jsonl } => {
            let cases = match suite.as_str() {
                "smoke" => dandelion_core::eval::smoke_suite(),
                other => {
                    return Err(usage_err(format!(
                        "unknown eval suite '{other}' (built-in: smoke)"
                    )))
                }
            };
            let root = std::env::temp_dir()
                .join(format!("dandelion-eval-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&root)
                .map_err(|err| usage_err(format!("cannot create eval root: {err}")))?;
            let mut events: Vec<EvalEvent> = Vec::new();
            let result = dandelion_core::eval::run_scripted_suite_stream(
                &root,
                &suite,
                cases,
                &mut |event| {
                    print!("{}", event.to_json_line());
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                    events.push(event.clone());
                },
            )
            .await
            .map_err(|err| usage_err(format!("evaluation could not run: {err}")))?;
            dandelion_core::eval::write_jsonl_report(&jsonl, &events)
                .map_err(|err| usage_err(format!("cannot write report: {err}")))?;
            println!("report: {}", jsonl.display());
            if !result.passed {
                return Err(format!("evaluation suite '{suite}' failed").into());
            }
        }
        EvalCommand::Live {
            jsonl,
            model,
            base_url,
        } => {
            use dandelion_core::eval::live;
            use std::sync::Arc;
            // Explicit opt-in only: this arm makes real model calls and costs
            // real tokens. Mirrors `tests/live_g1.rs` (DANDELION_LIVE_TEST=1).
            if !live::live_enabled() {
                return Err(usage_err(
                    "live eval calls a real model endpoint; set DANDELION_LIVE_TEST=1 to allow it",
                ));
            }
            let (base_url, model) = live::resolve_endpoint(base_url, model);
            // The pairing string is content-free (model + backend + posture,
            // never the URL or a key) and is what `compare` later enforces.
            let pairing = live::live_pairing(&model);
            let root = std::env::temp_dir().join(format!(
                "dandelion-eval-live-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&root)
                .map_err(|err| usage_err(format!("cannot create eval root: {err}")))?;
            let build_settings =
                |case_root: std::path::PathBuf| live::live_settings(case_root, &base_url, &model);
            let provider = Arc::new(
                dandelion_core::provider::OpenAiSse::from_settings(&build_settings(root.clone()))
                    .map_err(|err| usage_err(format!("live provider could not be built: {err}")))?,
            );
            let mut events: Vec<EvalEvent> = Vec::new();
            let result = live::run_live_suite_stream(
                &root,
                &pairing,
                live::live_suite(),
                &move || Ok(Arc::clone(&provider) as Arc<dyn dandelion_core::provider::Provider>),
                &build_settings,
                &mut |event| {
                    print!("{}", event.to_json_line());
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                    events.push(event.clone());
                },
            )
            .await
            .map_err(|err| usage_err(format!("live evaluation could not run: {err}")))?;
            dandelion_core::eval::write_jsonl_report(&jsonl, &events)
                .map_err(|err| usage_err(format!("cannot write report: {err}")))?;
            println!("report: {} (pairing: {pairing})", jsonl.display());
            if !result.passed {
                return Err(format!(
                    "live suite failed: {} passed, {} failed",
                    result.passed_cases, result.failed_cases
                )
                .into());
            }
        }
        EvalCommand::Compare {
            baseline,
            candidate,
        } => {
            let baseline = load_eval_report(&baseline)?;
            let candidate = load_eval_report(&candidate)?;
            let comparison = Comparison::between(&baseline, &candidate)
                .map_err(|err| usage_err(err.to_string()))?;
            print!("{}", comparison.render());
            if !comparison.is_acceptable() {
                return Err("eval compare: regressions or removed cases".into());
            }
        }
    }
    Ok(())
}

struct ChatOpts {
    prompt: Option<String>,
    session: Option<String>,
    format: OutputFormat,
    overlay: SettingsOverlay,
    skill_ids: Vec<String>,
    mcp_server_ids: Vec<String>,
    corpus_ids: Vec<String>,
    persist_bindings: bool,
}

fn overlay(
    workspace: Option<PathBuf>,
    permission_mode: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    max_iterations: Option<u32>,
    thinking_intensity: Option<String>,
) -> CmdResult<SettingsOverlay> {
    Ok(SettingsOverlay {
        workspace,
        permission_mode: permission_mode
            .as_deref()
            .map(PermissionMode::parse)
            .transpose()?,
        base_url,
        default_model: model,
        api_key,
        temperature,
        max_tokens,
        max_iterations,
        thinking_intensity: thinking_intensity
            .as_deref()
            .map(ThinkingIntensity::parse)
            .transpose()?,
        allow_shell: None,
    })
}

fn init_tracing(filter: Option<&str>) {
    let filter = filter.unwrap_or("dandelion_cli=info,dandelion_core=info");
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

async fn doctor() -> CmdResult<()> {
    let host = cli_host();
    let data_dir = host.data_dir();
    let _ = Settings::write_template_if_absent(&data_dir);
    let rt = Runtime::new(Arc::new(host))?;
    let info = rt.info();
    let settings = rt.settings();
    println!("{PRODUCT_NAME} doctor");
    println!("  version     {}", info.version);
    println!("  cli         {CLI_BIN}");
    println!("  host        {:?}", info.host);
    println!("  data_dir    {}", info.data_dir.display());
    println!("  workspace   {}", info.workspace.display());
    println!(
        "  agent_loop  {}",
        if info.loop_ready {
            "ready"
        } else {
            "not configured — set dandelion.baseUrl and dandelion.defaultModel"
        }
    );
    println!(
        "  base_url    {}",
        if info.base_url_set { "set" } else { "missing" }
    );
    println!(
        "  model       {}",
        if info.model_set { "set" } else { "missing" }
    );
    println!(
        "  api_key     {}",
        if info.api_key_set { "set" } else { "not set" }
    );
    println!("  thinking    {}", settings.thinking_intensity.as_str());
    let skills = rt.skills_list(Some(&info.workspace)).unwrap_or_default();
    let user_skills: Vec<_> = skills.iter().filter(|i| !i.framework).collect();
    let skills_enabled = user_skills.iter().filter(|i| i.enabled).count();
    let skills_disabled = user_skills.len() - skills_enabled;
    let mcp_items = rt.mcp_list().await.unwrap_or_default();
    let mcp_ready = mcp_items.iter().filter(|i| i.ready).count();
    let mcp_failed = mcp_items.iter().filter(|i| i.enabled && !i.ready).count();
    println!(
        "  skills {skills_enabled} enabled {skills_disabled} disabled / mcp {mcp_ready} ready {mcp_failed} failed"
    );
    if !info.missing.is_empty() {
        println!("  missing     {}", info.missing.join(", "));
        println!("  hint        copy examples/settings.g1.json into data_dir/settings.json");
        println!("              or set DANDELION_BASE_URL and DANDELION_MODEL");
    }
    println!(
        "  desktop     apps/desktop embeds dandelion-core in-process (not a {CLI_BIN} sidecar)"
    );
    println!("  packaging   one installer; optional extra {CLI_BIN}.exe next to the GUI");
    Ok(())
}

async fn compact_cmd(session: &str) -> CmdResult<()> {
    let rt = Runtime::new(Arc::new(cli_host()))?;
    let sid = SessionId::parse(session.to_string())?;
    rt.compact(&sid).await?;
    println!("compacted {session}");
    Ok(())
}

async fn spec_cmd(command: SpecCommand) -> CmdResult<()> {
    match command {
        SpecCommand::Ask {
            prompt,
            session,
            workspace,
            permission_mode,
            format,
        } => {
            spec_start(
                SpecMode::Ask,
                prompt,
                session,
                workspace,
                permission_mode,
                format,
            )
            .await
        }
        SpecCommand::Plan {
            prompt,
            session,
            workspace,
            permission_mode,
            format,
        } => {
            spec_start(
                SpecMode::Plan,
                prompt,
                session,
                workspace,
                permission_mode,
                format,
            )
            .await
        }
        SpecCommand::Auto {
            prompt,
            session,
            workspace,
            permission_mode,
            format,
        } => {
            spec_start(
                SpecMode::Auto,
                prompt,
                session,
                workspace,
                permission_mode,
                format,
            )
            .await
        }
        SpecCommand::Revise {
            session,
            prompt,
            workspace,
            format,
        } => spec_revise(session, prompt, workspace, format).await,
        SpecCommand::Diff { session } => spec_diff(&session),
        SpecCommand::Show { session } => spec_show(&session),
        SpecCommand::Approve { session, format } => spec_approve(session, format).await,
        SpecCommand::Run {
            session,
            prompt,
            workspace,
            format,
        } => spec_run(session, prompt, workspace, format).await,
    }
}

fn spec_overlay(
    workspace: Option<PathBuf>,
    permission_mode: Option<String>,
) -> CmdResult<SettingsOverlay> {
    overlay(
        workspace,
        permission_mode,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

fn spec_req(session_id: SessionId, text: String, overlay: &SettingsOverlay) -> PromptRequest {
    let mut req = PromptRequest::new(session_id, text);
    req.workspace = overlay.workspace.clone();
    req.permission_mode = overlay.permission_mode;
    req
}

async fn spec_start(
    mode: SpecMode,
    prompt: String,
    session: Option<String>,
    workspace: Option<PathBuf>,
    permission_mode: Option<String>,
    format: OutputFormat,
) -> CmdResult<()> {
    let overlay = spec_overlay(workspace, permission_mode)?;
    let rt = Runtime::new_with_overlay(Arc::new(cli_host()), overlay.clone())?;
    let session_id = match session {
        Some(raw) => SessionId::parse(raw)?,
        None => rt.new_session(None),
    };
    let req = spec_req(session_id.clone(), prompt, &overlay);
    let rt_task = rt.clone();
    let handle = pump_spec(&rt, session_id.clone(), format, async move {
        rt_task.spec_start(mode, req).await
    })
    .await?;
    println!("spec {} session {}", handle.spec_id, handle.session_id);
    Ok(())
}

async fn spec_revise(
    session: String,
    prompt: String,
    workspace: Option<PathBuf>,
    format: OutputFormat,
) -> CmdResult<()> {
    let overlay = spec_overlay(workspace, None)?;
    let rt = Runtime::new_with_overlay(Arc::new(cli_host()), overlay.clone())?;
    let sid = SessionId::parse(session)?;
    let spec_id = spec_id_for(&rt, &sid)?;
    let req = spec_req(sid.clone(), prompt, &overlay);
    let spec_id_owned = spec_id.clone();
    let rt_task = rt.clone();
    let diff = pump_spec(&rt, sid, format, async move {
        rt_task.spec_revise(&spec_id_owned, req).await
    })
    .await?;
    println!(
        "spec {spec_id} v{} → v{}",
        diff.version_from, diff.version_to
    );
    Ok(())
}

fn spec_diff(session: &str) -> CmdResult<()> {
    let rt = Runtime::new(Arc::new(cli_host()))?;
    let sid = SessionId::parse(session.to_string())?;
    let spec_id = spec_id_for(&rt, &sid)?;
    let snap = rt.spec_show(&spec_id)?;
    if let Some(md) = snap.diff_markdown {
        print!("{md}");
    } else {
        let diff = rt.spec_diff(&spec_id)?;
        print!("{}", diff.render_markdown());
    }
    Ok(())
}

fn spec_show(session: &str) -> CmdResult<()> {
    let rt = Runtime::new(Arc::new(cli_host()))?;
    let sid = SessionId::parse(session.to_string())?;
    let spec_id = spec_id_for(&rt, &sid)?;
    let snap = rt.spec_show(&spec_id)?;
    println!(
        "{}\t{}\tv{}\t{}",
        snap.spec_id,
        snap.status.as_str(),
        snap.version,
        snap.path
    );
    if let Some(spec) = snap.spec {
        println!("goal\t{}", spec.goal);
    }
    if let Some(plan) = snap.plan {
        println!("steps\t{}", plan.steps.len());
    }
    Ok(())
}

async fn spec_approve(session: String, format: OutputFormat) -> CmdResult<()> {
    let rt = Runtime::new(Arc::new(cli_host()))?;
    let sid = SessionId::parse(session)?;
    let spec_id = spec_id_for(&rt, &sid)?;
    let spec_id_owned = spec_id.clone();
    let rt_task = rt.clone();
    let snap = pump_spec(&rt, sid, format, async move {
        rt_task.spec_approve(&spec_id_owned).await
    })
    .await?;
    println!("{}\t{}", snap.spec_id, snap.status.as_str());
    Ok(())
}

async fn spec_run(
    session: String,
    prompt: Option<String>,
    workspace: Option<PathBuf>,
    format: OutputFormat,
) -> CmdResult<()> {
    let overlay = spec_overlay(workspace, None)?;
    let rt = Runtime::new_with_overlay(Arc::new(cli_host()), overlay.clone())?;
    let sid = SessionId::parse(session)?;
    let spec_id = spec_id_for(&rt, &sid)?;
    let text =
        prompt.unwrap_or_else(|| "Execute the approved PLAN.md using workspace tools.".into());
    let req = spec_req(sid.clone(), text, &overlay);
    let spec_id_owned = spec_id.clone();
    let rt_task = rt.clone();
    pump_spec(&rt, sid.clone(), format, async move {
        rt_task.spec_run(&spec_id_owned, req).await
    })
    .await?;
    println!("ran {spec_id} session {sid}");
    Ok(())
}

fn spec_id_for(rt: &Runtime, session: &SessionId) -> CmdResult<String> {
    let row = rt
        .spec_for_session(session)?
        .ok_or_else(|| format!("no spec for session {session}"))?;
    Ok(row)
}

async fn pump_spec<T: Send + 'static>(
    rt: &Runtime,
    session_id: SessionId,
    format: OutputFormat,
    fut: impl std::future::Future<Output = dandelion_core::Result<T>> + Send + 'static,
) -> CmdResult<T> {
    let mut rx = rt.subscribe();
    let mut work = tokio::spawn(fut);
    let mut text_state = crate::render::TextRenderState::default();
    let mut done = false;
    let mut out: Option<dandelion_core::Result<T>> = None;
    let mut cancelled_once = false;
    loop {
        tokio::select! {
            frame = rx.recv(), if !done => {
                match frame {
                    Ok(frame) => crate::render::print_frame(format, &frame, &mut text_state)?,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
            res = &mut work, if out.is_none() => {
                done = true;
                match res {
                    Ok(r) => out = Some(r),
                    Err(join) => return Err(join.into()),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                if cancelled_once {
                    std::process::exit(130);
                }
                cancelled_once = true;
                let _ = rt.cancel(session_id.clone()).await;
            }
        }
        if done && out.is_some() {
            break;
        }
    }
    crate::render::finish_text(format, &mut text_state)?;
    match out {
        Some(Ok(v)) => Ok(v),
        Some(Err(err)) => Err(err.to_string().into()),
        None => Err("spec pipeline did not finish".into()),
    }
}

fn corpus_cmd(command: CorpusCommand) -> CmdResult<()> {
    let rt = Runtime::new(Arc::new(cli_host()))?;
    match command {
        CorpusCommand::List => {
            for doc in rt.corpus_list()? {
                println!(
                    "{}\t{}\tchunks={}\t{}",
                    doc.id, doc.status, doc.chunks, doc.name
                );
            }
        }
        CorpusCommand::Add { path } => {
            let doc = rt.corpus_import(&path)?;
            println!("{}\t{} chunks", doc.id, doc.chunks);
        }
        CorpusCommand::Remove { id } => rt.corpus_delete(&id)?,
    }
    Ok(())
}

async fn wiki_cmd(command: WikiCommand) -> CmdResult<()> {
    let rt = Runtime::new(Arc::new(cli_host()))?;
    match command {
        WikiCommand::Status => {
            let st = rt.wiki_status();
            println!(
                "path={}\thas_compiled={}\tversion={}\tpages={}",
                st.path,
                st.has_compiled,
                st.version,
                st.pages
                    .iter()
                    .map(|p| format!("{}({})", p.file, p.title))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            if let Some(ws) = st.workspace {
                println!("workspace={ws}");
            }
            if let Some(at) = st.compiled_at {
                println!("compiled_at={at}");
            }
        }
        WikiCommand::Show { page } => {
            for page in rt.wiki_show(page.as_deref())? {
                println!("--- {}\n{}\n", page.name, page.body);
            }
        }
        WikiCommand::Compile {
            workspace,
            ai,
            incremental,
            space,
            subject,
        } => {
            // `--space` deep-builds a knowledge space from its published
            // documents: Editor gate → materialize the store mirror → the
            // same pipeline over the space's data dir (see
            // `Runtime::space_build_llm`). `--ai` is implied; there is no
            // space-keyed quick scan here.
            if let Some(sid) = space.as_deref() {
                let report = rt.space_build_llm(&subject, sid, incremental).await?;
                println!(
                    "space={sid}\tbuild={}\tpublished={}\tskipped_user_edited={}",
                    report.build,
                    report.published.join(","),
                    report.skipped_user_edited.join(",")
                );
                for w in &report.warnings {
                    println!("warning: {w}");
                }
                return Ok(());
            }
            // `--ai` now runs the DEEP build (overview + evidence-backed topic
            // articles, verified and published all-or-nothing). The legacy
            // single-overview `wiki_compile_llm` stays on Runtime for old
            // callers but is no longer a primary entry point — it must not
            // overwrite a deep-built project overview.
            if ai {
                let report = rt.wiki_build_llm(workspace.as_deref(), incremental).await?;
                println!(
                    "build={}\tpublished={}\tskipped_user_edited={}",
                    report.build,
                    report.published.join(","),
                    report.skipped_user_edited.join(",")
                );
                // Repairs (e.g. fabricated line numbers neutralized to
                // 引用待核实) must be visible, never silently swallowed.
                for w in &report.warnings {
                    println!("warning: {w}");
                }
            } else {
                let meta = rt.wiki_compile(workspace.as_deref())?;
                println!(
                    "version={}\tsource_files={}\tpages={}",
                    meta.version,
                    meta.source_files,
                    meta.pages.join(",")
                );
            }
        }
        WikiCommand::Search { query } => {
            for hit in rt.wiki_search(&query) {
                println!("{}\t{}", hit.page, hit.excerpt);
            }
        }
        WikiCommand::Ask { question, workspace } => {
            let result = rt.wiki_ask_for(workspace.as_deref(), &question).await?;
            println!("{}", result.answer);
            for (i, source) in result.sources.iter().enumerate() {
                println!("[{}] {} — {}", i + 1, source.page, source.excerpt);
            }
        }
        // `--space` routes to the space-keyed ledger (the same fail-closed
        // gate the desktop review uses: id existence, archived reject,
        // Reader/Editor roles); without it the workspace path is untouched.
        WikiCommand::Entries { workspace, space, subject } => {
            let entries = match space.as_deref() {
                Some(sid) => rt.space_entries(&subject, sid)?,
                None => rt.wiki_entries_for(workspace.as_deref()),
            };
            for e in entries {
                let turn = e.turn.map(|t| t.to_string()).unwrap_or_default();
                println!(
                    "{id}\t{status}\t{kind}\tturn={turn}\tsession={sid}\t{content}",
                    id = e.id,
                    status = e.status,
                    kind = e.kind,
                    sid = e.session_id,
                    content = e.content,
                );
            }
        }
        WikiCommand::Review { id, action, workspace, space, subject } => {
            match space.as_deref() {
                Some(sid) => rt.space_review_entry(&subject, sid, &id, &action)?,
                None => rt.wiki_review_entry(workspace.as_deref(), &id, &action)?,
            }
            println!("{action} ok (recompile queued when a workspace is linked)");
        }
        WikiCommand::Edit { id, content, kind, workspace, space, subject } => {
            let kind = kind.map(|k| match k {
                WikiEntryKind::Flow => "flow",
                WikiEntryKind::Decision => "decision",
                WikiEntryKind::Pitfall => "pitfall",
                WikiEntryKind::Term => "term",
            });
            if content.is_none() && kind.is_none() {
                return Err("wiki edit needs --content and/or --kind".into());
            }
            match space.as_deref() {
                Some(sid) => rt.space_edit_entry(&subject, sid, &id, content.as_deref(), kind)?,
                None => rt.wiki_edit_entry(workspace.as_deref(), &id, content.as_deref(), kind)?,
            }
            println!("edit ok (recompile queued when a workspace is linked)");
        }
        WikiCommand::Page { name, content, file, space, subject } => {
            // Exactly one body source: inline or file. The page identity and
            // every gate (Editor, archived, size cap, name shape) live in
            // core `space_page_save` — same entry the desktop uses.
            let body = match (content, file) {
                (Some(_), Some(_)) => {
                    return Err("wiki page needs exactly one of --content / --file".into())
                }
                (None, None) => return Err("wiki page needs --content or --file".into()),
                (Some(text), None) => text,
                (None, Some(path)) => std::fs::read_to_string(&path)
                    .map_err(|e| format!("read {}: {e}", path.display()))?,
            };
            let out = rt.space_page_save(&subject, &space, &name, &body)?;
            println!(
                "space={space}\tpage={}\tprotected={}\tversion={}",
                out.file, out.protected, out.version
            );
        }
        WikiCommand::Policy { space, value, subject } => {
            rt.space_set_policy(&subject, &space, &value)?;
            println!("space={space}\texternal_policy={value} (effective on the next decision)");
        }
    }
    Ok(())
}

fn skills_list() -> CmdResult<()> {
    let host = cli_host();
    let rt = Runtime::new(Arc::new(host))?;
    let ws = rt.info().workspace;
    for item in rt.skills_list(Some(&ws))? {
        let mark = if item.framework {
            " [framework]"
        } else if !item.enabled {
            " [disabled]"
        } else {
            ""
        };
        println!("{}{mark}\t{}", item.id, item.description);
    }
    Ok(())
}

fn skills_import(path: &PathBuf) -> CmdResult<()> {
    let host = cli_host();
    let rt = Runtime::new(Arc::new(host))?;
    let item = rt.skills_import(path)?;
    println!("{}\t{}", item.id, item.path.display());
    Ok(())
}

async fn skills_install(url: &str, subdir: Option<&str>, force_git: bool) -> CmdResult<()> {
    let host = cli_host();
    let rt = Runtime::new(Arc::new(host))?;
    let looks_like_file = url.trim().to_ascii_lowercase().ends_with(".md");
    let report = if looks_like_file && !force_git && subdir.is_none() {
        rt.skills_install_url(url).await?
    } else {
        rt.skills_install_git(url, subdir).await?
    };
    for item in &report.items {
        println!("{} [disabled]\t{}", item.id, item.path.display());
    }
    for note in &report.notes {
        println!("  {note}");
    }
    println!("  review the body, then: skills panel / settings dandelion.skills.disabledIds");
    Ok(())
}

fn skills_remove(id: &str) -> CmdResult<()> {
    let host = cli_host();
    let rt = Runtime::new(Arc::new(host))?;
    let ws = rt.info().workspace;
    let removed = rt.skills_remove(id, Some(&ws))?;
    println!("{id}\t{}", removed.display());
    Ok(())
}

async fn mcp_list() -> CmdResult<()> {
    let host = cli_host();
    let rt = Runtime::new(Arc::new(host))?;
    for item in rt.mcp_list().await? {
        let state = if !item.enabled {
            "disabled"
        } else if item.ready {
            "ready"
        } else if item.error.is_some() {
            "failed"
        } else {
            "stopped"
        };
        println!(
            "{}\t{state}\tlisted={} snapshot={} skipped={}",
            item.id,
            item.listed,
            item.in_snapshot,
            item.skipped_by_budget.len()
        );
        if let Some(err) = item.error {
            println!("  error: {err}");
        }
        for name in item.tool_names {
            println!("  {name}");
        }
    }
    Ok(())
}

async fn mcp_start(id: &str) -> CmdResult<()> {
    let host = cli_host();
    let rt = Runtime::new(Arc::new(host))?;
    rt.mcp_start(id).await?;
    Ok(())
}

async fn mcp_stop(id: &str) -> CmdResult<()> {
    let host = cli_host();
    let rt = Runtime::new(Arc::new(host))?;
    rt.mcp_stop(id).await?;
    Ok(())
}

async fn mcp_remove(id: &str) -> CmdResult<()> {
    let host = cli_host();
    let data_dir = host.data_dir();
    let rt = Runtime::new(Arc::new(host))?;
    rt.mcp_remove(id).await?;
    // Credentials for a removed server must not linger in the store.
    let store = FileCredentialStore::new(&data_dir);
    let prefix = format!("mcp__{}__", id.trim());
    for name in store.list() {
        if name.strip_prefix(&prefix).is_some() {
            store.delete(&name)?;
            println!("  removed secret {name}");
        }
    }
    Ok(())
}

fn mcp_secret_store() -> CmdResult<FileCredentialStore> {
    let host = cli_host();
    Ok(FileCredentialStore::new(&host.data_dir()))
}

fn mcp_secret_put(server: &str, name: &str) -> CmdResult<()> {
    let store = mcp_secret_store()?;
    // The value comes from stdin only: argv would leak it into shell history
    // and the process list, which is exactly what `secret:<name>` refs avoid.
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty() {
        return Err("empty secret read from stdin; e.g. printf '%s' \"$TOKEN\" | dln mcp secret put <server> <name>".into());
    }
    let full = dandelion_core::secret::mcp_secret_name(server, name)?;
    store.put(&full, value)?;
    println!("stored secret '{name}' for mcp server '{server}'");
    Ok(())
}

fn mcp_secret_list() -> CmdResult<()> {
    let store = mcp_secret_store()?;
    for name in store.list() {
        // api-key sealing lives outside this surface; show MCP entries only.
        if let Some(rest) = name.strip_prefix("mcp__") {
            println!("{rest}");
        }
    }
    Ok(())
}

fn mcp_secret_remove(server: &str, name: &str) -> CmdResult<()> {
    let store = mcp_secret_store()?;
    let full = dandelion_core::secret::mcp_secret_name(server, name)?;
    store.delete(&full)?;
    println!("removed secret '{name}' for mcp server '{server}'");
    Ok(())
}

async fn mcp_search(query: &str, cursor: Option<&str>) -> CmdResult<()> {
    let host = cli_host();
    let rt = Runtime::new(Arc::new(host))?;
    let page = rt.registry_mcp_search(query, cursor).await?;
    for e in &page.entries {
        let forms = e
            .variants
            .iter()
            .filter(|v| v.supported)
            .map(|v| v.label.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let forms = if forms.is_empty() {
            "unsupported".into()
        } else {
            forms
        };
        println!("{}\t{}\t{forms}\t{}", e.name, e.version, e.description);
    }
    if let Some(cursor) = &page.next_cursor {
        println!("  more: --cursor {cursor}");
    }
    Ok(())
}

async fn mcp_install(name: &str, version: Option<&str>) -> CmdResult<()> {
    let host = cli_host();
    let rt = Runtime::new(Arc::new(host))?;
    let plan = rt.registry_mcp_install(name, version).await?;
    let target = plan
        .server
        .url
        .clone()
        .unwrap_or_else(|| format!("{} {}", plan.server.command, plan.server.args.join(" ")));
    println!("{}\t{}", plan.server.id, target.trim());
    // A server installed without its required key would look connected and
    // expose almost nothing, so say plainly why it is off and what is needed.
    for need in &plan.missing_env {
        let kind = if need.secret { "secret" } else { "required" };
        println!("  needs env: {}\t{kind}\t{}", need.name, need.description);
    }
    for note in &plan.notes {
        println!("  {note}");
    }
    Ok(())
}

async fn chat(opts: ChatOpts) -> CmdResult<()> {
    let overlay = opts.overlay;
    let rt = Runtime::new_with_overlay(Arc::new(cli_host()), overlay.clone())?;
    let session_id = match opts.session {
        Some(raw) => SessionId::parse(raw)?,
        None => rt.new_session(None),
    };

    if let Some(text) = opts.prompt {
        run_prompt(
            &rt,
            session_id,
            text,
            opts.format,
            opts.skill_ids,
            opts.mcp_server_ids,
            opts.corpus_ids,
            opts.persist_bindings,
            &overlay,
        )
        .await?;
        return Ok(());
    }

    println!(
        "{PRODUCT_NAME} chat. Empty line exits. Ctrl+C cancels the current turn.\nSession {session_id}"
    );
    loop {
        let mut stdout = io::stdout();
        write!(stdout, "> ")?;
        stdout.flush()?;
        let mut line = String::new();
        let n = io::stdin().lock().read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let line = line.trim().to_string();
        if line.is_empty() {
            break;
        }
        run_prompt(
            &rt,
            session_id.clone(),
            line,
            opts.format,
            opts.skill_ids.clone(),
            opts.mcp_server_ids.clone(),
            opts.corpus_ids.clone(),
            opts.persist_bindings,
            &overlay,
        )
        .await?;
    }
    Ok(())
}

/// Subscribe first, then prompt concurrently so SSE chunks are not stuck in the bus.
async fn run_prompt(
    rt: &Runtime,
    session_id: SessionId,
    text: String,
    format: OutputFormat,
    skill_ids: Vec<String>,
    mcp_server_ids: Vec<String>,
    corpus_ids: Vec<String>,
    persist_bindings: bool,
    overlay: &SettingsOverlay,
) -> CmdResult<()> {
    let mut rx = rt.subscribe();
    let mut req = PromptRequest::new(session_id.clone(), text);
    req.skill_ids = skill_ids;
    req.mcp_server_ids = mcp_server_ids;
    req.corpus_ids = corpus_ids;
    req.persist_bindings = persist_bindings;
    req.workspace = overlay.workspace.clone();
    req.permission_mode = overlay.permission_mode;
    req.model = overlay.default_model.clone();
    req.temperature = overlay.temperature;
    req.max_tokens = overlay.max_tokens;
    req.thinking_intensity = overlay.thinking_intensity;
    let rt_task = rt.clone();
    let mut prompt = tokio::spawn(async move { rt_task.prompt(req).await });
    let mut text_state = crate::render::TextRenderState::default();
    let mut prompt_done = false;
    let mut saw_running = false;
    let mut idle = false;
    let mut prompt_err: Option<dandelion_core::Error> = None;
    let mut cancelled_once = false;

    loop {
        tokio::select! {
            frame = rx.recv(), if !idle => {
                match frame {
                    Ok(frame) => {
                        if matches!(
                            frame.params.update,
                            SessionUpdate::StateUpdate { state: SessionState::Running, .. }
                        ) {
                            saw_running = true;
                        }
                        crate::render::print_frame(format, &frame, &mut text_state)?;
                        if matches!(
                            frame.params.update,
                            SessionUpdate::StateUpdate { state: SessionState::Idle, .. }
                        ) {
                            idle = true;
                        }
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
            res = &mut prompt, if !prompt_done => {
                prompt_done = true;
                match res {
                    Ok(Ok(_)) => {}
                    Ok(Err(err)) => prompt_err = Some(err),
                    Err(join) => return Err(join.into()),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                if cancelled_once {
                    std::process::exit(130);
                }
                cancelled_once = true;
                let _ = rt.cancel(session_id.clone()).await;
            }
        }

        if let Some(err) = &prompt_err {
            if !saw_running {
                crate::render::finish_text(format, &mut text_state)?;
                return Err(err.to_string().into());
            }
        }
        if idle && prompt_done {
            break;
        }
        if prompt_done && prompt_err.is_some() && !saw_running {
            break;
        }
    }

    crate::render::finish_text(format, &mut text_state)?;

    if let Some(err) = prompt_err {
        return Err(err.to_string().into());
    }
    Ok(())
}

async fn session_cmd(command: SessionCommand) -> CmdResult<()> {
    let rt = Runtime::new(Arc::new(cli_host()))?;
    match command {
        SessionCommand::List { limit, offset } => {
            for row in rt.sessions_list(limit, offset)? {
                println!(
                    "{}\t{}\t{}\tdepth={}",
                    row.id, row.kind, row.title, row.depth
                );
            }
        }
        SessionCommand::Export { id } => {
            let sid = SessionId::parse(id)?;
            let v = rt.session_export(&sid)?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        SessionCommand::Search { query } => {
            for hit in rt.sessions_search(&query, None)? {
                let snip: String = hit.snippet.chars().take(80).collect();
                println!("{}\t{}\t{snip}", hit.id, hit.title);
            }
        }
        SessionCommand::SetTitle { id, title } => {
            let sid = SessionId::parse(id)?;
            rt.session_set_title(&sid, &title)?;
        }
        SessionCommand::Delete { id } => {
            let sid = SessionId::parse(id)?;
            rt.session_delete(&sid).await?;
        }
        SessionCommand::Prefs { id } => {
            let sid = SessionId::parse(id)?;
            let prefs = rt.session_prefs(&sid)?;
            println!("{}", serde_json::to_string_pretty(&prefs)?);
        }
        SessionCommand::SetPrefs {
            id,
            model,
            temperature,
            max_tokens,
            permission_mode,
            system_prompt,
            workspace,
        } => {
            let sid = SessionId::parse(id)?;
            let patch = SessionPrefs {
                agent_engine: None,
                model,
                temperature,
                max_tokens,
                permission_mode: permission_mode
                    .as_deref()
                    .map(PermissionMode::parse)
                    .transpose()?,
                system_prompt,
                workspace,
            };
            let saved = rt.session_merge_prefs(&sid, patch)?;
            println!("{}", serde_json::to_string_pretty(&saved)?);
        }
        SessionCommand::Occupy { id, surface } => {
            let sid = SessionId::parse(id)?;
            rt.occupy_session(&sid, &surface)?;
        }
        SessionCommand::Release { id, surface } => {
            let sid = SessionId::parse(id)?;
            rt.release_session(&sid, &surface)?;
        }
        SessionCommand::Duplicate { id } => {
            let sid = SessionId::parse(id)?;
            let copy = rt.session_duplicate(&sid)?;
            println!("{copy}");
        }
    }
    Ok(())
}

async fn cron_cmd(command: CronCommand) -> CmdResult<()> {
    let rt = Runtime::new(Arc::new(cli_host()))?;
    match command {
        CronCommand::List => {
            for job in rt.cron_list()? {
                let on = if job.enabled { "on" } else { "off" };
                println!("{}\t{on}\t{}\tnext={}", job.id, job.expr, job.next_run);
            }
        }
        CronCommand::Add {
            expr,
            prompt,
            workspace,
            model,
            missed_policy,
        } => {
            let job = rt.cron_add(&expr, &prompt, workspace, model, &missed_policy)?;
            println!("{}", job.id);
        }
        CronCommand::Pause { id } => rt.cron_pause(&id)?,
        CronCommand::Resume { id } => rt.cron_resume(&id)?,
        CronCommand::Remove { id } => rt.cron_remove(&id)?,
        CronCommand::Run { id } => {
            let sid = rt.cron_run_now(&id).await?;
            println!("{sid}");
        }
    }
    Ok(())
}

fn memory_cmd(command: MemoryCommand) -> CmdResult<()> {
    let rt = Runtime::new(Arc::new(cli_host()))?;
    match command {
        MemoryCommand::Show => {
            print!("{}", rt.memory_show()?);
        }
        MemoryCommand::Set { kind, file } => {
            let kind = dandelion_core::MemoryKind::parse(&kind)?;
            let body = if file == "-" {
                use std::io::Read;
                let mut buf = String::new();
                io::stdin().read_to_string(&mut buf)?;
                buf
            } else {
                std::fs::read_to_string(file)?
            };
            rt.memory_set(kind, &body)?;
        }
    }
    Ok(())
}
