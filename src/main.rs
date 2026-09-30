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
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::{Frame, Terminal};
use std::io::Stdout;
use std::path::PathBuf;
use std::time::Duration;
use wt_tui::app::{App, Mode, NoticeKind, REFRESH_INTERVAL};
use wt_tui::ui;
use wt_tui::{watch, wt};

/// Background results arriving from spawned work.
enum Event_ {
    List(Result<wt::Envelope, wt::command::ListError>),
    Created(Result<wt::model::SwitchResult, String>, String),
    Removed(Result<Vec<wt::model::RemoveResult>, String>, String),
    Preview(String),
}

type Backend = ratatui::backend::CrosstermBackend<Stdout>;

#[derive(Debug, clap::Parser)]
#[command(
    name = "wt-tui",
    about = "A TUI dashboard for worktrunk worktrees",
    long_about = "wt-tui watches worktrunk worktrees and lets you browse, filter, preview, \
                  create, and remove them. It reads worktrunk through its documented JSON \
                  output, so it stays current as worktrunk gains features.\n\n\
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

    // Background results channel, created up front so the first preview load can
    // be queued before the loop starts.
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

    // Prime the first load and the first preview.
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

        // Act on work queued by the key handler.
        if let Some(branch) = app.pending.create.take() {
            app.set_notice(format!("creating {branch}…"), NoticeKind::Info);
            let repo = repo.clone();
            let tx = events.clone();
            tokio::spawn(async move {
                let result = wt::command::create(&repo, &branch, None).await;
                let _ = tx.send(Event_::Created(result, branch));
            });
        }
        if let Some((branch, force)) = app.pending.remove.take() {
            app.set_notice(format!("removing {branch}…"), NoticeKind::Info);
            let repo = repo.clone();
            let tx = events.clone();
            tokio::spawn(async move {
                let result = wt::command::remove(&repo, &branch, force).await;
                let _ = tx.send(Event_::Removed(result, branch));
            });
        }
        if let Some((editor, path)) = app.pending.editor.take() {
            // Give the editor the terminal, then take it back.
            let _ = restore(&mut terminal);
            let status = std::process::Command::new(&editor).arg(&path).status();
            let _ = setup_with(&mut terminal);
            match status {
                Ok(s) if s.success() => app.set_notice("editor closed", NoticeKind::Info),
                Ok(s) => app.set_notice(format!("{editor} exited {s}"), NoticeKind::Error),
                Err(e) => app.set_notice(format!("could not run {editor}: {e}"), NoticeKind::Error),
            }
        }
        if app.pending.preview_stale {
            app.pending.preview_stale = false;
            if let Some(path) = app.selected_path() {
                spawn_preview(&mut app, path.clone(), &events);
                preview_for = Some(path);
            }
        }
        if app.pending.take_refresh() {
            let _ = watch_tx.send(());
        }

        // Load a preview when the selection moved to a different worktree.
        if app.mode == Mode::Normal
            && let Some(path) = app.selected_path()
            && preview_for.as_ref() != Some(&path)
        {
            spawn_preview(&mut app, path.clone(), &events);
            preview_for = Some(path);
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
                    fetch(&repo, scope, &events);
                }
            }

            _ = ticker.tick() => {
                // Safety net for missed watch notifications.
                if !fetching {
                    fetching = true;
                    app.fetching = true;
                    fetch(&repo, scope, &events);
                }
            }

            maybe = event_rx.recv() => {
                let Some(ev) = maybe else { break };
                match ev {
                    Event_::List(result) => {
                        fetching = false;
                        app.fetching = false;
                        match result {
                            Ok(envelope) => {
                                let default_branch =
                                    envelope.repo.as_ref().and_then(|r| r.default_branch.clone());
                                app.apply(envelope, default_branch);
                            }
                            Err(e) => app.set_error(e.to_string()),
                        }
                    }
                    Event_::Created(result, branch) => {
                        app.mode = Mode::Normal;
                        match result {
                            Ok(res) => {
                                let path = res.path.unwrap_or_default();
                                app.set_notice(format!("created {branch} @ {path}"), NoticeKind::Info);
                            }
                            Err(e) => {
                                app.set_notice(format!("could not create {branch}: {e}"), NoticeKind::Error);
                            }
                        }
                        // Refresh now rather than waiting for the next tick.
                        fetching = true;
                        app.fetching = true;
                        fetch(&repo, scope, &events);
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
                                app.set_notice(format!("could not remove {branch}: {e}"), NoticeKind::Error);
                            }
                        }
                        fetching = true;
                        app.fetching = true;
                        fetch(&repo, scope, &events);
                    }
                    Event_::Preview(body) => app.preview.finish_load(body),
                }
            }
        }
    }

    let _ = restore(&mut terminal);
    drop(watcher);
    std::process::ExitCode::SUCCESS
}

/// Kick off a `wt list` load without blocking the loop.
///
/// The path is taken by reference and cloned in, because the spawned future
/// must be `'static` and cannot borrow from the event loop.
#[allow(clippy::ptr_arg, reason = "the repo is cloned into a spawned task")]
fn fetch(
    repo: &PathBuf,
    scope: wt::ListScope,
    events: &tokio::sync::mpsc::UnboundedSender<Event_>,
) {
    let repo = repo.clone();
    let tx = events.clone();
    tokio::spawn(async move {
        let result = wt::command::list(&repo, scope).await;
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
///
/// The body is produced here rather than shipping a `std::process::Output`
/// across the channel, because the only thing the UI needs is text.
fn spawn_preview(app: &mut App, path: PathBuf, tx: &tokio::sync::mpsc::UnboundedSender<Event_>) {
    let args = ui::preview::Preview::command(app.preview.tab, &path);
    app.preview.begin_load(path);
    let tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let body = match std::process::Command::new("git").args(&args).output() {
            Ok(output) if output.status.success() => {
                let out = String::from_utf8_lossy(&output.stdout).into_owned();
                if out.trim().is_empty() {
                    "(no output — is this worktree clean and up to date?)".to_string()
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
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(body);
        ui::table::render(frame, panes[0], app);
        ui::preview_view::render(frame, panes[1], app);
    }

    ui::status::render(frame, chunks[1], app);

    if app.show_help {
        ui::help::render(frame, body);
    }
    if matches!(app.mode, Mode::ConfirmRemove { .. }) {
        ui::remove::render(frame, body, app);
    }
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
