//! Preview pane rendering: tab bar plus ANSI body.

use crate::app::App;
use crate::ui::preview::{PreviewState, PreviewTab};
use ansi_to_tui::IntoText;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let tabs = PreviewTab::ALL
        .iter()
        .map(|t| ratatui::text::Span::raw(t.label()))
        .collect::<Vec<_>>();
    let active = match app.preview.tab {
        PreviewTab::Diff => 0,
        PreviewTab::Log => 1,
    };
    let title = app
        .selected_item()
        .map(|i| format!(" {} ", i.label()))
        .unwrap_or_else(|| " preview ".to_string());

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(" t:tab  ↑↓:scroll ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let tab_row = Tabs::new(tabs)
        .select(active)
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::UNDERLINED),
        )
        .divider("·");
    let tab_area = Rect { height: 1, ..inner };
    frame.render_widget(tab_row, tab_area);

    let body_area = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    if body_area.height == 0 {
        return;
    }
    app.preview.clamp(body_area.height);

    let text = match app.preview.state {
        PreviewState::Empty => "no worktree selected".to_string(),
        PreviewState::Loading => "loading…".to_string(),
        PreviewState::Ready => app.preview.body.clone(),
    };

    // git emits colour; `ansi-to-tui` turns it into styled text so the pane
    // shows the same colours a pager would.
    let lines: Vec<Line> = match text.into_text() {
        Ok(t) => t.lines,
        Err(_) => vec![Line::from("could not render preview")],
    };

    let total = lines.len();
    let height = body_area.height as usize;
    let start = (app.preview.scroll as usize).min(total.saturating_sub(height));
    let end = (start + height).min(total);

    frame.render_widget(Paragraph::new(lines[start..end].to_vec()), body_area);
}
