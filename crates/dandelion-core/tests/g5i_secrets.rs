//! G5-I: API key sealed on disk; export / redacted JSON stay key-free.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::ScriptedProvider;
use dandelion_core::{Runtime, Settings, SettingsOverlay};
use serde_json::{json, Map};
use std::collections::HashMap;
use std::sync::Arc;

const SECRET: &str = "sk-g5i-plain-key-must-not-hit-disk";

fn settings_with_secret() -> Settings {
    let mut s = Settings::defaults_configured();
    s.api_key = Some(SECRET.into());
    s.encrypt_api_key = true;
    s
}

#[test]
fn save_seals_api_key_and_load_restores() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    settings_with_secret().save(data).unwrap();
    let text = std::fs::read_to_string(data.join("settings.json")).unwrap();
    assert!(
        !text.contains(SECRET),
        "settings.json must not contain plaintext key: {text}"
    );
    assert!(
        text.contains("enc:"),
        "sealed apiKey must use enc: prefix: {text}"
    );
    assert!(text.contains("\"dandelion.encryptApiKey\": true"), "{text}");

    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut merged =
        Settings::merge(Some(&file), &HashMap::new(), &SettingsOverlay::default()).unwrap();
    merged.unseal_secrets(data).unwrap();
    assert_eq!(merged.api_key.as_deref(), Some(SECRET));

    if std::env::var("DANDELION_API_KEY")
        .ok()
        .filter(|v| !v.is_empty())
        .is_none()
    {
        let loaded = Settings::load(data, &SettingsOverlay::default()).unwrap();
        assert_eq!(loaded.api_key.as_deref(), Some(SECRET));
    }
}

#[test]
fn encrypt_false_writes_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let mut s = settings_with_secret();
    s.encrypt_api_key = false;
    s.save(data).unwrap();
    let text = std::fs::read_to_string(data.join("settings.json")).unwrap();
    assert!(text.contains(SECRET), "{text}");
    assert!(!text.contains("enc:dpapi:"), "{text}");
    assert!(!text.contains("\"enc:file\""), "{text}");
}

#[test]
fn legacy_plaintext_migrates_on_next_save() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    std::fs::write(
        data.join("settings.json"),
        serde_json::to_string_pretty(&json!({
            "dandelion.apiKey": SECRET,
            "dandelion.encryptApiKey": true
        }))
        .unwrap(),
    )
    .unwrap();
    let mut loaded = Settings::merge(
        Some(
            &serde_json::from_str(&std::fs::read_to_string(data.join("settings.json")).unwrap())
                .unwrap(),
        ),
        &HashMap::new(),
        &SettingsOverlay::default(),
    )
    .unwrap();
    loaded.unseal_secrets(data).unwrap();
    assert_eq!(loaded.api_key.as_deref(), Some(SECRET));
    loaded.save(data).unwrap();
    let text = std::fs::read_to_string(data.join("settings.json")).unwrap();
    assert!(
        !text.contains(SECRET),
        "migrated file still has plaintext: {text}"
    );
    assert!(text.contains("enc:"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_and_redacted_json_never_leak_key() {
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let rt = Runtime::new_with_provider(
        host,
        settings_with_secret(),
        Arc::new(ScriptedProvider::new(vec![])),
    )
    .expect("runtime");
    let mut patch = Map::new();
    patch.insert("dandelion.temperature".into(), json!(0.3));
    rt.settings_patch(patch).await.expect("patch");
    let disk = std::fs::read_to_string(rt.info().data_dir.join("settings.json")).unwrap();
    assert!(!disk.contains(SECRET), "{disk}");
    let redacted = rt.settings_json_redacted().unwrap();
    assert!(!redacted.contains("dandelion.apiKey"), "{redacted}");
    assert!(!redacted.contains(SECRET), "{redacted}");
    assert_eq!(rt.settings().api_key.as_deref(), Some(SECRET));

    let sid = rt.create_session(Some("g5i")).unwrap();
    let export = rt.session_export(&sid).unwrap();
    let dump = export.to_string();
    assert!(!dump.contains("apiKey"), "{dump}");
    assert!(!dump.contains(SECRET), "{dump}");
}

#[cfg(not(windows))]
#[test]
fn unix_uses_enc_file_and_secrets_path() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    settings_with_secret().save(data).unwrap();
    let text = std::fs::read_to_string(data.join("settings.json")).unwrap();
    assert!(text.contains("enc:file"), "{text}");
    let secret_path = data.join("secrets").join("api-key");
    let stored = std::fs::read_to_string(&secret_path).unwrap();
    assert_eq!(stored, SECRET);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&secret_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "secrets file mode {mode:o}");
    }
}

#[cfg(windows)]
#[test]
fn windows_uses_dpapi_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    settings_with_secret().save(data).unwrap();
    let text = std::fs::read_to_string(data.join("settings.json")).unwrap();
    assert!(text.contains("enc:dpapi:v1:"), "{text}");
    assert!(!data.join("secrets").join("api-key").exists());
}
