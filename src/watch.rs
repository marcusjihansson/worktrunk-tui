//! Filesystem watching, which is what lets wt-tui see an agent create a
//! worktree without being restarted.
//!
//! There is no server here and no IPC. Git records one directory per worktree
//! under the repository's common `.git/worktrees/`, so adding or removing a
//! worktree produces a filesystem event a watcher can observe. That is the
//! whole mechanism.
//!
//! Events are debounced, because worktrunk touches several paths while creating
//! a worktree and a burst should trigger one refresh rather than ten. A polling
//! interval in the event loop covers anything the watcher misses.

use notify::{Event, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long to coalesce a burst of filesystem events into one refresh.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// Whether an event could have changed the set of worktrees.
///
/// Writes *inside* `.git/worktrees/<name>/` — lock files, gitdir pointers,
/// index updates — do not. Adding or removing those directories does.
fn is_relevant(event: &Event) -> bool {
    use notify::EventKind;
    use notify::event::{CreateKind, RemoveKind};

    matches!(
        event.kind,
        EventKind::Create(CreateKind::Folder)
            | EventKind::Remove(RemoveKind::Folder | RemoveKind::Any)
    )
}

/// Resolve the directory that holds per-worktree metadata.
fn worktrees_dir(repo: &Path) -> Result<PathBuf, String> {
    // `git rev-parse --git-common-dir` resolves correctly from inside a linked
    // worktree, where `.git` is a file rather than a directory.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;

    if !out.status.success() {
        return Err("not a git repository".to_string());
    }

    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return Err("git returned no common directory".to_string());
    }

    // The reported path may be absolute or relative to the repo.
    let common = if Path::new(&raw).is_absolute() {
        PathBuf::from(&raw)
    } else {
        repo.join(&raw)
    };

    Ok(common.join("worktrees"))
}

/// Start watching. The returned handle must be kept alive for the watch to
/// remain active.
pub fn spawn(
    repo: &Path,
    tx: tokio::sync::mpsc::UnboundedSender<()>,
) -> Result<notify::RecommendedWatcher, String> {
    let worktrees = worktrees_dir(repo)?;

    // A repo with no linked worktrees has no `.git/worktrees` yet. Watch the
    // common git directory instead, so the first worktree appearing is noticed.
    let target = if worktrees.exists() {
        worktrees
    } else {
        worktrees
            .parent()
            .ok_or_else(|| "cannot resolve the git directory".to_string())?
            .to_path_buf()
    };

    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = raw_tx.send(res);
    })
    .map_err(|e| format!("cannot create a watcher: {e}"))?;

    watcher
        .watch(&target, RecursiveMode::NonRecursive)
        .map_err(|e| format!("cannot watch {}: {e}", target.display()))?;

    // Debounce on a dedicated thread: wake on the first event, then drain
    // whatever followed it before firing a single refresh.
    std::thread::Builder::new()
        .name("wt-tui-watch".into())
        .spawn(move || {
            // The event that wakes this loop counts too. Discarding it and only
            // inspecting the *drained* events would miss a lone folder
            // creation — exactly the event a new worktree produces — and defer
            // the refresh to the polling fallback.
            while let Ok(Ok(event)) = raw_rx.recv() {
                let mut relevant = is_relevant(&event);

                // Collapse the burst that follows into this one refresh.
                std::thread::sleep(DEBOUNCE);
                while let Ok(Ok(event)) = raw_rx.try_recv() {
                    relevant |= is_relevant(&event);
                }

                if relevant && tx.send(()).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| format!("cannot start the watch thread: {e}"))?;

    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::Event;
    use notify::event::{CreateKind, EventKind, RemoveKind};

    fn event(kind: EventKind) -> Event {
        Event {
            kind,
            paths: vec![],
            attrs: Default::default(),
        }
    }

    #[test]
    fn folder_creation_and_removal_are_relevant() {
        assert!(is_relevant(&event(EventKind::Create(CreateKind::Folder))));
        assert!(is_relevant(&event(EventKind::Remove(RemoveKind::Folder))));
        assert!(is_relevant(&event(EventKind::Remove(RemoveKind::Any))));
    }

    #[test]
    fn writes_inside_a_worktree_are_not_relevant() {
        // A lock file or index write cannot change the worktree list, and
        // treating it as relevant would cause a refresh storm.
        assert!(!is_relevant(&event(EventKind::Create(CreateKind::File))));
        assert!(!is_relevant(&event(EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))));
        assert!(!is_relevant(&event(EventKind::Any)));
    }
}
