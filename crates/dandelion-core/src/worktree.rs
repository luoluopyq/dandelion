//! H2: Git worktree isolation for subagent subtasks.
//!
//! A child spawned with worktree isolation runs inside
//! `<repo>/.dandelion-worktrees/subagent-<id>` on branch
//! `dandelion-subagent/<id>`, created from the parent HEAD. The child never
//! writes the parent checkout. When the child finishes, the parent receives a
//! reviewable changeset (commits + uncommitted work, including untracked
//! files) and must explicitly `merge` or `discard` it: nothing merges
//! silently.
//!
//! Parity notes (Hermes `tools/subagent_worktree.py`): non-git workspaces
//! degrade to the shared cwd; git invocations are hardened against hooks,
//! credential prompts and pagers; automatic pruning only removes a worktree
//! that is **affirmatively proven** empty (zero commits and clean). Any probe
//! failure keeps the worktree and marks the report `inspectionFailed` so the
//! parent never loses unreviewed work to a failed probe.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Directory under the repository root that hosts all Dandelion worktrees.
pub const WORKTREE_DIR: &str = ".dandelion-worktrees";
pub const BRANCH_PREFIX: &str = "dandelion-subagent/";

/// A created subagent worktree. Pure data: every method re-derives state by
/// asking Git, so a stale handle can never report more than Git agrees to.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeHandle {
    /// Main checkout top-level (absolute).
    pub repo_root: PathBuf,
    /// Worktree directory (absolute).
    pub path: PathBuf,
    /// `dandelion-subagent/<child-id>`.
    pub branch: String,
    /// HEAD of the parent checkout when the worktree was created.
    pub base_commit: String,
}

/// Hardened local git invocation. Never interactive: hooks are redirected to
/// a nonexistent path, credential prompts, fsmonitor, signing and the pager
/// are disabled, and optional lock files are skipped so concurrent probes
/// cannot collide.
pub fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-c")
        .arg("core.hooksPath=.dandelion-hooks-disabled")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .arg("-c")
        .arg("credential.helper=")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_PAGER", "cat")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|err| format!("git unavailable: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git_lenient(cwd: &Path, args: &[&str]) -> Option<String> {
    git(cwd, args).ok()
}

/// Absolute repository top-level for a workspace path, if it is a git
/// checkout. Used before deciding whether isolation can apply at all.
pub fn repo_root_for(workspace: &Path) -> Option<PathBuf> {
    let out = git_lenient(workspace, &["rev-parse", "--show-toplevel"])?;
    let root = out.trim();
    if root.is_empty() {
        return None;
    }
    Some(PathBuf::from(root))
}

/// Unborn HEAD (fresh `git init` without commits) cannot anchor a worktree
/// base commit; the caller must degrade to a shared workspace instead.
pub fn head_commit(repo_root: &Path) -> Option<String> {
    git_lenient(repo_root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string())
}

/// Create the per-child worktree with its own branch from `base_commit`.
/// Best-effort local ignore via `.git/info/exclude` so the worktree directory
/// never shows up as untracked noise in the parent status.
pub fn create(
    repo_root: &Path,
    base_commit: &str,
    child_id: &str,
) -> Result<WorktreeHandle, String> {
    let branch = format!("{BRANCH_PREFIX}{child_id}");
    let path = repo_root
        .join(WORKTREE_DIR)
        .join(format!("subagent-{child_id}"));
    if path.exists() {
        return Err(format!("worktree path already exists: {}", path.display()));
    }
    exclude_local(repo_root, WORKTREE_DIR);
    git(
        repo_root,
        &[
            "worktree",
            "add",
            "--quiet",
            &path.to_string_lossy(),
            "-b",
            &branch,
            base_commit,
        ],
    )
    .map_err(|err| format!("git worktree add failed: {err}"))?;
    Ok(WorktreeHandle {
        repo_root: repo_root.to_path_buf(),
        path,
        branch,
        base_commit: base_commit.to_string(),
    })
}

/// Append to `.git/info/exclude` (local, untracked by design) instead of the
/// tracked `.gitignore`, so isolation never dirties the parent workspace.
fn exclude_local(repo_root: &Path, entry: &str) {
    let common = match git(repo_root, &["rev-parse", "--git-common-dir"]) {
        Ok(dir) => PathBuf::from(dir.trim()),
        Err(_) => return,
    };
    let path = if common.is_absolute() {
        common.join("info").join("exclude")
    } else {
        repo_root.join(common).join("info").join("exclude")
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let needle = format!("{entry}/");
    if existing
        .lines()
        .any(|line| matches!(line.trim(), l if l == entry || l == needle))
    {
        return;
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!("{needle}\n"));
    let _ = std::fs::write(path, content);
}

/// What Git can currently prove about a worktree relative to its base.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeReport {
    pub branch: String,
    pub path: String,
    pub base_commit: String,
    /// Commits on the branch since base (`None` when the probe failed).
    pub commits: Option<u32>,
    pub commit_log: Vec<String>,
    /// Working tree has uncommitted or untracked changes (`None` unknown).
    pub dirty: Option<bool>,
    /// Combined reviewable patch: base..worktree diff plus untracked files.
    pub patch: String,
    pub patch_truncated: bool,
    /// True when any probe failed: `commits`/`dirty`/`patch` are not
    /// trustworthy and dependent actions (auto-prune, unforced discard)
    /// must treat the changeset as unsafe.
    pub inspection_failed: bool,
    /// Set when the worktree directory no longer exists (pruned or removed).
    pub gone: bool,
}

const PATCH_REVIEW_CAP_BYTES: usize = 256 * 1024;

impl WorktreeHandle {
    pub fn inspect(&self) -> WorktreeReport {
        let mut report = WorktreeReport {
            branch: self.branch.clone(),
            path: self.path.display().to_string(),
            base_commit: self.base_commit.clone(),
            commits: None,
            commit_log: Vec::new(),
            dirty: None,
            patch: String::new(),
            patch_truncated: false,
            inspection_failed: false,
            gone: !self.path.exists(),
        };
        if report.gone {
            report.inspection_failed = true;
            return report;
        }
        match git(
            &self.path,
            &[
                "rev-list",
                "--count",
                &format!("{}..HEAD", self.base_commit),
            ],
        ) {
            Ok(count) => report.commits = count.trim().parse::<u32>().ok(),
            Err(_) => report.inspection_failed = true,
        }
        if report.commits.is_none() {
            report.inspection_failed = true;
        }
        match git(
            &self.path,
            &[
                "log",
                "--format=%h %s",
                &format!("{}..HEAD", self.base_commit),
            ],
        ) {
            Ok(log) => report.commit_log = log.lines().map(str::to_string).collect(),
            Err(_) => report.inspection_failed = true,
        }
        let status = match git(&self.path, &["status", "--porcelain", "-uall"]) {
            Ok(status) => {
                report.dirty = Some(!status.trim().is_empty());
                status
            }
            Err(_) => {
                report.inspection_failed = true;
                String::new()
            }
        };
        // `git diff <base>` covers committed and unstaged tracked edits.
        // Untracked files are appended with `--no-index` hunks (same trick
        // the H0 eval fixture diff uses) so brand-new child files are
        // reviewable before any merge ever commits them.
        let mut patch = git(
            &self.path,
            &["diff", "--binary", "--no-ext-diff", &self.base_commit],
        )
        .unwrap_or_default();
        for untracked in status
            .lines()
            .filter(|line| line.starts_with("?? "))
            .filter_map(|line| line.get(3..))
        {
            if let Ok(out) = git_no_index(&self.path, untracked) {
                patch.push_str(&out);
            }
        }
        if patch.len() > PATCH_REVIEW_CAP_BYTES {
            let mut cut = PATCH_REVIEW_CAP_BYTES;
            while !patch.is_char_boundary(cut) {
                cut -= 1;
            }
            patch.truncate(cut);
            patch.push_str("\n… patch truncated …");
            report.patch_truncated = true;
        }
        report.patch = patch;
        report
    }

    pub fn report_json(report: &WorktreeReport, merged: bool, pruned: bool) -> Value {
        serde_json::json!({
            "branch": report.branch,
            "path": report.path,
            "baseCommit": report.base_commit,
            "commits": report.commits,
            "commitLog": report.commit_log,
            "dirty": report.dirty,
            "inspectionFailed": report.inspection_failed,
            "gone": report.gone || pruned,
            "merged": merged,
            "pruned": pruned,
        })
    }

    /// Hermes fail-safe (#88113): only a *proven* empty worktree (zero
    /// commits, clean, every probe succeeded) may be auto-pruned. Any unknown
    /// keeps everything on disk and reports the failure upward.
    pub fn prune_if_empty(&self) -> Result<bool, String> {
        let report = self.inspect();
        if report.gone {
            return Ok(false);
        }
        let proven_empty =
            !report.inspection_failed && report.commits == Some(0) && report.dirty == Some(false);
        if !proven_empty {
            return Ok(false);
        }
        self.remove()?;
        Ok(true)
    }

    /// Remove the worktree directory and its branch. Refuses nothing here —
    /// callers decide emptiness/force policy before calling.
    pub fn remove(&self) -> Result<(), String> {
        git(
            &self.repo_root,
            &[
                "worktree",
                "remove",
                "--force",
                &self.path.to_string_lossy(),
            ],
        )?;
        let _ = git(&self.repo_root, &["branch", "-D", &self.branch]);
        Ok(())
    }
}

/// `git diff --no-index` exits 1 exactly when a difference exists; both 0
/// and 1 carry the patch on stdout.
fn git_no_index(root: &Path, relative: &str) -> Result<String, String> {
    let output = Command::new("git")
        .args([
            "diff",
            "--no-index",
            "--binary",
            "--",
            "/dev/null",
            relative,
        ])
        .current_dir(root)
        .stdin(Stdio::null())
        .env("GIT_PAGER", "cat")
        .output()
        .map_err(|err| err.to_string())?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Outcome of an explicit merge attempt.
#[derive(Clone, Debug)]
pub enum MergeOutcome {
    /// Branch had no commits and no uncommitted work; nothing to apply.
    NothingToMerge,
    /// Branch (including committed working-tree changes) is now merged into
    /// the parent HEAD.
    Merged,
    /// The identical changeset was already applied earlier.
    AlreadyApplied,
    /// Refused before touching the checkout: parent dirty / child unproven.
    Refused(String),
    /// Merge conflicted; the merge was aborted and the parent checkout is
    /// back at its pre-merge HEAD. The worktree is kept for rework.
    Conflict(Vec<String>),
}

impl MergeOutcome {
    pub fn to_json(&self) -> Value {
        match self {
            Self::NothingToMerge => serde_json::json!({"status": "nothingToMerge"}),
            Self::Merged => serde_json::json!({"status": "merged"}),
            Self::AlreadyApplied => serde_json::json!({"status": "alreadyApplied"}),
            Self::Refused(reason) => {
                serde_json::json!({"status": "refused", "reason": reason})
            }
            Self::Conflict(paths) => {
                serde_json::json!({"status": "conflict", "conflictingPaths": paths})
            }
        }
    }
}

/// Explicit parent-side merge of a child worktree into the main checkout.
/// Rules: the child's uncommitted work (including untracked files) is
/// committed onto its branch first so review and merge cover it; the parent
/// checkout must be clean (baseline changes are fine — git merges against
/// the real merge base, not the recorded base); a conflict aborts and
/// restores the parent HEAD.
pub fn merge(handle: &WorktreeHandle) -> MergeOutcome {
    if handle.path.exists() {
        let status = match git(&handle.path, &["status", "--porcelain", "-uall"]) {
            Ok(status) => status,
            Err(err) => return MergeOutcome::Refused(format!("cannot inspect worktree: {err}")),
        };
        if !status.trim().is_empty() {
            if let Err(err) = git(&handle.path, &["add", "--all"]) {
                return MergeOutcome::Refused(format!("git add failed: {err}"));
            }
            let msg = format!("dandelion: subagent {} working tree", handle.branch);
            // Identity is supplied per-invocation so a machine without a
            // global git identity can still finalize a child changeset.
            if let Err(err) = commit_on_behalf(&handle.path, &msg) {
                return MergeOutcome::Refused(format!("cannot commit child changes: {err}"));
            }
        }
    }
    let branch_commit = match git(
        &handle.repo_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", handle.branch),
        ],
    ) {
        Ok(commit) => commit.trim().to_string(),
        Err(err) => {
            return if handle.path.exists() {
                MergeOutcome::Refused(format!("child branch unusable: {err}"))
            } else {
                MergeOutcome::NothingToMerge
            };
        }
    };
    if branch_commit == handle.base_commit {
        return MergeOutcome::NothingToMerge;
    }
    if git(
        &handle.repo_root,
        &["merge-base", "--is-ancestor", &handle.branch, "HEAD"],
    )
    .is_ok()
    {
        return MergeOutcome::AlreadyApplied;
    }
    let parent_status = git(&handle.repo_root, &["status", "--porcelain"]);
    if parent_status.as_deref().map(str::trim) != Ok("") {
        return MergeOutcome::Refused(
            "parent workspace has uncommitted changes; commit or stash them first".into(),
        );
    }
    let merge_res = git(
        &handle.repo_root,
        &[
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            &format!("dandelion: merge {}", handle.branch),
            &handle.branch,
        ],
    );
    if merge_res.is_err() {
        let conflicts = git(
            &handle.repo_root,
            &["diff", "--name-only", "--diff-filter=U"],
        )
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
        // Always return the checkout to its pre-merge state.
        let _ = git(&handle.repo_root, &["merge", "--abort"]);
        return MergeOutcome::Conflict(conflicts);
    }
    MergeOutcome::Merged
}

fn commit_on_behalf(cwd: &Path, message: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-c")
        .arg("core.hooksPath=.dandelion-hooks-disabled")
        .arg("-c")
        .arg("user.name=dandelion-subagent")
        .arg("-c")
        .arg("user.email=subagent@dandelion.invalid")
        .args(["commit", "--quiet", "-m", message])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::new())
}

/// Explicit discard. Non-empty or unproven changesets need `force`, and the
/// result states exactly what was thrown away.
pub fn discard(handle: &WorktreeHandle, force: bool) -> Result<Value, String> {
    let report = handle.inspect();
    if report.gone {
        return Ok(serde_json::json!({"status": "alreadyGone", "branch": report.branch}));
    }
    let has_content = report.commits.map(|c| c > 0).unwrap_or(true)
        || report.dirty.unwrap_or(true)
        || report.inspection_failed;
    if has_content && !force {
        return Err(format!(
            "worktree has unmerged content (commits: {:?}, dirty: {:?}, inspectionFailed: {}); re-run with force=true to discard",
            report.commits, report.dirty, report.inspection_failed
        ));
    }
    handle.remove()?;
    Ok(serde_json::json!({
        "status": "discarded",
        "branch": report.branch,
        "discardedCommits": report.commits,
        "discardedDirty": report.dirty,
    }))
}

/// One-line summary of a report for the parent's child-summary text.
pub fn summary_line(report: &WorktreeReport) -> String {
    if report.gone {
        return format!("worktree {} already gone", report.branch);
    }
    format!(
        "changeset: branch {} (commits: {:?}, uncommitted: {:?}{}) — review with delegate_task action=changes, then merge or discard",
        report.branch,
        report.commits,
        report.dirty,
        if report.inspection_failed {
            ", inspection incomplete"
        } else {
            ""
        }
    )
}

/// Note appended to the child's task text so the model knows where to work.
/// Mirrors Hermes `build_worktree_context_note`.
pub fn context_note(handle: &WorktreeHandle) -> String {
    format!(
        "\n\n[Worktree isolation]\n\
         Your files live in an isolated git worktree at `{path}` on branch `{branch}`, \
         created from commit `{base}`. Your changes never touch the parent workspace. \
         Write and edit files normally; relative paths resolve inside your worktree. \
         If you can run commands, commit your finished work to this branch. \
         The parent will review the changeset and explicitly merge or discard it — \
         never merge or push yourself.",
        path = handle.path.display(),
        branch = handle.branch,
        base = handle.base_commit,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git_available() -> bool {
        Command::new("git")
            .args(["--version"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet", "."]).unwrap();
        git(
            dir.path(),
            &["config", "user.email", "test@dandelion.invalid"],
        )
        .unwrap();
        git(dir.path(), &["config", "user.name", "Dandelion Test"]).unwrap();
        fs::write(dir.path().join("seed.txt"), "seed\n").unwrap();
        git(dir.path(), &["add", "--all"]).unwrap();
        git(dir.path(), &["commit", "--quiet", "-m", "baseline"]).unwrap();
        dir
    }

    #[test]
    fn create_and_inspect_capture_uncommitted_and_untracked_child_work() {
        if !git_available() {
            return;
        }
        let dir = repo();
        let repo_root = dir.path();
        let base = head_commit(repo_root).unwrap();
        let handle = create(&repo_root_for(repo_root).unwrap(), &base, "child1").unwrap();
        assert!(handle.path.join("seed.txt").exists());
        fs::write(handle.path.join("child.txt"), "child work\n").unwrap();
        let report = handle.inspect();
        assert_eq!(report.commits, Some(0));
        assert_eq!(report.dirty, Some(true));
        assert!(!report.inspection_failed);
        assert!(report.patch.contains("child work"), "{}", report.patch);
        // The parent checkout is untouched — no silent merge.
        assert!(!repo_root.join("child.txt").exists());
        // Parent status never shows the worktree directory.
        let status = git(repo_root, &["status", "--porcelain"]).unwrap();
        assert!(!status.contains(WORKTREE_DIR), "{status}");
    }

    #[test]
    fn inspect_includes_committed_diff() {
        if !git_available() {
            return;
        }
        let dir = repo();
        let repo_root = dir.path();
        let base = head_commit(repo_root).unwrap();
        let handle = create(&repo_root_for(repo_root).unwrap(), &base, "c").unwrap();
        fs::write(handle.path.join("seed.txt"), "edited\n").unwrap();
        git(handle.path.as_path(), &["add", "--all"]).unwrap();
        git(handle.path.as_path(), &["commit", "--quiet", "-m", "edit"]).unwrap();
        let report = handle.inspect();
        assert_eq!(report.commits, Some(1));
        assert_eq!(report.dirty, Some(false));
        assert!(report.patch.contains("edited"));
    }

    #[test]
    fn prune_keeps_dirty_and_prunes_proven_empty() {
        if !git_available() {
            return;
        }
        let dir = repo();
        let repo_root = dir.path();
        let base = head_commit(repo_root).unwrap();
        let dirty_wt = create(&repo_root_for(repo_root).unwrap(), &base, "dirty").unwrap();
        fs::write(dirty_wt.path.join("x.txt"), "keep me\n").unwrap();
        assert!(!dirty_wt.prune_if_empty().unwrap());
        assert!(dirty_wt.path.exists());
        let empty_wt = create(&repo_root_for(repo_root).unwrap(), &base, "empty").unwrap();
        assert!(empty_wt.prune_if_empty().unwrap());
        assert!(!empty_wt.path.exists());
    }

    #[test]
    fn merge_applies_committed_and_uncommitted_changes() {
        if !git_available() {
            return;
        }
        let dir = repo();
        let repo_root = dir.path();
        let base = head_commit(repo_root).unwrap();
        let handle = create(&repo_root_for(repo_root).unwrap(), &base, "mergeme").unwrap();
        fs::write(handle.path.join("committed.txt"), "committed\n").unwrap();
        git(handle.path.as_path(), &["add", "--all"]).unwrap();
        git(
            handle.path.as_path(),
            &["commit", "--quiet", "-m", "child commit"],
        )
        .unwrap();
        fs::write(handle.path.join("loose.txt"), "uncommitted\n").unwrap();
        assert!(matches!(merge(&handle), MergeOutcome::Merged));
        assert!(repo_root.join("committed.txt").exists());
        assert!(repo_root.join("loose.txt").exists());
    }

    #[test]
    fn merge_refuses_dirty_parent_and_duplicate_is_already_applied() {
        if !git_available() {
            return;
        }
        let dir = repo();
        let repo_root = dir.path();
        let base = head_commit(repo_root).unwrap();
        let handle = create(&repo_root_for(repo_root).unwrap(), &base, "dup").unwrap();
        fs::write(handle.path.join("new.txt"), "v\n").unwrap();
        fs::write(repo_root.join("parent-dirty.txt"), "uncommitted parent\n").unwrap();
        match merge(&handle) {
            MergeOutcome::Refused(reason) => assert!(reason.contains("uncommitted"), "{reason}"),
            other => panic!("expected refusal, got {other:?}"),
        }
        // The refusal must not touch the parent checkout…
        assert!(!repo_root.join("new.txt").exists());
        // …while the child's work is durably saved on its own branch (a
        // side-effect-free place to park it until the retry merge).
        assert_eq!(handle.inspect().commits, Some(1));
        fs::remove_file(repo_root.join("parent-dirty.txt")).unwrap();
        assert!(matches!(merge(&handle), MergeOutcome::Merged));
        // Duplicate application is a no-op, never a second merge commit.
        let head_before = git(repo_root, &["rev-parse", "HEAD"]).unwrap();
        assert!(matches!(merge(&handle), MergeOutcome::AlreadyApplied));
        assert_eq!(head_before, git(repo_root, &["rev-parse", "HEAD"]).unwrap());
    }

    #[test]
    fn merge_conflict_aborts_and_restores_parent() {
        if !git_available() {
            return;
        }
        let dir = repo();
        let repo_root = dir.path();
        let base = head_commit(repo_root).unwrap();
        let handle = create(&repo_root_for(repo_root).unwrap(), &base, "conflict").unwrap();
        fs::write(handle.path.join("seed.txt"), "child version\n").unwrap();
        // Parent moves the same file after the base commit (baseline change).
        fs::write(repo_root.join("seed.txt"), "parent version\n").unwrap();
        git(repo_root, &["add", "--all"]).unwrap();
        git(repo_root, &["commit", "--quiet", "-m", "parent change"]).unwrap();
        let head_before = git(repo_root, &["rev-parse", "HEAD"]).unwrap();
        match merge(&handle) {
            MergeOutcome::Conflict(paths) => assert!(
                paths
                    .iter()
                    .any(|p| p.replace('\\', "/").contains("seed.txt")),
                "{paths:?}"
            ),
            other => panic!("expected conflict, got {other:?}"),
        }
        // Aborted: parent back at pre-merge HEAD, child worktree kept.
        assert_eq!(head_before, git(repo_root, &["rev-parse", "HEAD"]).unwrap());
        let seed = fs::read_to_string(repo_root.join("seed.txt")).unwrap();
        assert_eq!(seed.replace("\r\n", "\n"), "parent version\n");
        assert!(handle.path.exists());
    }

    #[test]
    fn discard_requires_force_for_content() {
        if !git_available() {
            return;
        }
        let dir = repo();
        let repo_root = dir.path();
        let base = head_commit(repo_root).unwrap();
        let handle = create(&repo_root_for(repo_root).unwrap(), &base, "gone").unwrap();
        fs::write(handle.path.join("keep.txt"), "work\n").unwrap();
        let err = discard(&handle, false).unwrap_err();
        assert!(err.contains("force=true"), "{err}");
        assert!(handle.path.exists());
        let res = discard(&handle, true).unwrap();
        assert_eq!(res["status"], "discarded");
        assert_eq!(res["discardedDirty"], serde_json::json!(true));
        assert!(!handle.path.exists());
    }

    #[test]
    fn empty_changeset_merges_as_nothing_to_merge() {
        if !git_available() {
            return;
        }
        let dir = repo();
        let repo_root = dir.path();
        let base = head_commit(repo_root).unwrap();
        let handle = create(&repo_root_for(repo_root).unwrap(), &base, "noop").unwrap();
        assert!(matches!(merge(&handle), MergeOutcome::NothingToMerge));
    }
}
