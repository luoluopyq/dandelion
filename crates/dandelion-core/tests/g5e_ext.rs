//! G5-E: Cron / memory / subagent list / hooks dir. UI hosts these Runtime methods.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::{MemoryKind, PermissionMode, PromptRequest, Runtime, Settings};
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

fn settings_auto() -> Settings {
    let mut s = Settings::defaults_configured();
    s.permission_mode = PermissionMode::Auto;
    s.cron_tick_sec = 3_600;
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cron_start_now_kind_and_stop_current() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::hang_until_cancel(),
    ]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let job = rt
        .cron_add(
            "30m",
            "cron hang",
            Some(dir.path().to_path_buf()),
            None,
            "skip",
        )
        .unwrap();
    let sid = rt.cron_start_now(&job.id).unwrap();
    let row = rt
        .sessions_list(None, 0)
        .unwrap()
        .into_iter()
        .find(|r| r.id == sid.as_str())
        .expect("cron session");
    assert_eq!(row.kind, "cron");
    rt.cron_stop_current(&job.id).await.expect("stop current");
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    let listed = rt.cron_list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, job.id);
}

/// A cron job runs unattended, so it must receive the stable acting toolset
/// even when its prompt contains no action keyword. Auto now follows that
/// same stable-surface policy; permission gates remain the execution boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cron_run_gets_tools_even_when_prompt_has_no_action_keyword() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(dir.path(), provider.clone(), settings_auto());

    // Same prompt through a normal Auto turn: stable tool exposure, with
    // sandbox/permission enforcement deferred to dispatch.
    let chat = rt.new_session(None);
    let mut plain = dandelion_core::PromptRequest::new(chat, "杀蒲狗");
    plain.workspace = Some(dir.path().to_path_buf());
    rt.prompt(plain).await.expect("plain prompt");
    assert!(
        provider
            .last_tool_names()
            .contains(&"write_file".to_string()),
        "Auto must expose the stable acting toolset: {:?}",
        provider.last_tool_names()
    );

    let job = rt
        .cron_add(
            "30m",
            "杀蒲狗",
            Some(dir.path().to_path_buf()),
            None,
            "skip",
        )
        .unwrap();
    rt.cron_run_now(&job.id).await.expect("cron run");
    let names = provider.last_tool_names();
    assert!(
        names.contains(&"read_file".to_string()) && names.contains(&"write_file".to_string()),
        "cron needs the acting toolset: {names:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_set_files_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    rt.memory_set(MemoryKind::Soul, "SOUL_G5E_LINE")
        .expect("set soul");
    let files = rt.memory_files();
    assert!(files.soul.contains("SOUL_G5E_LINE"), "{}", files.soul);
    let shown = rt.memory_show().unwrap();
    assert!(shown.contains("SOUL_G5E_LINE"));
    rt.memory_set(MemoryKind::User, "USER_G5E").unwrap();
    assert!(rt.memory_files().user.contains("USER_G5E"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_is_injected_into_new_sessions_and_updates_after_edit() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("ok"),
        ScriptedTurn::text("ok"),
    ]));
    let rt = runtime_with(dir.path(), provider.clone(), settings_auto());
    rt.memory_set(MemoryKind::User, "用户喜欢简短回答。")
        .unwrap();
    rt.memory_set(MemoryKind::Memory, "测试代号：蒲公英蓝色风筝。")
        .unwrap();
    let first = rt.new_session(None);
    rt.prompt(PromptRequest::new(first, "你记得什么？"))
        .await
        .unwrap();
    let system = provider.last_system().unwrap();
    assert!(system.contains("用户喜欢简短回答。"), "{system}");
    assert!(system.contains("测试代号：蒲公英蓝色风筝。"), "{system}");

    rt.memory_set(MemoryKind::Memory, "测试代号：蒲公英绿色纸船。")
        .unwrap();
    let second = rt.new_session(None);
    rt.prompt(PromptRequest::new(second, "你记得什么？"))
        .await
        .unwrap();
    let system = provider.last_system().unwrap();
    assert!(system.contains("蒲公英绿色纸船"), "{system}");
    assert!(!system.contains("蒲公英蓝色风筝"), "{system}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn children_list_empty_without_delegate() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let sid = rt.new_session(None);
    assert!(rt.children_list(&sid).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hooks_dir_is_under_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let hooks = rt.hooks_dir();
    assert!(hooks.ends_with("hooks"));
    std::fs::create_dir_all(&hooks).unwrap();
    assert!(hooks.is_dir());
}
