//! H3 exit condition: user → project → session tightening-only merge,
//! observed end-to-end against the real fake MCP server.
//!
//! The project layer is `{workspace}/.dandelion/mcp.json`. It may only
//! restrict the user's configured fleet: remove a tool, disable a server.
//! Removal must be visible to BOTH injection paths — the direct fleet and a
//! lazy `find_tools` search must behave identically as if the server never
//! advertised the tool.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::ToolCall;
use dandelion_core::{McpServerConfig, PermissionMode, PromptRequest, Runtime, Settings};
use std::collections::HashMap;
use std::sync::Arc;

fn runtime_with(
    dir: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    mut settings: Settings,
) -> Runtime {
    settings.workspace = Some(dir.to_path_buf());
    settings.cron_tick_sec = 3_600;
    settings.permission_mode = PermissionMode::Auto;
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(dir.join("data"))),
        settings,
        provider,
    )
    .expect("runtime")
}

fn fake_server(tool_count: usize) -> McpServerConfig {
    let mut env = HashMap::new();
    env.insert("FAKE_MCP_TOOL_COUNT".to_string(), tool_count.to_string());
    McpServerConfig {
        id: "fake".into(),
        enabled: true,
        command: env!("CARGO_BIN_EXE_fake_mcp").into(),
        env,
        ..Default::default()
    }
}

fn write_project_scope(dir: &std::path::Path, json: &str) {
    std::fs::create_dir_all(dir.join(".dandelion")).unwrap();
    std::fs::write(dir.join(".dandelion").join("mcp.json"), json).unwrap();
}

async fn one_prompt(dir: &std::path::Path, provider: Arc<ScriptedProvider>, settings: Settings) {
    let rt = runtime_with(dir, provider, settings);
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "hello");
    req.mcp_server_ids = vec!["fake".into()];
    rt.prompt(req).await.expect("loop completes");
}

/// A project-level tool deny removes the tool from the direct fleet — the
/// model never sees its schema at all, while its sibling survives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_tool_deny_removes_only_that_tool() {
    let dir = tempfile::tempdir().unwrap();
    write_project_scope(
        dir.path(),
        r#"{"servers":{"fake":{"disabledTools":["screenshot_tool"]}}}"#,
    );
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let mut settings = Settings::defaults_configured();
    settings.mcp_servers = vec![fake_server(1)];
    settings.mcp_spawn_timeout_ms = 20_000;
    one_prompt(dir.path(), provider.clone(), settings).await;

    let hist = provider.history();
    let names = &hist[0].tool_names;
    assert!(
        names.iter().any(|n| n == "mcp__fake__tool_0"),
        "non-denied tool must survive: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.ends_with("screenshot_tool")),
        "project-denied tool must never be injected: {names:?}"
    );
}

/// A project-level server deny wins even when the session explicitly asks
/// for that server this turn — session may narrow, project may forbid.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_server_deny_wins_over_session_request() {
    let dir = tempfile::tempdir().unwrap();
    write_project_scope(dir.path(), r#"{"disabled":["fake"]}"#);
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let mut settings = Settings::defaults_configured();
    settings.mcp_servers = vec![fake_server(1)];
    settings.mcp_spawn_timeout_ms = 20_000;
    one_prompt(dir.path(), provider.clone(), settings).await;

    let hist = provider.history();
    assert!(
        !hist[0].tool_names.iter().any(|n| n.starts_with("mcp__")),
        "project-disabled server must inject nothing: {:?}",
        hist[0].tool_names
    );
}

/// The deny also holds on the lazy path: with a wide fleet the catalog is
/// built after scope filtering, so `find_tools` cannot rediscover what the
/// user (here, the user layer) denied.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_deny_floor_holds_through_lazy_search() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "c1".into(),
                name: "find_tools".into(),
                arguments: serde_json::json!({"query": "screenshot"}).to_string(),
            }],
        ),
        ScriptedTurn::text("done"),
    ]));
    let mut settings = Settings::defaults_configured();
    let mut server = fake_server(500);
    server.disabled_tools = vec!["screenshot_tool".into()];
    settings.mcp_servers = vec![server];
    settings.mcp_spawn_timeout_ms = 20_000;
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "take a screenshot");
    req.mcp_server_ids = vec!["fake".into()];
    rt.prompt(req).await.expect("loop completes");

    let last = format!("{:?}", provider.last_messages());
    assert!(
        last.contains("No external tool matches"),
        "denied tool must be absent from the lazy catalog: {last}"
    );
    let hist = provider.history();
    assert!(
        !hist
            .iter()
            .any(|h| h.tool_names.iter().any(|n| n.ends_with("screenshot_tool"))),
        "denied tool must never reach any turn's tool list"
    );
}
