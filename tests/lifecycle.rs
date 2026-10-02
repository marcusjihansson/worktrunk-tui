//! Merge and prune against real worktrunk.
//!
//! These cover the hazards that motivated the design: `wt merge`'s direction, a
//! conflicted merge leaving a git operation in progress, and prune's `--min-age`
//! guard silently skipping freshly created worktrees.

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

fn run(args: &[&str], cwd: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    /// Every worktree path git reports for this repository, including the main
    /// one. Used for cleanup so nothing outside the fixture is ever a deletion
    /// candidate.
    fn worktree_paths(&self) -> Vec<PathBuf> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .expect("git worktree list");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("worktree ").map(PathBuf::from))
            .collect()
    }
}

impl Fixture {
    fn new() -> Fixture {
        // A unique directory that is *not* removed when the guard drops: the
        // fixture is `Drop`-based so it can clean up the sibling worktrees it
        // creates, which a plain TempDir cannot express.
        let root = unique_dir("wt-tui-lifecycle");
        let f = Fixture { root: root.clone() };

        run(&["init", "-q", "-b", "main"], &root);
        run(&["config", "user.email", "t@example.com"], &root);
        run(&["config", "user.name", "Test"], &root);
        std::fs::write(root.join("f.txt"), "base\n").unwrap();
        run(&["add", "-A"], &root);
        run(&["commit", "-qm", "base"], &root);
        f
    }

    fn wt(&self, args: &[&str]) -> String {
        let out = Command::new("wt")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .expect("run wt");
        // Worktrunk writes progress and results to stderr when stderr is not a
        // terminal, so both streams are read.
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    /// The worktree git reports as having `branch` checked out.
    ///
    /// Read from `git worktree list --porcelain` rather than guessing the path
    /// template, which is user-configurable.
    fn worktree_for_branch(&self, branch: &str) -> Option<PathBuf> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .expect("git worktree list");
        let text = String::from_utf8_lossy(&out.stdout);

        let wanted = format!("refs/heads/{branch}");
        let mut current: Option<PathBuf> = None;
        for line in text.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                current = Some(PathBuf::from(path));
            } else if line.strip_prefix("branch ") == Some(wanted.as_str()) {
                return current;
            }
        }
        None
    }

    /// Create a worktree and return its path.
    fn make_worktree(&self, branch: &str) -> PathBuf {
        self.wt(&["switch", "--create", branch, "--no-cd", "-y"]);
        self.worktree_for_branch(branch).unwrap_or_else(|| {
            panic!(
                "no worktree checked out on {branch} under {}",
                self.root.display()
            )
        })
    }

    /// The main worktree's `f.txt`, for checking what a merge did to it.
    fn main_file(&self) -> String {
        std::fs::read_to_string(self.root.join("f.txt")).unwrap_or_default()
    }
}

/// A directory path under the temp dir that no one else will claim.
fn unique_dir(prefix: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create fixture dir");
    path
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Remove exactly the worktrees git reports, then the repo.
        //
        // This used to scan the parent for any directory whose name started with
        // the fixture's. That worked, but it meant cleanup authority was decided
        // by a name prefix in a shared directory: on a CI runner whose
        // `TMPDIR` is shared, an unrelated directory sharing the prefix would
        // have been deleted. `git worktree list` names the paths this
        // repository actually owns.
        for path in self.worktree_paths() {
            let _ = std::fs::remove_dir_all(path);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn merging_runs_from_the_source_worktree_and_moves_its_commits() {
    require_wt!();
    let fx = Fixture::new();
    let worktree = fx.make_worktree("feature");

    std::fs::write(worktree.join("f.txt"), "base\nfeature work\n").unwrap();
    run(&["add", "-A"], &worktree);
    run(&["commit", "-qm", "feature work"], &worktree);

    // This is the direction that matters: `-C <the row's worktree>`, so the
    // branch being merged is the *current* one.
    let outcome = wt_tui::wt::command::merge(&worktree, "main", false)
        .await
        .expect("merge runs");

    assert!(
        outcome.succeeded,
        "merge should report a result: {outcome:?}"
    );
    assert!(
        !outcome.left_operation_in_progress(&worktree),
        "a clean merge must not leave a git operation in progress"
    );
    let result = outcome.result.clone().expect("a result object");
    assert_eq!(result.branch.as_deref(), Some("feature"));
    assert_eq!(result.target.as_deref(), Some("main"));

    assert!(
        fx.main_file().contains("feature work"),
        "main should have received the branch's work, got {:?}",
        fx.main_file()
    );
}

#[tokio::test]
async fn a_merge_from_the_wrong_directory_would_reverse_the_direction() {
    // This is the hazard the design avoids. Run from the repo root, `wt merge
    // <branch>` merges the root's branch INTO <branch> — so the guard is that
    // the caller passes the row's worktree, not the repo.
    require_wt!();
    let fx = Fixture::new();
    fx.make_worktree("victim");

    // Give main a commit the branch does not have.
    std::fs::write(fx.root.join("f.txt"), "base\nmain-only\n").unwrap();
    run(&["add", "-A"], &fx.root);
    run(&["commit", "-qm", "main only"], &fx.root);

    let victim = fx
        .worktree_for_branch("victim")
        .expect("the victim worktree");
    let victim_before = std::fs::read_to_string(victim.join("f.txt")).unwrap_or_default();

    // Wrong direction, run deliberately to demonstrate the hazard.
    let outcome = wt_tui::wt::command::merge(&fx.root, "victim", true)
        .await
        .expect("merge runs");

    let victim_after = std::fs::read_to_string(victim.join("f.txt")).unwrap_or_default();
    assert_ne!(
        victim_before, victim_after,
        "merging from the repo root pushes main's commits onto the branch, \
         which is the opposite of what a user means"
    );
    assert!(
        !fx.main_file().contains("main-only") || outcome.succeeded,
        "main should not be the target in the wrong-direction case"
    );
}

#[tokio::test]
async fn keeping_the_worktree_leaves_it_in_place() {
    require_wt!();
    let fx = Fixture::new();
    let worktree = fx.make_worktree("keep-branch");

    std::fs::write(worktree.join("f.txt"), "base\nkept\n").unwrap();
    run(&["add", "-A"], &worktree);
    run(&["commit", "-qm", "kept"], &worktree);

    let outcome = wt_tui::wt::command::merge(&worktree, "main", true)
        .await
        .expect("merge runs");

    let result = outcome.result.expect("a result");
    assert!(!result.removed, "the worktree should have been kept");
    assert!(
        worktree.exists(),
        "the worktree directory should still exist"
    );
    assert!(fx.main_file().contains("kept"));
}

#[tokio::test]
async fn a_conflicted_merge_is_detected_as_leaving_an_operation_in_progress() {
    require_wt!();
    let fx = Fixture::new();

    // Two branches touching the same line, so the second merge conflicts.
    let a = fx.make_worktree("conflict-src");
    let b = fx.make_worktree("conflict-other");
    std::fs::write(a.join("f.txt"), "base\nFROM-A\n").unwrap();
    std::fs::write(b.join("f.txt"), "base\nFROM-B\n").unwrap();
    run(&["add", "-A"], &a);
    run(&["commit", "-qm", "a"], &a);
    run(&["add", "-A"], &b);
    run(&["commit", "-qm", "b"], &b);

    // Land B on main first, then merging A must conflict.
    let outcome_b = wt_tui::wt::command::merge(&b, "main", false).await;
    assert!(outcome_b.is_ok());

    let outcome = wt_tui::wt::command::merge(&a, "main", false)
        .await
        .expect("merge attempt runs");

    // A conflicted merge reports no result and exits non-zero, yet leaves the
    // repository mid-rebase. That is why the UI checks the repository state
    // instead of trusting the JSON payload.
    assert!(
        !outcome.succeeded,
        "a conflicted merge must not report a completed merge: {outcome:?}"
    );
    assert!(
        outcome.left_operation_in_progress(&a),
        "the worktree should be left with a rebase in progress: {outcome:?}"
    );

    // Whether worktrunk reported success or not, the repository state is what
    // decides.
    if outcome.left_operation_in_progress(&a) {
        // Clean up so the fixture can be dropped.
        let _ = Command::new("git")
            .args(["rebase", "--abort"])
            .current_dir(&a)
            .output();
        let _ = Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(&a)
            .output();
    }
    assert!(
        !outcome.left_operation_in_progress(&a),
        "after aborting, no operation should remain"
    );
}

#[tokio::test]
async fn merging_from_a_nonexistent_worktree_does_not_report_success() {
    // The command runs and fails, so it is an outcome rather than an `Err` — but
    // it must never look like a completed merge.
    require_wt!();
    let fx = Fixture::new();
    let outcome = wt_tui::wt::command::merge(&fx.root.join("no-such-dir"), "main", false)
        .await
        .expect("the command runs even when it fails");

    assert!(!outcome.succeeded, "a merge that never ran is not a merge");
    assert!(outcome.result.is_none(), "there should be no result object");
    assert!(!outcome.exit_ok, "worktrunk should exit non-zero");
    assert!(
        !outcome.stderr.trim().is_empty(),
        "the failure must be explained to the user"
    );
}

// ---------------------------------------------------------------------------
// Prune
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prune_preview_reports_nothing_for_fresh_worktrees_by_default() {
    // This is why the dialog surfaces min-age: with the default of 1d, prune
    // silently skips a worktree created moments ago, which reads as broken.
    require_wt!();
    let fx = Fixture::new();
    fx.make_worktree("fresh-one");
    fx.make_worktree("fresh-two");

    let default_age = wt_tui::wt::command::prune_preview(&fx.root, None)
        .await
        .expect("prune preview runs");
    assert!(
        default_age.is_empty(),
        "the default min-age of 1d should skip fresh worktrees: {default_age:?}"
    );

    let no_guard = wt_tui::wt::command::prune_preview(&fx.root, Some("0"))
        .await
        .expect("prune preview runs");
    assert!(
        !no_guard.is_empty(),
        "min-age 0 should surface the fresh worktrees"
    );
}

#[tokio::test]
async fn prune_preview_explains_why_each_candidate_is_eligible() {
    require_wt!();
    let fx = Fixture::new();
    fx.make_worktree("integrated");

    let candidates = wt_tui::wt::command::prune_preview(&fx.root, Some("0"))
        .await
        .expect("prune preview runs");

    let candidate = candidates
        .first()
        .unwrap_or_else(|| panic!("expected a candidate: {candidates:?}"));
    assert!(
        candidate.path.as_deref().is_some_and(|p| fx
            .worktree_for_branch("integrated")
            .is_some_and(|w| w.to_string_lossy() == p)),
        "the candidate should name the integrated worktree: {candidate:?}"
    );

    assert!(
        candidate.reason.is_some(),
        "each candidate must say why it is safe: {candidate:?}"
    );
    assert_eq!(candidate.target.as_deref(), Some("main"));
}

#[tokio::test]
async fn a_branch_with_uncommitted_work_is_never_prunable() {
    require_wt!();
    let fx = Fixture::new();
    let worktree = fx.make_worktree("dirty-branch");
    std::fs::write(worktree.join("scratch.txt"), "wip\n").unwrap();

    let candidates = wt_tui::wt::command::prune_preview(&fx.root, Some("0"))
        .await
        .expect("prune preview runs");

    let dirty_path = fx
        .worktree_for_branch("dirty-branch")
        .map(|p| p.to_string_lossy().into_owned());
    assert!(
        !candidates
            .iter()
            .any(|c| c.path.is_some() && Some(c.path.as_deref().unwrap()) == dirty_path.as_deref()),
        "worktrunk always skips dirty worktrees, and the preview must reflect it: {candidates:?}"
    );
}
