//! The subprocess boundary: every call into worktrunk goes through here.
//!
//! wt-tui shells out to `wt` rather than depending on the `worktrunk` crate.
//! That is deliberate. The crate's own `lib.rs` says "The library API is not
//! stable", its changelog records breaking library changes in 8 of the last 10
//! releases, and the modules that matter most here (`list`, `picker`,
//! `remove`) live in `main.rs`, not the library. Shelling out to the documented
//! JSON schema keeps wt-tui working across worktrunk updates and picks up new
//! fields for free.

use super::model::{Envelope, PayloadError, RemoveResult, SwitchResult, parse_list_json};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// Flags for which rows to include.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListScope {
    /// Include local branches that have no worktree.
    pub branches: bool,
    /// Fetch PR/CI data. Network-bound, so never on the refresh hot path.
    pub full: bool,
}

impl ListScope {
    /// The default scope: worktrees only, no network.
    pub const WORKTREES: Self = Self {
        branches: false,
        full: false,
    };

    /// Include branch-only rows. Cheap, and needed to show "removable"
    /// branches that never got a worktree.
    pub const WITH_BRANCHES: Self = Self {
        branches: true,
        full: false,
    };
}

/// Find the `wt` binary, honouring `WT_TUI_WT_BIN` for tests and for setups
/// where `wt` is not on `PATH`.
pub fn wt_binary() -> PathBuf {
    std::env::var_os("WT_TUI_WT_BIN").map_or_else(|| PathBuf::from("wt"), PathBuf::from)
}

/// Extract a JSON value from stdout that may be preceded by human-readable
/// progress lines.
///
/// This is not hypothetical: `wt remove --format=json` writes progress lines to
/// stdout *before* the JSON array, so a naive `serde_json::from_str(stdout)`
/// fails. We try the whole string first, then scan forward for the first line
/// that begins a JSON document and parse from there.
pub fn extract_json<T: serde::de::DeserializeOwned>(stdout: &str) -> Result<T, String> {
    let trimmed = stdout.trim();

    if let Ok(v) = serde_json::from_str::<T>(trimmed) {
        return Ok(v);
    }

    // Find the first offset where a line starts with '[' or '{' and a parse
    // from there succeeds. Scanning every line is cheap and robust to whatever
    // progress decoration worktrunk prints.
    for (idx, _) in trimmed.match_indices(['[', '{']) {
        if idx != 0 && trimmed.as_bytes()[idx - 1] != b'\n' {
            continue; // not at a line start
        }
        if let Ok(v) = serde_json::from_str::<T>(&trimmed[idx..]) {
            return Ok(v);
        }
    }

    Err(format!(
        "no JSON found in output: {}",
        truncate(trimmed, 400)
    ))
}

/// Environment variables that redirect where git finds its repository.
///
/// `git -C <path>` changes the *working directory*; it does **not** override
/// these. A `GIT_DIR` in the environment wins outright, so every child process
/// would silently act on a different repository than the one wt-tui was
/// launched against. Verified against `wt` v0.80.0: with `GIT_DIR` pointing at
/// another repo, `wt -C <repoA> list` reported repoB's worktrees, branches and
/// paths, and `wt -C <repoA> remove <branch> -y` deleted repoB's branch.
///
/// `direnv`'s `layout_git` is the realistic source, and `.envrc` is repository
/// content — so this is reachable by cloning a repository, not just by
/// misconfiguring a shell.
///
/// wt-tui is a destructive tool (`remove`, `prune`, `merge`, `switch`), so it
/// has to be hermetic about which repository it operates on. Clearing these
/// makes `-C <repo>` the single source of truth, which is the entire design of
/// [`wt_command`] and [`git_command`].
pub const GIT_RESOLUTION_VARS: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
];

/// A `git` invocation rooted at `path`, insulated from ambient git variables.
///
/// Same reasoning as [`wt_command`]: the `-C` argument is the only thing that
/// should decide which repository is meant.
pub fn git_command(path: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    for var in GIT_RESOLUTION_VARS {
        cmd.env_remove(var);
    }
    cmd.arg("-C").arg(path);
    cmd
}

/// Build a `wt` invocation rooted at `repo`.
///
/// `-C` is always passed explicitly so the TUI behaves identically regardless
/// of where it was launched from, and so a mutation targets the worktree the
/// user is looking at rather than an ambient directory.
///
/// Ambient `GIT_*` variables are cleared because `-C` does not override them;
/// see [`GIT_RESOLUTION_VARS`].
fn wt_command(repo: &Path) -> Command {
    let mut cmd = Command::new(wt_binary());
    for var in GIT_RESOLUTION_VARS {
        cmd.env_remove(var);
    }
    cmd.arg("-C").arg(repo);
    cmd.stdin(Stdio::null());
    cmd
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

async fn run(mut cmd: Command) -> Result<(String, String), String> {
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("failed to run `{}`: {e}", wt_binary().display()))?;

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    if !out.status.success() {
        // worktrunk puts the human explanation on stderr.
        let detail = if stderr.trim().is_empty() {
            format!("exited with {}", out.status)
        } else {
            stderr.trim().to_string()
        };
        return Err(detail);
    }

    Ok((stdout, stderr))
}

/// Fetch worktree state as a schema-2 envelope.
pub async fn list(repo: &Path, scope: ListScope) -> Result<Envelope, ListError> {
    list_with(repo, scope, scope.full).await
}

/// Fetch worktree state, choosing whether to collect PR/CI data.
///
/// `full` is separate from `scope.full` so a caller can ask for `--full`
/// without the scope flag meaning anything else.
pub async fn list_with(repo: &Path, scope: ListScope, full: bool) -> Result<Envelope, ListError> {
    let mut cmd = wt_command(repo);
    cmd.args(["list", "--format=json"]);
    if scope.branches {
        cmd.arg("--branches");
    }
    if full {
        cmd.arg("--full");
    }

    let (stdout, _) = run(cmd).await.map_err(ListError::Wt)?;
    parse_list_json(&stdout).map_err(ListError::Payload)
}

/// Create a branch and its worktree.
///
/// Uses `--no-cd`: wt-tui is observational, and it cannot change the user's
/// shell directory anyway. Hooks still run.
pub async fn create(repo: &Path, branch: &str, base: Option<&str>) -> Result<SwitchResult, String> {
    let mut cmd = wt_command(repo);
    cmd.args([
        "switch",
        "--create",
        branch,
        "--no-cd",
        "-y",
        "--format=json",
    ]);
    if let Some(base) = base {
        cmd.args(["--base", base]);
    }

    let (stdout, _) = run(cmd).await?;
    extract_json(&stdout)
}

/// Merge a branch into `target`.
///
/// # Direction matters
///
/// `wt merge` merges **the current branch into the target**. Running
/// `wt merge <branch>` from the repository root therefore merges the *root's*
/// branch *into* `<branch>` — the opposite of what a user pressing "merge" on a
/// row in the table means. Verified: from the main worktree, `wt merge
/// feature-x` moved main's commits onto feature-x and left feature-x at
/// ahead=0.
///
/// So the worktree is passed as `-C`, making the row's branch the current one:
/// `wt -C <the row's worktree> merge <target>`.
///
/// Returns the parsed result *and* whether the command reported success, because
/// the two disagree on a conflicted merge — see [`MergeOutcome`].
pub async fn merge(
    worktree_path: &Path,
    target: &str,
    keep_worktree: bool,
) -> Result<MergeOutcome, String> {
    let mut cmd = wt_command(worktree_path);
    cmd.args(["merge", target, "-y", "--format=json"]);
    if keep_worktree {
        cmd.arg("--no-remove");
    }

    // A conflicted merge is a legitimate outcome, not a failure to report: the
    // user needs to be told that a git operation is waiting on them. So a
    // non-zero exit is folded into the outcome rather than turned into an `Err`,
    // which would discard exactly the information that matters. `Err` is
    // reserved for the command failing to run at all.
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("failed to run `{}`: {e}", wt_binary().display()))?;

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    // Worktrunk writes its progress and results to stderr when stderr is not a
    // terminal, so both streams are searched for the payload.
    let result: Option<MergeResult> = extract_json(&stdout)
        .ok()
        .or_else(|| extract_json(&stderr).ok());

    Ok(MergeOutcome {
        succeeded: result.is_some(),
        result,
        stderr,
        exit_ok: out.status.success(),
    })
}

/// `wt merge --format=json` reports a single object (unlike `wt remove`, which
/// reports an array).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MergeResult {
    #[serde(default)]
    pub branch: Option<String>,
    /// Whether worktrunk created a commit from uncommitted changes.
    #[serde(default)]
    pub committed: bool,
    #[serde(default)]
    pub rebased: bool,
    /// Whether the worktree and branch were removed afterwards.
    #[serde(default)]
    pub removed: bool,
    #[serde(default)]
    pub squashed: bool,
    #[serde(default)]
    pub target: Option<String>,
}

/// One row of `wt step prune --dry-run --format=json`.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct PruneCandidate {
    #[serde(default)]
    pub path: Option<String>,
    /// Why worktrunk considers this branch integrated.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    /// Present when the row describes a branch with no worktree.
    #[serde(default)]
    pub pruned: Option<bool>,
}

/// The outcome of a merge attempt.
#[derive(Debug, Clone, Default)]
pub struct MergeOutcome {
    pub result: Option<MergeResult>,
    pub stderr: String,
    /// True when worktrunk reported a completed merge.
    pub succeeded: bool,
    /// Whether the process exited cleanly.
    ///
    /// `false` with no result is the normal shape of a conflicted merge, which
    /// leaves the repository mid-operation for the user to resolve.
    pub exit_ok: bool,
}

impl MergeOutcome {
    /// Whether worktrunk left a git operation in progress, which is the state a
    /// conflicted merge can leave behind and which the user must resolve.
    ///
    /// Verified: a merge that hit a conflict still exited 0 in one path and left
    /// `rebase-merge/` in the worktree's git dir, after which any further `wt`
    /// command fails with "a git operation is already in progress".
    pub fn left_operation_in_progress(&self, worktree_path: &Path) -> bool {
        let Ok(git_dir) = git_dir(worktree_path) else {
            return false;
        };
        git_dir.join("rebase-merge").exists()
            || git_dir.join("rebase-apply").exists()
            || git_dir.join("MERGE_HEAD").exists()
            || git_dir.join("CHERRY_PICK_HEAD").exists()
    }
}

fn git_dir(path: &Path) -> Result<PathBuf, String> {
    let out = git_command(path)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err("not a git repository".into());
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let dir = if Path::new(&raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        path.join(raw)
    };
    Ok(dir)
}

/// Preview what `wt step prune` would remove.
///
/// `--dry-run` is what makes prune reviewable: the same criteria run, but
/// nothing is removed.
pub async fn prune_preview(
    repo: &Path,
    min_age: Option<&str>,
) -> Result<Vec<PruneCandidate>, String> {
    let mut cmd = wt_command(repo);
    cmd.args(["step", "prune", "--dry-run", "--format=json", "-y"]);
    if let Some(age) = min_age {
        cmd.args(["--min-age", age]);
    }

    let (stdout, _) = run(cmd).await?;
    extract_json(&stdout)
}

/// Remove everything worktrunk considers integrated.
///
/// `min_age` matters more than it looks: the default is `1d`, which silently
/// skips a worktree created minutes ago — including a fresh branch off the
/// default branch, which looks merged because it points at the same commit.
/// Callers pass the age through explicitly so the UI can show it.
pub async fn prune(repo: &Path, min_age: Option<&str>) -> Result<String, String> {
    let mut cmd = wt_command(repo);
    cmd.args(["step", "prune", "--foreground", "--format=json", "-y"]);
    if let Some(age) = min_age {
        cmd.args(["--min-age", age]);
    }
    let (stdout, stderr) = run(cmd).await?;
    Ok(format!("{stdout}{stderr}"))
}

/// Remove a worktree and, when safe, its branch.
///
/// `--foreground` matters: removal otherwise runs in the background, so the
/// command returns before the branch is gone and the JSON would not describe
/// the final state.
pub async fn remove(
    repo: &Path,
    branch: &str,
    force_worktree: bool,
) -> Result<Vec<RemoveResult>, String> {
    let mut cmd = wt_command(repo);
    cmd.args(["remove", branch, "-y", "--foreground", "--format=json"]);
    if force_worktree {
        cmd.arg("--force");
    }

    let (stdout, _) = run(cmd).await?;
    extract_json(&stdout)
}

/// The editor to open a worktree with, split into a program and its flags.
///
/// `$EDITOR` is routinely set to a command *with arguments* —
/// `EDITOR="code --wait"`, `EDITOR="emacsclient -c"`, `EDITOR="nvim -p"` — so
/// the value has to be word-split. `Command::new` treats its program as a single
/// path and never consults a shell, so an unsplit value simply failed to launch
/// with `No such file or directory`.
///
/// The split happens here, in Rust, and the worktree is appended as a separate
/// argument. Deliberately *not* `sh -c "$EDITOR \"$path\""`: that would fix a
/// cosmetic bug by introducing a shell, and would hand `$EDITOR` — which can
/// come from a `.envrc` or a plugin host's environment — a quoting context to
/// break out of.
pub fn editor_command() -> Option<EditorCommand> {
    let raw = std::env::var("EDITOR").ok()?;
    let mut words = raw.split_whitespace().map(str::to_string);
    let program = words.next()?;
    Some(EditorCommand {
        program,
        args: words.collect(),
    })
}

/// A program and its arguments, as resolved from `$EDITOR`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCommand {
    pub program: String,
    /// Flags that precede the path.
    pub args: Vec<String>,
}

#[derive(Debug)]
pub enum ListError {
    Wt(String),
    Payload(PayloadError),
}

impl std::fmt::Display for ListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wt(msg) => write!(f, "{msg}"),
            Self::Payload(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ListError {}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    #[test]
    fn prefers_a_whole_document_json_line() {
        let out = "[{\"a\":1}]";
        let v: serde_json::Value = extract_json(out).unwrap();
        assert_eq!(v[0]["a"], 1);
    }

    #[test]
    fn ignores_a_bracket_inside_prose() {
        // A progress line mentioning a path in brackets must not be mistaken
        // for the start of the payload.
        let out = "note: [building] 50%\n[{\"branch\":\"x\"}]";
        let v: Vec<RemoveResult> = extract_json(out).unwrap();
        assert_eq!(v.len(), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wt::model::BranchOutcome;

    #[test]
    fn parses_bare_json_with_no_prose() {
        let out = r#"{"action":"created","branch":"x","path":"/p","created_branch":true}"#;
        let v: SwitchResult = extract_json(out).unwrap();
        assert_eq!(v.branch.as_deref(), Some("x"));
        assert!(v.created_branch);
    }

    #[test]
    fn skips_progress_lines_before_json() {
        // This is the real shape of `wt remove --format=json` output.
        let out = "◎ Removing throwaway worktree...\n\
                   ✓ Removed throwaway worktree & branch (same commit as main, _)\n\
                   [\n  {\n    \"branch\": \"throwaway\",\n    \
                    \"branch_outcome\": \"deleted\",\n    \"kind\": \"worktree\",\n    \
                    \"path\": \"/p\"\n  }\n]\n";
        let v: Vec<RemoveResult> = extract_json(out).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].branch_outcome, Some(BranchOutcome::Deleted));
    }

    #[test]
    fn skips_prose_before_an_object() {
        let out = "⏳ waiting for lock\n{\"action\":\"created\",\"branch\":\"b\"}";
        let v: SwitchResult = extract_json(out).unwrap();
        assert_eq!(v.action.as_deref(), Some("created"));
    }

    #[test]
    fn reports_helpfully_when_there_is_no_json() {
        let err = extract_json::<SwitchResult>("error: something went wrong").unwrap_err();
        assert!(err.contains("no JSON found"), "unexpected message: {err}");
    }

    /// `-C <path>` sets the working directory but does **not** override `GIT_DIR`,
    /// so an inherited one silently retargets every child at a different repo.
    /// Verified against `wt` v0.80.0: with `GIT_DIR` set elsewhere,
    /// `wt -C <repoA> remove <branch> -y` destroyed repoB's branch.
    ///
    /// Asserted on the command's own environment rather than by running git, so
    /// this needs no subprocess and no mutation of this process's environment
    /// (which would race every other test in this binary).
    #[test]
    fn git_command_clears_git_resolution_vars() {
        let cmd = git_command(Path::new("/repo"));
        let set: Vec<&std::ffi::OsStr> = cmd
            .get_envs()
            .filter_map(|(key, value)| value.map(|_| key))
            .collect();

        for var in GIT_RESOLUTION_VARS {
            assert!(
                !set.contains(&std::ffi::OsStr::new(var)),
                "{var} would be inherited by a child process: {set:?}"
            );
        }
    }

    /// Pinned so a var cannot be dropped from the shared list — `wt_command`
    /// clears the same list, and `tokio::process::Command` exposes no
    /// `get_envs` to assert on directly.
    #[test]
    fn the_cleared_var_list_is_complete() {
        for expected in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
        ] {
            assert!(
                GIT_RESOLUTION_VARS.contains(&expected),
                "{expected} is no longer cleared; -C alone would not protect against it"
            );
        }
    }

    fn split_editor(raw: &str) -> Option<EditorCommand> {
        let mut words = raw.split_whitespace().map(str::to_string);
        let program = words.next()?;
        Some(EditorCommand {
            program,
            args: words.collect(),
        })
    }

    /// `$EDITOR` is routinely set to a command *with flags*. `Command::new`
    /// treats the program as a single path and never consults a shell, so
    /// `EDITOR="code --wait"` used to fail with `No such file or directory`.
    #[test]
    fn an_editor_with_flags_is_split_into_a_program_and_args() {
        let editor = split_editor("code --wait").expect("split");
        assert_eq!(editor.program, "code");
        assert_eq!(editor.args, vec!["--wait".to_string()]);
    }

    #[test]
    fn a_bare_editor_name_has_no_args() {
        let editor = split_editor("nvim").expect("split");
        assert_eq!(editor.program, "nvim");
        assert!(editor.args.is_empty());
    }

    /// Empty and whitespace-only values are "not configured", not a program
    /// named "".
    #[test]
    fn an_unset_or_blank_editor_is_reported_as_absent() {
        assert!(split_editor("").is_none());
        assert!(split_editor("   \t ").is_none());
    }

    /// Splitting is whitespace-only and never shell-aware: a quoted argument
    /// stays one word with its quotes, because introducing a shell here would
    /// hand `$EDITOR` a quoting context to escape.
    #[test]
    fn editor_arguments_are_not_shell_parsed() {
        let editor = split_editor("emacsclient -c 'a b'").expect("split");
        assert_eq!(editor.program, "emacsclient");
        assert_eq!(
            editor.args,
            vec!["-c".to_string(), "'a".to_string(), "b'".to_string()],
            "quoting is deliberately not interpreted"
        );
    }
}
