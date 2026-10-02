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
    /// Typing a search query, bound to `/` in search mode.
    Searching(String),
    /// Confirming a removal, bound to `d`.
    ConfirmRemove {
        branch: String,
        force: bool,
        dirty: bool,
    },
    /// Confirming a merge, bound to `m`.
    ConfirmMerge {
        branch: String,
        target: String,
        /// The confirmed row's worktree, captured when the dialog opened.
        ///
        /// Captured deliberately. `wt merge` merges the *current* branch into the
        /// target, so the worktree is what decides which branch is merged — and
        /// with `keep_worktree` off it is also what decides which branch is
        /// deleted. Re-reading the selection at execute time would merge the
        /// wrong branch: a refresh replaces `rows` (and may fall back to an
        /// index) while the dialog is open, so the cursor can silently move to
        /// a different row between the user reading the prompt and pressing `y`.
        worktree: PathBuf,
        keep_worktree: bool,
    },
    /// Confirming a bulk prune, bound to `P`.
    ConfirmPrune {
        candidates: Vec<command::PruneCandidate>,
        min_age: String,
    },
    /// A `wt` command is running; only quit is honoured.
    Busy,
}

/// What the main area is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The worktree table.
    Worktrees,
    /// Cross-worktree search results.
    Search,
}

/// Identifies the file a context body was read from: the worktree, and the path
/// within it.
///
/// Keyed on both, not on the relative path alone. Two worktrees can each hold a
/// `src/main.rs` that a query matches, and because search deduplicates by tree
/// rather than by path those are genuinely different hits with identical
/// `rel_path` values. Keying on the relative path alone would show one
/// worktree's file under another's hit.
pub type ContextKey = (String, String);

/// The search session: the query, its results, and the file being previewed.
#[derive(Debug, Default)]
pub struct SearchState {
    pub query: String,
    pub results: crate::search::Results,
    /// The file around the selected hit.
    pub context_body: String,
    /// The file `context_body` holds, if any.
    pub context_path: Option<ContextKey>,
    /// The file with a read in flight, if any.
    ///
    /// Tracked so moving the cursor does not spawn a read per keystroke, and so
    /// a response for a file the user has navigated away from is discarded
    /// rather than shown against the wrong hit.
    pub context_loading: Option<ContextKey>,
    /// Index into `results.hits`.
    pub selected: usize,
    pub scroll: u16,
    pub running: bool,
    pub elapsed: Option<std::time::Duration>,
    pub options: crate::search::QueryOptions,
    /// Whether to search every worktree or only the filtered rows.
    pub only_filtered: bool,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn selected_hit(&self) -> Option<&crate::search::Hit> {
        self.results.hits.get(self.selected)
    }

    /// Move the cursor, clamped to the result list.
    ///
    /// The context body is *not* cleared here. It holds a whole file, so it stays
    /// valid for every hit inside that file, and clearing it per keystroke is
    /// what made each `j` re-read from disk. Deciding whether the body on screen
    /// still matches the selected hit needs the hit's *worktree* as well as its
    /// path, which only the event loop can resolve — see [`ContextKey`].
    pub fn move_by(&mut self, delta: isize) {
        if self.results.hits.is_empty() {
            self.selected = 0;
            return;
        }
        let len = self.results.hits.len() as isize;
        self.selected = (self.selected as isize + delta).clamp(0, len - 1) as usize;
        self.scroll = 0;
    }

    /// Set the query and clear stale results.
    ///
    /// Results are cleared rather than left on screen because they belong to the
    /// previous query; showing them under new text would be a lie.
    pub fn set_query(&mut self, query: String) {
        if self.query != query {
            self.query = query;
            self.results = crate::search::Results::default();
            self.context_body.clear();
            self.context_path = None;
            self.context_loading = None;
            self.selected = 0;
            self.scroll = 0;
        }
    }
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

/// An editor invocation queued from the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorLaunch {
    pub program: String,
    /// Flags from `$EDITOR`, which precede the path.
    pub args: Vec<String>,
    /// The worktree to open.
    pub path: PathBuf,
}

/// Work handed from the key handler to the event loop.
#[derive(Debug, Default)]
pub struct Pending {
    pub create: Option<String>,
    pub remove: Option<(String, bool)>,
    pub editor: Option<EditorLaunch>,
    pub refresh: bool,
    /// True when the preview tab changed and its body must be re-fetched.
    pub preview_stale: bool,
    /// A search to run: the query and the targets to search.
    pub search: Option<SearchRequest>,
    /// A merge to run: the source worktree, the target branch, and whether to
    /// keep the worktree.
    pub merge: Option<MergeRequest>,
    /// Load the prune candidate list without removing anything.
    pub prune_preview: bool,
    /// A min-age override chosen in the prune dialog, applied to the preview
    /// that follows so the list matches what would actually happen.
    pub prune_age: Option<String>,
    /// Prune with the given min-age.
    pub prune: Option<String>,
    /// Fetch PR/CI data with `wt list --full`.
    pub fetch_ci: bool,
}

/// A search to run off the UI thread.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub options: crate::search::QueryOptions,
}

/// A merge to run off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRequest {
    /// The worktree whose branch is the merge source.
    pub worktree: PathBuf,
    pub branch: String,
    pub target: String,
    pub keep_worktree: bool,
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
    /// What the main area shows.
    pub view: View,
    /// The search session, populated when `view` is `Search`.
    pub search: SearchState,
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
    /// True once a `--full` load has been requested. Before that, the CI column
    /// says "not fetched" instead of implying a branch has no checks.
    pub ci_attempted: bool,
}

impl App {
    pub fn new(repo: PathBuf) -> Self {
        Self {
            repo,
            view: View::Worktrees,
            search: SearchState::new(),
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
            ci_attempted: false,
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

    pub fn selected_hit(&self) -> Option<&crate::search::Hit> {
        self.search.selected_hit()
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.selected_item()
            .and_then(|i| i.worktree.as_ref())
            .and_then(|w| w.path.as_deref())
            .map(PathBuf::from)
    }

    /// The row whose worktree is `path`, regardless of where the cursor is.
    ///
    /// Confirmation dialogs use this instead of `selected_item` so the facts on
    /// screen describe the row that will actually be acted on. A refresh can
    /// move the cursor between opening a dialog and confirming it, and showing
    /// another branch's ahead/behind and conflict warnings under this
    /// branch's name is how a user confirms the wrong thing.
    pub fn item_for_worktree(&self, path: &std::path::Path) -> Option<&Item> {
        self.rows.iter().find(|item| {
            item.worktree
                .as_ref()
                .and_then(|w| w.path.as_deref())
                .is_some_and(|p| std::path::Path::new(p) == path)
        })
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
            Mode::Busy => {
                // Nothing that could queue another command is honoured while
                // one is running. Cancel and quit still are: a `wt` call that
                // hangs must not leave the user unable to back out of the
                // dialog it was opened from.
                match key.code {
                    KeyCode::Esc | KeyCode::Char('n') => self.mode = Mode::Normal,
                    KeyCode::Char('q') => self.should_quit = true,
                    _ => {}
                }
                return true;
            }
            Mode::Filtering(buffer) => return self.on_key_filtering(key, &buffer),
            Mode::Creating(buffer) => return self.on_key_creating(key, &buffer),
            Mode::Searching(buffer) => return self.on_key_searching(key, &buffer),
            Mode::ConfirmRemove {
                branch,
                force,
                dirty,
            } => {
                return self.on_key_confirm(key, branch, force, dirty);
            }
            Mode::ConfirmMerge {
                branch,
                target,
                worktree,
                keep_worktree,
            } => {
                return self.on_key_merge(key, branch, target, worktree, keep_worktree);
            }
            Mode::ConfirmPrune { min_age, .. } => {
                return self.on_key_prune(key, min_age);
            }
            Mode::Normal => {}
        }

        // While the search query is being typed, it owns every key. Without this the
        // table's bindings would fire mid-query: typing "r" would refresh and
        // "i" would toggle case-insensitivity instead of producing a character.
        if matches!(self.mode, Mode::Searching(_)) {
            return true;
        }

        // In search mode the same navigation keys drive the results list.
        if self.view == View::Search && self.on_key_in_search(key) {
            return true;
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
            // `/` refines the table's filter, keeping its text so it can be
            // narrowed. In the search view it starts a new query instead:
            // pre-filling the previous one would leave you appending to a
            // finished search, which is not what pressing `/` means there.
            KeyCode::Char('/') => {
                self.mode = if self.view == View::Search {
                    self.search.query.clear();
                    Mode::Searching(String::new())
                } else {
                    Mode::Filtering(self.filter.clone())
                };
            }
            KeyCode::Char('n') => self.mode = Mode::Creating(String::new()),
            KeyCode::Char('d') => self.begin_remove(),
            KeyCode::Char('m') => self.begin_merge(),
            // `s` toggles the view. Entering it focuses the query when there is no query
            // yet, and otherwise leaves the cursor on the results so the
            // navigation and modifier keys work immediately.
            KeyCode::Char('s') => {
                if self.view == View::Search {
                    self.view = View::Worktrees;
                } else {
                    self.view = View::Search;
                    if self.search.query.is_empty() {
                        self.mode = Mode::Searching(String::new());
                    }
                }
            }
            KeyCode::Char('S') => {
                // Only fetch PR/CI data; never on the watcher's hot path,
                // because `--full` reaches the forge over the network.
                self.pending.fetch_ci = true;
                self.set_notice("fetching PR and CI status…", NoticeKind::Info);
            }
            KeyCode::Char('P') => {
                self.pending.prune_preview = true;
                self.mode = Mode::Busy;
            }
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

    /// Keys that act only while the search view is showing.
    ///
    /// Returns true when the key was consumed, so the table's bindings do not
    /// also fire.
    fn on_key_in_search(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.search.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.search.move_by(-1),
            KeyCode::Char('g') | KeyCode::Home => self.search.move_by(-(i32::MAX as isize)),
            KeyCode::Char('G') | KeyCode::End => self.search.move_by(i32::MAX as isize),
            KeyCode::PageDown => self.search.move_by(10),
            KeyCode::PageUp => self.search.move_by(-10),
            // Modifiers that change how the *next* search behaves.
            KeyCode::Char('i') => self.search.options.case_insensitive = true,
            KeyCode::Char('I') => self.search.options.case_insensitive = false,
            KeyCode::Char('r') => self.search.options.force_regex = true,
            KeyCode::Char('R') => self.search.options.force_regex = false,
            KeyCode::Char('f') => self.search.only_filtered = !self.search.only_filtered,
            // Re-run the current query.
            KeyCode::Enter => self.queue_search(),
            _ => return false,
        }
        true
    }

    /// Ask the event loop to run a search over the current targets.
    fn queue_search(&mut self) {
        let query = self.search.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.search.running = true;
        self.pending.search = Some(SearchRequest {
            query,
            options: self.search.options,
        });
    }

    /// The worktrees to search, honouring the scope toggle.
    pub fn search_targets(&self) -> Vec<crate::search::Target> {
        self.rows
            .iter()
            .filter(|item| !self.search.only_filtered || self.filter_matches(item))
            .filter_map(|item| {
                let path = item.worktree.as_ref()?.path.as_ref()?;
                Some(crate::search::Target {
                    branch: item.label(),
                    path: PathBuf::from(path),
                })
            })
            .collect()
    }

    /// Whether an item passes the table's filter, for search scoping.
    fn filter_matches(&self, item: &Item) -> bool {
        let needle = self.filter.to_lowercase();
        if needle.is_empty() {
            return true;
        }
        [
            item.branch.clone(),
            item.worktree.as_ref().and_then(|w| w.path.clone()),
            item.marker.clone(),
        ]
        .iter()
        .flatten()
        .any(|f| f.to_lowercase().contains(&needle))
    }

    /// Open the merge dialog for the selected row.
    fn begin_merge(&mut self) {
        // Read what the dialog needs before touching `self.mode`, which would
        // otherwise conflict with the borrow of the selected item.
        let Some(item) = self.selected_item() else {
            return;
        };

        // A merge needs a worktree: `wt merge` merges the *current* branch, and
        // a branch-only row has none.
        if item.worktree.is_none() {
            self.set_notice(
                "this branch has no worktree, so there is nothing to merge from",
                NoticeKind::Error,
            );
            return;
        }
        let Some(branch) = item.branch.clone() else {
            self.set_notice(
                "a detached worktree has no branch to merge",
                NoticeKind::Error,
            );
            return;
        };

        // A merge needs a worktree path, not merely a worktree: `wt merge`
        // operates on the worktree's current branch, so the path is the thing
        // that decides what gets merged and what gets deleted.
        let Some(worktree) = item
            .worktree
            .as_ref()
            .and_then(|w| w.path.as_deref())
            .map(PathBuf::from)
        else {
            self.set_notice(
                format!("{branch} has no worktree path, so there is nothing to merge from"),
                NoticeKind::Error,
            );
            return;
        };

        let target = self.default_branch().unwrap_or("main").to_string();
        // `worktree` is captured here so the executed command cannot drift from
        // what this dialog describes. See `Mode::ConfirmMerge`.
        self.mode = Mode::ConfirmMerge {
            branch,
            target,
            worktree,
            keep_worktree: false,
        };
    }

    fn on_key_merge(
        &mut self,
        key: KeyEvent,
        branch: String,
        target: String,
        worktree: PathBuf,
        keep_worktree: bool,
    ) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => self.mode = Mode::Normal,
            KeyCode::Char('w') => {
                self.mode = Mode::ConfirmMerge {
                    branch,
                    target,
                    worktree,
                    keep_worktree: !keep_worktree,
                };
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                // The captured worktree, never a fresh read of the selection:
                // a refresh can move the cursor to a different row while this
                // dialog is open, and `keep_worktree` off means the wrong row
                // would also be deleted. See `Mode::ConfirmMerge`.
                self.pending.merge = Some(MergeRequest {
                    worktree,
                    branch,
                    target,
                    keep_worktree,
                });
                self.mode = Mode::Busy;
            }
            _ => {}
        }
        true
    }

    fn on_key_prune(&mut self, key: KeyEvent, min_age: String) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => self.mode = Mode::Normal,
            // Cycling the age guard matters: with the default of 1d, prune
            // silently skips a worktree created minutes ago.
            KeyCode::Char('a') => {
                let next = match min_age.as_str() {
                    "0" => "1d",
                    "1d" => "7d",
                    _ => "0",
                };
                // Deliberately does *not* write the new age into `mode`. The
                // candidate list on screen was selected by a `--dry-run` at the
                // current age, so labelling it with a different age would
                // describe a different set of worktrees than `y` actually prunes
                // — and `--min-age 0` is a strict superset of the shown list,
                // including worktrees created seconds ago.
                //
                // Instead, ask for a fresh dry run. `Event_::PrunePreview` sets
                // the age and the candidates together from the same request, so
                // the two cannot disagree. `Busy` in the meantime means `y` does
                // nothing, which is the right behaviour for a destructive
                // confirmation whose contents are being recomputed.
                self.pending.prune_age = Some(next.to_string());
                self.pending.prune_preview = true;
                self.mode = Mode::Busy;
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                self.pending.prune = Some(min_age);
                self.mode = Mode::Busy;
            }
            _ => {}
        }
        true
    }

    /// Handle a keystroke while typing a search query.
    fn on_key_searching(&mut self, key: KeyEvent, current: &str) -> bool {
        let mut buffer = current.to_string();
        let mut done = false;

        match key.code {
            KeyCode::Esc => {
                // Escape abandons editing rather than clearing it: the text is
                // often worth coming back to.
                self.mode = Mode::Normal;
                return true;
            }
            KeyCode::Enter => done = true,
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char(c) => buffer.push(c),
            _ => return true,
        }

        self.search.set_query(buffer.clone());

        // Write the buffer back into the mode: the next keystroke reads it from
        // there, so not updating it would leave only the last character typed.
        if done {
            self.mode = Mode::Normal;
            self.queue_search();
        } else if let Mode::Searching(text) = &mut self.mode {
            *text = buffer;
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
        let Some(url) = url else {
            self.set_notice("no pull request for this branch", NoticeKind::Info);
            return;
        };

        // Only web URLs reach the desktop handler. `open` dispatches *any*
        // registered scheme, so a `vscode://`, `file://` or other custom URL
        // would launch that handler with an attacker-chosen argument. In practice
        // `pr.url` is forge-generated and not attacker-settable — but on a
        // self-hosted forge the host derives from `remote.origin.url`, which is
        // repository content, and this is a one-line check.
        let scheme = url.split_once(':').map(|(scheme, _)| scheme);
        if !matches!(scheme, Some("http" | "https")) {
            self.set_notice(
                format!("refusing to open {url}: not an http(s) URL"),
                NoticeKind::Error,
            );
            return;
        }

        if let Err(e) = open::that_detached(&url) {
            self.set_notice(format!("could not open {url}: {e}"), NoticeKind::Error);
        }
    }

    fn open_editor(&mut self) {
        let Some(path) = self.selected_path() else {
            return;
        };
        match command::editor_command() {
            Some(editor) => {
                self.pending.editor = Some(EditorLaunch {
                    program: editor.program,
                    args: editor.args,
                    path,
                });
            }
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
