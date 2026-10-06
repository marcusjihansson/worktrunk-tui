//! The status bar's key hints: what it promises, and what must survive a
//! terminal too narrow to show everything.
//!
//! The line is a single unclipped row, so its content is a function of the
//! width it is given. These tests pin both halves: the hints that must always
//! be present, and the guarantee that the line never exceeds the space it was
//! given, because ratatui truncates rather than wraps and a wrapped status bar
//! would push the table up a row.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;
use wt_tui::app::{App, Pane, View};
use wt_tui::ui;

/// Render the status bar at `width` and read the single row back as text.
fn status_line(app: &App, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 1)).expect("terminal");
    terminal
        .draw(|frame| ui::status::render(frame, frame.area(), app))
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    (0..width)
        .map(|x| {
            buffer
                .cell((x, 0))
                .map_or(" ", |cell| cell.symbol())
                .to_string()
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn app() -> App {
    // A fresh App is in Normal mode with no notice, which is the state in which
    // the hint line is the status bar's whole content.
    App::new(PathBuf::from("/repo"))
}

#[test]
fn the_table_hints_offer_switching_and_quitting() {
    let line = status_line(&app(), 120);
    assert!(
        line.contains("Enter:switch"),
        "the primary action must be on the status bar: {line}"
    );
    assert!(line.contains("q:quit"), "quit must be discoverable: {line}");
    assert!(
        line.contains("n:new") && line.contains("?:keys"),
        "the existing hints must survive: {line}"
    );
}

/// Hints are separated, and the gap has to be drawn as well as measured: a
/// budget that charges for a separator the line never emits runs the hints
/// together into one unreadable word.
#[test]
fn adjacent_hints_are_separated() {
    let line = status_line(&app(), 120);
    assert!(
        line.contains("Enter:switch  q:quit"),
        "hints must be separated on screen: {line:?}"
    );
    assert!(
        !line.contains("switchq"),
        "no hint may run into the next: {line:?}"
    );
}

/// The reason the hints are ordered by priority: the two keys whose absence
/// would strand someone have to be leftmost, since the right edge is what gets
/// cut on a narrow terminal.
#[test]
fn switch_and_quit_survive_a_narrow_terminal() {
    let line = status_line(&app(), 80);
    assert!(
        line.chars().count() <= 80,
        "the line must not overflow its row: {line:?}"
    );
    assert!(
        line.contains("Enter:switch"),
        "switching must stay visible at 80 columns: {line}"
    );
    assert!(
        line.contains("q:quit"),
        "the way out must stay visible at 80 columns: {line}"
    );
}

/// Width is the terminal's, not ours, so the line has to degrade rather than
/// overflow: a hint that does not fit is dropped, not truncated mid-word.
#[test]
fn hints_are_dropped_rather_than_overflowed() {
    for width in [12u16, 20, 30, 45, 60, 80, 100, 200] {
        let line = status_line(&app(), width);
        assert!(
            line.chars().count() <= width as usize,
            "at {width} columns the line overflowed: {line:?}"
        );
    }
}

/// The prefixes are state, not hints: they describe what the view is doing
/// right now, so they are laid out first and the hints take what remains.
#[test]
fn state_prefixes_are_charged_against_the_hints() {
    let mut with_filter = app();
    with_filter.filter = "a-fairly-long-filter-string".to_string();

    let line = status_line(&with_filter, 60);
    assert!(
        line.chars().count() <= 60,
        "the filter must not push the line past its row: {line:?}"
    );
    assert!(
        line.contains("a-fairly-long-filter-string"),
        "the filter is state and must be shown even when a hint is dropped: {line}"
    );
    assert!(
        line.contains("Enter:switch"),
        "the leading hint still fits after a 26-character filter: {line}"
    );
}

#[test]
fn the_preview_prefix_also_costs_room() {
    let mut focused = app();
    focused.pane = Pane::Preview;

    let line = status_line(&focused, 80);
    assert!(line.chars().count() <= 80, "overflowed: {line:?}");
    assert!(
        line.contains("preview focused"),
        "the pane indicator is state: {line}"
    );
    assert!(line.contains("Enter:switch"), "the hint still fits: {line}");
}

/// Enter re-runs the query in the search view, so offering a switch there would
/// be a lie about what the key does.
#[test]
fn the_search_view_does_not_offer_a_switch() {
    let mut searching = app();
    searching.view = View::Search;
    searching.search.query = "auth".to_string();

    let line = status_line(&searching, 120);
    assert!(
        !line.contains("switch"),
        "Enter re-runs the query in this view, so it must not say switch: {line}"
    );
}

/// A notice replaces the hints, which is how a failure like "could not switch"
/// is reported. It has to fit too.
#[test]
fn a_notice_replaces_the_hints_and_still_fits() {
    let mut busy = app();
    busy.set_notice(
        "could not switch to feature: a git operation is already in progress",
        wt_tui::app::NoticeKind::Error,
    );

    let line = status_line(&busy, 80);
    assert!(
        line.chars().count() <= 80,
        "a notice must not overflow its row: {line:?}"
    );
    assert!(
        line.contains("could not switch"),
        "the reason must survive: {line}"
    );
    assert!(
        !line.contains("Enter:switch"),
        "the hints make way for the notice: {line}"
    );
}