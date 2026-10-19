//! G5-F: local GGUF catalog, sidecar start errors, GPU slot queue (remote skips).

use dandelion_core::acp::{SessionState, SessionUpdate};
use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings};
use std::sync::Arc;
use std::time::Duration;

fn runtime_with(
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

fn write_fake_gguf(path: &std::path::Path) {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend_from_slice(&[0u8; 64]);
    std::fs::write(path, bytes).unwrap();
}

#[tokio::test]
async fn import_gguf_hashes_and_lists() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(
        dir.path(),
        Arc::new(ScriptedProvider::new(vec![])),
        Settings::defaults_configured(),
    );
    let src = dir.path().join("toy-Q4_K_M.gguf");
    write_fake_gguf(&src);
    let row = rt.local_model_import(&src).expect("import");
    assert!(row.sha256_ok);
    assert_eq!(row.quant, "Q4_K_M");
    assert_eq!(row.source, "import");
    let listed = rt.local_models();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].active);
    rt.local_model_verify(&row.id).expect("verify");
}

#[tokio::test]
async fn mlx_local_model_runtime_smoke_when_configured() {
    let Ok(path) = std::env::var("DANDELION_TEST_MLX_MODEL") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(
        dir.path(),
        Arc::new(ScriptedProvider::new(vec![])),
        Settings::defaults_configured(),
    );
    let row = rt
        .local_model_import_mlx(std::path::Path::new(&path))
        .expect("import MLX");
    assert_eq!(row.format, "mlx");
    let base = rt.sidecar_start().await.expect("start MLX runner");
    let response: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/chat/completions"))
        .json(&serde_json::json!({
            "model": "default_model",
            "messages": [{"role": "user", "content": "请只回答你好"}],
            "max_tokens": 128,
            "stream": false
        }))
        .send()
        .await
        .expect("chat request")
        .error_for_status()
        .expect("chat status")
        .json()
        .await
        .expect("chat json");
    assert!(
        response["choices"][0]["message"]["content"]
            .as_str()
            .is_some_and(|s| s.contains("你好")),
        "{response}"
    );
    let stream = reqwest::Client::new()
        .post(format!("{base}/chat/completions"))
        .json(&serde_json::json!({
            "model": "default_model",
            "messages": [{"role": "user", "content": "请只回答你好"}],
            "max_tokens": 128,
            "stream": true
        }))
        .send()
        .await
        .expect("stream request")
        .error_for_status()
        .expect("stream status")
        .text()
        .await
        .expect("stream body");
    assert!(
        stream.contains("data:") && stream.contains("[DONE]"),
        "{stream}"
    );
    rt.sidecar_stop();
}

#[tokio::test]
async fn mlx_managed_runner_install_when_configured() {
    if std::env::var_os("DANDELION_TEST_MLX_INSTALL").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(
        dir.path(),
        Arc::new(ScriptedProvider::new(vec![])),
        Settings::defaults_configured(),
    );
    let installed = rt.install_mlx_runner().await.expect("install MLX runner");
    assert!(std::path::Path::new(&installed).is_file());
    assert!(installed.contains("mlx-venv"), "{installed}");
}

#[tokio::test]
async fn sidecar_start_without_gguf_is_11a() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(
        dir.path(),
        Arc::new(ScriptedProvider::new(vec![])),
        Settings::defaults_configured(),
    );
    let err = rt.sidecar_start().await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("sidecar start"), "{msg}");
    assert!(msg.contains("import"), "{msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sidecar_backend_queues_second_session() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::hang_until_cancel(),
        ScriptedTurn::hang_until_cancel(),
    ]));
    let mut settings = Settings::defaults_configured();
    settings.default_backend = "sidecar".into();
    settings.max_local_concurrency = 1;
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let a = rt.new_session(None);
    let b = rt.new_session(None);
    let mut rx = rt.subscribe();
    let rt_a = rt.clone();
    let a_p = a.clone();
    let ha = tokio::spawn(async move { rt_a.prompt(PromptRequest::new(a_p, "one")).await });
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(provider.complete_count(), 1);
    let rt_b = rt.clone();
    let b_p = b.clone();
    let hb = tokio::spawn(async move { rt_b.prompt(PromptRequest::new(b_p, "two")).await });
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        provider.complete_count(),
        1,
        "second must wait for the slot"
    );
    let mut saw_queued = false;
    while let Ok(frame) = rx.try_recv() {
        if matches!(
            frame.params.update,
            SessionUpdate::StateUpdate {
                state: SessionState::Queued,
                ..
            }
        ) {
            saw_queued = true;
        }
    }
    assert!(saw_queued, "queued state_update for the waiter");
    rt.cancel(b).await.unwrap();
    let _ = hb.await;
    rt.cancel(a).await.unwrap();
    let _ = ha.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_backend_does_not_use_local_queue() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::hang_until_cancel(),
        ScriptedTurn::hang_until_cancel(),
    ]));
    let mut settings = Settings::defaults_configured();
    settings.default_backend = "openai_compat".into();
    settings.max_local_concurrency = 1;
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let a = rt.new_session(None);
    let b = rt.new_session(None);
    let rt_a = rt.clone();
    let a_p = a.clone();
    let ha = tokio::spawn(async move { rt_a.prompt(PromptRequest::new(a_p, "one")).await });
    let rt_b = rt.clone();
    let b_p = b.clone();
    let hb = tokio::spawn(async move { rt_b.prompt(PromptRequest::new(b_p, "two")).await });
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(
        provider.complete_count(),
        2,
        "remote sessions must not share the sidecar slot"
    );
    rt.cancel(a).await.ok();
    rt.cancel(b).await.ok();
    let _ = ha.await;
    let _ = hb.await;
}

#[tokio::test]
async fn hardware_probe_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(
        dir.path(),
        Arc::new(ScriptedProvider::new(vec![])),
        Settings::defaults_configured(),
    );
    let hw = rt.hardware_probe();
    let _ = (
        hw.gpu_name,
        hw.vram_mb,
        hw.ram_mb,
        hw.disk_free_mb,
        hw.notes,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dummy_sidecar_timeout_kills_child() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("dummy-llama.cmd");
    std::fs::write(&script, "@echo off\r\nping -n 30 127.0.0.1 >nul\r\n").unwrap();
    std::env::set_var("DANDELION_LLAMA_SERVER", &script);
    let src = dir.path().join("toy.gguf");
    write_fake_gguf(&src);
    let mut settings = Settings::defaults_configured();
    settings.sidecar_ready_timeout_ms = 400;
    settings.default_backend = "sidecar".into();
    let rt = runtime_with(
        dir.path(),
        Arc::new(ScriptedProvider::new(vec![])),
        settings,
    );
    rt.local_model_import(&src).unwrap();
    let err = rt.sidecar_start().await.unwrap_err();
    assert!(err.to_string().contains("sidecar start"), "{err}");
    std::env::remove_var("DANDELION_LLAMA_SERVER");
    rt.sidecar_kill_on_exit();
}
