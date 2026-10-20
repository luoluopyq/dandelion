//! 会话切换工作单（2026-10-19）钉死的安全契约。
//!
//! 背景：启动路径曾在「工作空间目录当前不存在」时直接连空间带对话物理删除，
//! 而读不到目录最常见的原因是移动盘/网络盘没就位 —— 目录回来时对话已经没了。
//! 桌面层现在只在 `projects_list` 里带 `accessible: false` 提示，不再自动删。
//! 本文件把边界钉在 store 层：
//! 1. `Store::open`（每次启动都会做的事）绝不因目录缺失而删任何项目/会话/消息；
//! 2. 显式 `project_delete(purge_sessions = true)` 仍然全量清除 —— 用户主动
//!    删除的路径不许被削弱；
//! 3. `messages(session_id, id)` 索引在任何一次 open 后都存在，包括建库早于
//!    索引问世的旧库（SCHEMA 每次 open 全量重跑）；
//! 4. `get_session` 只读元数据就能拿到 workspace，不需要导出整个会话历史；
//! 5. `Runtime::project_delete`（工作单第二版·条目 2）：删除运行中的空间必须
//!    先取消并等待牵连会话的回合、收掉其后台进程，最后才删记录 —— 记录不会
//!    在被删除时仍有回合在往里写（对照 ref/codex residency 的
//!    is_unloadable → shutdown_and_wait → remove_thread 次序）。

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::store::sqlite::Store;
use dandelion_core::{PermissionMode, PromptRequest, Runtime, SessionId, Settings};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn open_store(data: &Path) -> Store {
    fs::create_dir_all(data).unwrap();
    Store::open(data, 5_000).unwrap()
}

fn db(data: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(data.join("sessions.sqlite")).unwrap()
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn index_names(conn: &rusqlite::Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA index_list('{table}')"))
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// 在 `ws` 目录下建一个项目，并给它挂一条会话、两条消息。返回项目 id。
/// 会话与消息按 `project_create` 收敛后的目录拼写写入（Windows 上
/// canonicalize/dunce 可能改写路径，字符串必须与项目行一致，删除才命中）。
fn seed_project_with_messages(data: &Path, ws: &Path) -> String {
    fs::create_dir_all(ws).unwrap();
    let store = open_store(data);
    let project = store.project_create(ws).unwrap();
    let conn = db(data);
    conn.execute(
        "INSERT INTO sessions (id, title, workspace, created_at, updated_at, kind, depth)
         VALUES ('tsk_keep', '移动盘上的对话', ?1, '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00', 'user', 0)",
        rusqlite::params![project.workspace],
    )
    .unwrap();
    for (role, content) in [("user", "第一句"), ("assistant", "第二句")] {
        conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at)
             VALUES ('tsk_keep', ?1, ?2, '2026-01-01T00:00:00+00:00')",
            rusqlite::params![role, content],
        )
        .unwrap();
    }
    drop(conn);
    drop(store);
    project.id
}

/// 核心回归：拔掉移动盘（目录消失）后重新启动 —— `Store::open` 什么都不删。
/// 项目、会话、消息必须原样还在；提示由桌面层的 accessible 标记负责。
#[test]
fn opening_a_store_never_deletes_a_temporarily_missing_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    seed_project_with_messages(&data, &ws);

    fs::remove_dir_all(&ws).unwrap();

    let store = open_store(&data); // 相当于一次全新的启动
    assert_eq!(store.projects_list().unwrap().len(), 1, "空间条目必须原地保留");
    let conn = db(&data);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM sessions WHERE id = 'tsk_keep'"),
        1,
        "目录暂时读不到不是删除对话的理由"
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM messages"), 2, "消息一条都不能少");
}

/// 另一半契约：`projects_prune_missing` 作为「用户显式动作」的后备仍然称职，
/// 真调用时连会话带消息一起清干净。收紧的是调用时机，不是删除能力本身。
#[test]
fn explicit_prune_missing_still_purges_completely() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    seed_project_with_messages(&data, &ws);
    fs::remove_dir_all(&ws).unwrap();

    let store = open_store(&data);
    let removed = store.projects_prune_missing().unwrap();
    assert_eq!(removed.len(), 1, "显式调用必须真删");
    assert_eq!(store.projects_list().unwrap().len(), 0);
    let conn = db(&data);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM sessions"), 0);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM messages"), 0);
}

/// 显式删除项目（purge_sessions = true，桌面右键「删除项目与对话」的口径）
/// 同样必须全量清除，别把「不再自动删」误做成「删不掉」。
#[test]
fn explicit_project_delete_still_purges_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    let project_id = seed_project_with_messages(&data, &ws);

    let store = open_store(&data);
    store.project_delete(&project_id, true).unwrap();
    let conn = db(&data);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM projects"), 0);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM sessions"), 0);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM messages"), 0);
}

/// 读消息的口径是「某个会话的最近 N 条」，没有 (session_id, id) 索引就是全表
/// 扫描。SCHEMA 每次 open 全量重跑，所以旧库也会在下次启动时自动补上 ——
/// 用「DROP 掉再 open」模拟一个早于索引问世的存量库。
#[test]
fn messages_session_index_exists_after_any_open_including_legacy_dbs() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");

    let conn = {
        let _store = open_store(&data);
        let conn = db(&data);
        assert!(
            index_names(&conn, "messages").iter().any(|n| n == "messages_session_idx"),
            "新库开箱即有索引: {:?}",
            index_names(&conn, "messages")
        );
        conn.execute("DROP INDEX messages_session_idx", []).unwrap();
        conn
    };
    drop(conn);

    let _store = open_store(&data); // 存量库的下一次启动
    let conn = db(&data);
    assert!(
        index_names(&conn, "messages").iter().any(|n| n == "messages_session_idx"),
        "旧库重启后必须自动补上索引: {:?}",
        index_names(&conn, "messages")
    );
}

/// `get_session` 是宿主取 workspace 的正确入口：一条查询就拿全元数据行，
/// 桌面端 context_status 不再为了这一个字段 session_export 整个历史。
#[test]
fn get_session_carries_workspace_metadata_in_one_row() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    seed_project_with_messages(&data, &ws);

    let store = open_store(&data);
    let row = store
        .get_session(&SessionId::parse("tsk_keep").unwrap())
        .unwrap()
        .expect("session row exists");
    assert_eq!(row.title, "移动盘上的对话");
    // 对照物取空间行自己的收敛拼写（dunce 可能改写 \\?\ 前缀，std canonicalize
    // 与入库口径不同）——契约是「会话行与空间行一致」，不是某种字面形式。
    let project_ws = store
        .projects_list()
        .unwrap()
        .into_iter()
        .next()
        .expect("project exists")
        .workspace;
    assert_eq!(row.workspace, project_ws, "workspace 随行返回");
    assert!(!row.workspace.is_empty());
    assert!(Path::new(&row.workspace).is_dir());
}

// ---------------------------------------------------------------------------
// 条目 2：删除运行中的空间 —— 先停活动，再删记录。
// ---------------------------------------------------------------------------

/// 在 `data` 里先登记一个指向 `ws` 的空间，返回收敛后的项目行。
/// Runtime 构造之前建好，`settings.workspace` 直接用项目行的拼写，
/// 保证会话落库的 workspace 与 doomed CTE 的比较串完全一致。
fn seed_project(data: &Path, ws: &Path) -> dandelion_core::ProjectRow {
    fs::create_dir_all(data).unwrap();
    fs::create_dir_all(ws).unwrap();
    let store = open_store(data);
    let project = store.project_create(ws).unwrap();
    drop(store);
    project
}

fn runtime_for(data: &Path, workspace: &str, provider: ScriptedProvider) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(PathBuf::from(workspace));
    settings.permission_mode = PermissionMode::Auto;
    settings.cron_tick_sec = 3_600;
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(data.join("data"))),
        settings,
        Arc::new(provider),
    )
    .expect("runtime")
}

/// 删除一个正跑到一半的空间：`project_delete` 返回时必须已经取消并等到该
/// 回合落定（对照测试在返回后才 join，不再需要自己的超时容忍），记录才会
/// 被删 —— 被删的会话不许还有回合在往里写。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_running_project_waits_for_the_turn_to_settle() {
    let tmp = tempfile::tempdir().unwrap();
    let (root, ws) = (tmp.path().join("root"), tmp.path().join("ws"));
    let project = seed_project(&root.join("data"), &ws);
    let rt = runtime_for(
        &root,
        &project.workspace,
        ScriptedProvider::new(vec![ScriptedTurn::hang_until_cancel()]),
    );
    let sid = rt.new_session(None);
    let rt2 = rt.clone();
    let sid2 = sid.clone();
    let handle = tokio::spawn(async move { rt2.prompt(PromptRequest::new(sid2, "永远跑不完的一轮")).await });

    let busy_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !rt.has_active_turns() {
        assert!(std::time::Instant::now() < busy_deadline, "回合从未挂上 busy");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    rt.project_delete(&project.id, true).await.expect("删除运行中的空间");

    // 返回即落定：删除是在等的循环里把这一轮取消掉的，这里 join 不该再等。
    let settled = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("project_delete 返回后回合必须已落定")
        .expect("join");
    assert!(settled.is_ok(), "被取消的回合按既有约定正常返回: {settled:?}");

    let store = open_store(&root.join("data"));
    assert!(store.get_session(&sid).unwrap().is_none(), "会话记录随删除消失");
    assert_eq!(store.projects_list().unwrap().len(), 0);
}

/// 回合与后台进程解耦：会话早就闲下来、后台终端还挂着 30 秒的命令 ——
/// 删空间也必须把它的进程树收掉，不能让进程在会话记录没了之后继续写。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_project_stops_its_background_terminals() {
    let tmp = tempfile::tempdir().unwrap();
    let (root, ws) = (tmp.path().join("root"), tmp.path().join("ws"));
    let project = seed_project(&root.join("data"), &ws);
    let rt = runtime_for(
        &root,
        &project.workspace,
        ScriptedProvider::new(vec![ScriptedTurn::text("seed")]),
    );
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "seed")).await.unwrap();

    #[cfg(windows)]
    let long_cmd = "ping -n 30 127.0.0.1";
    #[cfg(not(windows))]
    let long_cmd = "sleep 30";
    rt.terminal_run_user(&sid, long_cmd, None).expect("后台终端起不来");

    let running_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if rt
            .background_processes(&sid)
            .iter()
            .any(|p| p.status == "running")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < running_deadline,
            "后台进程从未进入 running"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    rt.project_delete(&project.id, true).await.expect("删除空间");

    let still_running = rt
        .background_processes(&sid)
        .iter()
        .any(|p| p.status == "running");
    assert!(!still_running, "删除空间后其后台终端不得仍在运行");
    let store = open_store(&root.join("data"));
    assert!(store.get_session(&sid).unwrap().is_none());
}

/// 归档分支不许被削弱：purge_sessions=false 只撤书签，会话行必须原样保留。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlisting_a_project_still_keeps_its_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let (root, ws) = (tmp.path().join("root"), tmp.path().join("ws"));
    let project = seed_project(&root.join("data"), &ws);
    let rt = runtime_for(
        &root,
        &project.workspace,
        ScriptedProvider::new(vec![ScriptedTurn::text("seed")]),
    );
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "seed")).await.unwrap();

    rt.project_delete(&project.id, false).await.expect("只撤书签");
    let store = open_store(&root.join("data"));
    assert_eq!(store.projects_list().unwrap().len(), 0);
    assert!(
        store.get_session(&sid).unwrap().is_some(),
        "归档分支不得连带删会话"
    );
}
