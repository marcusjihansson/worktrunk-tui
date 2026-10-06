//! The status bar, plus the banner shown while typing or running a command.

use crate::app::{App, Mode, NoticeKind, Pane};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let prompt = match &app.mode {
        Mode::Filtering(buffer) => Some(format!("filter: {buffer}▏")),
        Mode::Creating(buffer) => Some(format!("new branch: {buffer}▏")),
        Mode::ConfirmRemove { branch, force, .. } => Some(format!(
            "remove {branch}{}?  (y/n, f force)",
            if *force { " [FORCED]" } else { "" }
        )),
        Mode::Searching(buffer) => Some(format!("search: {buffer}▏")),
        Mode::ConfirmMerge {
            branch,
            target,
            keep_worktree,
            ..
        } => Some(format!(
            "merge {branch} → {target}{}?  (y/n, w keep worktree)",
            if *keep_worktree {
                " [keep worktree]"
            } else {
                ""
            }
        )),
        Mode::ConfirmPrune {
            candidates,
            min_age,
        } => Some(format!(
            "prune {} candidate(s) at min-age {min_age}?  (y/n, a cycle)",
            candidates.len()
        )),
        Mode::Busy => Some("running…".to_string()),
        Mode::Normal => None,
    };

    let style = match app.mode {
        Mode::ConfirmRemove { .. } | Mode::ConfirmPrune { .. } => {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
        Mode::ConfirmMerge { .. } => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        Mode::Busy => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::Cyan),
    };

    let line = match prompt {
        Some(text) => Line::from(Span::styled(text, style)),
        None => match &app.notice {
            Some(notice) => Line::from(Span::styled(
                notice.text.clone(),
                Style::default().fg(if notice.kind == NoticeKind::Error {
                    Color::Red
                } else {
                    Color::Gray
                }),
            )),
            None => default_line(app, area.width),
        },
    };

    frame.render_widget(Paragraph::new(line), area);
}

/// Gap between two hints, chosen to read as a column at small sizes.
const SEPARATOR: &str = "  ";

/// The worktree table's key hints, most important first, with whether each is
/// drawn in the accent colour.
///
/// Order is priority, because the line is clipped at the right edge on a narrow
/// terminal and whatever sits rightmost is what disappears. `Enter` leads since
/// switching is the table's primary action, and `q` follows immediately because
/// losing the way out is worse than losing any of the verbs behind `?` — a
/// user who cannot see `q` has to guess or interrupt the process.
const HINTS: &[(&str, bool)] = &[
    ("Enter:switch", true),
    ("q:quit", false),
    ("?:keys", false),
    ("/:filter", false),
    ("n:new", false),
    ("d:remove", false),
    ("m:merge", false),
    ("P:prune", false),
    ("s:search", false),
];

fn default_line(app: &App, width: u16) -> Line<'static> {
    // The search view's status line reports the search itself; a results
    // summary is more useful there than a list of keybindings. It also cannot
    // offer a switch, since Enter there re-runs the query.
    if app.view == crate::app::View::Search {
        return search_line(app);
    }

    let hint = Style::default().fg(Color::DarkGray);
    let accent = Style::default().fg(Color::Cyan);

    let mut spans = Vec::new();
    if app.fetching {
        spans.push(Span::styled("⟳ ", accent));
    }
    if !app.filter.is_empty() {
        spans.push(Span::styled(format!("/{} ", app.filter), accent));
        spans.push(Span::styled("· ", hint));
    }
    if app.pane == Pane::Preview {
        spans.push(Span::styled("preview focused · ", hint));
    }

    // Everything so far is state rather than a hint, so it is never dropped:
    // the whole point of the prefixes is to say what the view is currently
    // doing. They are laid out first so the hints get whatever room is left.
    let mut line = Line::from(spans);
    let mut room = (width as usize).saturating_sub(line.width());

    for (i, (text, accented)) in HINTS.iter().enumerate() {
        let needed = text.chars().count() + if i == 0 { 0 } else { SEPARATOR.len() };
        // Stopping at the first hint that does not fit keeps the remainder
        // from wrapping into the next row, which would push the table up.
        if needed > room {
            break;
        }
        room -= needed;
        // The gap is part of the same budget as the hint, so it is emitted here
        // rather than folded into the text: measured without being drawn, the
        // hints run together and read as one run-on word.
        if i > 0 {
            line.spans.push(Span::styled(SEPARATOR, hint));
        }
        let style = if *accented { accent } else { hint };
        line.spans.push(Span::styled(*text, style));
    }

    line
}

/// The search status line: the query, its modifiers, and the result summary.
fn search_line(app: &App) -> Line<'static> {
    let hint = Style::default().fg(Color::DarkGray);
    let accent = Style::default().fg(Color::Cyan);
    let search = &app.search;

    let mut spans = vec![Span::styled(format!("/{} ", search.query), accent)];

    // The active modifiers, so it is obvious why a search matched nothing.
    if search.options.case_insensitive {
        spans.push(Span::styled("i ", Style::default().fg(Color::Yellow)));
    }
    if search.options.force_regex {
        spans.push(Span::styled("re ", Style::default().fg(Color::Yellow)));
    }
    if search.only_filtered {
        spans.push(Span::styled(
            "filtered ",
            Style::default().fg(Color::Yellow),
        ));
    }
    spans.push(Span::styled("· ", hint));

    if search.running {
        spans.push(Span::styled("searching…", accent));
    } else if search.query.is_empty() {
        spans.push(Span::styled("type a query, then Enter to search", hint));
    } else {
        spans.push(Span::styled(
            crate::ui::search_view::summary(&search.results, search.elapsed),
            Style::default().fg(Color::Gray),
        ));
    }

    Line::from(spans)
}

/// A full-screen message for problems the user must fix, with instructions.
pub fn render_fatal(frame: &mut Frame, area: Rect, message: &str) {
    let width = 78.min(area.width.saturating_sub(4));
    let lines: Vec<Line> = message.lines().map(|l| Line::from(l.to_string())).collect();
    let height = (lines.len() as u16 + 4).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    frame.render_widget(ratatui::widgets::Clear, popup);
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" wt-tui cannot start ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .style(Style::default().fg(Color::White)),
        inner,
    );
}

/// A full-screen loading state for the very first load.
pub fn render_loading(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "loading worktrees…",
            Style::default().fg(Color::DarkGray),
        )))
        .centered(),
        area,
    );
}
