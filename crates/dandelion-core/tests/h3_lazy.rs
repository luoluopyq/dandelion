//! H3 exit conditions, end to end against a real (fake) MCP server:
//! - 500 candidate external tools ⇒ first model request carries no MCP
//!   schemas at all — only `find_tools`;
//! - one search activates the match and it is callable on the *next*
//!   iteration of the same loop;
//! - a small fleet still injects directly (no forced extra round-trip).

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::ToolCall;
use dandelion_core::{McpServerConfig, PromptRequest, Runtime, Settings};
use std::collections::HashMap;
use std::sync::Arc;

fn runtime_with(
    dir: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    mut settings: Settings,
) -> Runtime {
    settings.workspace = Some(dir.to_path_buf());
    settings.cron_tick_sec = 3_600;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn five_hundred_tools_first_turn_lean_then_activated_and_callable() {
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
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "c2".into(),
                name: "mcp__fake__screenshot_tool".into(),
                arguments: "{}".into(),
            }],
        ),
        ScriptedTurn::text("done"),
    ]));
    let mut settings = Settings::defaults_configured();
    // Non-interactive host auto-denies in Ask mode; the loop itself is the
    // subject of this test.
    settings.permission_mode = dandelion_core::PermissionMode::Auto;
    settings.mcp_servers = vec![fake_server(500)];
    settings.mcp_spawn_timeout_ms = 20_000;
    let rt = runtime_with(dir.path(), provider.clone(), settings);

    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "take a screenshot please");
    req.mcp_server_ids = vec!["fake".into()];
    req.max_iterations = Some(6);
    rt.prompt(req).await.expect("loop completes");

    let hist = provider.history();
    assert!(
        hist.len() >= 3,
        "expected 3 model calls, got {}",
        hist.len()
    );
    // Turn 1: lean — find_tools present, zero of the 501 MCP schemas.
    assert!(
        hist[0].tool_names.iter().any(|n| n == "find_tools"),
        "lazy fleet must expose find_tools: {:?}",
        hist[0].tool_names
    );
    assert!(
        !hist[0].tool_names.iter().any(|n| n.starts_with("mcp__")),
        "500 candidates must NOT be injected on the first turn"
    );
    // Turn 2: the activated tool is in the model-visible list…
    assert!(
        hist[1]
            .tool_names
            .iter()
            .any(|n| n == "mcp__fake__screenshot_tool"),
        "activation must reach the next iteration: {:?}",
        hist[1].tool_names
    );
    // …and the call actually round-tripped to the server (its answer is in
    // the messages of the final request).
    let last = format!("{:?}", provider.last_messages());
    assert!(
        last.contains("fake ok from screenshot_tool"),
        "activated proxy must dispatch to the real MCP server; messages: {last}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_fleet_still_injects_directly() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let mut settings = Settings::defaults_configured();
    settings.mcp_servers = vec![fake_server(2)];
    settings.mcp_spawn_timeout_ms = 20_000;
    let rt = runtime_with(dir.path(), provider.clone(), settings);

    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "what tools do you have?");
    req.mcp_server_ids = vec!["fake".into()];
    rt.prompt(req).await.expect("prompt");

    let hist = provider.history();
    assert!(
        hist[0]
            .tool_names
            .iter()
            .any(|n| n == "mcp__fake__screenshot_tool"),
        "small fleets keep zero-extra-hop injection: {:?}",
        hist[0].tool_names
    );
    assert!(
        !hist[0].tool_names.iter().any(|n| n == "find_tools"),
        "find_tools is only registered for lazy fleets"
    );
}
