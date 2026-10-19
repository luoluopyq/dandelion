//! H2 offline gates (git worktree isolation for subagents).
//!
//! | requirement | test |
//! |---|---|
//! | 子任务在 worktree 隔离执行，父工作区不被静默修改 | `isolated_child_writes_worktree_not_parent` |
//! | 父显式合并；changeset 可审阅 | `merge_explicit_applies_changeset` |
//! | isolation=shared 保持旧共享行为 | `shared_isolation_writes_parent_directly` |
//! | 非 git 工作区降级共享 | `non_git_workspace_degrades_to_shared` |
//! | 父显式丢弃；未合并内容需 force | `discard_requires_force_then_discards` |
//! | 取消后已证实为空的 worktree 被清理 | `cancelled_child_prunes_proven_empty_worktree` |
//! | 冲突合并回滚父工作区 | 单元层 `worktree::tests::merge_conflict_aborts_and_restores_parent` |
//! | 重复应用 = alreadyApplied | 单元层 `worktree::tests::merge_refuses_dirty_parent_and_duplicate_is_already_applied` |

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::{ChatMessage, ToolCall};
use dandelion_core::store::Store;
use dandelion_core::{PermissionMode, PromptRequest, Runtime, SessionId, Settings};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git in PATH");
    assert!(status.success(), "git {args:?} failed");
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn git_status_porcelain(repo: &Path) -> String {
    String::from_utf8_lossy(
        &Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repo)
            .output()
            .expect("git")
            .stdout,
    )
    .to_string()
}

/// `root/repo` is the git workspace; `root/data` (outside the repo) keeps
/// SQLite and settings so runtime bookkeeping never dirties the checkout.
struct Env {
    root: tempfile::TempDir,
}

impl Env {
    fn new(git_repo: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        if git_repo {
            git(&repo, &["init", "--quiet", "."]);
            git(&repo, &["config", "user.email", "test@dandelion.invalid"]);
            git(&repo, &["config", "user.name", "Dandelion Test"]);
            git(&repo, &["add", "--all"]);
            git(&repo, &["commit", "--quiet", "-m", "baseline"]);
        }
        Self { root }
    }

    fn repo(&self) -> std::path::PathBuf {
        self.root.path().join("repo")
    }

    fn runtime(&self, provider: Arc<ScriptedProvider>) -> Runtime {
        let mut settings = Settings::defaults_configured();
        settings.workspace = Some(self.repo());
        settings.permission_mode = PermissionMode::Auto;
        settings.cron_tick_sec = 3_600;
        Runtime::new_with_provider(
            Arc::new(CliHost::with_data_dir(self.root.path().join("data"))),
            settings,
            provider,
        )
        .expect("runtime")
    }
}

fn tool_call(id: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "delegate_task".into(),
        arguments: args.to_string(),
    }
}

fn write_file(id: &str, path: &str, content: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "write_file".into(),
        arguments: serde_json::json!({"path": path, "content": content}).to_string(),
    }
}

fn tool_contents(store: &Store, sid: &SessionId) -> Vec<String> {
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

fn first_child_id(contents: &[String]) -> SessionId {
    // Park rows carry the child summary; the finalize line names the branch
    // `dandelion-subagent/<childSessionId>`, which is the child's id. Spawn
    // rows (Handle mode) carry "childSessionId" JSON directly.
    for row in contents {
        if let Some(start) = row.find("dandelion-subagent/tsk_") {
            let rest = &row[start + "dandelion-subagent/".len()..];
            let id: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if let Ok(sid) = SessionId::parse(&id) {
                return sid;
            }
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(row) {
            if let Some(id) = value.get("childSessionId").and_then(|v| v.as_str()) {
                if let Ok(sid) = SessionId::parse(id) {
                    return sid;
                }
            }
        }
    }
    panic!("no childSessionId in tool rows: {contents:?}");
}

async fn wait_children_done(rt: &Runtime, parent: &SessionId) {
    for _ in 0..200 {
        if rt.children_list(parent).is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("children did not finish in time");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn isolated_child_writes_worktree_not_parent() {
    if !git_available() {
        return;
    }
    let env = Env::new(true);
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![tool_call(
                "call_d",
                serde_json::json!({"goal": "write a note", "isolation": "worktree"}),
            )],
        ),
        ScriptedTurn::tools("", vec![write_file("call_w", "note.txt", "child work\n")]),
        ScriptedTurn::delayed_text("child finished", Duration::from_millis(60)),
        ScriptedTurn::text("parent done"),
    ]));
    let rt = env.runtime(provider);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "delegate isolated work"))
        .await
        .expect("prompt");
    let store = Store::open(&env.root.path().join("data"), 5000).unwrap();
    let contents = tool_contents(&store, &sid);
    let child = first_child_id(&contents);
    wait_children_done(&rt, &sid).await;
    // Parent checkout untouched — the child's file exists nowhere in it.
    assert!(!env.repo().join("note.txt").exists());
    // Parent summary carries the reviewable changeset line.
    let combined = contents.join("\n");
    assert!(combined.contains("changeset:"), "{combined}");
    let report = rt.subagent_worktree(&child).expect("worktree");
    assert_eq!(report["dirty"], serde_json::json!(true));
    assert_eq!(report["merged"], serde_json::json!(false));
    // The worktree is kept for review but leaves no untracked noise behind.
    let status = git_status_porcelain(&env.repo());
    assert!(!status.contains(".dandelion-worktrees"), "{status}");
    // Explicit discard cleans it up.
    let changeset = rt.subagent_changeset(&child).await.expect("changeset");
    assert!(changeset["patch"]
        .as_str()
        .unwrap_or("")
        .contains("child work"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merge_explicit_applies_changeset() {
    if !git_available() {
        return;
    }
    let env = Env::new(true);
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![tool_call(
                "call_d",
                serde_json::json!({"goal": "add feature note"}),
            )],
        ),
        ScriptedTurn::tools("", vec![write_file("call_w", "feature.md", "shipped\n")]),
        ScriptedTurn::delayed_text("child finished", Duration::from_millis(60)),
        ScriptedTurn::text("parent reviewing"),
    ]));
    let rt = env.runtime(provider);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "delegate"))
        .await
        .expect("prompt");
    let store = Store::open(&env.root.path().join("data"), 5000).unwrap();
    let child = first_child_id(&tool_contents(&store, &sid));
    wait_children_done(&rt, &sid).await;
    // Nothing merged yet.
    assert!(!env.repo().join("feature.md").exists());
    // Explicit parent-side merge applies the changeset.
    let merged = rt.subagent_merge(&child).await;
    assert_eq!(merged["status"], serde_json::json!("merged"), "{merged}");
    assert!(env.repo().join("feature.md").exists());
    // Duplicate merge is a no-op, never a second application.
    let again = rt.subagent_merge(&child).await;
    assert_eq!(
        again["status"],
        serde_json::json!("alreadyApplied"),
        "{again}"
    );
    let report = rt.subagent_worktree(&child).expect("report");
    assert_eq!(report["merged"], serde_json::json!(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_isolation_writes_parent_directly() {
    let env = Env::new(true);
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![tool_call(
                "call_d",
                serde_json::json!({"goal": "write directly", "isolation": "shared"}),
            )],
        ),
        ScriptedTurn::tools("", vec![write_file("call_w", "direct.txt", "shared\n")]),
        ScriptedTurn::delayed_text("child finished", Duration::from_millis(40)),
        ScriptedTurn::text("parent done"),
    ]));
    let rt = env.runtime(provider);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "shared delegate"))
        .await
        .expect("prompt");
    wait_children_done(&rt, &sid).await;
    // Shared mode writes the parent checkout directly and creates no
    // worktree machinery at all.
    assert!(env.repo().join("direct.txt").exists());
    assert!(!env.repo().join(".dandelion-worktrees").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_git_workspace_degrades_to_shared() {
    let env = Env::new(false);
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![tool_call(
                "call_d",
                serde_json::json!({"goal": "write in non-git dir"}),
            )],
        ),
        ScriptedTurn::tools("", vec![write_file("call_w", "plain.txt", "plain\n")]),
        ScriptedTurn::delayed_text("child finished", Duration::from_millis(40)),
        ScriptedTurn::text("parent done"),
    ]));
    let rt = env.runtime(provider);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "delegate in plain dir"))
        .await
        .expect("prompt");
    wait_children_done(&rt, &sid).await;
    // Degrade: the child wrote the shared workspace, no worktree exists.
    assert!(env.repo().join("plain.txt").exists());
    assert!(!env.repo().join(".dandelion-worktrees").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discard_requires_force_then_discards() {
    if !git_available() {
        return;
    }
    let env = Env::new(true);
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![tool_call(
                "call_d",
                serde_json::json!({"goal": "scratch work"}),
            )],
        ),
        ScriptedTurn::tools("", vec![write_file("call_w", "scratch.txt", "wip\n")]),
        ScriptedTurn::delayed_text("child finished", Duration::from_millis(40)),
        ScriptedTurn::text("parent done"),
    ]));
    let rt = env.runtime(provider);
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "delegate"))
        .await
        .expect("prompt");
    let store = Store::open(&env.root.path().join("data"), 5000).unwrap();
    let child = first_child_id(&tool_contents(&store, &sid));
    wait_children_done(&rt, &sid).await;
    let report = rt.subagent_worktree(&child).expect("worktree");
    let wt_path = std::path::PathBuf::from(report["path"].as_str().unwrap());
    assert!(wt_path.exists());
    // Unmerged content refuses an unforced discard…
    let err = rt.subagent_discard(&child, false).await.unwrap_err();
    assert!(err.contains("force=true"), "{err}");
    assert!(wt_path.exists());
    // …and an explicit forced discard removes it and leaves the parent clean.
    let done = rt.subagent_discard(&child, true).await.expect("discard");
    assert_eq!(done["status"], serde_json::json!("discarded"));
    assert!(!wt_path.exists());
    assert!(!env.repo().join("scratch.txt").exists());
    assert_eq!(git_status_porcelain(&env.repo()), "");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_child_prunes_proven_empty_worktree() {
    if !git_available() {
        return;
    }
    let env = Env::new(true);
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![tool_call(
                "call_d",
                serde_json::json!({"goal": "hang forever"}),
            )],
        ),
        ScriptedTurn::hang_until_cancel(),
    ]));
    let rt = env.runtime(provider);
    let sid = rt.new_session(None);
    let running = rt.clone();
    let prompt_sid = sid.clone();
    let handle = tokio::spawn(async move {
        running
            .prompt(PromptRequest::new(prompt_sid, "delegate hang"))
            .await
    });
    // Wait for the child to register, then cancel the parent tree.
    let child = loop {
        let live = rt.children_list(&sid);
        if let Some(first) = live.first() {
            break first.child_session_id.clone();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    rt.cancel(sid.clone()).await.expect("cancel");
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    wait_children_done(&rt, &sid).await;
    // Proven-empty (no commits, clean) worktree is auto-pruned on cancel.
    let report = rt.subagent_worktree(&child).expect("report");
    assert_eq!(report["pruned"], serde_json::json!(true), "{report}");
    assert_eq!(report["gone"], serde_json::json!(true), "{report}");
    assert_eq!(git_status_porcelain(&env.repo()), "");
}
