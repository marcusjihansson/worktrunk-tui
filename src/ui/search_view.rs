//! The search view: query input, results, and the matching file's content.

use crate::app::App;
use crate::search::Results;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Render the results list.
///
/// Hits are grouped by file so a file with twelve matches reads as one thing
/// rather than twelve, and each group carries the worktrees that hold it.
pub fn render_results(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title(" results ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let results = &app.search.results;

    if let Some(error) = &results.error {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("invalid query: {error}"),
                Style::default().fg(Color::Red),
            ))),
            inner,
        );
        return;
    }

    if results.is_empty() {
        let message = if app.search.query.is_empty() {
            "type a query to search every worktree"
        } else {
            "no matches"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                message,
                Style::default().fg(Color::DarkGray),
            ))),
            inner,
        );
        return;
    }

    // Group consecutive hits by file, preserving the order results arrived in.
    let mut lines: Vec<Line> = Vec::new();
    let mut current_file: Option<&str> = None;
    let selected_file = app
        .selected_hit()
        .map(|h| (h.rel_path.as_str(), h.line_number));

    for hit in &results.hits {
        if current_file != Some(hit.rel_path.as_str()) {
            current_file = Some(&hit.rel_path);
            let badges = if hit.worktrees.len() > 1 {
                format!("  ×{}", hit.worktrees.len())
            } else {
                String::new()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    hit.rel_path.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(badges, Style::default().fg(Color::DarkGray)),
            ]));
        }

        let is_selected = selected_file == Some((hit.rel_path.as_str(), hit.line_number));
        let style = if is_selected {
            Style::default().bg(Color::Rgb(45, 55, 72))
        } else {
            Style::default()
        };
        let marker = if hit.reach() > 1 { '≡' } else { ' ' };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:>5} ", hit.line_number),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(format!("{marker} "), style.fg(Color::DarkGray)),
            Span::styled(hit.line.trim_end().to_string(), style.fg(Color::Gray)),
        ]));
    }

    frame.render_widget(Paragraph::new(lines).scroll((app.search.scroll, 0)), inner);
}

/// Render the file containing the selected hit, with the matching line marked.
pub fn render_match_context(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(match app.selected_hit() {
            Some(hit) => format!(" {}:{} ", hit.rel_path, hit.line_number),
            None => " no match selected ".to_string(),
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(hit) = app.selected_hit() else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "select a result to see the file",
                Style::default().fg(Color::DarkGray),
            ))),
            inner,
        );
        return;
    };

    let body = app.search.context_body.clone();
    if body.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "loading…",
                Style::default().fg(Color::DarkGray),
            ))),
            inner,
        );
        return;
    }

    let lines: Vec<Line> = body
        .lines()
        .enumerate()
        .map(|(idx, text)| {
            let number = idx as u64 + 1;
            let is_match = number == hit.line_number;
            let style = if is_match {
                Style::default().bg(Color::Rgb(45, 55, 72)).fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(vec![
                Span::styled(
                    format!("{number:>5} "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(text.to_string(), style),
            ])
        })
        .collect();

    // Scroll so the match is roughly centred, which is where the eye goes.
    let height = inner.height as usize;
    let target = (hit.line_number as usize)
        .saturating_sub(height / 2)
        .max(hit.line_number as usize - height + 1);
    frame.render_widget(Paragraph::new(lines).scroll((target as u16, 0)), inner);
}

/// A one-line summary for the status bar.
pub fn summary(results: &Results, elapsed: Option<std::time::Duration>) -> String {
    if results.is_empty() {
        let searched = results.covered();
        return if searched == 0 {
            "no worktrees to search".to_string()
        } else {
            format!("no matches across {searched} worktree{}", plural(searched))
        };
    }

    let mut text = format!(
        "{} match{} in {} file{} across {} worktree{}",
        results.hits.len(),
        plural(results.hits.len()),
        results.file_count(),
        plural(results.file_count()),
        results.covered(),
        plural(results.covered()),
    );
    if results.deduped > 0 {
        text.push_str(&format!(
            " ({} identical tree{} searched once)",
            results.deduped,
            plural(results.deduped)
        ));
    }
    if let Some(elapsed) = elapsed {
        text.push_str(&format!(" in {}ms", elapsed.as_millis()));
    }
    if results.truncated {
        text.push_str(" — truncated");
    }
    text
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
