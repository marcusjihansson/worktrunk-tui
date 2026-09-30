//! Behavioural tests for selection, filtering, and the input modes.
//!
//! These cover what is hard to confirm by looking at a terminal: that a refresh
//! keeps the cursor on the same branch, and that typed input accumulates rather
//! than replacing itself.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use wt_tui::app::{App, Mode};
use wt_tui::wt::model::{Changes, Envelope, Head, Item, Worktree};

fn worktree_item(branch: &str, path: &str, current: bool) -> Item {
    Item {
        branch: Some(branch.to_string()),
        worktree: Some(Worktree {
            path: Some(path.to_string()),
            current,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn envelope(items: Vec<Item>) -> Envelope {
    Envelope {
        schema: 2,
        items,
        ..Default::default()
    }
}

fn app_with(items: Vec<Item>) -> App {
    let mut app = App::new(std::path::PathBuf::from("/repo"));
    app.apply(envelope(items), Some("main".to_string()));
    app
}

#[test]
fn rows_are_all_visible_before_filtering() {
    let app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("feature", "/repo.feature", false),
    ]);
    assert_eq!(app.visible.len(), 2);
}

#[test]
fn a_refresh_keeps_the_cursor_on_the_same_branch() {
    let mut app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("feature", "/repo.feature", false),
    ]);

    // Move to the second row.
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.selected_item().unwrap().label(), "feature");

    // An agent creates a worktree that sorts before `feature`.
    app.apply(
        envelope(vec![
            worktree_item("main", "/repo", true),
            worktree_item("aaa-new", "/repo.aaa-new", false),
            worktree_item("feature", "/repo.feature", false),
        ]),
        Some("main".to_string()),
    );

    // The cursor must still be on `feature`, not shifted onto the new row.
    assert_eq!(
        app.selected_item().unwrap().label(),
        "feature",
        "selection must follow the branch, not the index"
    );
}

#[test]
fn a_removed_selection_clamps_to_a_valid_row() {
    let mut app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("a", "/repo.a", false),
        worktree_item("b", "/repo.b", false),
    ]);
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.selected_item().unwrap().label(), "b");

    // `b` disappears.
    app.apply(
        envelope(vec![
            worktree_item("main", "/repo", true),
            worktree_item("a", "/repo.a", false),
        ]),
        Some("main".to_string()),
    );

    assert!(app.selected_item().is_some(), "selection must never dangle");
    assert!(
        app.selected_row() < app.visible.len(),
        "selection must stay in range"
    );
}

#[test]
fn typing_a_filter_accumulates_characters() {
    let mut app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("alpha", "/repo.alpha", false),
        worktree_item("beta", "/repo.beta", false),
    ]);

    app.on_key(key(KeyCode::Char('/')));
    for c in "alp".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }

    assert_eq!(app.filter, "alp", "each keystroke must extend the buffer");
    assert_eq!(app.visible.len(), 1);
    assert_eq!(app.selected_item().unwrap().label(), "alpha");
}

#[test]
fn escape_clears_the_filter_and_restores_rows() {
    let mut app = app_with(vec![
        worktree_item("alpha", "/repo.alpha", false),
        worktree_item("zulu", "/repo.zulu", false),
    ]);
    app.on_key(key(KeyCode::Char('/')));
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.visible.len(), 1, "'a' matches alpha but not zulu");

    app.on_key(key(KeyCode::Esc));
    assert!(app.filter.is_empty());
    assert_eq!(app.visible.len(), 2);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn backspace_shortens_the_filter() {
    let mut app = app_with(vec![
        worktree_item("alpha", "/repo.alpha", false),
        worktree_item("zulu", "/repo.zulu", false),
    ]);
    app.on_key(key(KeyCode::Char('/')));
    for c in "alpb".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    assert_eq!(app.filter, "alpb");
    assert_eq!(app.visible.len(), 0, "no branch matches 'alpb'");

    app.on_key(key(KeyCode::Backspace));
    assert_eq!(app.filter, "alp");
    assert_eq!(app.visible.len(), 1);
}

#[test]
fn creating_accumulates_the_branch_name_and_sends_it_on_enter() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);

    app.on_key(key(KeyCode::Char('n')));
    for c in "my-feature".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    assert_eq!(app.pending.create, None, "nothing runs before Enter");

    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.pending.create.as_deref(), Some("my-feature"));
}

#[test]
fn escape_abandons_a_half_typed_branch_name() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.on_key(key(KeyCode::Char('n')));
    for c in "oops".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.pending.create, None);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn confirming_a_removal_queues_the_work_with_the_dirty_flag() {
    let mut item = worktree_item("feature", "/repo.feature", false);
    let changes = Changes {
        modified: true,
        ..Default::default()
    };
    item.worktree.as_mut().unwrap().changes = Some(changes);

    let mut app = app_with(vec![worktree_item("main", "/repo", true), item]);
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('d')));

    match &app.mode {
        Mode::ConfirmRemove {
            branch,
            force,
            dirty,
        } => {
            assert_eq!(branch, "feature");
            assert!(!force);
            assert!(dirty, "the dialog needs to know the worktree is dirty");
        }
        other => panic!("expected a confirm dialog, got {other:?}"),
    }

    // 'f' toggles force.
    app.on_key(key(KeyCode::Char('f')));
    match &app.mode {
        Mode::ConfirmRemove { force, .. } => assert!(force),
        other => panic!("expected a confirm dialog, got {other:?}"),
    }

    app.on_key(key(KeyCode::Char('y')));
    let (branch, force) = app.pending.remove.clone().expect("removal queued");
    assert_eq!(branch, "feature");
    assert!(force, "force should carry through from the toggle");
}

#[test]
fn declining_a_removal_queues_nothing() {
    let mut app = app_with(vec![worktree_item("feature", "/repo.feature", false)]);
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('n')));
    assert_eq!(app.pending.remove, None);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn movement_wraps_at_both_ends() {
    let mut app = app_with(vec![
        worktree_item("a", "/repo.a", false),
        worktree_item("b", "/repo.b", false),
    ]);
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(
        app.selected_item().unwrap().label(),
        "b",
        "up from the top wraps to the end"
    );
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(
        app.selected_item().unwrap().label(),
        "a",
        "down from the end wraps to the top"
    );
}

#[test]
fn the_preview_tab_cycles() {
    let mut app = app_with(vec![worktree_item("a", "/repo.a", false)]);
    let start = app.preview.tab;
    app.on_key(key(KeyCode::Char('t')));
    assert_ne!(app.preview.tab, start);
    assert!(
        app.pending.preview_stale,
        "the new tab's body must be re-fetched"
    );
}

#[test]
fn filter_matches_path_and_subject_not_just_branch() {
    let mut with_subject = worktree_item("main", "/repo", true);
    with_subject.head = Some(Head {
        subject: Some("fix the login page".into()),
        ..Default::default()
    });

    let app = app_with(vec![
        with_subject,
        worktree_item("other", "/repo.other", false),
    ]);
    assert_eq!(app.rows.len(), 2);

    // The filter helper is exercised through the live path below.
    let mut live = app;
    live.on_key(key(KeyCode::Char('/')));
    for c in "login".chars() {
        live.on_key(key(KeyCode::Char(c)));
    }
    assert_eq!(
        live.visible.len(),
        1,
        "a commit subject should be searchable"
    );
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
