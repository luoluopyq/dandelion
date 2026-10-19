use clap::{Parser, Subcommand, ValueEnum};
use dandelion_core::{CLI_BIN, VERSION};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = CLI_BIN,
    version = VERSION,
    about = "Dandelion CLI — thin host over dandelion-core (same runtime as the future Tauri app)"
)]
pub struct Cli {
    /// Log filter, e.g. `dandelion_core=debug`
    #[arg(long, global = true, env = "DLN_LOG")]
    pub log: Option<String>,

    /// Where skills, wiki, sessions and settings live for this invocation.
    /// Point it at a scratch directory to try the tool without touching the
    /// real user data dir; defaults to the platform product data dir.
    #[arg(long = "data-dir", global = true, env = "DANDELION_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Send a prompt through the shared runtime bus
    Chat {
        /// Non-interactive prompt
        #[arg(short = 'q', long = "prompt")]
        prompt: Option<String>,
        /// Resume / pin a session id (ACP sessionId)
        #[arg(short = 'c', long = "session")]
        session: Option<String>,
        /// How to print bus frames
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        #[arg(long, env = "DANDELION_WORKSPACE")]
        workspace: Option<PathBuf>,
        #[arg(long = "permission-mode", env = "DANDELION_PERMISSION_MODE")]
        permission_mode: Option<String>,
        #[arg(long = "base-url", env = "DANDELION_BASE_URL")]
        base_url: Option<String>,
        #[arg(long, env = "DANDELION_MODEL")]
        model: Option<String>,
        #[arg(long = "api-key", env = "DANDELION_API_KEY")]
        api_key: Option<String>,
        #[arg(long, env = "DANDELION_TEMPERATURE")]
        temperature: Option<f32>,
        #[arg(long = "max-tokens", env = "DANDELION_MAX_TOKENS")]
        max_tokens: Option<u32>,
        #[arg(long = "max-iterations", env = "DANDELION_MAX_ITERATIONS")]
        max_iterations: Option<u32>,
        /// Reasoning effort: xhigh (default), medium, low
        #[arg(long = "thinking-intensity", env = "DANDELION_THINKING_INTENSITY")]
        thinking_intensity: Option<String>,
        /// Preload skill id or SKILL.md path for this turn (repeatable)
        #[arg(short = 's', long = "skill")]
        skill: Vec<String>,
        /// Enable MCP server id for this turn (repeatable)
        #[arg(long = "mcp")]
        mcp: Vec<String>,
        /// Bind corpus document id for this turn (repeatable). Unbound = no retrieval.
        #[arg(long = "corpus")]
        corpus: Vec<String>,
    },
    /// List skills under data_dir/skills and workspace/.dandelion/skills
    Skills {
        #[command(subcommand)]
        command: SkillsCommand,
    },
    /// MCP servers from settings.json
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// List / export / search SQLite sessions
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Cron jobs (process-local ticker)
    Cron {
        #[command(subcommand)]
        command: CronCommand,
    },
    /// User-document corpus (sqlite-vec). Not wiki.
    Corpus {
        #[command(subcommand)]
        command: CorpusCommand,
    },
    /// Soul / user / memory files
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Compact older turns for a session (no new user prompt)
    Compact {
        /// Session id
        #[arg(short = 'c', long = "session")]
        session: String,
    },
    /// Spec pipeline (C16). Does not change `dln chat`.
    Spec {
        #[command(subcommand)]
        command: SpecCommand,
    },
    /// Project wiki under data_dir (C17). Not corpus.
    Wiki {
        #[command(subcommand)]
        command: WikiCommand,
    },
    /// Run deterministic offline agent-loop evaluation suites
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
    /// Stdio ACP Agent (G6 C14). Default off.
    Acp,
    /// OpenAI-compat HTTP host (G6 C15). Default bind 127.0.0.1 only.
    Serve {
        /// Override bind. Non-loopback requires this flag explicitly.
        #[arg(long)]
        bind: Option<String>,
    },
    /// Show host, data dir, and whether the agent loop is configured
    Doctor,
    /// Print version (core and CLI share one workspace version)
    Version,
}

#[derive(Debug, Subcommand)]
pub enum EvalCommand {
    /// Run the built-in, network-free smoke suite
    Smoke {
        /// Destination JSON report (must be outside any real project fixture)
        #[arg(long)]
        report: PathBuf,
    },
    /// Run a built-in suite, streaming a versioned JSONL report (H5).
    /// Exit codes: 0 all passed, 1 a case failed, 2 usage or could-not-run.
    Run {
        /// Built-in suite name (currently: smoke)
        #[arg(long, default_value = "smoke")]
        suite: String,
        /// Destination JSONL report; events also go to stdout as they happen
        #[arg(long)]
        jsonl: PathBuf,
    },
    /// Run the live-model suite (H5b) against a real endpoint, streaming the
    /// same JSONL report as `run`. Requires DANDELION_LIVE_TEST=1; cases are
    /// scored by resulting files, duration is recorded but never gates.
    /// Exit codes: 0 all passed, 1 a case failed, 2 gate off / could-not-run.
    Live {
        /// Destination JSONL report; events also go to stdout as they happen
        #[arg(long)]
        jsonl: PathBuf,
        /// Model under test (default: $DANDELION_MODEL, else the repo default)
        #[arg(long)]
        model: Option<String>,
        /// Endpoint base URL (default: $DANDELION_BASE_URL, else repo default)
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Compare two JSONL reports (readable transition table).
    /// Exit codes: 0 no regressions, 1 regressions or removed cases, 2 bad input.
    Compare {
        /// Earlier JSONL report
        baseline: PathBuf,
        /// Newer JSONL report
        candidate: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum SkillsCommand {
    /// Scan skill roots
    List,
    /// Copy a SKILL.md or skill folder into data_dir/skills
    Import { path: PathBuf },
    /// Install from a SKILL.md URL or a git repository. Lands disabled.
    Install {
        /// https URL: a `.md` file is fetched directly, anything else is cloned
        url: String,
        /// Only scan this subdirectory of the cloned repository
        #[arg(long)]
        subdir: Option<String>,
        /// Force the git clone path even when the URL ends in `.md`
        #[arg(long)]
        git: bool,
    },
    /// Delete a global skill folder under data_dir/skills
    Remove { id: String },
}

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// List configured MCP servers and current slot state (does not spawn)
    List,
    /// Connect a configured server (stdio child or HTTP session)
    Start { id: String },
    /// Stop a connected MCP server (stdio child or HTTP session)
    Stop { id: String },
    /// Search the official MCP registry (read-only; installs nothing)
    Search {
        query: String,
        /// Continue a previous page using its cursor
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Install a registry entry into dandelion.mcp.servers
    Install {
        /// Exact registry name, e.g. io.github.owner/server
        name: String,
        #[arg(long)]
        version: Option<String>,
    },
    /// Remove a configured MCP server (stops it first)
    Remove { id: String },
    /// Manage MCP credentials stored outside settings.json (`secret:<name>` refs)
    Secret {
        #[command(subcommand)]
        command: McpSecretCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum McpSecretCommand {
    /// Store a credential; the value is read from stdin, never from argv
    Put {
        /// MCP server id, e.g. jira
        server: String,
        /// Reference name used as `secret:<name>` in that server's config
        name: String,
    },
    /// List stored credential names (values are never printed)
    List,
    /// Delete one stored credential
    Remove { server: String, name: String },
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    List {
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long, default_value_t = 0)]
        offset: u32,
    },
    Export {
        id: String,
    },
    Search {
        query: String,
    },
    /// Rename a session (SQLite `sessions.title`)
    SetTitle {
        id: String,
        title: String,
    },
    /// Cancel the tree then delete parent + children
    Delete {
        id: String,
    },
    Prefs {
        id: String,
    },
    SetPrefs {
        id: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        temperature: Option<f32>,
        #[arg(long)]
        max_tokens: Option<u32>,
        #[arg(long = "permission-mode")]
        permission_mode: Option<String>,
        #[arg(long)]
        system_prompt: Option<String>,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    Occupy {
        id: String,
        #[arg(long, default_value = "cli")]
        surface: String,
    },
    Release {
        id: String,
        #[arg(long, default_value = "cli")]
        surface: String,
    },
    /// New id; copy prefs, bindings, and messages
    Duplicate {
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum CronCommand {
    List,
    Add {
        #[arg(long)]
        expr: String,
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value = "skip")]
        missed_policy: String,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Run {
        id: String,
    },
    Remove {
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum CorpusCommand {
    List,
    Add { path: PathBuf },
    Remove { id: String },
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    Show,
    Set {
        kind: String,
        #[arg(long = "file", value_name = "PATH")]
        file: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum SpecCommand {
    Ask {
        #[arg(short = 'q', long = "prompt")]
        prompt: String,
        #[arg(short = 'c', long = "session")]
        session: Option<String>,
        #[arg(long, env = "DANDELION_WORKSPACE")]
        workspace: Option<PathBuf>,
        #[arg(long = "permission-mode", env = "DANDELION_PERMISSION_MODE")]
        permission_mode: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    Plan {
        #[arg(short = 'q', long = "prompt")]
        prompt: String,
        #[arg(short = 'c', long = "session")]
        session: Option<String>,
        #[arg(long, env = "DANDELION_WORKSPACE")]
        workspace: Option<PathBuf>,
        #[arg(long = "permission-mode", env = "DANDELION_PERMISSION_MODE")]
        permission_mode: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    Auto {
        #[arg(short = 'q', long = "prompt")]
        prompt: String,
        #[arg(short = 'c', long = "session")]
        session: Option<String>,
        #[arg(long, env = "DANDELION_WORKSPACE")]
        workspace: Option<PathBuf>,
        #[arg(long = "permission-mode", env = "DANDELION_PERMISSION_MODE")]
        permission_mode: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    Revise {
        #[arg(short = 'c', long = "session")]
        session: String,
        #[arg(short = 'q', long = "prompt")]
        prompt: String,
        #[arg(long, env = "DANDELION_WORKSPACE")]
        workspace: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    Diff {
        #[arg(short = 'c', long = "session")]
        session: String,
    },
    Show {
        #[arg(short = 'c', long = "session")]
        session: String,
    },
    Approve {
        #[arg(short = 'c', long = "session")]
        session: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    Run {
        #[arg(short = 'c', long = "session")]
        session: String,
        #[arg(short = 'q', long = "prompt")]
        prompt: Option<String>,
        #[arg(long, env = "DANDELION_WORKSPACE")]
        workspace: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
}

#[derive(Debug, Subcommand)]
pub enum WikiCommand {
    Status,
    Show {
        /// Compiled page name (e.g. ARCHITECTURE.md). Omit to list all.
        #[arg(long)]
        page: Option<String>,
    },
    Compile {
        #[arg(long, env = "DANDELION_WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Also generate a Chinese overview using the configured model.
        #[arg(long)]
        ai: bool,
        /// With --ai: only rewrite articles whose evidence changed or were
        /// never published (matches the desktop incremental rebuild).
        #[arg(long)]
        incremental: bool,
        /// Deep-build this knowledge space from its PUBLISHED documents
        /// (the space's on-disk mirror is regenerated from the store first).
        /// Runs the deep build regardless of `--ai`. Mutually exclusive
        /// with `--workspace`.
        #[arg(long, conflicts_with = "workspace")]
        space: Option<String>,
        /// Subject the space authorization runs as (default: the local
        /// desktop user). Ignored without `--space`.
        #[arg(long, default_value = "local-user")]
        subject: String,
    },
    Ask {
        question: String,
        #[arg(long, env = "DANDELION_WORKSPACE")]
        workspace: Option<PathBuf>,
    },
    Search {
        query: String,
    },
    /// List knowledge entries (incl. pending proposals) of a project wiki,
    /// or of one knowledge space with `--space`.
    Entries {
        #[arg(long, env = "DANDELION_WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Review the ledger of this knowledge space id instead of a
        /// workspace (pure spaces included). Mutually exclusive with
        /// `--workspace`.
        #[arg(long, conflicts_with = "workspace")]
        space: Option<String>,
        /// Subject the space authorization runs as (default: the local
        /// desktop user). Ignored without `--space`.
        #[arg(long, default_value = "local-user")]
        subject: String,
    },
    /// Review one entry: approve / reject a pending proposal, or undo it.
    Review {
        /// Entry id (from `wiki entries`).
        id: String,
        /// approve | reject | undo
        action: String,
        #[arg(long, env = "DANDELION_WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Review inside this knowledge space's ledger (see `entries --space`).
        #[arg(long, conflicts_with = "workspace")]
        space: Option<String>,
        #[arg(long, default_value = "local-user")]
        subject: String,
    },
    /// Edit one entry's content and/or kind.
    Edit {
        id: String,
        #[arg(long)]
        content: Option<String>,
        #[arg(long, value_enum)]
        kind: Option<WikiEntryKind>,
        #[arg(long, env = "DANDELION_WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Edit inside this knowledge space's ledger (see `entries --space`).
        #[arg(long, conflicts_with = "workspace")]
        space: Option<String>,
        #[arg(long, default_value = "local-user")]
        subject: String,
    },
    /// Create or replace one human-authored Wiki page of a knowledge space
    /// (the reading tab's save: Editor and up). Editing a machine-built page
    /// PROTECTS it — the next build will not overwrite the body and files
    /// its own text as a pending conflict instead.
    Page {
        /// Page name (e.g. runbook.md; a missing .md is appended). Must be
        /// a plain name — path separators are rejected.
        #[arg(long)]
        name: String,
        /// Page body. Exactly one of --content / --file is required.
        #[arg(long)]
        content: Option<String>,
        /// Read the body from this file (for longer pages).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Knowledge space id the page belongs to.
        #[arg(long)]
        space: String,
        /// Subject the space authorization runs as.
        #[arg(long, default_value = "local-user")]
        subject: String,
    },
    /// Change a knowledge space's external-model policy (space Owner):
    /// allow_summary | local_only | deny_cloud. Nothing is rewritten — the
    /// next ask/build/publish decision re-reads it.
    Policy {
        /// Knowledge space id.
        #[arg(long)]
        space: String,
        /// allow_summary | local_only | deny_cloud
        value: String,
        /// Subject the space authorization runs as (must be an Owner).
        #[arg(long, default_value = "local-user")]
        subject: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum WikiEntryKind {
    Flow,
    Decision,
    Pitfall,
    Term,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    /// One ACP `session/update` envelope per line — the same JSON Tauri will emit
    Jsonl,
}
