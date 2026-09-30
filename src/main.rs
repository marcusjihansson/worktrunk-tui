//! wt-tui — an observational TUI dashboard for worktrunk worktrees.
//!
//! The reusable logic lives in the library ([`wt_tui`]); this binary owns the
//! terminal and the event loop.

use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::{Frame, Terminal};
use std::io::Stdout;
use std::path::PathBuf;
use std::time::Duration;
use wt_tui::app::{
    App, MergeRequest, Mode, NoticeKind, Pending, REFRESH_INTERVAL, SearchRequest, View,
};
use wt_tui::search;
use wt_tui::ui;
use wt_tui::watch;
use wt_tui::wt;

/// Background results arriving from spawned work.
enum Event_ {
    List(Result<wt::Envelope, wt::command::ListError>),
    Created(Result<wt::model::SwitchResult, String>, String),
    Removed(Result<Vec<wt::model::RemoveResult>, String>, String),
    Preview(String),
    SearchDone(search::Results, Duration),
    SearchContext(String),
    MergeDone(Result<wt::command::MergeOutcome, String>, MergeRequest),
    PrunePreview(
        Result<Vec<wt::command::PruneCandidate>, String>,
        Option<String>,
    ),
    Pruned(Result<String, String>, String),
}

type Backend = ratatui::backend::CrosstermBackend<Stdout>;

#[derive(Debug, Parser)]
#[command(
    name = "wt-tui",
    about = "A TUI dashboard for worktrunk worktrees",
    long_about = "wt-tui watches worktrunk worktrees and lets you browse, search across \
                  worktrees, create, merge, and remove them. It reads worktrunk through its \
                  documented JSON output, so it stays current as worktrunk gains features.\n\n\
                  Because it is a `wt-tui` binary on PATH, worktrunk exposes it as `wt tui`."
)]
struct Cli {
    /// Repository to inspect. Defaults to the current directory.
    #[arg(short = 'C', long = "repo", value_name = "PATH")]
    repo: Option<PathBuf>,

    /// Also list branches that have no worktree.
    #[arg(long, default_value_t = true)]
    branches: bool,

    /// Interval between fallback refreshes, in seconds.
    #[arg(long, default_value_t = REFRESH_INTERVAL.as_secs())]
    interval: u64,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    let repo = match cli.repo.clone().map_or_else(std::env::current_dir, Ok) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("wt-tui: cannot determine the current directory: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let repo = match repo.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("wt-tui: cannot resolve {}: {e}", repo.display());
            return std::process::ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("wt-tui: cannot start the async runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    runtime.block_on(run(cli, repo))
}

async fn run(cli: Cli, repo: PathBuf) -> std::process::ExitCode {
    let mut terminal = match setup() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("wt-tui: cannot set up the terminal: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut app = App::new(repo.clone());
    let scope = if cli.branches {
        wt::ListScope::WITH_BRANCHES
    } else {
        wt::ListScope::WORKTREES
    };

    let (events, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<Event_>();
    let mut input_rx = spawn_input();

    // Watching `.git/worktrees/` is what makes an agent's new worktree appear
    // without restarting the TUI. There is no server involved: creating a
    // worktree adds a directory there, which is a filesystem event.
    let (watch_tx, mut watch_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let watcher = match watch::spawn(&repo, watch_tx.clone()) {
        Ok(handle) => Some(handle),
        Err(e) => {
            // Not fatal: the fallback ticker still refreshes.
            app.set_notice(
                format!("live watch unavailable ({e}); polling instead"),
                NoticeKind::Info,
            );
            None
        }
    };

    let _ = watch_tx.send(());
    if let Some(path) = app.selected_path() {
        spawn_preview(&mut app, path, &events);
    }

    let mut ticker = tokio::time::interval(Duration::from_secs(cli.interval.max(2)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut preview_for: Option<PathBuf> = None;
    let mut fetching = false;

    loop {
        if let Err(e) = terminal.draw(|frame| draw(frame, &mut app)) {
            eprintln!("wt-tui: draw failed: {e}");
        }

        if app.should_quit {
            break;
        }

        let editor_request = dispatch_pending(&mut app, &repo, scope, &events);

        // The editor owns the terminal, so hand it over and take it back. This
        // cannot live in `dispatch_pending`, which has no terminal handle.
        if let Some((editor, path)) = editor_request {
            let _ = restore(&mut terminal);
            let status = std::process::Command::new(&editor).arg(&path).status();
            let _ = setup_with(&mut terminal);
            match status {
                Ok(s) if s.success() => {
                    app.set_notice(format!("closed {editor}"), NoticeKind::Info)
                }
                Ok(s) => app.set_notice(format!("{editor} exited {s}"), NoticeKind::Error),
                Err(e) => app.set_notice(format!("could not run {editor}: {e}"), NoticeKind::Error),
            }
        }

        // Load a preview when the selection moved to a different worktree.
        if app.mode == Mode::Normal
            && app.view == View::Worktrees
            && let Some(path) = app.selected_path()
            && preview_for.as_ref() != Some(&path)
        {
            spawn_preview(&mut app, path.clone(), &events);
            preview_for = Some(path);
        }

        // Load the file around the selected search hit.
        if app.view == View::Search
            && app.search.context_body.is_empty()
            && let Some(hit) = app.selected_hit().cloned()
            && let Some(path) = worktree_for_branch(&app, hit.worktrees.first())
        {
            spawn_search_context(&app, path, &hit.rel_path, &events);
        }

        tokio::select! {
            maybe_event = input_rx.recv() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        app.on_key(key);
                    }
                    Some(Ok(Event::Resize(_, _))) => {}
                    Some(Ok(_)) | Some(Err(_)) | None => {}
                }
            }

            _ = watch_rx.recv() => {
                // Coalesce: a burst of events should trigger one load.
                if !fetching {
                    fetching = true;
                    app.fetching = true;
                    fetch(&repo, scope, false, &events);
                }
            }

            _ = ticker.tick() => {
                // Safety net for missed watch notifications. Never `--full`
                // here: it reaches the forge over the network.
                if !fetching {
                    fetching = true;
                    app.fetching = true;
                    fetch(&repo, scope, false, &events);
                }
            }

            maybe = event_rx.recv() => {
                let Some(ev) = maybe else { break };
                fetching = false;
                app.fetching = false;
                handle_event(&mut app, ev, &repo, scope, &mut preview_for, &events);
            }
        }
    }

    let _ = restore(&mut terminal);
    drop(watcher);
    std::process::ExitCode::SUCCESS
}

fn dispatch_pending(
    app: &mut App,
    repo: &PathBuf,
    scope: wt::ListScope,
    events: &tokio::sync::mpsc::UnboundedSender<Event_>,
) -> Option<(String, PathBuf)> {
    let Pending {
        create,
        remove,
        editor,
        refresh,
        preview_stale,
        search: search_request,
        merge,
        prune_preview,
        prune,
        prune_age,
        fetch_ci,
    } = std::mem::take(&mut app.pending);

    if let Some(branch) = create {
        app.set_notice(format!("creating {branch}…"), NoticeKind::Info);
        let repo = repo.clone();
        let tx = events.clone();
        tokio::spawn(async move {
            let result = wt::command::create(&repo, &branch, None).await;
            let _ = tx.send(Event_::Created(result, branch));
        });
    }

    if let Some((branch, force)) = remove {
        app.set_notice(format!("removing {branch}…"), NoticeKind::Info);
        let repo = repo.clone();
        let tx = events.clone();
        tokio::spawn(async move {
            let result = wt::command::remove(&repo, &branch, force).await;
            let _ = tx.send(Event_::Removed(result, branch));
        });
    }

    if let Some(request) = search_request {
        run_search(app, request, events);
    }

    if let Some(request) = merge {
        app.set_notice(
            format!("merging {} into {}…", request.branch, request.target),
            NoticeKind::Info,
        );
        let tx = events.clone();
        tokio::spawn(async move {
            let result =
                wt::command::merge(&request.worktree, &request.target, request.keep_worktree).await;
            let _ = tx.send(Event_::MergeDone(result, request));
        });
    }

    if prune_preview {
        let repo_path = repo.clone();
        let tx = events.clone();
        tokio::spawn(async move {
            let result = wt::command::prune_preview(&repo_path, prune_age.as_deref()).await;
            let _ = tx.send(Event_::PrunePreview(result, prune_age));
        });
    }

    if let Some(min_age) = prune {
        app.set_notice(format!("pruning (min-age {min_age})…"), NoticeKind::Info);
        let repo_path = repo.clone();
        let tx = events.clone();
        tokio::spawn(async move {
            let result = wt::command::prune(&repo_path, Some(&min_age)).await;
            let _ = tx.send(Event_::Pruned(result, min_age));
        });
    }

    if fetch_ci {
        // Recorded before the load so the column says "not fetched" rather than
        // "no checks" while the request is in flight.
        app.ci_attempted = true;
        fetch(repo, scope, true, events);
    }

    if preview_stale {
        app.pending.preview_stale = true;
        if let Some(path) = app.selected_path() {
            spawn_preview(app, path, events);
        }
    }

    if refresh {
        fetch(repo, scope, false, events);
    }

    editor
}

/// Handle a background result.
fn handle_event(
    app: &mut App,
    ev: Event_,
    repo: &PathBuf,
    scope: wt::ListScope,
    preview_for: &mut Option<PathBuf>,
    events: &tokio::sync::mpsc::UnboundedSender<Event_>,
) {
    match ev {
        Event_::List(result) => match result {
            Ok(envelope) => {
                let default_branch = envelope
                    .repo
                    .as_ref()
                    .and_then(|r| r.default_branch.clone());
                app.apply(envelope, default_branch);
            }
            Err(e) => app.set_error(e.to_string()),
        },

        Event_::Created(result, branch) => {
            app.mode = Mode::Normal;
            match result {
                Ok(res) => {
                    let path = res.path.unwrap_or_default();
                    app.set_notice(format!("created {branch} @ {path}"), NoticeKind::Info);
                }
                Err(e) => {
                    app.set_notice(format!("could not create {branch}: {e}"), NoticeKind::Error)
                }
            }
            // A new branch may sort before the current selection.
            *preview_for = None;
            fetch(repo, scope, false, events);
        }

        Event_::Removed(result, branch) => {
            app.mode = Mode::Normal;
            match result {
                Ok(results) => match results.first() {
                    Some(first) => {
                        let (text, kind) = ui::remove::removal_summary(first);
                        app.set_notice(text, kind);
                    }
                    None => app.set_notice(
                        format!("removal of {branch} reported no result"),
                        NoticeKind::Error,
                    ),
                },
                Err(e) => {
                    app.set_notice(format!("could not remove {branch}: {e}"), NoticeKind::Error)
                }
            }
            *preview_for = None;
            fetch(repo, scope, false, events);
        }

        Event_::Preview(body) => app.preview.finish_load(body),

        Event_::SearchDone(results, elapsed) => {
            app.search.running = false;
            app.search.elapsed = Some(elapsed);
            app.search.selected = 0;
            app.search.scroll = 0;
            app.search.context_body.clear();

            if let Some(error) = &results.error {
                app.set_notice(error.clone(), NoticeKind::Error);
            } else if results.is_empty() {
                let scope_note = if results.covered() == 0 {
                    " (no worktrees to search)"
                } else {
                    ""
                };
                app.set_notice(format!("no matches{scope_note}"), NoticeKind::Info);
            }
            app.search.results = results;
        }

        Event_::SearchContext(body) => app.search.context_body = body,

        Event_::MergeDone(result, request) => {
            app.mode = Mode::Normal;
            match result {
                Ok(outcome) => {
                    // A conflicted merge can still report success, so the
                    // repository state is checked rather than trusted.
                    if outcome.left_operation_in_progress(&request.worktree) {
                        app.set_notice(
                            format!(
                                "merge of {} left a git operation in progress — resolve it in {}",
                                request.branch,
                                request.worktree.display()
                            ),
                            NoticeKind::Error,
                        );
                    } else if let Some(result) = outcome.result {
                        let mut notes = Vec::new();
                        if result.committed {
                            notes.push("committed changes");
                        }
                        if result.squashed {
                            notes.push("squashed");
                        }
                        if result.rebased {
                            notes.push("rebased");
                        }
                        if result.removed {
                            notes.push("removed the worktree");
                        }
                        let detail = if notes.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", notes.join(", "))
                        };
                        app.set_notice(
                            format!("merged {} into {}{detail}", request.branch, request.target),
                            NoticeKind::Info,
                        );
                    } else {
                        app.set_notice(
                            format!(
                                "merge of {} did not complete: {}",
                                request.branch,
                                tail(&outcome.stderr)
                            ),
                            NoticeKind::Error,
                        );
                    }
                }
                Err(e) => app.set_notice(format!("merge failed: {e}"), NoticeKind::Error),
            }
            *preview_for = None;
            fetch(repo, scope, false, events);
        }

        Event_::PrunePreview(result, min_age) => match result {
            Ok(candidates) => {
                let age = min_age.unwrap_or_else(|| "1d".to_string());
                app.mode = Mode::ConfirmPrune {
                    candidates,
                    min_age: age,
                };
            }
            Err(e) => {
                app.mode = Mode::Normal;
                app.set_notice(format!("could not preview prune: {e}"), NoticeKind::Error);
            }
        },

        Event_::Pruned(result, min_age) => {
            app.mode = Mode::Normal;
            match result {
                Ok(_) => app.set_notice(format!("pruned (min-age {min_age})"), NoticeKind::Info),
                Err(e) => app.set_notice(format!("prune failed: {e}"), NoticeKind::Error),
            }
            *preview_for = None;
            fetch(repo, scope, false, events);
        }
    }
}

/// Run a search on a blocking thread, since it is CPU-bound.
fn run_search(
    app: &App,
    request: SearchRequest,
    events: &tokio::sync::mpsc::UnboundedSender<Event_>,
) {
    let targets = app.search_targets();
    let tx = events.clone();
    let query = request.query.clone();
    let options = request.options;
    tokio::task::spawn_blocking(move || {
        let started = std::time::Instant::now();
        let results = search::search(&query, &targets, options);
        let _ = tx.send(Event_::SearchDone(results, started.elapsed()));
    });
}

/// The worktree path for a branch name, if it is still listed.
fn worktree_for_branch(app: &App, branch: Option<&String>) -> Option<PathBuf> {
    let branch = branch?;
    app.rows
        .iter()
        .find(|item| item.branch.as_deref() == Some(branch.as_str()))
        .and_then(|item| item.worktree.as_ref())
        .and_then(|w| w.path.as_deref())
        .map(PathBuf::from)
}

/// Read a file so the selected hit's surrounding lines can be shown.
fn spawn_search_context(
    app: &App,
    worktree: PathBuf,
    rel_path: &str,
    events: &tokio::sync::mpsc::UnboundedSender<Event_>,
) {
    let _ = app;
    let path = worktree.join(rel_path);
    let tx = events.clone();
    tokio::task::spawn_blocking(move || {
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = tx.send(Event_::SearchContext(body));
    });
}

/// Kick off a `wt list` load without blocking the loop.
///
/// `full` adds PR/CI data, which reaches the forge over the network, so it is
/// only ever requested explicitly.
#[allow(clippy::ptr_arg, reason = "the repo is cloned into a spawned task")]
fn fetch(
    repo: &PathBuf,
    scope: wt::ListScope,
    full: bool,
    events: &tokio::sync::mpsc::UnboundedSender<Event_>,
) {
    let repo = repo.clone();
    let tx = events.clone();
    tokio::spawn(async move {
        let result = wt::command::list_with(&repo, scope, full).await;
        let _ = tx.send(Event_::List(result));
    });
}

/// The receiving end of the input channel.
///
/// crossterm's `poll` is synchronous, so it gets its own thread and forwards
/// events over a channel rather than blocking the `select!` loop.
///
/// The thread outlives individual timeouts: a `poll` that reports no event is
/// an idle tick, not a reason to stop. Exiting on the first timeout would close
/// the channel, and a closed channel makes `recv()` resolve immediately on
/// every iteration of the `select!` — a busy loop that starves the runtime and
/// stops background loads from ever completing.
fn spawn_input() -> tokio::sync::mpsc::UnboundedReceiver<std::io::Result<Event>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("wt-tui-input".into())
        .spawn(move || {
            loop {
                match event::poll(Duration::from_millis(150)) {
                    Ok(true) => {
                        if tx.send(event::read()).is_err() {
                            break;
                        }
                    }
                    // Idle tick: keep waiting.
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        })
        .expect("cannot start the input thread");
    rx
}

/// Start a background `git` run for the preview pane.
fn spawn_preview(app: &mut App, path: PathBuf, tx: &tokio::sync::mpsc::UnboundedSender<Event_>) {
    let args = ui::preview::Preview::command(app.preview.tab, &path);
    app.preview.begin_load(path);
    let tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let body = match std::process::Command::new("git").args(&args).output() {
            Ok(output) if output.status.success() => {
                let out = String::from_utf8_lossy(&output.stdout).into_owned();
                if out.trim().is_empty() {
                    "(no output \u{2014} is this worktree clean and up to date?)".to_string()
                } else {
                    out
                }
            }
            Ok(output) => {
                let err = String::from_utf8_lossy(&output.stderr);
                let err = err.trim();
                if err.is_empty() {
                    format!("git exited with {}", output.status)
                } else {
                    err.to_string()
                }
            }
            Err(e) => format!("could not run git: {e}"),
        };
        let _ = tx.send(Event_::Preview(body));
    });
}

/// The last line of a command's stderr, for a one-line status message.
fn tail(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no detail")
        .trim()
        .to_string()
}

fn draw(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(frame.area());

    let body = chunks[0];

    if let Some(message) = app.fatal() {
        ui::status::render_fatal(frame, body, message);
    } else if app.initial_loading() {
        ui::status::render_loading(frame, body);
    } else {
        draw_body(frame, body, app);
    }

    ui::status::render(frame, chunks[1], app);

    if app.show_help {
        ui::help::render(frame, body);
    }

    // Dialogs last, so they sit above everything.
    match app.mode {
        Mode::ConfirmRemove { .. } => ui::remove::render(frame, body, app),
        Mode::ConfirmMerge { .. } => ui::merge::render(frame, body, app),
        Mode::ConfirmPrune { .. } => ui::prune::render(frame, body, app),
        _ => {}
    }
}

fn draw_body(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.view == View::Search {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(area);
        ui::search_view::render_results(frame, panes[0], app);
        ui::search_view::render_match_context(frame, panes[1], app);
        return;
    }

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(area);
    ui::table::render(frame, panes[0], app);
    ui::preview_view::render(frame, panes[1], app);
}

fn setup() -> std::io::Result<Terminal<Backend>> {
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
    setup_with(&mut terminal)?;
    Ok(terminal)
}

fn setup_with(terminal: &mut Terminal<Backend>) -> std::io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        ratatui::crossterm::cursor::Hide
    )?;
    terminal.clear()
}

fn restore(terminal: &mut Terminal<Backend>) -> std::io::Result<()> {
    disable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(
        stdout,
        LeaveAlternateScreen,
        ratatui::crossterm::cursor::Show
    )?;
    terminal.show_cursor()
}
