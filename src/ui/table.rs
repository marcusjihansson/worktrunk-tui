//! The worktree table.
//!
//! Status colours and symbols come from worktrunk's own `display` object
//! rather than being recomputed here. Worktrunk has already collapsed the
//! priority rules (conflicts over in-progress operations over prunable over
//! locked, and `^ ∅ _ ⊂ ✗ – ↕ ↑ ↓` for the default-branch relation), so
//! reusing `display.symbols` and `display.state` inherits that logic exactly
//! instead of duplicating it and drifting.

use crate::app::App;
use crate::wt::model::Item;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, Table};

/// Header row. The widths must match `COLUMN_WIDTHS`.
const HEADERS: [&str; 8] = [
    "BRANCH", "STATUS", "MAIN", "REMOTE", "PATH", "CI", "MARKER", "SUBJECT",
];
const COLUMN_WIDTHS: [u16; 8] = [24, 8, 7, 8, 32, 10, 10, 0];

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" worktrunk worktrees ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.visible.is_empty() {
        let msg = if app.rows.is_empty() {
            "no worktrees"
        } else {
            "no rows match the filter — Esc clears it"
        };
        frame.render_widget(ratatui::widgets::Paragraph::new(msg).centered(), inner);
        return;
    }

    // Reserve room for the header so the viewport math excludes it.
    let viewport = inner.height.saturating_sub(1) as usize;
    app.viewport(viewport);

    let first = app.scroll as usize;
    let last = (first + viewport).min(app.visible.len());

    let rows: Vec<Row> = app.visible[first..last]
        .iter()
        .enumerate()
        .map(|(offset, &index)| {
            let item = &app.rows[index];
            let selected = first + offset == app.selected_row();
            build_row(item, selected, app)
        })
        .collect();

    let widths = ratatui::layout::Constraint::from_lengths(COLUMN_WIDTHS);
    let table = Table::new(rows, widths)
        .header(
            Row::new(HEADERS.iter().map(|h| {
                Cell::from(Span::styled(
                    *h,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ))
            }))
            .bottom_margin(0),
        )
        .column_spacing(1);

    frame.render_widget(table, inner);
}

fn build_row<'a>(item: &'a Item, selected: bool, app: &App) -> Row<'a> {
    let worktree = item.worktree.as_ref();
    let base_style = if selected {
        Style::default()
            .bg(Color::Rgb(45, 55, 72))
            .add_modifier(Modifier::BOLD)
    } else if item.is_safe_to_delete() {
        // Worktrunk dims rows it considers safe to delete; match that so the
        // two tools agree about which branches are disposable.
        Style::default().add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };

    let branch_style = if worktree.is_some_and(|w| w.current) {
        base_style.fg(Color::Cyan)
    } else {
        base_style.fg(Color::White)
    };

    let branch_cell = Cell::from(Line::from(vec![
        Span::styled(format!("{} ", current_marker(item)), branch_style),
        Span::styled(item.label(), branch_style),
    ]));

    // `display.symbols` is worktrunk's pre-collapsed status string, e.g. "!↕".
    let status = item
        .display
        .as_ref()
        .and_then(|d| d.symbols.clone())
        .unwrap_or_default();
    let status_cell = Cell::from(Span::styled(status, base_style.fg(symbol_color(item))));

    let main_cell = Cell::from(Line::from(
        item.default_branch
            .as_ref()
            .and_then(|d| d.ahead.zip(d.behind))
            .map(|(ahead, behind)| {
                let ahead = Span::styled(format!("↑{ahead}"), base_style.fg(Color::Green));
                let behind = Span::styled(format!(" ↓{behind}"), base_style.fg(Color::Red));
                vec![ahead, behind]
            })
            .unwrap_or_default(),
    ));

    let remote_cell = Cell::from(Line::from(
        item.upstream
            .as_ref()
            .map(|u| {
                let (ahead, behind) = (u.ahead.unwrap_or(0), u.behind.unwrap_or(0));
                match (ahead > 0, behind > 0) {
                    (true, true) => vec![Span::styled("⇅", base_style.fg(Color::Yellow))],
                    (true, false) => vec![Span::styled("⇡", base_style.fg(Color::Green))],
                    (false, true) => vec![Span::styled("⇣", base_style.fg(Color::Yellow))],
                    (false, false) => vec![Span::styled("|", base_style.fg(Color::DarkGray))],
                }
            })
            .unwrap_or_default(),
    ));

    let path = worktree
        .and_then(|w| w.path.as_deref())
        .map(shorten_home)
        .unwrap_or_else(|| "—".to_string());
    let path_cell = Cell::from(Span::styled(path, base_style.fg(Color::DarkGray)));

    let ci = ci_text(item, app);
    let ci_cell = Cell::from(Span::styled(ci.0, base_style.fg(ci.1)));

    let marker = item.marker.clone().unwrap_or_default();
    let marker_cell = Cell::from(Span::styled(marker, base_style.fg(Color::Magenta)));

    let subject = item
        .head
        .as_ref()
        .and_then(|h| h.subject.clone())
        .unwrap_or_default();
    let subject_cell = Cell::from(Span::styled(subject, base_style.fg(Color::Gray)));

    let mut cells = vec![
        branch_cell,
        status_cell,
        main_cell,
        remote_cell,
        path_cell,
        ci_cell,
        marker_cell,
        subject_cell,
    ];

    // User-defined custom columns render as trailing cells, so per-repo
    // tagging works without wt-tui knowing anything about the configuration.
    for value in custom_column_values(item) {
        cells.push(Cell::from(Span::styled(
            value,
            base_style.fg(Color::DarkGray),
        )));
    }

    Row::new(cells)
}

fn custom_column_values(item: &Item) -> Vec<String> {
    item.display
        .as_ref()
        .map(|d| d.columns.values().cloned().collect())
        .unwrap_or_default()
}

/// `▸` marks the worktree the TUI was launched from; `^` is worktrunk's own
/// marker for the main worktree.
fn current_marker(item: &Item) -> &'static str {
    match item.worktree.as_ref() {
        Some(w) if w.current => "▸",
        _ => " ",
    }
}

/// Colour for the status column: red when something needs attention, yellow
/// while work is in flight, green when the worktree is settled.
fn symbol_color(item: &Item) -> Color {
    let conflicted = item.would_conflict()
        || item
            .worktree
            .as_ref()
            .and_then(|w| w.changes.as_ref())
            .is_some_and(|c| c.conflicted == Some(true));

    if conflicted {
        Color::Red
    } else if item.has_changes() {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// CI cell text and colour.
///
/// The `--full` facts are only present when that load ran, so the cell says so
/// rather than implying a branch has no checks.
fn ci_text(item: &Item, app: &App) -> (String, Color) {
    if let Some(checks) = &item.checks {
        let mark = match checks.status.as_deref() {
            Some("passed") => "✓",
            Some("running") => "◐",
            Some("failed") => "✗",
            _ => "·",
        };
        let pr = item.pr.as_ref().and_then(|p| p.number);
        let text = match pr {
            Some(n) => format!("{mark} #{n}"),
            None => mark.to_string(),
        };
        let color = match checks.status.as_deref() {
            Some("passed") => Color::Green,
            Some("running") => Color::Yellow,
            Some("failed") => Color::Red,
            _ => Color::DarkGray,
        };
        return (text, color);
    }
    if item.pr.is_some() {
        return ("· PR".to_string(), Color::DarkGray);
    }
    // PR/CI facts only arrive with `--full`, and `collected.ci` reports whether
    // the run actually gathered them. Three states must stay distinct, or the
    // column lies: not requested, requested but not collected (no remote
    // forge), and collected with nothing to report.
    if app.fetching && app.ci_attempted {
        ("fetching".to_string(), Color::DarkGray)
    } else if app.collected_ci {
        ("—".to_string(), Color::DarkGray)
    } else if app.ci_attempted {
        // Requested, but worktrunk had no forge to ask (no remote, or no `gh`).
        ("no forge".to_string(), Color::DarkGray)
    } else {
        ("".to_string(), Color::DarkGray)
    }
}

/// `~/…` for paths under the home directory, which is most of them.
fn shorten_home(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}
