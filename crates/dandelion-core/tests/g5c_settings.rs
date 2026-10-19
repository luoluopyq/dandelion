//! G5-C: live Settings patch (no Runtime rebuild), 17 invalid JSON, allowShell, gate 3.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::{ChatMessage, ToolCall};
use dandelion_core::store::Store;
use dandelion_core::{
    PermissionMode, PromptRequest, RecordingHost, Runtime, Settings, SettingsOverlay,
};
use serde_json::{json, Map};
use std::sync::Arc;

fn runtime_with(
    dir: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    mut settings: Settings,
) -> Runtime {
    settings.workspace = Some(dir.to_path_buf());
    settings.cron_tick_sec = 3_600;
    settings.permission_mode = PermissionMode::Ask;
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(dir.join("data"))),
        settings,
        provider,
    )
    .expect("runtime")
}

fn tool_contents(store: &Store, sid: &dandelion_core::SessionId) -> Vec<String> {
    store
        .load_openai_messages(sid)
        .unwrap()
        .into_iter()
        .filter_map(|m| match m {
            ChatMessage::Tool { content, .. } => Some(content),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_temperature_keeps_occupancy() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(dir.path(), provider, Settings::defaults_configured());
    let sid = rt.create_session(Some("g5c")).unwrap();
    rt.occupy_session(&sid, "main").unwrap();
    let mut patch = Map::new();
    patch.insert("dandelion.temperature".into(), json!(0.55));
    rt.settings_patch(patch).await.expect("patch");
    assert!((rt.settings().temperature - 0.55).abs() < f32::EPSILON);
    let err = rt.occupy_session(&sid, "detached").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("occupied"), "{msg}");
    let loaded = Settings::load(&rt.info().data_dir, &SettingsOverlay::default()).unwrap();
    assert!((loaded.temperature - 0.55).abs() < f32::EPSILON);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_roundtrip_and_invalid_does_not_write() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, Settings::defaults_configured());
    let mut patch = Map::new();
    patch.insert("dandelion.maxTokens".into(), json!(4096));
    rt.settings_patch(patch).await.expect("seed");
    let text = rt.settings_json_redacted().unwrap();
    assert!(text.contains("dandelion.maxTokens"), "{text}");
    assert!(!text.contains("dandelion.apiKey"), "{text}");
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value["dandelion.maxTokens"] = json!(1024);
    rt.settings_replace_json(&serde_json::to_string_pretty(&value).unwrap())
        .await
        .expect("replace");
    assert_eq!(rt.settings().max_tokens, 1024);

    let before = std::fs::read_to_string(rt.info().data_dir.join("settings.json")).unwrap();
    let err = rt.settings_replace_json("{not json").await.unwrap_err();
    assert!(err.to_string().contains("invalid JSON"), "{err}");
    let after = std::fs::read_to_string(rt.info().data_dir.join("settings.json")).unwrap();
    assert_eq!(before, after);
    assert_eq!(rt.settings().max_tokens, 1024);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_shell_false_returns_disabled_error() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_sh".into(),
                name: "terminal".into(),
                arguments: r#"{"command":"echo Dandelion"}"#.into(),
            }],
        ),
        ScriptedTurn::text("ok"),
    ]));
    let rt = runtime_with(dir.path(), provider, Settings::defaults_configured());
    let mut patch = Map::new();
    patch.insert("dandelion.allowShell".into(), json!(false));
    rt.settings_patch(patch).await.expect("patch");
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "echo"))
        .await
        .expect("prompt");
    let store = Store::open(&rt.info().data_dir, 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    assert!(
        tools.iter().any(|t| t.contains("Error: shell disabled")),
        "{tools:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate3_deny_write_file_skips_host_and_disk() {
    let dir = tempfile::tempdir().unwrap();
    let rec = RecordingHost::new(dir.path().join("data"));
    let host = Arc::new(rec.clone());
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Ask;
    settings.workspace = Some(dir.path().to_path_buf());
    settings.cron_tick_sec = 3_600;
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![ToolCall {
                id: "call_w".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"g5c-deny.txt","content":"nope"}"#.into(),
            }],
        ),
        ScriptedTurn::text("done"),
    ]));
    let rt = Runtime::new_with_provider(host, settings, provider).expect("runtime");
    let mut patch = Map::new();
    patch.insert(
        "dandelion.permission.rules".into(),
        json!({ "deny": ["write_file"], "allow": [] }),
    );
    rt.settings_patch(patch).await.expect("patch");
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "write"))
        .await
        .expect("prompt");
    assert!(rec.call_titles().is_empty(), "{:?}", rec.call_titles());
    assert!(!dir.path().join("g5c-deny.txt").exists());
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    assert!(
        tools.iter().any(|t| t.contains("Error: blocked by rule")),
        "{tools:?}"
    );
}
