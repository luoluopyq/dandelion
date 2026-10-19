use dandelion_core::host::DesktopHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::{Error, PromptRequest, Runtime, Settings};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[tokio::test]
async fn host_authorization_blocks_chat_and_scheduled_work_before_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let granted = Arc::new(AtomicBool::new(false));
    let host = Arc::new(DesktopHost::new(dir.path().join("data")));
    let policy = granted.clone();
    host.set_execution_guard(Arc::new(move || {
        if policy.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Error::NotConfigured("application authorization".into()))
        }
    }));
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(dir.path().to_path_buf());
    settings.cron_tick_sec = 3600;
    let rt = Runtime::new_with_provider(
        host,
        settings,
        Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")])),
    )
    .unwrap();
    let sid = rt.new_session(None);
    let req = PromptRequest::new(sid.clone(), "hello");
    assert!(rt
        .prompt(req.clone())
        .await
        .unwrap_err()
        .to_string()
        .contains("application authorization"));
    let job = rt
        .cron_add("30m", "hello", Some(dir.path().to_path_buf()), None, "skip")
        .unwrap();
    assert!(rt.cron_start_now(&job.id).is_err());
    assert!(rt.cron_tick_once().await.is_err());
    assert!(rt
        .sessions_list(None, 0)
        .unwrap()
        .iter()
        .all(|row| row.kind != "cron"));
    granted.store(true, Ordering::SeqCst);
    rt.prompt(req.clone()).await.unwrap();
    granted.store(false, Ordering::SeqCst);
    rt.cancel_host_work().await;
    assert!(rt.prompt(req).await.is_err());
    assert!(rt.cron_start_now(&job.id).is_err());
    assert!(
        !rt.sessions_list(None, 0).unwrap().is_empty(),
        "revocation must retain local history"
    );
}

#[tokio::test]
async fn host_revocation_cancels_inflight_work_and_keeps_the_history() {
    let dir = tempfile::tempdir().unwrap();
    let granted = Arc::new(AtomicBool::new(true));
    let host = Arc::new(DesktopHost::new(dir.path().join("data")));
    let policy = granted.clone();
    host.set_execution_guard(Arc::new(move || {
        if policy.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Error::NotConfigured("application authorization".into()))
        }
    }));
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(dir.path().to_path_buf());
    settings.cron_tick_sec = 3600;
    let rt = Runtime::new_with_provider(
        host,
        settings,
        Arc::new(ScriptedProvider::new(vec![
            ScriptedTurn::hang_until_cancel(),
        ])),
    )
    .unwrap();
    let sid = rt.new_session(None);
    let req = PromptRequest::new(sid.clone(), "waiting task");
    let running = rt.clone();
    let task = tokio::spawn(async move { running.prompt(req).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !rt.has_active_turns() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    granted.store(false, Ordering::SeqCst);
    rt.cancel_host_work().await;
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .ok();
    assert!(!rt.has_active_turns());
    assert!(rt
        .sessions_list(None, 0)
        .unwrap()
        .iter()
        .any(|row| row.id == sid.as_str()));
}
