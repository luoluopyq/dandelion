//! G5-B core gates: Skill disable dual-door, 26 context badge, skill import.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::{PromptRequest, Runtime, Settings};
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
async fn disabled_skill_not_injected() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(data.join("skills/demo-skill")).unwrap();
    std::fs::write(
        data.join("skills/demo-skill/SKILL.md"),
        "---\nname: demo-skill\ndescription: demo\n---\nVISIBLE_SKILL_BODY\n",
    )
    .unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let listed = rt.skills_list(Some(dir.path())).unwrap();
    assert!(listed.iter().any(|i| i.id == "demo-skill" && i.enabled));
    rt.skills_set_enabled("demo-skill", false).unwrap();
    let listed = rt.skills_list(Some(dir.path())).unwrap();
    assert!(listed.iter().any(|i| i.id == "demo-skill" && !i.enabled));

    let sid = rt.new_session(None);
    let mut req = PromptRequest::new(sid, "skill?");
    req.skill_ids = vec!["demo-skill".into()];
    rt.prompt(req).await.expect("prompt");
    let sys = provider.last_system().expect("system");
    assert!(!sys.contains("VISIBLE_SKILL_BODY"), "{sys}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_status_lists_dandelion_not_dev_agents() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".dandelion.md"), "PROJECT_NOTE_UNIQUE\n").unwrap();
    std::fs::write(
        dir.path().join("AGENTS.md"),
        "this file is for developers of dandelion\nSECRET_DEV\n",
    )
    .unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, Settings::defaults_configured());
    let status = rt.context_status_for(Some(dir.path())).unwrap();
    assert!(status.injected);
    assert_eq!(status.kind.as_deref(), Some("dandelion"));
    assert!(status.files.iter().any(|f| f.rel.contains("dandelion")));
    assert!(!status.files.iter().any(|f| f.rel.ends_with("AGENTS.md")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skills_import_from_outside_scan_root() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("incoming/pack-skill");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("SKILL.md"),
        "---\nname: pack-skill\ndescription: packed\n---\nPACKED\n",
    )
    .unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, Settings::defaults_configured());
    let item = rt.skills_import(&src).unwrap();
    assert_eq!(item.id, "pack-skill");
    let listed = rt.skills_list(None).unwrap();
    assert!(listed.iter().any(|i| i.id == "pack-skill"));
}
