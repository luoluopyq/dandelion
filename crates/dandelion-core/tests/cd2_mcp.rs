//! CD2: `mcp_start` / `mcp_apply` without restarting Runtime.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::{McpServerConfig, PromptRequest, Runtime, Settings};
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_start_list_stop_without_restart() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(dir.path(), provider, Settings::defaults_configured());
    assert!(rt.mcp_list().await.unwrap().is_empty());

    let cfg = McpServerConfig {
        id: "echo".into(),
        enabled: true,
        command: "dandelion-mcp-does-not-exist-xyz".into(),
        ..Default::default()
    };
    rt.mcp_apply(vec![cfg]).await.expect("apply");
    let listed = rt.mcp_list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].ready);
    assert!(listed[0].error.is_none(), "list must not auto-start");

    rt.mcp_start("echo").await.expect("start");
    let after_start = rt.mcp_list().await.unwrap();
    assert_eq!(after_start.len(), 1);
    assert!(!after_start[0].ready);
    assert!(after_start[0].error.is_some());

    rt.mcp_stop("echo").await.expect("stop");
    let after_stop = rt.mcp_list().await.unwrap();
    assert_eq!(after_stop.len(), 1);
    assert!(!after_stop[0].ready);
    assert!(after_stop[0].error.is_none());

    rt.mcp_start("missing").await.expect_err("unknown id");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_prevents_snapshot_until_start() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("first"),
        ScriptedTurn::text("second"),
    ]));
    let mut settings = Settings::defaults_configured();
    settings.mcp_servers = vec![McpServerConfig {
        id: "echo".into(),
        enabled: true,
        command: "dandelion-mcp-does-not-exist-xyz".into(),
        ..Default::default()
    }];
    settings.mcp_spawn_timeout_ms = 2_000;
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    rt.mcp_stop("echo").await.unwrap();
    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "tools?");
    req.mcp_server_ids = vec!["echo".into()];
    rt.prompt(req).await.expect("prompt");
    let names = provider.last_tool_names();
    assert!(names.iter().all(|n| !n.starts_with("mcp__")), "{names:?}");
}
