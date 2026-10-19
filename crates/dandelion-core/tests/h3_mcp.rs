//! H3: MCP credentials (`secret:<name>` refs), failure isolation, and
//! off-disk guarantees. Exit conditions exercised here: a server whose
//! credential is missing fails only itself; plaintext never reaches
//! settings.json, the project workspace, or another server's error text.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::secret::{mcp_secret_name, CredentialStore, FileCredentialStore};
use dandelion_core::{McpServerConfig, PromptRequest, Runtime, Settings};
use std::sync::Arc;

const SECRET: &str = "tok-h3-must-never-hit-disk";

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

fn stdio_server(id: &str, env: &[(&str, &str)]) -> McpServerConfig {
    McpServerConfig {
        id: id.into(),
        enabled: true,
        command: "dandelion-mcp-does-not-exist-xyz".into(),
        env: env
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secret_ref_requires_stored_credential() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = Settings::defaults_configured();
    settings.mcp_servers = vec![stdio_server("vault", &[("TOKEN", "secret:tok")])];
    settings.mcp_spawn_timeout_ms = 2_000;
    let rt = runtime_with(
        dir.path(),
        Arc::new(ScriptedProvider::new(vec![])),
        settings,
    );

    // Without a stored credential the server fails with a precise, local
    // message — not a spawn error, which would mean the ref leaked through.
    rt.mcp_start("vault")
        .await
        .expect("start records, not throws");
    let listed = rt.mcp_list().await.unwrap();
    let vault = listed.iter().find(|i| i.id == "vault").expect("listed");
    let err = vault.error.clone().unwrap_or_default();
    assert!(
        err.contains("no stored credential for 'tok'"),
        "missing credential must surface by name: {err}"
    );

    // Once stored, resolution passes and the failure (if any) comes from
    // the spawn itself, never from the secret path again.
    let store = FileCredentialStore::new(&dir.path().join("data"));
    store
        .put(&mcp_secret_name("vault", "tok").unwrap(), SECRET)
        .unwrap();
    rt.mcp_start("vault").await.expect("start again");
    let listed = rt.mcp_list().await.unwrap();
    let vault = listed.iter().find(|i| i.id == "vault").expect("listed");
    let err = vault.error.clone().unwrap_or_default();
    assert!(
        !err.contains("no stored credential"),
        "stored credential must satisfy the ref: {err}"
    );
    assert!(
        !err.contains(SECRET),
        "error text must not echo the secret: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_broken_server_never_blocks_loop_or_others() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let mut settings = Settings::defaults_configured();
    settings.mcp_servers = vec![
        stdio_server("broken-a", &[]),
        stdio_server("broken-b", &[("KEY", "secret:tok")]),
    ];
    settings.mcp_spawn_timeout_ms = 2_000;
    let rt = runtime_with(dir.path(), provider.clone(), settings);

    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "tools?");
    req.mcp_server_ids = vec!["broken-a".into(), "broken-b".into()];
    rt.prompt(req).await.expect("loop survives both failures");
    let names = provider.last_tool_names();
    assert!(names.iter().all(|n| !n.starts_with("mcp__")), "{names:?}");

    let listed = rt.mcp_list().await.unwrap();
    for id in ["broken-a", "broken-b"] {
        let item = listed.iter().find(|i| i.id == id).expect("listed");
        assert!(!item.ready, "{id} must not report ready");
    }
}

#[test]
fn secret_plaintext_stays_out_of_settings_and_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let workspace = dir.path().join("proj");
    std::fs::create_dir_all(&workspace).unwrap();

    let store = FileCredentialStore::new(&data);
    store
        .put(&mcp_secret_name("vault", "tok").unwrap(), SECRET)
        .unwrap();

    let mut settings = Settings::defaults_configured();
    settings.mcp_servers = vec![stdio_server("vault", &[("TOKEN", "secret:tok")])];
    settings.save(&data).unwrap();

    let json = std::fs::read_to_string(data.join("settings.json")).unwrap();
    assert!(
        json.contains("secret:tok"),
        "the ref itself is config: {json}"
    );
    assert!(!json.contains(SECRET), "settings.json must hold refs only");

    // Only {data_dir}/secrets may hold the payload — never the project.
    let mut hits = Vec::new();
    for root in [&data, &workspace] {
        for entry in walkdir(root) {
            if std::fs::read(&entry)
                .ok()
                .is_some_and(|b| String::from_utf8_lossy(&b).contains(SECRET))
            {
                hits.push(entry);
            }
        }
    }
    #[cfg(windows)]
    assert!(
        hits.is_empty(),
        "DPAPI payload must not contain plaintext: {hits:?}"
    );
    #[cfg(not(windows))]
    assert_eq!(
        hits,
        vec![data.join("secrets").join("mcp__vault__tok")],
        "plaintext may only live in the 0600 secret file"
    );
}

fn walkdir(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walkdir(&path));
        } else {
            out.push(path);
        }
    }
    out
}
