//! End-to-end checks against a real `wt` binary and a real repository.
//!
//! These exercise the parts a unit test cannot: that worktrunk actually accepts
//! the arguments wt-tui sends, and that the read/watch model reflects changes
//! made by another process — the agent scenario wt-tui exists for.
//!
//! Each test skips (rather than fails) when `wt` is unavailable, so the suite
//! still runs on a machine without worktrunk installed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn have_wt() -> bool {
    Command::new("wt")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

macro_rules! require_wt {
    () => {
        if !have_wt() {
            eprintln!("skipping: `wt` is not installed");
            return;
        }
    };
}

/// A throwaway repository with a remote and two worktrees.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        // `keep()` rather than `into_path()`: sibling worktrees live outside
        // the repo directory, so cleanup is handled in `Drop`.
        let root = tempfile::tempdir().expect("tempdir").keep();
        let f = Fixture { root: root.clone() };

        run(&mut git(), &["init", "-q", "-b", "main"], &root);
        run(
            &mut git(),
            &["config", "user.email", "t@example.com"],
            &root,
        );
        run(&mut git(), &["config", "user.name", "Test"], &root);

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("data.txt"), "alpha\nbeta\n").unwrap();

        run(&mut git(), &["add", "-A"], &root);
        run(&mut git(), &["commit", "-qm", "initial commit"], &root);

        // A remote so the fixture covers the `upstream` object.
        let remote = root.with_extension("remote.git");
        run(
            &mut git(),
            &["init", "-q", "--bare", remote.to_str().unwrap()],
            Path::new("/"),
        );
        run(
            &mut git(),
            &["remote", "add", "origin", remote.to_str().unwrap()],
            &root,
        );
        run(&mut git(), &["push", "-q", "-u", "origin", "main"], &root);

        f
    }

    fn wt(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = wt_bin();
        cmd.arg("-C").arg(&self.root);
        cmd.args(args);
        cmd.output().expect("run wt")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Ask git which directories this repository actually created, instead of
        // scanning the parent for names that merely start with the fixture's.
        // The prefix scan worked only because worktrunk happens to name siblings
        // `<repo>-<branch>`, and it would `remove_dir_all` anything else in that
        // directory sharing the prefix — on a shared CI runner, that can be a
        // directory belonging to another job.
        let mut cmd = git();
        cmd.arg("-C")
            .arg(&self.root)
            .args(["worktree", "list", "--porcelain"]);
        if let Ok(out) = cmd.output()
            && out.status.success()
        {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(path) = line.strip_prefix("worktree ") {
                    let _ = std::fs::remove_dir_all(path);
                }
            }
        }

        // The bare remote is a sibling created by `Fixture::new`, and is not a
        // worktree, so it is named explicitly.
        let _ = std::fs::remove_dir_all(self.root.with_extension("remote.git"));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn wt_bin() -> Command {
    Command::new(std::env::var("WT_TUI_WT_BIN").unwrap_or_else(|_| "wt".to_string()))
}

fn git() -> Command {
    Command::new("git")
}

fn run(cmd: &mut Command, args: &[&str], cwd: &Path) {
    let status = cmd
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {cmd:?}: {e}"));
    assert!(status.success(), "{cmd:?} failed with {status}");
}

#[tokio::test]
async fn list_reads_a_real_repository() {
    require_wt!();
    let fx = Fixture::new();

    let envelope = wt_tui::wt::command::list(&fx.root, wt_tui::wt::ListScope::WORKTREES)
        .await
        .expect("wt list should succeed");

    assert_eq!(envelope.schema, 2);
    assert!(!envelope.items.is_empty());
    assert_eq!(
        envelope
            .repo
            .as_ref()
            .and_then(|r| r.default_branch.as_deref()),
        Some("main")
    );
    assert!(
        envelope
            .items
            .iter()
            .any(|i| i.branch.as_deref() == Some("main")),
        "the main worktree should be listed"
    );
}

#[tokio::test]
async fn a_worktree_created_by_another_process_appears() {
    require_wt!();
    let fx = Fixture::new();

    let before = wt_tui::wt::command::list(&fx.root, wt_tui::wt::ListScope::WORKTREES)
        .await
        .expect("first list");
    let before_branches: Vec<String> = before
        .items
        .iter()
        .filter_map(|i| i.branch.clone())
        .collect();
    assert!(!before_branches.contains(&"agent-made".to_string()));

    // Simulate an agent working in a different process.
    let out = fx.wt(&["switch", "--create", "agent-made", "--no-cd", "-y"]);
    assert!(
        out.status.success(),
        "wt switch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // No restart, no cache: the same command simply reports it now. This is the
    // whole premise of the live-update design.
    let after = wt_tui::wt::command::list(&fx.root, wt_tui::wt::ListScope::WORKTREES)
        .await
        .expect("second list");
    let after_branches: Vec<String> = after
        .items
        .iter()
        .filter_map(|i| i.branch.clone())
        .collect();

    assert!(
        after_branches.contains(&"agent-made".to_string()),
        "a worktree created elsewhere must show up on the next read: {after_branches:?}"
    );
}

#[tokio::test]
async fn create_and_remove_round_trip() {
    require_wt!();
    let fx = Fixture::new();

    // Create.
    let created = wt_tui::wt::command::create(&fx.root, "round-trip", None)
        .await
        .expect("create should succeed");
    assert_eq!(created.branch.as_deref(), Some("round-trip"));
    assert!(created.created_branch);
    assert!(created.path.is_some(), "the new worktree path is reported");

    let listed = wt_tui::wt::command::list(&fx.root, wt_tui::wt::ListScope::WORKTREES)
        .await
        .expect("list");
    assert!(
        listed
            .items
            .iter()
            .any(|i| i.branch.as_deref() == Some("round-trip"))
    );

    // Remove. The branch is at the same commit as main, so worktrunk deletes it
    // and reports `branch_outcome: deleted` — the path through the JSON-boundary
    // parsing that `wt remove`'s progress lines precede.
    let removed = wt_tui::wt::command::remove(&fx.root, "round-trip", false)
        .await
        .expect("remove should succeed");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].branch.as_deref(), Some("round-trip"));
    assert_eq!(
        removed[0].branch_outcome,
        Some(wt_tui::wt::model::BranchOutcome::Deleted)
    );

    let after = wt_tui::wt::command::list(&fx.root, wt_tui::wt::ListScope::WORKTREES)
        .await
        .expect("list after remove");
    assert!(
        !after
            .items
            .iter()
            .any(|i| i.branch.as_deref() == Some("round-trip")),
        "the worktree should be gone"
    );
}

#[tokio::test]
async fn a_dirty_worktree_is_refused_without_force() {
    require_wt!();
    let fx = Fixture::new();

    fx.wt(&["switch", "--create", "dirty-one", "--no-cd", "-y"]);

    let listed = wt_tui::wt::command::list(&fx.root, wt_tui::wt::ListScope::WORKTREES)
        .await
        .expect("list");
    let path = listed
        .items
        .iter()
        .find(|i| i.branch.as_deref() == Some("dirty-one"))
        .and_then(|i| i.worktree.as_ref())
        .and_then(|w| w.path.clone())
        .expect("worktree path");

    // Make it dirty.
    std::fs::write(Path::new(&path).join("uncommitted.txt"), "wip\n").unwrap();

    let dirty = wt_tui::wt::command::list(&fx.root, wt_tui::wt::ListScope::WORKTREES)
        .await
        .expect("list");
    let item = dirty
        .items
        .iter()
        .find(|i| i.branch.as_deref() == Some("dirty-one"))
        .expect("the row is still there");

    // Branch integration and worktree cleanliness are orthogonal: this branch is
    // at the same commit as main (so its *branch* is safe to delete) while the
    // *worktree* has uncommitted work. Both facts have to reach the dialog,
    // because only one of them blocks a non-forced removal.
    assert!(
        item.has_changes(),
        "the TUI must warn before discarding uncommitted work"
    );
    assert!(
        item.is_integrated(),
        "a branch at the default branch's commit is integrated, even when dirty"
    );
    assert!(
        item.removal_reason().contains("same_commit"),
        "the reason should come from worktrunk's own check: {}",
        item.removal_reason()
    );

    // wt-tui's dialog exists precisely because this refusal is expected.
    let refused = wt_tui::wt::command::remove(&fx.root, "dirty-one", false).await;
    assert!(
        refused.is_err(),
        "worktrunk must refuse to drop uncommitted changes without --force"
    );
    let message = refused.unwrap_err();
    assert!(
        message.contains("uncommitted changes"),
        "the message should name the reason: {message}"
    );

    // With force, it goes.
    wt_tui::wt::command::remove(&fx.root, "dirty-one", true)
        .await
        .expect("forced removal should succeed");
}

#[tokio::test]
async fn removing_an_unknown_branch_reports_an_error_not_a_panic() {
    require_wt!();
    let fx = Fixture::new();

    let result = wt_tui::wt::command::remove(&fx.root, "no-such-branch", false).await;
    assert!(
        result.is_err(),
        "removing nothing must be an error the UI can show"
    );
    let message = result.unwrap_err();
    assert!(
        !message.is_empty(),
        "the error must explain itself to the user"
    );
}

#[test]
fn the_watch_directory_is_the_shared_git_worktrees_dir() {
    require_wt!();
    let fx = Fixture::new();
    fx.wt(&["switch", "--create", "watched", "--no-cd", "-y"]);

    // Git records one directory per worktree; that is what wt-tui watches.
    let worktrees = fx.root.join(".git/worktrees");
    assert!(
        worktrees.is_dir(),
        "expected the worktrees dir at {}",
        worktrees.display()
    );
    let entries: Vec<_> = std::fs::read_dir(&worktrees)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        entries.iter().any(|e| e.contains("watched")),
        "the new worktree should have an entry under .git/worktrees: {entries:?}"
    );
}

#[test]
fn running_outside_a_repository_fails_cleanly() {
    require_wt!();
    let dir = tempfile::tempdir().expect("tempdir");
    let result = std::process::Command::new("wt")
        .arg("-C")
        .arg(dir.path())
        .args(["list", "--format=json"])
        .output()
        .expect("wt runs");
    assert!(!result.status.success());
    assert!(
        !result.stderr.is_empty(),
        "worktrunk should explain why it failed"
    );
}
