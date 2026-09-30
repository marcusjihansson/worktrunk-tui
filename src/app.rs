//! Application state: rows, selection, input modes, and pending work.
//!
//! The terminal and the event loop live in `main.rs`; this module is the state
//! they drive, kept free of I/O so its behaviour can be tested directly.

use crate::ui;
use crate::wt::command;
use crate::wt::model::{Envelope, Item};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

/// Fallback poll interval, used when the filesystem watcher has nothing to say.
///
/// This is a safety net for missed watch notifications and for changes made
/// outside the repository entirely.
pub const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Table,
    Preview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Typing a filter, bound to `/`.
    Filtering(String),
    /// Typing a new branch name, bound to `n`.
    Creating(String),
    /// Confirming a removal, bound to `d`.
    ConfirmRemove {
        branch: String,
        force: bool,
        dirty: bool,
    },
    /// A `wt` command is running; only quit is honoured.
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Error,
}

/// A transient message in the status bar.
#[derive(Debug, Clone)]
pub struct Notice {
    pub text: String,
    pub kind: NoticeKind,
}

/// Work handed from the key handler to the event loop.
#[derive(Debug, Default)]
pub struct Pending {
    pub create: Option<String>,
    pub remove: Option<(String, bool)>,
    pub editor: Option<(String, PathBuf)>,
    pub refresh: bool,
    /// True when the preview tab changed and its body must be re-fetched.
    pub preview_stale: bool,
}

impl Pending {
    /// Take the refresh request, clearing it.
    pub fn take_refresh(&mut self) -> bool {
        std::mem::take(&mut self.refresh)
    }
}

pub struct App {
    /// The repository under watch.
    repo: PathBuf,
    /// Rows as last loaded from `wt list`.
    pub rows: Vec<Item>,
    /// Indices into `rows` that survive the current filter.
    pub visible: Vec<usize>,
    /// True once at least one load has succeeded.
    loaded: bool,
    /// A persistent, screen-filling problem (e.g. a config mismatch).
    fatal: Option<String>,
    /// Whether a fetch is in flight, so the status bar can show progress.
    pub fetching: bool,
    /// The repo's default branch, from the envelope's `repo` object. Used as
    /// the base branch when creating a worktree.
    default_branch_name: Option<String>,

    selected: usize,
    /// Identity of the selected row, so a refresh can restore the cursor onto
    /// the same branch even if rows were added or removed.
    selected_identity: Option<String>,
    pub scroll: u16,

    pub pane: Pane,
    pub mode: Mode,
    pub filter: String,
    pub notice: Option<Notice>,
    pub should_quit: bool,
    pub show_help: bool,
    pub pending: Pending,

    pub preview: ui::preview::Preview,
    pub branches: Vec<String>,
    /// True when a `--full` load has happened, so PR/CI columns are meaningful.
    pub collected_ci: bool,
}

impl App {
    pub fn new(repo: PathBuf) -> Self {
        Self {
            repo,
            rows: Vec::new(),
            visible: Vec::new(),
            loaded: false,
            fatal: None,
            fetching: false,
            default_branch_name: None,
            selected: 0,
            selected_identity: None,
            scroll: 0,
            pane: Pane::Table,
            mode: Mode::Normal,
            filter: String::new(),
            notice: None,
            should_quit: false,
            show_help: false,
            pending: Pending::default(),
            preview: ui::preview::Preview::new(),
            branches: Vec::new(),
            collected_ci: false,
        }
    }

    /// The repository under watch.
    #[allow(dead_code, reason = "used by the multi-repo and search phases")]
    pub fn repo(&self) -> &std::path::Path {
        &self.repo
    }

    /// The default branch, used as the base when creating a branch.
    #[allow(dead_code, reason = "used by the create dialog with a base selector")]
    pub fn default_branch(&self) -> Option<&str> {
        self.default_branch_name.as_deref()
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.visible
            .get(self.selected)
            .and_then(|&i| self.rows.get(i))
    }

    /// The row index the table should highlight.
    pub fn selected_row(&self) -> usize {
        self.selected
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.selected_item()
            .and_then(|i| i.worktree.as_ref())
            .and_then(|w| w.path.as_deref())
            .map(PathBuf::from)
    }

    /// True only before the very first load, so the first paint can show a
    /// loading state rather than an empty table.
    pub fn initial_loading(&self) -> bool {
        !self.loaded && self.fatal.is_none()
    }

    /// A problem worth showing full-screen with instructions.
    pub fn fatal(&self) -> Option<&str> {
        self.fatal.as_deref()
    }

    pub fn set_notice(&mut self, text: impl Into<String>, kind: NoticeKind) {
        self.notice = Some(Notice {
            text: text.into(),
            kind,
        });
    }

    /// Apply a freshly fetched envelope.
    pub fn apply(&mut self, envelope: Envelope, default_branch: Option<String>) {
        self.fatal = None;
        self.loaded = true;
        self.collected_ci = envelope
            .collected
            .as_ref()
            .is_some_and(|c| c.ci || c.summary);
        self.default_branch_name = default_branch;

        // The previous selection is remembered by name so the cursor can be
        // restored after the rows are replaced.
        let previous_identity = self.selected_identity.clone();
        let previous_len = self.rows.len();

        self.rows = envelope.items;
        self.branches = self
            .rows
            .iter()
            .filter(|i| i.is_worktree())
            .filter_map(|i| i.branch.clone())
            .collect();
        self.refilter();

        // An agent creating a worktree must not silently move the user onto a
        // different branch, so restore by identity before falling back to an
        // index.
        let restored = previous_identity.as_deref().and_then(|id| {
            self.visible
                .iter()
                .position(|&i| self.rows[i].identity() == id)
        });
        self.selected = restored.unwrap_or_else(|| previous_len.min(self.selected));
        self.clamp_selection();
        self.selected_identity = self.selected_item().map(Item::identity);
    }

    /// Report a failed load.
    ///
    /// Setup problems (a legacy schema, an unsupported version) get the
    /// full-screen treatment; anything else is transient and belongs in the
    /// status bar.
    pub fn set_error(&mut self, message: String) {
        if is_setup_problem(&message) {
            self.fatal = Some(message);
        } else {
            self.set_notice(message, NoticeKind::Error);
        }
    }

    fn refilter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.visible = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, item)| matches_filter(item, &needle))
            .map(|(i, _)| i)
            .collect();
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        if self.visible.is_empty() {
            self.selected = 0;
            self.scroll = 0;
        } else if self.selected >= self.visible.len() {
            self.selected = self.visible.len() - 1;
        }
    }

    /// Scroll so the cursor stays inside a viewport of `height` rows.
    pub fn viewport(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        let selected = self.selected as u16;
        if selected < self.scroll {
            self.scroll = selected;
        } else if selected >= self.scroll + height as u16 {
            self.scroll = selected + 1 - height as u16;
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
        self.selected_identity = self.selected_item().map(Item::identity);
    }

    /// Handle a key press. The caller redraws when this returns true.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return true;
        }

        // Dispatch on a clone of the mode so a handler can mutate `self`
        // without borrowing the mode twice.
        match self.mode.clone() {
            Mode::Busy => return true,
            Mode::Filtering(buffer) => return self.on_key_filtering(key, &buffer),
            Mode::Creating(buffer) => return self.on_key_creating(key, &buffer),
            Mode::ConfirmRemove {
                branch,
                force,
                dirty,
            } => {
                return self.on_key_confirm(key, branch, force, dirty);
            }
            Mode::Normal => {}
        }

        if self.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter => self.show_help = false,
                _ => {}
            }
            return true;
        }

        // With the preview focused, Up/Down scroll it rather than moving the
        // cursor, which is what a pane that owns its viewport should do.
        if self.pane == Pane::Preview {
            match key.code {
                KeyCode::Up => {
                    self.preview.scroll_up(1);
                    return true;
                }
                KeyCode::Down => {
                    self.preview.scroll_down(1);
                    return true;
                }
                KeyCode::PageUp => {
                    self.preview.scroll_up(20);
                    return true;
                }
                KeyCode::PageDown => {
                    self.preview.scroll_down(20);
                    return true;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('g') | KeyCode::Home => self.move_by(-(self.visible.len() as isize)),
            KeyCode::Char('G') | KeyCode::End => self.move_by(self.visible.len() as isize),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::Tab => {
                self.pane = match self.pane {
                    Pane::Table => Pane::Preview,
                    Pane::Preview => Pane::Table,
                };
            }
            // Cycle the preview tab. It acts on the preview rather than the
            // table, so it works from either pane.
            KeyCode::Char('t') => {
                self.preview.tab = self.preview.tab.next();
                self.preview.state = ui::preview::PreviewState::Loading;
                self.preview.body.clear();
                self.preview.line_count = 0;
                self.preview.scroll = 0;
                self.pending.preview_stale = true;
            }
            KeyCode::Char('/') => self.mode = Mode::Filtering(self.filter.clone()),
            KeyCode::Char('n') => self.mode = Mode::Creating(String::new()),
            KeyCode::Char('d') => self.begin_remove(),
            KeyCode::Char('y') => {
                if let Some(item) = self.selected_item() {
                    let text = item.branch.clone().unwrap_or_else(|| item.label());
                    self.copy(text);
                }
            }
            KeyCode::Char('c') => {
                if let Some(path) = self.selected_path() {
                    self.copy(path.to_string_lossy().into_owned());
                }
            }
            KeyCode::Char('o') => self.open_pr(),
            KeyCode::Char('e') => self.open_editor(),
            KeyCode::Char('r') => {
                self.set_notice("refreshing…", NoticeKind::Info);
                self.pending.refresh = true;
            }
            _ => {}
        }
        true
    }

    fn begin_remove(&mut self) {
        let Some(item) = self.selected_item() else {
            return;
        };
        let target = item
            .branch
            .clone()
            .or_else(|| item.worktree.as_ref().and_then(|w| w.path.clone()));
        match target {
            Some(branch) => {
                let dirty = item.has_changes();
                self.mode = Mode::ConfirmRemove {
                    branch,
                    force: false,
                    dirty,
                };
            }
            None => self.set_notice(
                "this row has no branch or path to remove",
                NoticeKind::Error,
            ),
        }
    }

    /// Handle a keystroke while filtering.
    ///
    /// The buffer is taken out of the mode, edited, and written back. Keeping
    /// it only in a local copy would reset the text on every keystroke, leaving
    /// just the last character typed.
    fn on_key_filtering(&mut self, key: KeyEvent, current: &str) -> bool {
        let mut buffer = current.to_string();
        let mut done = false;

        match key.code {
            // Escape abandons the filter entirely; Enter commits it.
            KeyCode::Esc => {
                buffer.clear();
                done = true;
            }
            KeyCode::Enter => done = true,
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char(c) => buffer.push(c),
            _ => {}
        }

        self.filter = buffer.clone();
        if done {
            self.mode = Mode::Normal;
        } else if let Mode::Filtering(text) = &mut self.mode {
            *text = buffer;
        }

        self.refilter();
        self.selected_identity = self.selected_item().map(Item::identity);
        true
    }

    /// Handle a keystroke while naming a new branch.
    fn on_key_creating(&mut self, key: KeyEvent, current: &str) -> bool {
        let mut buffer = current.to_string();

        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return true;
            }
            KeyCode::Enter => {
                let name = buffer.trim().to_string();
                self.mode = Mode::Normal;
                if name.is_empty() {
                    self.set_notice("a branch name is required", NoticeKind::Error);
                } else {
                    self.pending.create = Some(name);
                    self.mode = Mode::Busy;
                }
                return true;
            }
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char(c) => buffer.push(c),
            _ => {}
        }

        if let Mode::Creating(text) = &mut self.mode {
            *text = buffer;
        }
        true
    }

    fn on_key_confirm(&mut self, key: KeyEvent, branch: String, force: bool, dirty: bool) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => self.mode = Mode::Normal,
            KeyCode::Char('f') => {
                self.mode = Mode::ConfirmRemove {
                    branch,
                    force: !force,
                    dirty,
                }
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                self.pending.remove = Some((branch, force));
                self.mode = Mode::Busy;
            }
            _ => {}
        }
        true
    }

    fn copy(&mut self, text: String) {
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text.clone())) {
            Ok(()) => self.set_notice(format!("copied {text}"), NoticeKind::Info),
            Err(e) => self.set_notice(format!("could not copy: {e}"), NoticeKind::Error),
        }
    }

    fn open_pr(&mut self) {
        let url = self
            .selected_item()
            .and_then(|i| i.pr.as_ref())
            .and_then(|p| p.url.clone());
        match url {
            Some(url) => {
                if let Err(e) = open::that_detached(&url) {
                    self.set_notice(format!("could not open {url}: {e}"), NoticeKind::Error);
                }
            }
            None => self.set_notice("no pull request for this branch", NoticeKind::Info),
        }
    }

    fn open_editor(&mut self) {
        let Some(path) = self.selected_path() else {
            return;
        };
        match command::editor_command() {
            Some(editor) => self.pending.editor = Some((editor, path)),
            None => self.set_notice("set $EDITOR to open a worktree", NoticeKind::Info),
        }
    }
}

fn matches_filter(item: &Item, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    // Mirrors what worktrunk's own picker matches on (branch, path, PR number),
    // plus the commit subject and marker, which matter when hunting for
    // whatever an agent was doing.
    let fields = [
        item.branch.clone(),
        item.worktree.as_ref().and_then(|w| w.path.clone()),
        item.marker.clone(),
        item.head.as_ref().and_then(|h| h.subject.clone()),
        item.pr
            .as_ref()
            .and_then(|p| p.number.map(|n| format!("#{n}"))),
    ];
    fields
        .iter()
        .flatten()
        .any(|f| f.to_lowercase().contains(needle))
}

/// Whether an error describes a setup problem that will not fix itself on the
/// next poll, and so deserves the full-screen banner.
fn is_setup_problem(message: &str) -> bool {
    message.contains("legacy bare-array format") || message.contains("reads worktrunk JSON schema")
}
