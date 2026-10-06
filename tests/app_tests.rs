//! Behavioural tests for selection, filtering, input modes, and the new
//! search, merge, and prune flows.
//!
//! These cover what is hard to confirm by looking at a terminal: that a refresh
//! keeps the cursor on the same branch, that typed input accumulates rather than
//! replacing itself, and that the dangerous actions queue the *right* work.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;
use wt_tui::app::{App, Mode, View};
use wt_tui::wt::command::PruneCandidate;
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
    let mut app = App::new(PathBuf::from("/repo"));
    app.apply(envelope(items), Some("main".to_string()));
    app
}

// ---------------------------------------------------------------------------
// Phase 1 behaviour, kept as regression cover.
// ---------------------------------------------------------------------------

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
        worktree_item("zulu", "/repo.zulu", false),
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
    assert_eq!(app.visible.len(), 1);

    app.on_key(key(KeyCode::Esc));
    assert!(app.filter.is_empty());
    assert_eq!(app.visible.len(), 2);
    assert!(matches!(app.mode, Mode::Normal));
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
fn confirming_a_removal_queues_the_work_with_the_dirty_flag() {
    let mut item = worktree_item("feature", "/repo.feature", false);
    item.worktree.as_mut().unwrap().changes = Some(Changes {
        modified: true,
        ..Default::default()
    });

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

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[test]
fn s_switches_into_the_search_view_and_back() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);

    // With no query yet, entering search focuses the query box.
    app.on_key(key(KeyCode::Char('s')));
    assert_eq!(app.view, View::Search);
    assert!(matches!(app.mode, Mode::Searching(_)));

    app.on_key(key(KeyCode::Esc));
    app.on_key(key(KeyCode::Char('s')));
    assert_eq!(app.view, View::Worktrees);
}

#[test]
fn entering_search_with_an_existing_query_leaves_the_cursor_on_results() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.search.query = "previous".to_string();

    app.on_key(key(KeyCode::Char('s')));
    assert_eq!(app.view, View::Search);
    assert!(
        matches!(app.mode, Mode::Normal),
        "an existing query should be browsable straight away"
    );

    // And the modifier keys work without an extra Escape.
    app.on_key(key(KeyCode::Char('i')));
    assert!(app.search.options.case_insensitive);
}

#[test]
fn typing_a_query_accumulates_and_only_runs_on_enter() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.on_key(key(KeyCode::Char('s')));

    for c in "auth".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    assert_eq!(app.search.query, "auth");
    assert!(app.pending.search.is_none(), "nothing runs while typing");

    app.on_key(key(KeyCode::Enter));
    let request = app.pending.search.take().expect("search queued on Enter");
    assert_eq!(request.query, "auth");
}

#[test]
fn an_empty_query_does_not_queue_a_search() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.on_key(key(KeyCode::Char('s')));
    app.on_key(key(KeyCode::Enter));
    assert!(
        app.pending.search.is_none(),
        "an empty query should do nothing"
    );
}

#[test]
fn escape_from_the_query_keeps_the_text_for_next_time() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.on_key(key(KeyCode::Char('s')));
    for c in "draft".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    app.on_key(key(KeyCode::Esc));
    // The query is often worth coming back to, so it is not discarded.
    assert_eq!(app.search.query, "draft");
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn search_modifiers_toggle_independently() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    // A query must already exist: with an empty one, search focuses the query
    // box and every key is text.
    app.search.query = "existing".to_string();
    app.on_key(key(KeyCode::Char('s')));

    app.on_key(key(KeyCode::Char('i')));
    assert!(app.search.options.case_insensitive);
    app.on_key(key(KeyCode::Char('I')));
    assert!(!app.search.options.case_insensitive);

    app.on_key(key(KeyCode::Char('r')));
    assert!(app.search.options.force_regex);
    app.on_key(key(KeyCode::Char('R')));
    assert!(!app.search.options.force_regex);
}

#[test]
fn search_navigation_moves_the_result_cursor_not_the_table() {
    let mut app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("a", "/repo.a", false),
    ]);
    app.on_key(key(KeyCode::Char('j')));
    let table_selection = app.selected_item().unwrap().label();

    app.view = View::Search;
    app.search.results = wt_tui::search::Results {
        hits: vec![
            hit("main", "a.rs", 1),
            hit("main", "b.rs", 2),
            hit("main", "c.rs", 3),
        ],
        ..Default::default()
    };

    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.search.selected, 1);
    assert_eq!(
        app.selected_item().unwrap().label(),
        table_selection,
        "the table selection must not move while browsing results"
    );

    // The cursor clamps rather than running off the end.
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.search.selected, 2);
}

/// Moving between hits in one file must not invalidate the loaded body: the pane
/// holds a whole file, so it stays correct, and clearing it per keystroke is what
/// made every `j` re-read from disk. `move_by` no longer touches the body at
/// all — the event loop decides, once it can resolve the hit's worktree.
#[test]
fn moving_between_hits_in_one_file_keeps_the_loaded_context() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.view = View::Search;
    app.search.results = wt_tui::search::Results {
        hits: vec![hit("main", "a.rs", 1), hit("main", "a.rs", 9)],
        ..Default::default()
    };

    app.search.context_path = Some(("/repo".to_string(), "a.rs".to_string()));
    app.search.context_body = "file body".to_string();

    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(
        app.search.context_body, "file body",
        "the same file is still the right file to show"
    );
    assert_eq!(
        app.search.context_path,
        Some(("/repo".to_string(), "a.rs".to_string())),
        "the body is still keyed to the file it was read from"
    );
}

/// Two worktrees can each hold a `src/main.rs` that a query matches, and search
/// deduplicates by tree rather than by path — so the same `rel_path` can belong
/// to two different files. The key includes the worktree so one worktree's file
/// is never shown under another's hit.
#[test]
fn the_same_relative_path_in_two_worktrees_is_a_different_file() {
    let mut app = app_with(vec![
        worktree_item("a", "/repo.a", false),
        worktree_item("b", "/repo.b", false),
    ]);
    app.view = View::Search;
    app.search.results = wt_tui::search::Results {
        hits: vec![hit("a", "src/main.rs", 1), hit("b", "src/main.rs", 1)],
        ..Default::default()
    };

    let from_a = ("/repo.a".to_string(), "src/main.rs".to_string());
    let from_b = ("/repo.b".to_string(), "src/main.rs".to_string());
    assert_ne!(
        from_a, from_b,
        "two worktrees' identically named files must not share a key"
    );

    app.search.context_path = Some(from_a);
    app.search.context_body = "a's file".to_string();

    // Moving to the hit in the other worktree leaves the stale key in place;
    // the event loop compares the full key and reloads. What matters here is
    // that the two are distinguishable at all.
    app.on_key(key(KeyCode::Char('j')));
    assert_ne!(
        app.search.context_path.as_ref(),
        Some(&from_b),
        "the body must still be keyed to worktree a, not silently reused for b"
    );
}

#[test]
fn search_targets_exclude_branch_only_rows() {
    let mut branch_only = worktree_item("no-worktree", "", false);
    branch_only.worktree = None;

    let app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("feature", "/repo.feature", false),
        branch_only,
    ]);

    let targets = app.search_targets();
    assert_eq!(
        targets.len(),
        2,
        "a branch with no worktree has nothing to search"
    );
}

#[test]
fn search_scope_can_be_limited_to_the_filtered_rows() {
    let mut app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("alpha", "/repo.alpha", false),
        worktree_item("zulu", "/repo.zulu", false),
    ]);

    assert_eq!(app.search_targets().len(), 3);

    app.on_key(key(KeyCode::Char('/')));
    app.on_key(key(KeyCode::Char('a')));
    app.on_key(key(KeyCode::Enter));

    app.search.query = "alpha".to_string();
    app.on_key(key(KeyCode::Char('s')));
    app.on_key(key(KeyCode::Char('f')));
    assert!(app.search.only_filtered);

    let targets = app.search_targets();
    let branches: Vec<&str> = targets.iter().map(|t| t.branch.as_str()).collect();
    assert!(
        !branches.contains(&"zulu"),
        "the filtered-out row must not be searched: {branches:?}"
    );
}

#[test]
fn changing_the_query_discards_the_previous_results() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.search.results = wt_tui::search::Results {
        hits: vec![hit("main", "old.rs", 1)],
        ..Default::default()
    };
    app.search.selected = 0;

    app.search.set_query("new".to_string());

    assert!(
        app.search.results.is_empty(),
        "results belong to the old query and must not linger"
    );
    assert!(app.search.context_body.is_empty());
}

#[test]
fn setting_the_same_query_keeps_the_results() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.search.set_query("same".to_string());
    app.search.results = wt_tui::search::Results {
        hits: vec![hit("main", "x.rs", 1)],
        ..Default::default()
    };

    app.search.set_query("same".to_string());
    assert!(
        !app.search.results.is_empty(),
        "an unchanged query should not discard work already done"
    );
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

#[test]
fn merge_opens_a_dialog_naming_the_branch_and_default_target() {
    let mut app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("feature", "/repo.feature", false),
    ]);
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('m')));

    match &app.mode {
        Mode::ConfirmMerge {
            branch,
            target,
            worktree,
            keep_worktree,
        } => {
            assert_eq!(branch, "feature");
            assert_eq!(target, "main", "the default branch is the merge target");
            assert_eq!(
                worktree,
                &PathBuf::from("/repo.feature"),
                "the dialog must name the worktree it will act on"
            );
            assert!(!keep_worktree);
        }
        other => panic!("expected a merge dialog, got {other:?}"),
    }
}

#[test]
fn merge_queues_the_source_worktree_not_the_repo_root() {
    // `wt merge` merges the *current* branch into the target, so it must run
    // from the row's worktree. Queuing the repo root would merge the wrong
    // direction, silently.
    let mut app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("feature", "/repo.feature", false),
    ]);
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('m')));
    app.on_key(key(KeyCode::Char('y')));

    let request = app.pending.merge.take().expect("merge queued");
    assert_eq!(
        request.worktree,
        PathBuf::from("/repo.feature"),
        "the merge must run from the row's worktree"
    );
    assert_ne!(request.worktree, PathBuf::from("/repo"));
    assert_eq!(request.branch, "feature");
    assert_eq!(request.target, "main");
}

#[test]
fn merge_can_keep_the_worktree() {
    let mut app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("feature", "/repo.feature", false),
    ]);
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('m')));
    app.on_key(key(KeyCode::Char('w')));

    match &app.mode {
        Mode::ConfirmMerge { keep_worktree, .. } => assert!(*keep_worktree),
        other => panic!("expected a merge dialog, got {other:?}"),
    }

    app.on_key(key(KeyCode::Char('y')));
    let request = app.pending.merge.take().expect("merge queued");
    assert!(request.keep_worktree);
}

/// The regression test for the worst bug in this tool's history: a refresh
/// landing between `m` and `y` moved the cursor, and the merge ran against
/// whatever row the index fallback happened to point at — merging and then
/// *deleting* a branch the user never confirmed.
///
/// The dialog must act on the row it named, whatever the selection does while
/// it is open.
#[test]
fn merge_acts_on_the_confirmed_row_even_if_the_selection_moves() {
    let mut app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("feature", "/repo.feature", false),
        worktree_item("other", "/repo.other", false),
    ]);
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('m')));

    // A refresh lands while the dialog is open: `feature` is gone and the
    // index fallback slides the cursor onto `other`.
    app.apply(
        envelope(vec![
            worktree_item("main", "/repo", true),
            worktree_item("other", "/repo.other", false),
        ]),
        Some("main".to_string()),
    );

    app.on_key(key(KeyCode::Char('y')));

    let request = app.pending.merge.take().expect("merge queued");
    assert_eq!(
        request.worktree,
        PathBuf::from("/repo.feature"),
        "the merge must target the row that was confirmed"
    );
    assert_eq!(
        request.branch, "feature",
        "the branch named in the dialog is the one that must be merged"
    );
    assert_ne!(
        request.worktree,
        PathBuf::from("/repo.other"),
        "a row the user never confirmed must never be merged"
    );
}

#[test]
fn merging_a_branch_only_row_is_refused_with_a_reason() {
    let mut branch_only = worktree_item("no-worktree", "", false);
    branch_only.worktree = None;

    let mut app = app_with(vec![branch_only]);
    app.on_key(key(KeyCode::Char('m')));

    assert!(
        matches!(app.mode, Mode::Normal),
        "there is no worktree to merge from, so no dialog"
    );
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|n| n.text.contains("no worktree")),
        "the refusal must explain itself: {:?}",
        app.notice
    );
}

#[test]
fn declining_a_merge_queues_nothing() {
    let mut app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("feature", "/repo.feature", false),
    ]);
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('m')));
    app.on_key(key(KeyCode::Char('n')));
    assert_eq!(app.pending.merge, None);
    assert!(matches!(app.mode, Mode::Normal));
}

// ---------------------------------------------------------------------------
// Prune
// ---------------------------------------------------------------------------

#[test]
fn prune_previews_before_removing_anything() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);

    app.on_key(key(KeyCode::Char('P')));
    assert!(app.pending.prune_preview, "prune must preview first");
    assert_eq!(
        app.pending.prune, None,
        "nothing is removed on the first press"
    );
}

#[test]
fn the_prune_dialog_lists_candidates_and_the_age_guard() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.on_key(key(KeyCode::Char('P')));

    // Simulate the preview returning two candidates.
    let candidates = vec![
        PruneCandidate {
            path: Some("/repo.old-one".into()),
            reason: Some("same commit as main".into()),
            ..Default::default()
        },
        PruneCandidate {
            path: Some("/repo.old-two".into()),
            reason: Some("content integrated".into()),
            ..Default::default()
        },
    ];
    app.mode = Mode::ConfirmPrune {
        candidates,
        min_age: "1d".to_string(),
    };

    match &app.mode {
        Mode::ConfirmPrune {
            candidates,
            min_age,
        } => {
            assert_eq!(candidates.len(), 2);
            assert_eq!(min_age, "1d");
        }
        other => panic!("expected a prune dialog, got {other:?}"),
    }

    app.on_key(key(KeyCode::Char('y')));
    assert_eq!(app.pending.prune.as_deref(), Some("1d"));
}

#[test]
fn the_age_guard_can_be_cycled_and_is_applied_to_the_preview() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.mode = Mode::ConfirmPrune {
        candidates: vec![],
        min_age: "1d".to_string(),
    };

    // Cycling the age must not relabel the list already on screen: the
    // candidates were chosen by a dry run at `1d`, and a different age selects a
    // different set. So the mode is left for the fresh preview to populate.
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.pending.prune_age.as_deref(), Some("7d"));
    assert!(
        app.pending.prune_preview,
        "a new dry run must be requested for the new age"
    );
    assert!(
        matches!(app.mode, Mode::Busy),
        "the dialog must not claim an age its list was not computed with"
    );
    assert_eq!(
        app.pending.prune, None,
        "nothing may be queued for prune while the preview is stale"
    );

    // While the preview is in flight, `y` cannot confirm: Busy swallows it.
    app.on_key(key(KeyCode::Char('y')));
    assert_eq!(app.pending.prune, None);

    // The preview returns and the dialog is rebuilt at the requested age, from
    // which the next cycle continues.
    app.mode = Mode::ConfirmPrune {
        candidates: vec![],
        min_age: "7d".to_string(),
    };
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.pending.prune_age.as_deref(), Some("0"));
}

#[test]
fn a_prune_preview_can_be_cancelled_while_it_is_in_flight() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.pending.prune_preview = true;
    app.mode = Mode::Busy;

    // A `wt` call that hangs must not strand the user.
    app.on_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.pending.prune, None);
}

#[test]
fn prune_cannot_run_with_an_age_other_than_the_one_previewed() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);

    // A preview lands at `1d` and populates the dialog.
    app.mode = Mode::ConfirmPrune {
        candidates: vec![],
        min_age: "1d".to_string(),
    };

    // Cycle to `0`. The list on screen is still the `1d` list.
    app.on_key(key(KeyCode::Char('a')));
    app.mode = Mode::ConfirmPrune {
        candidates: vec![],
        min_age: "0".to_string(),
    };

    // Confirming runs at the age that produced the list now on screen.
    app.on_key(key(KeyCode::Char('y')));
    assert_eq!(app.pending.prune.as_deref(), Some("0"));
}

#[test]
fn declining_a_prune_removes_nothing() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.mode = Mode::ConfirmPrune {
        candidates: vec![],
        min_age: "1d".to_string(),
    };
    app.on_key(key(KeyCode::Char('n')));
    assert_eq!(app.pending.prune, None);
    assert!(matches!(app.mode, Mode::Normal));
}

// ---------------------------------------------------------------------------
// Switch
// ---------------------------------------------------------------------------

#[test]
fn enter_queues_a_switch_to_the_selected_branch() {
    let mut app = app_with(vec![
        worktree_item("main", "/repo", true),
        worktree_item("feature", "/repo.feature", false),
    ]);
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.pending.switch.as_deref(), Some("feature"));
    assert!(
        matches!(app.mode, Mode::Busy),
        "the TUI waits while `wt switch` runs"
    );
}

#[test]
fn enter_on_the_current_worktree_is_a_noop_with_a_notice() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(
        app.pending.switch, None,
        "switching to yourself would only rewrite where you already are"
    );
    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|n| n.text.contains("already on")),
        "the no-op must explain itself: {:?}",
        app.notice
    );
}

#[test]
fn enter_on_a_branch_only_row_switches_by_branch() {
    // A branch with no worktree yet: `wt switch <branch>` creates one, the
    // same as the picker's `--branches` rows.
    let mut branch_only = worktree_item("no-worktree", "", false);
    branch_only.worktree = None;

    let mut app = app_with(vec![branch_only]);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.pending.switch.as_deref(), Some("no-worktree"));
}

#[test]
fn enter_on_a_detached_row_switches_by_path() {
    let detached = Item {
        branch: None,
        worktree: Some(Worktree {
            path: Some("/repo.detached".to_string()),
            detached: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut app = app_with(vec![detached]);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(
        app.pending.switch.as_deref(),
        Some("/repo.detached"),
        "a detached worktree has no branch, so the path is the only name it has"
    );
}

#[test]
fn enter_with_no_visible_rows_queues_nothing() {
    let mut app = app_with(vec![]);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.pending.switch, None);
}

#[test]
fn enter_in_the_search_view_reruns_the_query_instead_of_switching() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);
    app.view = View::Search;
    app.search.query = "auth".to_string();
    app.on_key(key(KeyCode::Enter));
    assert!(
        app.pending.search.is_some(),
        "Enter in search re-runs the query"
    );
    assert_eq!(
        app.pending.switch, None,
        "the table must not switch while browsing results"
    );
}

// ---------------------------------------------------------------------------
// PR / CI
// ---------------------------------------------------------------------------

#[test]
fn ci_is_fetched_only_when_asked() {
    let mut app = app_with(vec![worktree_item("main", "/repo", true)]);

    // A refresh must not trigger a network call.
    assert!(!app.pending.fetch_ci);
    assert!(!app.ci_attempted);

    app.on_key(key(KeyCode::Char('S')));
    assert!(app.pending.fetch_ci, "S requests PR/CI data");
}

fn hit(branch: &str, path: &str, line: u64) -> wt_tui::search::Hit {
    wt_tui::search::Hit {
        worktrees: vec![branch.to_string()],
        rel_path: path.to_string(),
        line_number: line,
        line: "content".to_string(),
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

// Keep the `Head` import used so the fixture type stays exercised.
#[test]
fn filter_matches_a_commit_subject() {
    let mut with_subject = worktree_item("main", "/repo", true);
    with_subject.head = Some(Head {
        subject: Some("fix the login page".into()),
        ..Default::default()
    });

    let mut app = app_with(vec![
        with_subject,
        worktree_item("other", "/repo.other", false),
    ]);

    app.on_key(key(KeyCode::Char('/')));
    for c in "login".chars() {
        app.on_key(key(KeyCode::Char(c)));
    }
    assert_eq!(
        app.visible.len(),
        1,
        "a commit subject should be searchable"
    );
}
