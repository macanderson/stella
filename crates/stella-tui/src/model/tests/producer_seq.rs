// SPDX-License-Identifier: AGPL-3.0-only
//! Which producer measured a call, and the diff its row therefore resolves.
//!
//! Split out of the parent `tests.rs` (file-size gate): this cluster took that
//! file to 1661 lines, and 1500 is a hard limit for anything not already
//! grandfathered — the baseline takes no new entries, so the answer is a
//! submodule rather than a bigger ceiling.
//!
//! Two producers now emit `AgentEvent::FileChange` and they fold in opposite
//! orders around the `ToolResult` they belong to: the registry measures a solo
//! mutating call the moment it returns (#4175), and the turn boundary sweeps
//! whatever no single call can own. `SessionModel` tells them apart from
//! `mutation_baselines` rather than from anything on the wire (#4205), so this
//! is where that discrimination is pinned — in both directions.
//!
//! # Why `landed` is `recorded > base` and not `== base + 1`
//!
//! Both forms agree on every input reachable today, so the choice is decided
//! by which one fails better — worth writing down, because neither predicate
//! shows it on its face.
//!
//! A landed change currently moves a path's `changes` by exactly one:
//! `turn_files::emit_measured_tree_changes` sends one `FileChange` per changed
//! path, because `WorkJournal::diff_trees` parses `git diff --name-status`,
//! which names a path once. Nothing *pins* that — it is a property of the
//! implementation, not an asserted invariant — and a per-hunk producer, a tool
//! that triggers two measurements, or another partial producer would each
//! break it.
//!
//! Suppose it breaks, so `recorded > base + 1`:
//!
//! - `>` still says landed and stamps `recorded` — the last change recorded
//!   for this path, which is one of the changes this call really made.
//!   Partial, but true.
//! - `==` says NOT landed and stamps `recorded + 1`, naming a change that does
//!   not exist yet. The row goes diffless, and if the boundary later records
//!   there it resolves to a change made *after* this call — the misattribution
//!   this whole mechanism exists to prevent.
//!
//! So the looser test is the safer one, and `==` is the form that quietly
//! depends on one-`FileChange`-per-call holding forever.
//!
//! The case that argues the other way is a baseline stranded by a turn
//! cancelled between `ToolStart` and `ToolResult`, then read by a provider
//! recycling the `call_id` — real enough that this repository's own telemetry
//! shows kimi reusing `read_file:0`..`:3` across messages. Under `>` that
//! would read the path's grown count as this call's own change. It is shut
//! twice over: the re-dispatched call's own `ToolStart` overwrites the entry
//! before anything reads it, and the one path that folds a result with no
//! start (the halted arm in `driver::dispatch`) answers `Err`, so it never
//! stamps. A witness for it was written and then deleted — it passed under
//! *both* predicates, which is how the unreachability was established rather
//! than assumed.

use super::*;

/// Fold one `edit_file` call: the start, the change it made, then its result.
///
/// The ordering is the whole subject of #4175 and is what a per-call producer
/// buys. `stella_tools`' registry measures the work tree after a solo mutating
/// call returns and publishes on the same channel the engine then sends
/// `ToolResult` on, so a consumer folding the stream sees the `FileChange`
/// *before* the row that made it.
fn fold_edit(model: &mut SessionModel, call_id: &str, path: &str, diff: &str, added: u32) {
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: call_id.into(),
            name: "edit_file".into(),
            input: serde_json::json!({ "path": path }),
        },
    });
    model.apply(&AgentEvent::FileChange {
        path: path.into(),
        kind: FileChangeKind::Modified,
        added,
        removed: 0,
        diff: Some(diff.into()),
    });
    model.apply(&AgentEvent::ToolResult {
        call_id: call_id.into(),
        output: stella_protocol::ToolOutput::Ok {
            content: "edited".into(),
            data: None,
        },
        duration_ms: 3,
        speculated: false,
    });
}

/// **Witness (#4175).** Two `edit_file` calls to one path in one turn each
/// render *their own* change.
///
/// This is the ceiling the issue was filed to lift, and it was lower than the
/// issue described: with the turn-boundary producer as the only one, a path's
/// `FileChange` arrives *after* every `ToolResult` of the turn has folded, so
/// the seq a row stamps is the pre-turn count while `remember_diff` records at
/// the post-bump one. Not "only the last row resolves" — **no** row resolved,
/// which is the diffless successful `edit_file` reported in #4155.
///
/// A per-call producer fixes it without the fold changing at all: the change
/// lands before the row, `changes` is already bumped when the row stamps, and
/// the two seqs agree. The assertion that matters is the *distinctness* — each
/// row must resolve the diff of the call it belongs to, never the other's and
/// never the turn's aggregate.
#[test]
fn two_calls_to_one_path_each_resolve_the_change_they_made() {
    let mut model = SessionModel::new();
    fold_edit(&mut model, "c1", "src/lib.rs", "@@\n+first", 1);
    fold_edit(&mut model, "c2", "src/lib.rs", "@@\n+second", 4);

    let first = inline_ref(&model, "c1").expect("the first row stamps a diff ref");
    let second = inline_ref(&model, "c2").expect("the second row stamps a diff ref");
    assert_ne!(
        first.seq, second.seq,
        "two calls to one path must point at two different changes, or one row \
         is rendering the other's work"
    );

    let file = model
        .files
        .iter()
        .find(|f| f.path == "src/lib.rs")
        .expect("the path is tracked");
    assert_eq!(
        file.diff_at(first.seq),
        Some("@@\n+first"),
        "the first row resolves the first edit — this was None before a \
         per-call producer existed, which is why a successful edit rendered \
         with no diff at all"
    );
    assert_eq!(
        file.diff_at(second.seq),
        Some("@@\n+second"),
        "and the second row resolves the second edit"
    );
    assert_eq!(
        (file.delta_at(first.seq), file.delta_at(second.seq)),
        (Some((1, 0)), Some((4, 0))),
        "each row's `+N −M` is its own call's measurement, not the turn's total"
    );
    assert_eq!(
        (file.added, file.removed),
        (5, 0),
        "and the path's cumulative total is still the sum of the calls — the \
         per-call readings partition the turn rather than double-counting it"
    );
}

/// A failed call stamps nothing, so it cannot inherit the previous call's
/// diff.
///
/// The per-call producer makes this reachable in a way it was not before: a
/// successful edit now really does leave a resolvable diff at the path's
/// current seq, so a following failure that stamped one would render that
/// change under its own ✗ — attributing work the call never did.
#[test]
fn a_failed_call_after_a_successful_one_claims_no_diff() {
    let mut model = SessionModel::new();
    fold_edit(&mut model, "c1", "src/lib.rs", "@@\n+first", 1);

    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c2".into(),
            name: "edit_file".into(),
            input: serde_json::json!({ "path": "src/lib.rs" }),
        },
    });
    // No FileChange: the registry measures only a successful call.
    model.apply(&AgentEvent::ToolResult {
        call_id: "c2".into(),
        output: stella_protocol::ToolOutput::error("no such occurrence"),
        duration_ms: 1,
        speculated: false,
    });

    assert!(
        inline_ref(&model, "c1").is_some(),
        "the successful row still resolves its own change"
    );
    assert!(
        inline_ref(&model, "c2").is_none(),
        "the failed row claims nothing — rendering the path's previous diff \
         under a ✗ would attribute a change that call never made"
    );
}

/// **Witness (#4227).** A successful mutating call that measured *nothing* must
/// not adopt the next change to its path as its own.
///
/// `write_file` has no identical-content short-circuit
/// (`stella_tools::write`), so writing the bytes already on disk succeeds and
/// moves the tree not at all: the registry measures, finds nothing, and
/// publishes no `FileChange`. The row's own measurement therefore came back
/// empty — and the `else` arm used to answer that by stamping `recorded + 1`,
/// a reference to a change that has not happened yet and that nothing binds to
/// this call. Whatever touched the path next — a concurrent `delegate`'s
/// writes, a human saving in another window — then rendered under this row: a
/// real, correctly formatted diff under the wrong call, which no reader can
/// detect.
#[test]
fn a_call_that_changed_nothing_must_not_adopt_a_later_boundary_change() {
    let mut model = SessionModel::new();
    // A real edit, measured per call — which is what establishes that the
    // registry is measuring in this session at all.
    fold_edit(&mut model, "c1", "src/a.rs", "@@\n+c1 wrote this", 1);

    // `write_file` of bytes identical to what is on disk: it SUCCEEDS and
    // moves nothing, so no `FileChange` rides with it.
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c2".into(),
            name: "write_file".into(),
            input: serde_json::json!({ "path": "src/a.rs" }),
        },
    });
    model.apply(&AgentEvent::ToolResult {
        call_id: "c2".into(),
        output: stella_protocol::ToolOutput::Ok {
            content: "wrote 42 bytes to src/a.rs".into(),
            data: None,
        },
        duration_ms: 1,
        speculated: false,
    });

    // Something else touches the path and the boundary sweeps it up.
    turn_boundary(
        &mut model,
        "src/a.rs",
        Some("@@\n+someone else wrote this"),
        9,
        9,
    );

    assert!(
        inline_ref(&model, "c2").is_none(),
        "a call whose own measurement came back empty claims no change — \
         stamping one would render the next writer's diff under this row"
    );
    let first = inline_ref(&model, "c1").expect("the measured row keeps its own ref");
    let file = model
        .files
        .iter()
        .find(|f| f.path == "src/a.rs")
        .expect("the path is tracked");
    assert_eq!(
        file.diff_at(first.seq),
        Some("@@\n+c1 wrote this"),
        "and the row that did measure a change still resolves it"
    );
}

/// **Witness for the discriminator.** One `edit_file` call resolves the change
/// it made under *either* producer — the registry's per-call measurement, which
/// publishes before the result folds (#4175), and the turn boundary's sweep,
/// which publishes after (#4155/#4176).
///
/// The two orderings are the same events in a different sequence, and a fold
/// with one fixed rule gets exactly one of them right: reading `changes` leaves
/// the boundary case short by one — every mutating row diffless, which is #4155
/// as reported — while adding `+1` points the per-call case one change into the
/// future. Neither failure is visible in a fixture that emits only one
/// ordering, which is why both are asserted here against one expectation.
#[test]
fn a_row_resolves_its_own_change_under_either_producer() {
    // Measured per call: the change folds before the row that made it.
    let mut per_call = SessionModel::new();
    fold_edit(&mut per_call, "c1", "src/a.rs", "@@ per-call @@\n", 2);

    // Measured at the boundary: the row folds first and the change follows.
    let mut boundary = SessionModel::new();
    edit_call(&mut boundary, "c1", "src/a.rs");
    turn_boundary(&mut boundary, "src/a.rs", Some("@@ boundary @@\n"), 2, 0);

    for (label, model, diff) in [
        ("per-call", &per_call, "@@ per-call @@\n"),
        ("boundary", &boundary, "@@ boundary @@\n"),
    ] {
        let dref = inline_ref(model, "c1").unwrap_or_else(|| panic!("{label}: no ref stamped"));
        let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
        assert_eq!(
            file.diff_at(dref.seq),
            Some(diff),
            "{label}: the row must resolve the change its own call made"
        );
        assert_eq!(
            file.delta_at(dref.seq),
            Some((2, 0)),
            "{label}: and the measurement that rides with it"
        );
    }
}
