//! Serde model for the `wt list --format=json` schema-2 envelope.
//!
//! # The contract rules
//!
//! This module is the only place that knows worktrunk's JSON shape, and it is
//! written to survive worktrunk shipping new data:
//!
//! * **No `deny_unknown_fields`.** New fields are ignored rather than fatal.
//! * **`#[serde(default)]` on every field.** A missing optional is never an
//!   error; it is simply `None`.
//! * **Absent and null are both `None`.** Worktrunk distinguishes "nothing to
//!   report" from "requested but not determined"; collapsing both to `None`
//!   loses nothing the UI acts on, and matches how `jq` treats them.
//!
//! The model deliberately mirrors the *whole* schema, including fields the UI
//! does not render yet (`dev_server`, `summary`, `vars`, lock and prunable
//! reasons, head SHAs and timestamps). Deserializing them is what proves the
//! model keeps up with worktrunk: an unexpected shape in any field the
//! published schema promises becomes a loud parse failure rather than a
//! silently ignored column. Fields are read as later phases render them, so
//! `dead_code` is allowed at the module level rather than annotated one by one.

#![allow(dead_code, reason = "the model mirrors the full published schema")]

use serde::Deserialize;
use std::collections::BTreeMap;

/// The only schema version this build understands.
///
/// Anything else is reported to the user rather than partially rendered.
pub const SUPPORTED_SCHEMA: u32 = 2;

/// A failed attempt to interpret the payload, with a message fit for a human.
#[derive(Debug)]
pub enum PayloadError {
    /// A different schema version than [`SUPPORTED_SCHEMA`].
    UnsupportedSchema { found: u32 },
    /// Schema 1 output, which this build does not read.
    LegacySchema,
    /// Not JSON at all, or JSON of an unexpected shape.
    Malformed(serde_json::Error),
    /// Valid JSON, but not the envelope we expect.
    WrongShape(String),
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchema { found } => write!(
                f,
                "this build of wt-tui reads worktrunk JSON schema {SUPPORTED_SCHEMA}, \
                 but `wt` reported schema {found}. Update wt-tui."
            ),
            Self::LegacySchema => write!(
                f,
                "`wt` returned the legacy bare-array format.\n\nThis is caused by \
                 `[list] json-schema = 1` in your worktrunk config. Remove that \
                 setting (or set it to 2) so `wt list --format=json` emits the \
                 envelope wt-tui reads."
            ),
            Self::Malformed(e) => {
                write!(f, "`wt list --format=json` did not return valid JSON: {e}")
            }
            Self::WrongShape(s) => write!(f, "unexpected JSON from `wt list --format=json`: {s}"),
        }
    }
}

impl std::error::Error for PayloadError {}

/// Parse the stdout of `wt list --format=json` into an [`Envelope`].
pub fn parse_list_json(stdout: &str) -> Result<Envelope, PayloadError> {
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).map_err(PayloadError::Malformed)?;

    if value.is_array() {
        return Err(PayloadError::LegacySchema);
    }

    let found = value.get("schema").and_then(serde_json::Value::as_u64);
    match found {
        None => Err(PayloadError::WrongShape(
            "no `schema` field at the top level".into(),
        )),
        Some(n) if n as u32 != SUPPORTED_SCHEMA => {
            Err(PayloadError::UnsupportedSchema { found: n as u32 })
        }
        Some(_) => serde_json::from_value(value).map_err(PayloadError::Malformed),
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Envelope {
    #[serde(default)]
    #[allow(dead_code, reason = "contract field, surfaced via Debug")]
    pub schema: u32,
    #[serde(default)]
    pub repo: Option<RepoInfo>,
    #[serde(default)]
    pub collected: Option<Collected>,
    #[serde(default)]
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RepoInfo {
    #[serde(default)]
    pub default_branch: Option<String>,
    /// Forge metadata, deserialized so an unexpected shape cannot fail the
    /// parse. Reserved for the PR view in a later phase.
    #[serde(default)]
    pub forge: Option<Forge>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Forge {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// Which gated fact families a run actually collected.
#[derive(Debug, Clone, Copy, Deserialize, Default)]
pub struct Collected {
    #[serde(default)]
    pub ci: bool,
    #[serde(default)]
    pub summary: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Item {
    #[serde(default)]
    pub branch: Option<String>,
    /// Present only on remote-only branch rows.
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub head: Option<Head>,
    /// Absent on branch-only rows.
    #[serde(default)]
    pub worktree: Option<Worktree>,
    #[serde(default)]
    pub default_branch: Option<DefaultBranch>,
    #[serde(default)]
    pub upstream: Option<Upstream>,
    #[serde(default)]
    pub pr: Option<Pr>,
    #[serde(default)]
    pub checks: Option<Checks>,
    #[serde(default)]
    pub dev_server: Option<DevServer>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub marker: Option<String>,
    #[serde(default)]
    pub vars: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub display: Option<Display>,
}

impl Item {
    /// A row with a worktree, as opposed to a bare or remote branch.
    pub fn is_worktree(&self) -> bool {
        self.worktree.is_some()
    }

    /// Stable identity for selection-preserving refreshes.
    ///
    /// Matched by name rather than row index so that a worktree created by an
    /// agent does not silently move the user's cursor onto a different branch.
    pub fn identity(&self) -> String {
        match (&self.remote, &self.branch) {
            (Some(r), Some(b)) => format!("{r}/{b}"),
            (_, Some(b)) => b.clone(),
            _ => self
                .worktree
                .as_ref()
                .and_then(|w| w.path.clone())
                .unwrap_or_default(),
        }
    }

    /// Worktrunk's single highest-priority state for this row.
    ///
    /// `empty` and `integrated` are the two states worktrunk dims because the
    /// branch is safe to delete.
    pub fn state(&self) -> Option<&str> {
        self.display.as_ref().and_then(|d| d.state.as_deref())
    }

    /// True when worktrunk considers this row safe to delete.
    pub fn is_safe_to_delete(&self) -> bool {
        matches!(self.state(), Some("empty") | Some("integrated"))
    }

    /// Whether the row has uncommitted work, in any form.
    pub fn has_changes(&self) -> bool {
        self.worktree
            .as_ref()
            .and_then(|w| w.changes.as_ref())
            .is_some_and(|c| c.staged || c.modified || c.untracked || c.renamed || c.deleted)
    }

    /// Whether merging would conflict with the default branch.
    pub fn would_conflict(&self) -> bool {
        self.default_branch.as_ref().and_then(|d| d.merge_conflicts) == Some(true)
    }

    //// Whether this branch is fully merged and would add nothing to the target,
    /// per worktrunk's own integration check.
    ///
    /// This is what makes a delete safe, and it is why the table dims rows the
    /// user can throw away.
    pub fn is_integrated(&self) -> bool {
        self.default_branch
            .as_ref()
            .and_then(|d| d.integration.as_ref())
            .is_some()
            || self.state() == Some("integrated")
    }

    /// A human sentence explaining why a branch would or would not be deleted.
    ///
    /// Surfacing this verbatim turns removal from a scary action into an
    /// informed one.
    pub fn removal_reason(&self) -> String {
        if let Some(reason) = self
            .default_branch
            .as_ref()
            .and_then(|d| d.integration.as_ref())
            .and_then(|i| i.reason.as_deref())
        {
            return format!("integrated into the default branch ({reason})");
        }
        match self.state() {
            Some("integrated") => "integrated into the default branch".to_string(),
            Some("empty") => "same commit as the default branch with a clean tree".to_string(),
            Some("orphan") => {
                "no common ancestor with the default branch — the branch will be kept".to_string()
            }
            Some(s) => format!("{s} — the branch will be kept"),
            None if self.branch.is_none() => {
                "detached HEAD: there is no branch to delete".to_string()
            }
            None => "not integrated — the branch will be kept".to_string(),
        }
    }

    /// A label for the row: branch name, or path for a detached-HEAD worktree.
    pub fn label(&self) -> String {
        self.branch.clone().unwrap_or_else(|| {
            self.worktree
                .as_ref()
                .and_then(|w| w.path.as_deref())
                .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                .unwrap_or_else(|| "(unnamed)".to_string())
        })
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Head {
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub short_sha: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub committed_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Worktree {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub main: bool,
    #[serde(default)]
    pub current: bool,
    #[serde(default)]
    pub previous: bool,
    #[serde(default)]
    pub detached: bool,
    #[serde(default)]
    pub locked: Option<Reasoned>,
    #[serde(default)]
    pub prunable: Option<Reasoned>,
    #[serde(default)]
    pub branch_mismatch: bool,
    #[serde(default)]
    pub duplicate_branch: bool,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub changes: Option<Changes>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Reasoned {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
pub struct Changes {
    #[serde(default)]
    pub staged: bool,
    #[serde(default)]
    pub modified: bool,
    #[serde(default)]
    pub untracked: bool,
    #[serde(default)]
    pub renamed: bool,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub conflicted: Option<bool>,
    #[serde(default)]
    pub diff: Option<LineDiff>,
}

impl Changes {
    /// The `+!?` flag triple worktrunk shows in its status column.
    pub fn symbols(&self) -> String {
        let mut s = String::new();
        if self.staged {
            s.push('+');
        }
        if self.modified {
            s.push('!');
        }
        if self.untracked {
            s.push('?');
        }
        s
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
pub struct LineDiff {
    #[serde(default)]
    pub added: i64,
    #[serde(default)]
    pub deleted: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DefaultBranch {
    #[serde(default)]
    pub ahead: Option<i64>,
    #[serde(default)]
    pub behind: Option<i64>,
    #[serde(default)]
    pub diff: Option<LineDiff>,
    #[serde(default)]
    pub orphan: Option<bool>,
    #[serde(default)]
    pub integration: Option<Integration>,
    #[serde(default)]
    pub merge_conflicts: Option<bool>,
}

/// Which integration check matched. Every reason renders as the same `⊂`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Integration {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Upstream {
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub ahead: Option<i64>,
    #[serde(default)]
    pub behind: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Pr {
    #[serde(default)]
    pub number: Option<i64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub review: Option<String>,
    #[serde(default)]
    pub mergeable: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Checks {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub stale: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DevServer {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub listening: Option<bool>,
}

/// Rendered strings. Every value here restates a fact found elsewhere.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Display {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub symbols: Option<String>,
    #[serde(default)]
    pub statusline: Option<String>,
    /// User-defined custom columns, keyed by header.
    #[serde(default)]
    pub columns: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// `wt switch --create --format=json`
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SwitchResult {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub created_branch: bool,
    #[serde(default)]
    pub base_branch: Option<String>,
}

// ---------------------------------------------------------------------------
// `wt remove --format=json`
// ---------------------------------------------------------------------------

/// What happened to a branch during removal.
///
/// This vocabulary is worktrunk's own, and it exists to distinguish a deletion
/// the removal *declined* from one it was never asked to make.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BranchOutcome {
    /// The branch is gone.
    Deleted,
    /// Left to the background removal, whose result this run does not see.
    Deferred,
    /// No deletion was attempted: detached worktree, sibling checkout, or
    /// `--no-delete-branch`.
    NotAttempted,
    /// Declined: the branch was not integrated into the target.
    RetainedUnmerged,
    /// Declined: another worktree has the branch checked out.
    RetainedCheckedOut,
    /// Declined: the branch moved during the removal. Retry.
    RetainedRaced,
    /// Declined: the delete command itself failed.
    RetainedFailed,
}

impl BranchOutcome {
    /// Whether the branch actually went away.
    pub fn removed_branch(&self) -> bool {
        matches!(self, Self::Deleted)
    }

    /// A sentence explaining the outcome, for the result banner.
    pub fn explain(&self) -> &'static str {
        match self {
            Self::Deleted => "branch deleted",
            Self::Deferred => "removal ran in the background; check `wt list` for the result",
            Self::NotAttempted => "branch kept (nothing to delete, or --no-delete-branch)",
            Self::RetainedUnmerged => "branch kept: it has unmerged commits (use -D to force)",
            Self::RetainedCheckedOut => "branch kept: checked out in another worktree",
            Self::RetainedRaced => "branch kept: it moved during removal; retry",
            Self::RetainedFailed => "branch kept: the delete command failed",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RemoveResult {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    /// Present instead of `path` for a branch-only removal.
    #[serde(default)]
    pub pruned: Option<bool>,
    #[serde(default)]
    pub branch_outcome: Option<BranchOutcome>,
    #[serde(default)]
    pub branch_checked_out_at: Option<String>,
}
