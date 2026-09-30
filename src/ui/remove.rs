//! The removal confirmation dialog.
//!
//! This is where the schema-2 payload earns its keep. Rather than a bare
//! "are you sure?", the dialog shows *why* a branch is or is not safe, read
//! straight from the item: uncommitted work, integration status, ahead/behind,
//! and whether a merge would conflict. That turns a scary irreversible action
//! into an informed one.

use crate::app::{Mode, NoticeKind};
use crate::wt::model::Item;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

pub fn render(frame: &mut Frame, area: Rect, app: &crate::app::App) {
    let Mode::ConfirmRemove {
        branch,
        force,
        dirty,
    } = &app.mode
    else {
        return;
    };

    let item = app.selected_item();
    let width = 66.min(area.width.saturating_sub(4));
    let height = 18.min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if *force { Color::Red } else { Color::Yellow }))
        .title(format!(" remove {branch} "));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines = Vec::new();
    if let Some(item) = item {
        lines.extend(facts(item));
        // A detached worktree has no branch, so `wt remove` needs the path.
        if item.branch.is_none() {
            lines.push(Line::from(Span::styled(
                "detached HEAD — only the worktree can be removed",
                Style::default().fg(Color::Yellow),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        if *force {
            "⚠  --force: uncommitted changes WILL be discarded"
        } else {
            "uncommitted changes will block removal unless forced"
        },
        Style::default().fg(if *force { Color::Red } else { Color::DarkGray }),
    )));
    if *dirty && !*force {
        lines.push(Line::from(Span::styled(
            "this worktree has uncommitted changes",
            Style::default().fg(Color::Yellow),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            " y",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" / "),
        Span::styled(
            "Enter",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" remove    "),
        Span::styled(
            "f",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" toggle force    "),
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
    lines.push(Line::from(Span::styled(
        "the branch is kept unless it is safe to delete, or you force it with -D",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

/// The evidence, straight from the item.
fn facts(item: &Item) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let plain = Style::default().fg(Color::Gray);
    let label = Style::default().fg(Color::DarkGray);

    if let Some(w) = &item.worktree {
        if let Some(p) = &w.path {
            out.push(Line::from(vec![
                Span::styled("path      ", label),
                Span::styled(p.clone(), plain),
            ]));
        }
    }

    if let Some(changes) = item.worktree.as_ref().and_then(|w| w.changes.as_ref()) {
        let mut flags = Vec::new();
        for (on, name) in [
            (changes.staged, "staged"),
            (changes.modified, "modified"),
            (changes.untracked, "untracked"),
            (changes.renamed, "renamed"),
            (changes.deleted, "deleted"),
            (changes.conflicted == Some(true), "conflicted"),
        ] {
            if on {
                flags.push(name);
            }
        }
        let (text, color) = if flags.is_empty() {
            ("clean".to_string(), Color::Green)
        } else {
            (flags.join(", "), Color::Yellow)
        };
        out.push(Line::from(vec![
            Span::styled("changes   ", label),
            Span::styled(text, Style::default().fg(color)),
        ]));
    }

    // The integration verdict, phrased from the item's own state so the user
    // sees worktrunk's reasoning rather than a bare yes/no.
    let (verdict, verdict_style) = if item.is_integrated() {
        ("yes", Style::default().fg(Color::Green))
    } else {
        ("no", Style::default().fg(Color::Yellow))
    };
    out.push(Line::from(vec![
        Span::styled("integrated", label),
        Span::styled(
            format!("{verdict} — {}", item.removal_reason()),
            verdict_style,
        ),
    ]));

    if let Some(db) = &item.default_branch {
        if let (Some(ahead), Some(behind)) = (db.ahead, db.behind) {
            out.push(Line::from(vec![
                Span::styled("vs default ", label),
                Span::styled(format!("↑{ahead} ↓{behind}"), plain),
            ]));
        }
        if db.merge_conflicts == Some(true) {
            out.push(Line::from(vec![
                Span::styled("merge      ", label),
                Span::styled("would conflict", Style::default().fg(Color::Red)),
            ]));
        }
    }

    if let Some(w) = &item.worktree {
        if w.operation.is_some() {
            out.push(Line::from(vec![
                Span::styled("operation  ", label),
                Span::styled(
                    format!("{} in progress", w.operation.as_deref().unwrap_or("git")),
                    Style::default().fg(Color::Red),
                ),
            ]));
        }
        if w.duplicate_branch {
            out.push(Line::from(vec![
                Span::styled("note       ", label),
                Span::styled(
                    "branch is checked out in more than one worktree",
                    Style::default().fg(Color::Yellow),
                ),
            ]));
        }
    }

    out
}

/// A one-line summary of a completed removal, in worktrunk's own vocabulary.
///
/// `branch_outcome` exists so a caller can tell a deletion the removal
/// *declined* from one it was never asked to make, so the wording follows it
/// rather than guessing.
pub fn removal_summary(result: &crate::wt::model::RemoveResult) -> (String, NoticeKind) {
    use crate::wt::model::BranchOutcome;

    let branch = result.branch.clone().unwrap_or_else(|| "branch".into());

    match result.branch_outcome.clone() {
        // A removed branch is a success; a retained one is not, even though
        // the command exited cleanly.
        Some(outcome) if outcome.removed_branch() => (
            format!("removed {branch} — {}", outcome.explain()),
            NoticeKind::Info,
        ),
        Some(outcome @ BranchOutcome::Deferred) => {
            (format!("{branch}: {}", outcome.explain()), NoticeKind::Info)
        }
        Some(outcome) => (
            format!("{branch}: {}", outcome.explain()),
            NoticeKind::Error,
        ),
        None => (format!("removed {branch}"), NoticeKind::Info),
    }
}
