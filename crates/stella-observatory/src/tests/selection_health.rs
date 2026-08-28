//! The selection-health panel's policy: it folds under the thresholds the
//! workspace tuned, not the shipped defaults (#1944). Split out of the parent
//! module, which sits at the 1500-line ratchet with no baseline entry.

use super::*;

/// Seed a `context.db` whose one record has three assessed not-helpful uses.
///
/// Chosen so the **verdict** turns on `min_attributable_uses` alone: three
/// assessed uses is below the shipped floor of 5 (not attributable, so not
/// failing) and at or above a tuned 2 (attributable, ratio 1.0 over any
/// threshold, so failing). Nothing else in the fixture moves.
fn seed_three_not_helpful_uses(ws: &Path) {
    let private = ws.join(".stella/private");
    std::fs::create_dir_all(&private).unwrap();
    let conn = Connection::open(private.join("context.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE context_records (
           record_id TEXT PRIMARY KEY,
           lineage_id TEXT,
           record_kind TEXT NOT NULL,
           record_hash TEXT,
           schema_version TEXT,
           body TEXT NOT NULL,
           observed_at TEXT,
           recorded_at TEXT,
           supersedes TEXT);",
    )
    .unwrap();
    for n in 0..3 {
        let use_body = serde_json::json!({
            "use_kind": "cited",
            "context_record_id": "ctx.acme.web.a",
            "use_trace_id": format!("trace-{n}"),
            "task_id": format!("task-{n}"),
            "influence_stage": "execution",
            "observed_at": "2026-08-04T09:00:00Z",
        })
        .to_string();
        let feedback_body = serde_json::json!({
            "context_use_id": format!("use-{n}"),
            "use_trace_id": format!("trace-{n}"),
            "task_id": format!("task-{n}"),
            "evaluation": "not_helpful",
            "had_opportunity": true,
            "influence_stage": "execution",
            "outcome_relation": "unrelated",
            "observable_effect_refs": ["diff:1"],
            "evaluation_method": "deterministic_validation",
            "attribution_confidence": 90,
            "observed_at": "2026-08-04T09:00:00Z",
        })
        .to_string();
        conn.execute(
            "INSERT INTO context_records (record_id, record_kind, body)
             VALUES (?1, 'context_use', ?2)",
            rusqlite::params![format!("use-{n}"), use_body],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO context_records (record_id, record_kind, body)
             VALUES (?1, 'context_use_feedback', ?2)",
            rusqlite::params![format!("fb-{n}"), feedback_body],
        )
        .unwrap();
    }
}

/// **Witness (#1944).** The selection-health panel folds under the thresholds
/// this workspace tuned, not the shipped defaults.
///
/// Fails on main, where `health_rows` always passed
/// `SelectionHealthPolicy::default()` while the CLI read tuned values out of
/// the settings scope chain — so a workspace with a tuned `context.efficacy`
/// read a different failing/earning verdict on the dashboard than the loop it
/// is a dashboard *of* actually applied.
///
/// The assertion is the **verdict**, not the echoed numbers: a payload that
/// reported tuned thresholds beside a default-folded verdict would be worse
/// than the default it replaces, which at least named the numbers it used.
#[test]
fn the_selection_health_panel_folds_under_the_tuned_policy() {
    let ws = TempDir::new().unwrap();
    seed_three_not_helpful_uses(ws.path());
    std::fs::write(
        ws.path().join(".stella/settings.json"),
        r#"{"context":{"efficacy":{"min_attributable_uses":2,
                                   "not_helpful_ratio_threshold":0.5}}}"#,
    )
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/context-lifecycle").body).unwrap();

    let row = &body["selection_health"][0];
    assert_eq!(row["assessed_uses"], 3);
    assert_eq!(
        row["attributable"], true,
        "three assessed uses clears the tuned floor of 2"
    );
    assert_eq!(
        row["failing"], true,
        "and a not-helpful ratio of 1.0 is over the tuned 0.5 — the verdict \
         the loop itself reaches"
    );

    let policy = &body["health_policy"];
    assert_eq!(policy["min_attributable_uses"], 2);
    assert_eq!(policy["not_helpful_ratio_threshold"], 0.5);
    // Per-field: tuning two values must not reset the third.
    assert_eq!(policy["min_attribution_confidence"], 80);
}

/// The same fixture under no settings at all reaches the OPPOSITE verdict.
///
/// This is what makes the test above a witness rather than a restatement: the
/// data is identical and only the policy differs, so a fold that ignored the
/// tuned policy would produce this row in both tests.
#[test]
fn the_same_uses_are_not_failing_under_the_shipped_policy() {
    let ws = TempDir::new().unwrap();
    seed_three_not_helpful_uses(ws.path());

    let body: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/context-lifecycle").body).unwrap();

    let row = &body["selection_health"][0];
    assert_eq!(row["assessed_uses"], 3);
    assert_eq!(
        row["attributable"], false,
        "three assessed uses is below the shipped floor of 5"
    );
    assert_eq!(row["failing"], false);

    let policy = &body["health_policy"];
    assert_eq!(policy["min_attributable_uses"], 5);
    assert_eq!(policy["not_helpful_ratio_threshold"], 0.8);
    assert_eq!(policy["min_attribution_confidence"], 80);
}
