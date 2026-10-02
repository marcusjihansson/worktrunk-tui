//! The merge confirmation dialog.
//!
//! Merge is the highest-blast-radius action in wt-tui, so the dialog leads with
//! what is about to happen and refuses to imply more certainty than worktrunk
//! provides.

use crate::app::Mode;
use crate::wt::model::Item;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

pub fn render(frame: &mut Frame, area: Rect, app: &crate::app::App) {
    let Mode::ConfirmMerge {
        branch,
        target,
        worktree,
        keep_worktree,
        ..
    } = &app.mode
    else {
        return;
    };

    let width = 70.min(area.width.saturating_sub(4));
    let height = 19.min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(format!(" merge {branch} → {target} "));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines = Vec::new();
    // Facts come from the captured worktree, not the live selection: if the
    // confirmed row is gone, show nothing rather than another branch's facts.
    match app.item_for_worktree(worktree) {
        Some(item) => lines.extend(facts(item, target, *keep_worktree)),
        None => lines.push(Line::from(Span::styled(
            "this worktree is no longer listed — its state is unknown",
            Style::default().fg(Color::Yellow),
        ))),
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "runs pre-merge hooks, then fast-forwards the target",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled(
        "a conflicting merge is left for you to resolve in the worktree",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            " y",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" merge    "),
        Span::styled(
            "w",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" keep worktree    "),
        Span::styled(
            "n",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" / "),
        Span::styled(
            "Esc",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" cancel"),
    ]));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn facts(item: &Item, target: &str, keep_worktree: bool) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let label = Style::default().fg(Color::DarkGray);
    let plain = Style::default().fg(Color::Gray);

    out.push(Line::from(vec![
        Span::styled("source     ", label),
        Span::styled(item.label(), plain),
    ]));
    out.push(Line::from(vec![
        Span::styled("target     ", label),
        Span::styled(target.to_string(), Style::default().fg(Color::Cyan)),
    ]));

    if let Some(db) = &item.default_branch {
        if let (Some(ahead), Some(behind)) = (db.ahead, db.behind) {
            out.push(Line::from(vec![
                Span::styled("vs target  ", label),
                Span::styled(
                    format!("↑{ahead} ↓{behind}"),
                    if ahead > 0 {
                        Style::default().fg(Color::Green)
                    } else {
                        plain
                    },
                ),
            ]));
        }
        if db.merge_conflicts == Some(true) {
            out.push(Line::from(vec![
                Span::styled("conflict   ", label),
                Span::styled("merging would conflict", Style::default().fg(Color::Red)),
            ]));
        }
    }

    if item.has_changes() {
        out.push(Line::from(vec![
            Span::styled("changes    ", label),
            Span::styled(
                "uncommitted changes will be committed first",
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }

    if let Some(changes) = item.worktree.as_ref().and_then(|w| w.changes.as_ref()) {
        if changes.conflicted == Some(true) {
            out.push(Line::from(vec![
                Span::styled("conflict   ", label),
                Span::styled(
                    "worktree has unresolved conflicts",
                    Style::default().fg(Color::Red),
                ),
            ]));
        }
    }

    out.push(Line::from(vec![
        Span::styled("afterwards ", label),
        Span::styled(
            if keep_worktree {
                "the worktree and branch are kept"
            } else {
                "the worktree and branch are removed"
            },
            plain,
        ),
    ]));

    out
}
