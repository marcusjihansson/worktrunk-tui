//! The preview pane: a diff (default) and a log for the selected worktree.
//!
//! Diff is the default because the question a worktree dashboard is asked most
//! often is "what changed in here?", and a diff answers it without scrolling.

use std::path::PathBuf;

/// Which preview body is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewTab {
    Diff,
    Log,
}

impl PreviewTab {
    pub const ALL: [PreviewTab; 2] = [PreviewTab::Diff, PreviewTab::Log];

    pub fn label(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::Log => "log",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Diff => Self::Log,
            Self::Log => Self::Diff,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewState {
    /// No worktree is selected, or it has no path.
    Empty,
    /// A git command is running.
    Loading,
    /// Loaded, possibly with an error message from git.
    Ready,
}

pub struct Preview {
    pub tab: PreviewTab,
    pub state: PreviewState,
    /// Raw ANSI output, rendered through `ansi-to-tui` at draw time.
    pub body: String,
    /// Lines of body, used for scroll clamping.
    pub line_count: usize,
    pub scroll: u16,
    /// Which worktree the current body belongs to, so we can tell a stale pane
    /// from a fresh one.
    pub loaded_for: Option<PathBuf>,
}

impl Default for Preview {
    fn default() -> Self {
        Self::new()
    }
}

impl Preview {
    pub fn new() -> Self {
        Self {
            tab: PreviewTab::Diff,
            state: PreviewState::Empty,
            body: String::new(),
            line_count: 0,
            scroll: 0,
            loaded_for: None,
        }
    }

    pub fn begin_load(&mut self, path: PathBuf) {
        self.state = PreviewState::Loading;
        self.body.clear();
        self.line_count = 0;
        self.scroll = 0;
        self.loaded_for = Some(path);
    }

    pub fn finish_load(&mut self, body: String) {
        self.line_count = body.lines().count();
        self.body = body;
        self.state = PreviewState::Ready;
    }

    pub fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }

    /// Clamp scroll to the visible height.
    pub fn clamp(&mut self, height: u16) {
        let max = self.line_count.saturating_sub(height as usize) as u16;
        if self.scroll > max {
            self.scroll = max;
        }
    }

    /// The git subcommand and flags for the given tab.
    ///
    /// Deliberately does *not* include the worktree path. The caller supplies it
    /// through [`crate::wt::command::git_command`], which also clears ambient
    /// `GIT_*` variables — keeping `-C <path>` here would be a quiet way to skip
    /// that, and `-C` alone does not override `GIT_DIR`.
    pub fn command(tab: PreviewTab) -> Vec<String> {
        match tab {
            PreviewTab::Diff => vec![
                "diff".to_string(),
                "--stat".to_string(),
                // Colour comes from git so `ansi-to-tui` has something to do,
                // and so `delta` can slot in later as a pager.
                "--color=always".to_string(),
                "HEAD".to_string(),
            ],
            PreviewTab::Log => vec![
                "log".to_string(),
                "--color=always".to_string(),
                "--date=short".to_string(),
                "--pretty=format:%C(auto)%h%d %s (%cr)".to_string(),
                "-n".to_string(),
                "50".to_string(),
            ],
        }
    }
}
