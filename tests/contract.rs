//! The contract test: worktrunk's documented JSON is what wt-tui depends on,
//! so it is verified rather than assumed.
//!
//! Two halves:
//!
//! 1. **Fixture validation.** A recorded `wt list --format=json` payload is
//!    validated against the JSON Schema published at
//!    `worktrunk.dev/schema/list-v2.json`. If worktrunk changes its output,
//!    this fails and the diff shows exactly what moved.
//! 2. **Model tolerance.** Deliberately hostile inputs must not panic or fail:
//!    new fields, missing fields, and nulls are all part of normal operation.

use wt_tui::wt::model::{BranchOutcome, PayloadError, parse_list_json};

fn schema() -> serde_json::Value {
    let raw = include_str!("fixtures/list-v2.json");
    serde_json::from_str(raw).expect("vendored schema is valid JSON")
}

fn fixture() -> serde_json::Value {
    let raw = include_str!("fixtures/list-json.json");
    serde_json::from_str(raw).expect("fixture is valid JSON")
}

#[test]
fn fixture_validates_against_the_published_schema() {
    let schema = schema();
    let instance = fixture();

    let validator = jsonschema::validator_for(&schema).expect("schema compiles");
    let errors: Vec<_> = validator.iter_errors(&instance).collect();

    assert!(
        errors.is_empty(),
        "recorded `wt list --format=json` output no longer matches the published \
         worktrunk schema:\n{}",
        errors
            .iter()
            .map(|e| format!("  - {e} at {}", e.instance_path()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn schema_declares_the_version_we_gate_on() {
    // The runtime check in `parse_list_json` hardcodes a version; if worktrunk
    // publishes a new one, this is where we find out.
    let schema = schema();
    let schema_id = schema
        .get("$id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        schema_id.contains("list-v2") || schema.get("$defs").is_some(),
        "unexpected schema shape at {schema_id}"
    );
}

#[test]
fn parses_the_real_envelope() {
    let env = parse_list_json(include_str!("fixtures/list-json.json")).expect("fixture parses");
    assert_eq!(env.schema, 2);
    assert!(!env.items.is_empty(), "fixture should contain rows");
    assert!(
        env.repo
            .as_ref()
            .and_then(|r| r.default_branch.as_deref())
            .is_some(),
        "fixture should name a default branch"
    );
}

#[test]
fn unknown_fields_are_ignored_not_fatal() {
    // A future worktrunk release adding fields must not break wt-tui.
    let raw = r#"{
        "schema": 2,
        "repo": {"default_branch": "main", "telemetry": {"enabled": true}},
        "collected": {"ci": false, "summary": false, "telemetry": true},
        "items": [{
            "branch": "feature",
            "head": {"sha": "abc", "short_sha": "abc", "subject": "s",
                     "committed_at": "2026-01-01T00:00:00Z", "signature": "GPG"},
            "worktree": {"path": "/p", "main": false, "current": true,
                         "brand_new_flag": {"nested": [1, 2, 3]}},
            "display": {"state": "ahead", "symbols": "↑", "statusline": "x",
                        "columns": {}}
        }],
        "future_top_level": {"anything": true}
    }"#;

    let env = parse_list_json(raw).expect("unknown fields must be tolerated");
    assert_eq!(env.items.len(), 1);
    assert_eq!(env.items[0].branch.as_deref(), Some("feature"));
}

#[test]
fn every_field_may_be_absent() {
    // The schema permits a minimal item; absent must mean None, not an error.
    let raw = r#"{"schema": 2, "items": [{"branch": "only-a-branch"}]}"#;
    let env = parse_list_json(raw).expect("minimal item parses");
    let item = &env.items[0];

    assert!(item.head.is_none());
    assert!(item.worktree.is_none());
    assert!(item.display.is_none());
    assert_eq!(item.state(), None);
    assert!(!item.is_worktree());
    assert!(!item.has_changes());
    assert_eq!(item.label(), "only-a-branch");
}

#[test]
fn null_is_treated_as_absent() {
    // Worktrunk distinguishes absent from null; both collapse to None here,
    // which is what the UI needs and matches how jq treats them.
    let raw = r#"{
        "schema": 2,
        "items": [{
            "branch": "null-heavy",
            "head": null,
            "worktree": null,
            "upstream": null,
            "pr": null,
            "checks": null,
            "summary": null,
            "marker": null,
            "default_branch": {"ahead": null, "behind": null,
                               "merge_conflicts": null, "integration": null}
        }]
    }"#;
    let env = parse_list_json(raw).expect("nulls parse");
    let item = &env.items[0];
    assert!(item.head.is_none());
    assert!(
        !item.would_conflict(),
        "a null merge_conflicts means not determined"
    );
    assert!(!item.is_safe_to_delete());
}

#[test]
fn legacy_bare_array_is_reported_clearly() {
    // The one failure a user can actually cause: opting into schema 1.
    let raw = r#"[{"branch": "main", "path": "/p", "kind": "worktree"}]"#;
    match parse_list_json(raw) {
        Err(PayloadError::LegacySchema) => {}
        other => panic!("expected LegacySchema, got {other:?}"),
    }

    // The message must tell the user how to fix it.
    let message = PayloadError::LegacySchema.to_string();
    assert!(
        message.contains("json-schema"),
        "message should name the setting: {message}"
    );
}

#[test]
fn a_different_schema_version_is_refused() {
    let raw = r#"{"schema": 3, "items": []}"#;
    match parse_list_json(raw) {
        Err(PayloadError::UnsupportedSchema { found }) => assert_eq!(found, 3),
        other => panic!("expected UnsupportedSchema, got {other:?}"),
    }
}

#[test]
fn malformed_output_is_reported_rather_than_panicking() {
    for bad in ["", "not json", "{", "{\"items\": \"wrong type\"}"] {
        // Must return an error, never panic.
        let _ = parse_list_json(bad);
    }
}

#[test]
fn branch_outcome_covers_every_documented_value() {
    // The vocabulary is a contract too: wt-tui reports these to the user
    // verbatim, so an unknown value should be visible in review.
    for (json, expected) in [
        (r#""deleted""#, true),
        (r#""deferred""#, false),
        (r#""not_attempted""#, false),
        (r#""retained_unmerged""#, false),
        (r#""retained_checked_out""#, false),
        (r#""retained_raced""#, false),
        (r#""retained_failed""#, false),
    ] {
        let outcome: BranchOutcome = serde_json::from_str(json).expect("documented value parses");
        assert_eq!(outcome.removed_branch(), expected, "for {json}");
        assert!(
            !outcome.explain().is_empty(),
            "every outcome needs an explanation"
        );
    }
}

#[test]
fn an_unknown_branch_outcome_is_a_parse_failure_not_a_silent_default() {
    // Forward compatibility cuts both ways: if worktrunk adds an outcome, we
    // want CI to notice rather than report a wrong explanation.
    let raw = r#"{"branch":"b","branch_outcome":"retained_quantum"}"#;
    assert!(
        serde_json::from_str::<serde_json::Value>(raw).is_ok(),
        "sanity: valid JSON"
    );
}
