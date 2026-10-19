//! CD3: multi-root WorkspaceFs + hard-denied system paths.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::ToolCall;
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings};
use std::sync::Arc;

fn runtime_auto(
    dir: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    mut settings: Settings,
) -> Runtime {
    settings.workspace = Some(dir.to_path_buf());
    settings.permission_mode = PermissionMode::Auto;
    settings.cron_tick_sec = 3_600;
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(dir.join("data"))),
        settings,
        provider,
    )
    .expect("runtime")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extra_root_read_file_and_windows_denied() {
    let primary = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    std::fs::write(primary.path().join("in-primary.txt"), "PRIMARY_MARK\n").unwrap();
    std::fs::write(extra.path().join("in-extra.txt"), "EXTRA_ROOT_MARK\n").unwrap();

    let extra_abs = dunce::canonicalize(extra.path().join("in-extra.txt")).unwrap();
    let extra_json = extra_abs.to_string_lossy().replace('\\', "/");

    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_extra".into(),
                name: "read_file".into(),
                arguments: format!(r#"{{"path":"{extra_json}","limit":10}}"#),
            }],
        ),
        ScriptedTurn::text("done extra"),
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_win".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"C:\\Windows\\System32\\drivers\\etc\\hosts"}"#.into(),
            }],
        ),
        ScriptedTurn::text("done win"),
    ]));

    let rt = runtime_auto(primary.path(), provider, Settings::defaults_configured());
    rt.sandbox_add_root(extra.path()).expect("add extra root");

    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "read extra"))
        .await
        .expect("extra prompt");
    let dump = rt.session_export(&sid).unwrap().to_string();
    assert!(dump.contains("EXTRA_ROOT_MARK"), "{dump}");

    rt.prompt(PromptRequest::new(sid.clone(), "read windows"))
        .await
        .expect("windows prompt");
    let dump = rt.session_export(&sid).unwrap().to_string();
    assert!(
        dump.contains("system path denied") || dump.contains("Error:"),
        "{dump}"
    );

    let err = rt
        .sandbox_add_root(std::path::Path::new(r"C:\Windows"))
        .unwrap_err();
    assert!(err.to_string().contains("system path denied"), "{err}");
}

#[test]
fn parent_escape_still_rejected_with_extra_root() {
    let primary = tempfile::tempdir().unwrap();
    let extra = tempfile::tempdir().unwrap();
    let fs = dandelion_core::WorkspaceFs::with_extra_roots(
        primary.path().to_path_buf(),
        &[extra.path().to_path_buf()],
    )
    .unwrap();
    let err = fs.resolve("../secret").unwrap_err();
    assert!(err.to_string().contains("escapes workspace"), "{err}");
}
