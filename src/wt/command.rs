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

/// Build a `wt` invocation rooted at `repo`.
///
/// `-C` is always passed explicitly so the TUI behaves identically regardless
/// of where it was launched from, and so a mutation targets the worktree the
/// user is looking at rather than an ambient directory.
fn wt_command(repo: &Path) -> Command {
    let mut cmd = Command::new(wt_binary());
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
    let mut cmd = wt_command(repo);
    cmd.args(["list", "--format=json"]);
    if scope.branches {
        cmd.arg("--branches");
    }
    if scope.full {
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

/// Open a path in the user's editor, used by the "open worktree" action.
pub fn editor_command() -> Option<String> {
    std::env::var("EDITOR")
        .ok()
        .filter(|s| !s.trim().is_empty())
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
}
