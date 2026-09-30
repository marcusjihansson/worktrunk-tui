//! Keybinding help overlay.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

const KEYS: &[(&str, &str)] = &[
    ("↑ ↓  j k", "move selection"),
    ("g / G", "first / last row"),
    ("Tab", "switch focus between table and preview"),
    ("↑ ↓  (preview)", "scroll the preview pane"),
    ("t", "cycle preview tab (diff / log)"),
    ("/", "filter (live; Enter commits, Esc clears)"),
    ("n", "create a worktree for a new branch"),
    ("d", "remove the selected worktree"),
    ("m", "merge the selected branch into the default branch"),
    ("P", "prune branches that are already integrated"),
    ("y", "copy branch name"),
    ("c", "copy worktree path"),
    ("e", "open the worktree in $EDITOR"),
    ("o", "open the pull request in a browser"),
    ("S", "fetch PR and CI status"),
    ("", "(needs a remote forge and gh/glab)"),
    ("r", "refresh now"),
    ("", ""),
    ("— search —", ""),
    ("s", "toggle search across worktrees"),
    ("/", "edit the query (Enter runs it)"),
    ("j / k", "move between results"),
    ("f", "search only the rows passing the filter"),
    ("i / I", "case-insensitive search on / off"),
    ("r / R", "regex mode on / off"),
    ("Enter", "re-run the current query"),
    ("s", "back to the worktree table"),
    ("", ""),
    ("?", "toggle this help"),
    ("q", "quit"),
];

pub fn render(frame: &mut Frame, area: Rect) {
    let width = 60.min(area.width.saturating_sub(4));
    let height = (KEYS.len() as u16 + 4).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" wt-tui keys ")
        .title_bottom(" any key closes ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(k, desc)| {
            Line::from(vec![
                Span::styled(format!("{k:<12}"), key_style),
                Span::styled(*desc, Style::default().fg(Color::Gray)),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}
