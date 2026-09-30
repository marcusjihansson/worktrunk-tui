//! The prune dialog: what `wt step prune` would remove, and why.
//!
//! Prune is bulk deletion, so it is never a single keystroke. The list comes
//! from `--dry-run`, which runs exactly the same selection criteria as the real
//! thing, so what is shown is what will happen.

use crate::app::Mode;
use crate::wt::command::PruneCandidate;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

pub fn render(frame: &mut Frame, area: Rect, app: &crate::app::App) {
    let (candidates, min_age) = match &app.mode {
        Mode::ConfirmPrune {
            candidates,
            min_age,
        } => (candidates, min_age),
        _ => return,
    };

    let width = 76.min(area.width.saturating_sub(4));
    let height = (candidates.len() as u16 + 10).min(area.height.saturating_sub(2));
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
        .title(format!(" prune {} candidate(s) ", candidates.len()));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let label = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();

    if candidates.is_empty() {
        lines.push(Line::from(Span::styled(
            "nothing is eligible for removal",
            Style::default().fg(Color::Green),
        )));
        lines.push(Line::from(Span::styled(
            "every branch has commits the default branch does not, or a worktree has uncommitted changes",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for candidate in candidates.iter() {
            let name = candidate
                .path
                .as_deref()
                .and_then(|p| p.rsplit('/').next())
                .unwrap_or("(unknown)");
            let reason = candidate.reason.as_deref().unwrap_or("integrated");
            lines.push(Line::from(vec![
                Span::styled(format!("  {name}"), Style::default().fg(Color::White)),
                Span::styled(
                    format!("  — {reason}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" min-age ", label),
        Span::styled(
            min_age.to_string(),
            Style::default().fg(if min_age == "0" {
                Color::Yellow
            } else {
                Color::Gray
            }),
        ),
    ]));
    if min_age == "0" {
        lines.push(Line::from(Span::styled(
            "  0 removes worktrees created moments ago — a fresh branch off the default branch looks merged",
            Style::default().fg(Color::Yellow),
        )));
    }
    lines.push(Line::from(Span::styled(
        "  the main worktree, locked worktrees, and dirty worktrees are always skipped",
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
        Span::raw(" prune all listed    "),
        Span::styled(
            "a",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" toggle min-age (0 / 1d / 7d)    "),
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

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// A short description of a candidate for the status bar.
pub fn describe(candidates: &[PruneCandidate]) -> String {
    match candidates.len() {
        0 => "nothing to prune".to_string(),
        1 => "1 candidate".to_string(),
        n => format!("{n} candidates"),
    }
}
