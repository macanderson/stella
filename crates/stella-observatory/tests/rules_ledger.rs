//! `/api/rules` end to end: the published context records under
//! `.stella/rules/`, and the enforcement-promotion ledger beside them.
//!
//! This suite lives in `tests/` for the reason the crate README gives for the
//! other four — it needs something the inline `src/tests.rs` module cannot
//! give it. Here that is room: `src/tests.rs` sits at the file-size gate's
//! 1500-line ratchet exactly, and this crate carries no baseline entry, so it
//! is closed to growth (`AGENTS.md` § "God files — plan around them, never
//! into them"). New coverage lands in a sibling instead of raising a ceiling.
//!
//! Nothing here seeds a `store.db`. `/api/rules` folds a file view onto a
//! database view, and a workspace with no store degrades the database half to
//! `[]` rather than erroring — which is exactly the shape being asserted.

use std::path::Path;

use stella_observatory::respond;

/// A published context record, a hand-written markdown rule, the governance
/// mode, and one ledger entry — the four things `.stella/rules/` can hold.
fn seed_rules(root: &Path) {
    let dir = root.join(".stella/rules");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ctx.demo.pin.toml"),
        r#"
schema = "context-record/v0.1"
set_id = "demo"

[[record]]
lineage_id = "ctx.demo.pin"
kind       = "constraint"
statement  = "The toolchain is pinned to a concrete version."
tags       = ["build", "toolchain"]

  [record.steering]
  force      = "must"
  precedence = 90

  [record.enforcement]
  mode = "hard"
"#,
    )
    .unwrap();
    std::fs::write(dir.join("style.md"), "Prefer witness tests.").unwrap();
    std::fs::write(dir.join("governance.toml"), "mode = \"regulated\"\n").unwrap();
    std::fs::write(
        dir.join("promotions.jsonl"),
        "{\"seq\":1,\"prev\":\"genesis\",\"at\":\"2026-08-03T04:12:38Z\",\
         \"lineage_id\":\"ctx.demo.pin\",\"from\":\"advisory\",\"to\":\"blocking\",\
         \"approver\":\"@someone\",\"reason\":\"unrecoverable after release\",\
         \"mode\":\"regulated\"}\n",
    )
    .unwrap();
}

fn rules_payload(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&respond(root, "/api/rules").body).unwrap()
}

/// The panel must show what actually steers the session, which means reading
/// the same extensions the engine reads (`RULE_EXTENSIONS` is `["md", "toml"]`
/// in `stella-cli`'s loader) and honouring the same reserved filenames.
#[test]
fn rules_route_serves_toml_records_markdown_and_reserves_governance() {
    let ws = tempfile::tempdir().unwrap();
    seed_rules(ws.path());
    let rules = rules_payload(ws.path());

    let files = rules["files"].as_array().unwrap();
    let record = files
        .iter()
        .find(|r| r["lineage_id"] == "ctx.demo.pin")
        .expect("the TOML record is served");
    assert_eq!(record["kind"], "constraint");
    assert_eq!(record["steering_force"], "must");
    assert_eq!(record["enforcement_mode"], "hard");
    assert_eq!(record["tags"], serde_json::json!(["build", "toolchain"]));

    // The page renders one list from record and markdown cards together and
    // reads these keys off every row, so a card missing them reaches the DOM
    // blank — a failure no Rust gate downstream of this one can see.
    assert_eq!(
        record["name"], "pin",
        "the handle `stella context list` prints"
    );
    assert_eq!(
        record["snippet"], "The toolchain is pinned to a concrete version.",
        "snippet is what the card body shows"
    );
    assert!(record["modified_unix"].as_i64().is_some());

    assert!(
        files.iter().any(|r| r["name"] == "style"),
        "markdown rules still render: {files:?}"
    );
    assert!(
        !files.iter().any(|r| r["name"] == "governance"),
        "governance.toml sets the mode; it is not a rule the engine loads: {files:?}"
    );
}

/// The hash-chained enforcement ledger is a different trail from the adaptive
/// loop's own `promotion_event` records in `context.db`: this one is the human
/// act that moves a *published* record between advisory and blocking. Serving
/// only the latter reported "no governance decisions" for a workspace whose
/// policy had been promoted by a named approver.
#[test]
fn rules_route_serves_the_enforcement_ledger_and_governance_mode() {
    let ws = tempfile::tempdir().unwrap();
    seed_rules(ws.path());
    let rules = rules_payload(ws.path());

    assert_eq!(rules["governance_mode"], "regulated");

    let promotions = rules["promotions"].as_array().unwrap();
    assert_eq!(promotions.len(), 1);
    let event = &promotions[0];
    assert_eq!(event["seq"], 1);
    assert_eq!(event["lineage_id"], "ctx.demo.pin");
    assert_eq!(event["from"], "advisory");
    assert_eq!(event["to"], "blocking");
    assert_eq!(event["approver"], "@someone");
    assert_eq!(event["mode"], "regulated");
    assert_eq!(event["reason"], "unrecoverable after release");
}

/// Missing is a state, never an error — the crate's rule for every view. A
/// workspace that has never published a record answers with empty collections
/// and a null mode, not a 500 and not an absent key, because the page reads
/// both fields unconditionally.
#[test]
fn a_workspace_with_no_rules_directory_degrades_to_empty_not_error() {
    let ws = tempfile::tempdir().unwrap();
    let response = respond(ws.path(), "/api/rules");
    assert_eq!(response.status, "200 OK");

    let rules: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(rules["files"].as_array().unwrap().len(), 0);
    assert_eq!(rules["promotions"].as_array().unwrap().len(), 0);
    assert!(rules["governance_mode"].is_null());
}

/// A ledger line that is not JSON, and one carrying no lineage, are both
/// skipped rather than poisoning the whole feed — the same tolerance
/// `fsview::lessons` applies to `reflections.jsonl`, and for the same reason:
/// an operator-editable append-only file will eventually contain a bad line,
/// and losing the other entries to it is the worse failure.
#[test]
fn malformed_ledger_lines_are_skipped_not_fatal() {
    let ws = tempfile::tempdir().unwrap();
    seed_rules(ws.path());
    let ledger = ws.path().join(".stella/rules/promotions.jsonl");
    let mut text = std::fs::read_to_string(&ledger).unwrap();
    text.push_str("not json at all\n");
    text.push_str("{\"seq\":2,\"note\":\"no lineage_id here\"}\n");
    std::fs::write(&ledger, text).unwrap();

    let rules = rules_payload(ws.path());
    let promotions = rules["promotions"].as_array().unwrap();
    assert_eq!(
        promotions.len(),
        1,
        "only the well-formed entry: {promotions:?}"
    );
    assert_eq!(promotions[0]["lineage_id"], "ctx.demo.pin");
}
