//! CD1 offline gates (`docs/g4-g5-rd-plan.md` §6).
//!
//! | gate | test |
//! |------|------|
//! | rename | `set_title_persists` |
//! | delete tree after cancel | `delete_cancels_then_removes_tree` |
//! | second occupy fails | `second_surface_occupy_fails` |
//! | corpus unbound → 0 hits | `unbound_corpus_hits_zero_bound_uses_settings` |
//! | saved temperature on next prompt | `saved_temperature_used_on_next_prompt` |
//! | user system prompt assembled in core | `user_system_prompt_assembled_in_core` |

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::store::Store;
use dandelion_core::{
    Error, PermissionMode, PromptRequest, Runtime, SessionId, SessionPrefs, Settings,
};
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

fn settings_auto() -> Settings {
    let mut s = Settings::defaults_configured();
    s.permission_mode = PermissionMode::Auto;
    s.temperature = 0.2;
    s.corpus_max_hits = 8;
    s.cron_tick_sec = 3_600;
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_title_persists() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "first turn"))
        .await
        .expect("prompt");
    rt.session_set_title(&sid, "renamed by cd1").unwrap();
    let listed = rt.sessions_list(None, 0).unwrap();
    let row = listed.iter().find(|r| r.id == sid.as_str()).unwrap();
    assert_eq!(row.title, "renamed by cd1");
    let export = rt.session_export(&sid).unwrap();
    assert_eq!(export["title"], "renamed by cd1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_cancels_then_removes_tree() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("parent"),
        ScriptedTurn::text("child"),
    ]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let parent = rt.new_session(None);
    rt.prompt(PromptRequest::new(parent.clone(), "parent turn"))
        .await
        .expect("parent");
    let child = rt.new_session(None);
    rt.prompt(PromptRequest::new(child.clone(), "child seed"))
        .await
        .expect("child");
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    store
        .set_session_lineage(&child, Some(parent.as_str()), "subagent", 1)
        .unwrap();
    rt.occupy_session(&parent, "main").unwrap();
    rt.session_delete(&parent).await.expect("delete");
    assert!(rt.session_export(&parent).is_err());
    assert!(store.get_session(&parent).unwrap().is_none());
    assert!(store.get_session(&child).unwrap().is_none());
    assert!(rt.occupancy_holder(&parent).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_cancels_busy_session() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::hang_until_cancel(),
    ]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let sid = rt.new_session(None);
    let running = rt.clone();
    let prompt_sid = sid.clone();
    let handle =
        tokio::spawn(async move { running.prompt(PromptRequest::new(prompt_sid, "hang")).await });
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    rt.session_delete(&sid).await.expect("delete busy");
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("join");
    assert!(result.is_ok());
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    assert!(store.get_session(&sid).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_surface_occupy_fails() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let sid = rt.new_session(None);
    rt.occupy_session(&sid, "main").unwrap();
    rt.occupy_session(&sid, "main").unwrap();
    let err = rt.occupy_session(&sid, "window-09").unwrap_err();
    match err {
        Error::SessionOccupied { session_id, holder } => {
            assert_eq!(session_id, sid.as_str());
            assert_eq!(holder, "main");
        }
        other => panic!("{other:?}"),
    }
    rt.release_session(&sid, "window-09").unwrap();
    assert_eq!(rt.occupancy_holder(&sid).unwrap().as_deref(), Some("main"));
    rt.release_session(&sid, "main").unwrap();
    rt.occupy_session(&sid, "window-09").unwrap();
    assert_eq!(
        rt.occupancy_holder(&sid).unwrap().as_deref(),
        Some("window-09")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfer_surface_main_to_window_09() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let sid = rt.new_session(None);
    rt.occupy_session(&sid, "main").unwrap();
    let prev = rt
        .transfer_session_surface(&sid, "main", "window-09")
        .unwrap();
    assert_eq!(prev.as_deref(), Some("main"));
    assert_eq!(
        rt.occupancy_holder(&sid).unwrap().as_deref(),
        Some("window-09")
    );
    let err = rt.occupy_session(&sid, "main").unwrap_err();
    match err {
        Error::SessionOccupied { holder, .. } => assert_eq!(holder, "window-09"),
        other => panic!("{other:?}"),
    }
    rt.transfer_session_surface(&sid, "window-09", "window-09")
        .unwrap();
    assert_eq!(
        rt.occupancy_holder(&sid).unwrap().as_deref(),
        Some("window-09")
    );
    let other = rt.new_session(None);
    assert!(rt
        .transfer_session_surface(&other, "main", "window-09")
        .unwrap()
        .is_none());
    assert_eq!(
        rt.occupancy_holder(&other).unwrap().as_deref(),
        Some("window-09")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unbound_corpus_hits_zero_bound_uses_settings() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "seed"))
        .await
        .expect("seed");
    assert_eq!(rt.corpus_max_hits_for(&sid).unwrap(), 0);
    rt.session_replace_bindings(&sid, "corpus", &["doc-a".into()])
        .unwrap();
    assert_eq!(rt.corpus_max_hits_for(&sid).unwrap(), 8);
    rt.session_replace_bindings(&sid, "corpus", &[]).unwrap();
    assert_eq!(rt.corpus_max_hits_for(&sid).unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_temperature_used_on_next_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("seed"),
        ScriptedTurn::text("from prefs"),
        ScriptedTurn::text("overlay"),
        ScriptedTurn::text("still prefs"),
    ]));
    let rt = runtime_with(dir.path(), provider.clone(), settings_auto());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "seed"))
        .await
        .expect("seed");
    rt.session_set_prefs(
        &sid,
        SessionPrefs {
            temperature: Some(0.9),
            ..SessionPrefs::default()
        },
    )
    .unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "use saved temp"))
        .await
        .expect("prefs turn");
    let hist = provider.history();
    assert_eq!(hist.last().unwrap().temperature, 0.9);
    let mut overlay = PromptRequest::new(sid.clone(), "one turn overlay");
    overlay.temperature = Some(0.1);
    rt.prompt(overlay).await.expect("overlay");
    assert_eq!(provider.history().last().unwrap().temperature, 0.1);
    rt.prompt(PromptRequest::new(sid.clone(), "back to saved"))
        .await
        .expect("again");
    assert_eq!(provider.history().last().unwrap().temperature, 0.9);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_system_prompt_assembled_in_core() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text("seed"),
        ScriptedTurn::text("with prompt"),
    ]));
    let rt = runtime_with(dir.path(), provider.clone(), settings_auto());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "seed"))
        .await
        .expect("seed");
    rt.session_set_prefs(
        &sid,
        SessionPrefs {
            system_prompt: Some("USER_PROMPT_UNIQUE_CD1".into()),
            ..SessionPrefs::default()
        },
    )
    .unwrap();
    rt.prompt(PromptRequest::new(sid, "second"))
        .await
        .expect("second");
    let system = provider.last_system().expect("system");
    assert!(system.contains("USER_PROMPT_UNIQUE_CD1"), "{system}");
    assert!(system.contains("任务说明："), "{system}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_session_title_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let sid = SessionId::parse("tsk_missing").unwrap();
    let err = rt.session_set_title(&sid, "nope").unwrap_err();
    assert!(matches!(err, Error::SessionNotFound(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_session_appears_in_list() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let sid = rt.create_session(Some("brand-new")).unwrap();
    let listed = rt.sessions_list(None, 0).unwrap();
    let row = listed.iter().find(|r| r.id == sid.as_str()).unwrap();
    assert_eq!(row.title, "brand-new");
    assert_eq!(row.kind, "user");
    assert_eq!(row.depth, 0);
}

/// 「新对话」= 没选过目录的白纸。
///
/// 以前 `create_session` 会把 `info().workspace`（settings.workspace 或进程 cwd）
/// 当成新会话的工作目录写进行里，桌面端于是每开一条新对话都自带一个用户没选过的
/// 目录（`tauri dev` 时进程 cwd 恰好是 `apps/desktop/src-tauri`，界面上就显示
/// `src-tauri`），必须点 × 才能清掉。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_session_has_no_workspace_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    // runtime_with 把 settings.workspace 指向了临时目录，所以 info().workspace 是
    // 非空的：这条断言能抓住「拿兜底目录盖章」的回归。
    assert!(!rt.info().workspace.as_os_str().is_empty());
    let sid = rt.create_session(Some("blank")).unwrap();
    let listed = rt.sessions_list(None, 0).unwrap();
    let row = listed.iter().find(|r| r.id == sid.as_str()).unwrap();
    assert_eq!(row.workspace, "", "新建的会话不该自带工作目录");
    assert_eq!(row.kind, "user");
}

/// 空目录不是坏数据：第一轮 prompt 照跑，而且**不能**把运行时兜底目录写回行里。
///
/// 这是「目录只由用户显式指定」的关键约束 —— 只要首轮 prompt 会把兜底目录写回去，
/// 界面上的目录 chip 就会在用户说第一句话之后自己冒出来。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompting_a_workspace_less_session_does_not_stamp_the_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let sid = rt.create_session(Some("blank")).unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "hello"))
        .await
        .expect("prompt without a session workspace");
    let listed = rt.sessions_list(None, 0).unwrap();
    let row = listed.iter().find(|r| r.id == sid.as_str()).unwrap();
    assert_eq!(row.workspace, "", "首轮 prompt 不该把兜底目录写回会话行");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_copies_prefs_bindings_and_messages() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("copied")]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "UNIQUE_DUP_NEEDLE"))
        .await
        .expect("prompt");
    rt.session_set_prefs(
        &sid,
        SessionPrefs {
            temperature: Some(0.5),
            system_prompt: Some("dup prompt".into()),
            ..SessionPrefs::default()
        },
    )
    .unwrap();
    rt.session_replace_bindings(&sid, "skill", &["demo-skill".into()])
        .unwrap();
    rt.session_replace_bindings(&sid, "mcp", &["echo".into()])
        .unwrap();
    let copy = rt.session_duplicate(&sid).unwrap();
    assert_ne!(copy.as_str(), sid.as_str());
    let prefs = rt.session_prefs(&copy).unwrap();
    assert_eq!(prefs.temperature, Some(0.5));
    assert_eq!(prefs.system_prompt.as_deref(), Some("dup prompt"));
    let binds = rt.session_task_bindings(&copy).unwrap();
    assert_eq!(binds["skill"], serde_json::json!(["demo-skill"]));
    assert_eq!(binds["mcp"], serde_json::json!(["echo"]));
    let dump = rt.session_export(&copy).unwrap().to_string();
    assert!(dump.contains("UNIQUE_DUP_NEEDLE"), "{dump}");
    assert!(rt.session_export(&copy).unwrap()["title"]
        .as_str()
        .unwrap()
        .contains("副本"));
}

/// 改空间名只动「名字」：目录不动，会话归属也不动。
///
/// 名字是用户给这个目录起的叫法，不是目录本身。改名如果牵动 `workspace` 列，
/// 那个空间名下的对话会整体掉进「未归属」—— 用户只是改了个叫法，不该丢归属。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renaming_a_project_keeps_its_workspace_and_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let project = rt.project_create(dir.path()).unwrap();
    let sid = rt.create_session(Some("in-space")).unwrap();
    rt.session_set_prefs(
        &sid,
        SessionPrefs {
            workspace: Some(dir.path().to_path_buf()),
            ..SessionPrefs::default()
        },
    )
    .unwrap();

    rt.project_rename(&project.id, "我的项目").unwrap();

    let renamed = rt
        .projects_list()
        .unwrap()
        .into_iter()
        .find(|p| p.id == project.id)
        .unwrap();
    assert_eq!(renamed.name, "我的项目");
    assert_eq!(renamed.workspace, project.workspace, "改名不该动目录");
    let row = rt
        .sessions_list(None, 0)
        .unwrap()
        .into_iter()
        .find(|r| r.id == sid.as_str())
        .unwrap();
    assert_eq!(
        std::fs::canonicalize(&row.workspace).unwrap(),
        std::fs::canonicalize(&project.workspace).unwrap(),
        "改名不该动会话归属"
    );
    // 空名字是无效输入：后端必须拒，而不是把空间名写成空白。
    assert!(rt.project_rename(&project.id, "   ").is_err());
}

/// 「从列表中移除」撤掉登记，**留下**对话。
///
/// 和 `project_delete` 的分工就在这里：那边销毁数据，这边只撤一个书签。移除之后
/// 会话的 `workspace` 列还写着那个目录，只是没有空间登记它了 —— 于是它们落进
/// 「未归属」，而不是被删掉或被归档（归档会让它们从主列表里消失，那和删除只有
/// 一步之差）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlisting_a_project_keeps_its_sessions_unarchived() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_with(dir.path(), provider, settings_auto());
    let project = rt.project_create(dir.path()).unwrap();
    let sid = rt.create_session(Some("in-space")).unwrap();
    rt.session_set_prefs(
        &sid,
        SessionPrefs {
            workspace: Some(dir.path().to_path_buf()),
            ..SessionPrefs::default()
        },
    )
    .unwrap();

    rt.project_unlist(&project.id).unwrap();

    assert!(
        rt.projects_list()
            .unwrap()
            .iter()
            .all(|p| p.id != project.id),
        "空间不该还在列表里"
    );
    let listed = rt.sessions_list(None, 0).unwrap();
    let row = listed.iter().find(|r| r.id == sid.as_str()).unwrap();
    assert_eq!(
        std::fs::canonicalize(&row.workspace).unwrap(),
        std::fs::canonicalize(&project.workspace).unwrap(),
        "会话还在原来那个目录里"
    );
    assert!(!row.archived, "移除不该把会话归档");
}
