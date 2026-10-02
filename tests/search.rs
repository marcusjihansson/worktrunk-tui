//! Search against real repositories and real worktrees.
//!
//! The unit tests in `src/search.rs` cover the matcher. These cover the part
//! that decides whether search is usable in practice: that a hit in one
//! worktree is found, that a hit in only one of several identical trees is not
//! misattributed, and that deduplication does not lose results.

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

/// Run git in `cwd`, panicking with output on failure.
fn run(args: &[&str], cwd: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed:\n{}\n{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.keep();
        let f = Fixture { root: root.clone() };

        run(&["init", "-q", "-b", "main"], &root);
        run(&["config", "user.email", "t@example.com"], &root);
        run(&["config", "user.name", "Test"], &root);

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("docs/guide.md"), "# Guide\n").unwrap();

        run(&["add", "-A"], &root);
        run(&["commit", "-qm", "initial"], &root);

        f
    }

    /// Run a `wt` command, returning its combined output.
    fn wt(&self, args: &[&str]) -> String {
        let out = Command::new("wt")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .expect("run wt");
        assert!(
            out.status.success(),
            "wt {args:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // Worktrunk writes to stderr when stderr is not a terminal.
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    /// Create a worktree for `branch` and return its path.
    ///
    /// The path is read back from `git worktree list` rather than scraped from
    /// worktrunk's human-readable output, which is not a stable interface.
    fn make_worktree(&self, branch: &str) -> PathBuf {
        self.wt(&["switch", "--create", branch, "--no-cd", "-y"]);
        self.worktree_paths()
            .into_iter()
            .find(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains(branch))
            })
            .unwrap_or_else(|| {
                panic!(
                    "no worktree for branch {branch}; have {:?}",
                    self.worktree_paths()
                )
            })
    }

    /// Every worktree path, discovered from git rather than assumed.
    fn worktree_paths(&self) -> Vec<PathBuf> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .expect("git worktree list");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix("worktree ").map(PathBuf::from))
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Remove exactly the worktrees git reports, then the repo.
        //
        // This used to scan the parent directory for any entry whose name
        // contained "worktrunk-fixture". That was wrong twice over: this fixture
        // uses `tempfile`'s names (`.tmpXXXXXX`), so the match never fired and
        // every worktree leaked into the temp directory — and had any unrelated
        // directory ever matched, on a shared CI runner it would have deleted a
        // directory belonging to another job.
        for path in self.worktree_paths() {
            let _ = std::fs::remove_dir_all(path);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Build search targets from a fixture's worktrees.
/// Every worktree in the fixture, named by the branch checked out in it.
fn targets(fx: &Fixture) -> Vec<wt_tui::search::Target> {
    let out = Command::new("git")
        .arg("-C")
        .arg(&fx.root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .expect("git worktree list");

    let mut targets = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch = String::from("main");
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(prev) = path.take() {
                targets.push(wt_tui::search::Target {
                    branch: std::mem::take(&mut branch),
                    path: prev,
                });
            }
            path = Some(PathBuf::from(p));
        } else if let Some(refs) = line.strip_prefix("branch refs/heads/") {
            branch = refs.to_string();
        }
    }
    if let Some(last) = path {
        targets.push(wt_tui::search::Target { branch, path: last });
    }
    targets
}

#[test]
fn finds_a_string_present_in_the_main_worktree() {
    require_wt!();
    let fx = Fixture::new();

    let results = wt_tui::search::search(
        "fn main",
        &targets(&fx),
        wt_tui::search::QueryOptions::default(),
    );

    assert!(results.error.is_none(), "{:?}", results.error);
    assert!(!results.is_empty(), "should find `fn main` in src/main.rs");
    assert!(
        results.hits.iter().any(|h| h.rel_path == "src/main.rs"),
        "expected a hit in src/main.rs, got {:?}",
        results.hits.iter().map(|h| &h.rel_path).collect::<Vec<_>>()
    );
}

#[test]
fn identical_trees_are_searched_once_but_reported_for_every_worktree() {
    require_wt!();
    let fx = Fixture::new();

    // Three worktrees branched from the same commit: byte-identical trees.
    fx.make_worktree("twin-one");
    fx.make_worktree("twin-two");

    let results = wt_tui::search::search(
        "fn main",
        &targets(&fx),
        wt_tui::search::QueryOptions::default(),
    );

    assert!(!results.is_empty());
    assert!(
        results.deduped > 0,
        "identical trees should have been deduplicated: {results:?}"
    );
    assert!(
        results.scanned < results.covered(),
        "fewer trees searched than worktrees covered: scanned={} covered={}",
        results.scanned,
        results.covered()
    );

    // The hit must still be attributed to every worktree holding the content.
    let hit = results
        .hits
        .iter()
        .find(|h| h.rel_path == "src/main.rs")
        .expect("a hit for src/main.rs");
    assert!(
        hit.reach() >= 2,
        "a deduplicated hit should still name every worktree sharing the tree: {:?}",
        hit.worktrees
    );
}

#[test]
fn a_string_unique_to_one_worktree_is_not_misattributed() {
    require_wt!();
    let fx = Fixture::new();

    let unique = fx.make_worktree("unique-one");
    std::fs::write(
        unique.join("src/unique_marker.rs"),
        "const UNIQUE_MARKER_ZZZ: u8 = 1;\n",
    )
    .unwrap();
    run(&["add", "-A"], &unique);
    run(&["commit", "-qm", "unique marker"], &unique);

    let results = wt_tui::search::search(
        "UNIQUE_MARKER_ZZZ",
        &targets(&fx),
        wt_tui::search::QueryOptions::default(),
    );

    assert!(!results.is_empty(), "should find the unique marker");
    // Every hit for it must come from the worktree that actually has it.
    for hit in results
        .hits
        .iter()
        .filter(|h| h.line.contains("UNIQUE_MARKER_ZZZ"))
    {
        assert!(
            hit.worktrees.iter().any(|b| b.contains("unique-one")),
            "the hit should name the worktree that has it: {:?}",
            hit.worktrees
        );
    }
}

#[test]
fn deduplication_does_not_lose_a_result_present_in_a_divergent_tree() {
    require_wt!();
    let fx = Fixture::new();

    // Two worktrees stay identical to main; one diverges by editing a file they
    // all share. Dedup must skip the twins without losing the divergent one.
    fx.make_worktree("twin-one");
    fx.make_worktree("twin-two");
    let edited = fx.make_worktree("edited");
    std::fs::write(edited.join("src/main.rs"), "fn main() { EDITED_HERE(); }\n").unwrap();
    run(&["add", "-A"], &edited);
    run(&["commit", "-qm", "edit"], &edited);

    let results = wt_tui::search::search(
        "EDITED_HERE",
        &targets(&fx),
        wt_tui::search::QueryOptions::default(),
    );

    assert!(
        !results.is_empty(),
        "the divergent tree's edit must be found"
    );
    assert!(
        results.deduped > 0,
        "the unchanged twins should still have been skipped: {results:?}"
    );
}

#[test]
fn a_query_matching_nothing_returns_no_hits_and_no_error() {
    require_wt!();
    let fx = Fixture::new();

    let results = wt_tui::search::search(
        "definitely_not_present_anywhere_zzz",
        &targets(&fx),
        wt_tui::search::QueryOptions::default(),
    );

    assert!(results.error.is_none());
    assert!(results.is_empty());
    assert!(
        results.covered() > 0,
        "it should still have searched the worktrees"
    );
}

#[test]
fn search_skips_gitignored_files() {
    require_wt!();
    let fx = Fixture::new();

    // A gitignored file holding a needle that must not be reported.
    std::fs::write(fx.root.join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(fx.root.join("ignored.txt"), "IGNORED_NEEDLE_ZZZ\n").unwrap();
    run(&["add", ".gitignore"], &fx.root);
    run(&["commit", "-qm", "ignore"], &fx.root);

    let results = wt_tui::search::search(
        "IGNORED_NEEDLE_ZZZ",
        &targets(&fx),
        wt_tui::search::QueryOptions::default(),
    );

    assert!(
        results.is_empty(),
        "a gitignored file should not be searched: {:?}",
        results.hits
    );
}

#[test]
fn search_honours_case_sensitivity() {
    require_wt!();
    let fx = Fixture::new();

    let sensitive = wt_tui::search::search(
        "fn main",
        &targets(&fx),
        wt_tui::search::QueryOptions::default(),
    );
    assert!(!sensitive.is_empty());

    let wrong_case = wt_tui::search::search(
        "FN MAIN",
        &targets(&fx),
        wt_tui::search::QueryOptions::default(),
    );
    assert!(
        wrong_case.is_empty(),
        "case-sensitive mode should not match"
    );

    let insensitive = wt_tui::search::search(
        "FN MAIN",
        &targets(&fx),
        wt_tui::search::QueryOptions {
            case_insensitive: true,
            force_regex: false,
        },
    );
    assert!(
        !insensitive.is_empty(),
        "case-insensitive mode should match"
    );
}

#[test]
fn a_literal_query_does_not_treat_dots_as_wildcards() {
    require_wt!();
    let fx = Fixture::new();

    // "main.rs" as a literal must not match a file called "mainXrs.rs".
    let worktree = fx.make_worktree("decoy");
    std::fs::write(worktree.join("src/mainXrs.rs"), "let x = 1;\n").unwrap();
    run(&["add", "-A"], &worktree);
    run(&["commit", "-qm", "decoy"], &worktree);

    let results = wt_tui::search::search(
        "main.rs",
        &targets(&fx),
        wt_tui::search::QueryOptions::default(),
    );

    for hit in &results.hits {
        assert!(
            !hit.rel_path.contains("mainXrs"),
            "a literal query must not match a wildcard decoy: {:?}",
            hit
        );
    }
}
