// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for [`super`], the transcript's event-source mapping.
//!
//! Split out of `transcript_source.rs` when that file crossed the gate's
//! 1500-line ceiling: the production half is ~520 lines and the tests were
//! twice that, so the tests are what moved. A first-time crossing is never
//! given a baseline entry (AGENTS.md, "God files"), so the remedy is a split.

use super::*;
use crate::model::SessionModel;
use stella_protocol::{AgentEvent, FileChangeKind, ToolCall, ToolOutput};
use stella_tui_theme::{glyph, token};

fn text_of(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.clone()).collect()
}

/// Every row of a head, joined — a head is one row today, but an assertion
/// that a number is absent must not pass merely by looking at the wrong
/// row if that ever changes.
fn text_of_rows(rows: &[Line<'static>]) -> String {
    rows.iter().map(text_of).collect::<Vec<_>>().join("\n")
}

/// One path's measured delta — the shape every single-file mutation
/// resolves to.
fn one_file(added: u32, removed: u32) -> Touched {
    Touched {
        files: 1,
        extent: Extent::delta(added, removed),
    }
}

/// The witness for #5030's fold half: a read head folds by default
/// (SPEC 6.3) — `▸` with `↵ open` — and the reader's expand opens it.
/// The live path used to force `collapsed = Some(false)` on every head,
/// so no live read ever folded and `↵ open` was reachable only from
/// fixtures.
#[test]
fn a_read_head_folds_by_default_and_opens_on_expand() {
    let folded = text_of_rows(&head_rows(
        "read_file",
        Some("src/lib.rs"),
        "{}",
        CallFacts::default(),
        120,
    ));
    assert!(folded.contains('▸'), "{folded}");
    assert!(folded.contains("↵ open"), "{folded}");
    let opened = text_of_rows(&head_rows(
        "read_file",
        Some("src/lib.rs"),
        "{}",
        CallFacts {
            expanded: true,
            ..Default::default()
        },
        120,
    ));
    assert!(!opened.contains('▸'), "{opened}");
    assert!(!opened.contains("↵ open"), "{opened}");
    // Every other kind keeps its own glyph either way — an edit head does
    // not fold, because its body is the result row beneath it.
    let edit = text_of_rows(&head_rows(
        "edit_file",
        Some("src/lib.rs"),
        "{}",
        CallFacts::default(),
        120,
    ));
    assert!(!edit.contains('▸'), "{edit}");
}

/// The witness for #5030's timing half: a settled call's head states the
/// wall time its paired result measured, `⚡7ms`; an unanswered call and a
/// zero-stamped synthetic echo render no metric at all.
#[test]
fn a_settled_call_head_carries_its_wall_time() {
    let timed = text_of_rows(&head_rows(
        "edit_file",
        Some("src/lib.rs"),
        "{}",
        CallFacts {
            duration_ms: Some(7),
            ..Default::default()
        },
        120,
    ));
    assert!(timed.contains("⚡7ms"), "{timed}");
    let unanswered = text_of_rows(&head_rows(
        "edit_file",
        Some("src/lib.rs"),
        "{}",
        CallFacts::default(),
        120,
    ));
    assert!(!unanswered.contains('⚡'), "{unanswered}");
    // And the resolver itself: the paired result's stamp, zero mapped to
    // `None`, the scan bounded at the turn's close.
    let result = |id: &str, ms: u64| TranscriptEntry::ToolResult {
        call_id: id.into(),
        name: "edit_file".into(),
        path: None,
        ok: true,
        summary: String::new(),
        full: String::new(),
        duration_ms: ms,
        speculated: false,
        diff: Vec::new(),
        read_size: None,
        graph: None,
        sub_agent_id: None,
    };
    assert_eq!(call_duration("c1", &[result("c1", 7)]), Some(7));
    assert_eq!(call_duration("c1", &[result("c1", 0)]), None);
    assert_eq!(call_duration("c1", &[result("c2", 7)]), None);
}

/// One settled model call, as the driver meters it — the shape
/// `stella-core`'s `driver::settlement::emit_step_usage` emits once per
/// call. Every field the row reads is a parameter, so a test that changes
/// one says which fact it is changing.
fn metered(
    role: stella_protocol::ModelCallRole,
    output_tokens: u64,
    duration_ms: u64,
    complete: bool,
) -> AgentEvent {
    AgentEvent::StepUsage {
        step: 0,
        turn_instance: Some(0),
        call_seq: Some(0),
        role,
        provider: "openrouter".into(),
        upstream_provider: None,
        output_text: None,
        model: "glm-5.2".into(),
        input_tokens: 4_000,
        output_tokens,
        cached_input_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: None,
        estimated_input_tokens: 0,
        cost_usd: 0.01,
        duration_ms,
        retries: 0,
        tool_calls: 1,
        complete,
        finish_reason: None,
        effort: None,
        max_output_tokens: None,
        temperature: None,
        params: None,
        sub_agent_id: None,
        task_id: None,
    }
}

/// The whole live chain for one model call: the driver's metering record,
/// through the fold, out of the deck's own renderer.
///
/// `render::entry_lines` rather than [`model_rows`] directly,
/// because the half that was missing was never the projection — it was
/// everything between the wire and it.
fn rendered_model_rows(usage: &AgentEvent) -> String {
    let mut model = SessionModel::new();
    model.apply(usage);
    let rows: Vec<_> = model
        .transcript
        .iter()
        .filter(|entry| matches!(entry, TranscriptEntry::Model { .. }))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "one metering record must fold to exactly one model row: {:?}",
        model.transcript
    );
    let mut out = Vec::new();
    crate::render::entry_lines(
        rows[0],
        crate::render::EntryView::of(&model.files),
        false,
        false,
        false,
        120,
        &mut out,
    );
    text_of_rows(&out)
}

/// **The witness (#5033).** A live turn shows SPEC 6.3's `◐ model` head
/// with a real tok/s figure, derived from the driver's own metering record
/// rather than invented.
///
/// Nothing produced this row before: `EventKind::Model` was reachable from
/// fixtures only, so a turn's most expensive work — the generation the
/// deterministic-first thesis is measured against (SPEC 1) — left no mark
/// on the transcript at the moment it happened.
///
/// 420 output tokens over 5 seconds is 84 tok/s, and the arithmetic is
/// what is asserted: the rate this row reports and the numbers the driver
/// measured have to be one reading.
#[test]
fn a_settled_model_call_renders_its_rate_from_the_drivers_own_metering() {
    let text = rendered_model_rows(&metered(
        stella_protocol::ModelCallRole::Worker,
        420,
        5_000,
        true,
    ));
    assert!(text.contains(glyph::RUNNING), "{text}");
    assert!(text.contains("model worker"), "{text}");
    assert!(text.contains("84 tok/s"), "{text}");
    // The call's own wall clock, which `Event::duration_ms` renders for
    // every kind — this row gets it for free and must not drop it.
    assert!(text.contains("⚡5000ms"), "{text}");
}

/// The footer states the half of SPEC 6.3's wording that has a source, and
/// **only** that half.
///
/// `irreducible generation` is a fact about the call. The
/// `n of m budgeted model calls this turn` clause is elided because
/// nothing budgets model calls per turn — `EngineConfig::max_steps` is a
/// declared backstop, not a plan — and a fabricated `m` on the one row
/// that prices model work would be worse than no clause. See
/// `MODEL_FOOTER`.
#[test]
fn the_model_footer_claims_no_budget_nobody_set() {
    let text = rendered_model_rows(&metered(
        stella_protocol::ModelCallRole::Worker,
        420,
        5_000,
        true,
    ));
    assert!(text.contains("irreducible generation"), "{text}");
    for fabricated in ["budgeted", " of ", "200"] {
        assert!(
            !text.contains(fabricated),
            "the footer states `{fabricated}`, which no source in this \
             workspace can back: {text}"
        );
    }
}

/// A rate with nothing to divide renders as no column, never `0 tok/s`.
///
/// Three ways the division fails, and the row still draws for all three: a
/// model call happened, and that is the fact the row exists to state.
/// `0 tok/s` would assert the model generated nothing per second, the same
/// substitution `+0 -0` made over a real edit (#4150).
#[test]
fn a_call_whose_rate_has_no_source_states_no_rate() {
    use stella_protocol::ModelCallRole::Worker;
    for (label, usage) in [
        (
            "an envelope the provider did not vouch for",
            metered(Worker, 420, 5_000, false),
        ),
        ("an untimed call", metered(Worker, 420, 0, true)),
        (
            "a call that generated nothing",
            metered(Worker, 0, 5_000, true),
        ),
    ] {
        let text = rendered_model_rows(&usage);
        // No column, which subsumes the `0 tok/s` this exists to refuse:
        // there is no rate the row could print that would be a
        // measurement.
        assert!(
            !text.contains("tok/s"),
            "{label} reported a rate nobody measured: {text}"
        );
        assert!(
            text.contains("model"),
            "{label} lost the row entirely — the call still happened: {text}"
        );
    }
}

/// A role this build cannot identify names no activity, rather than
/// printing `unknown` as though the engine had a stage by that name.
///
/// The same refusal the turn rule's model name makes for that role
/// (#4124): a legacy stream keeps its blank.
#[test]
fn a_call_with_no_recorded_role_names_no_activity() {
    let text = rendered_model_rows(&metered(
        stella_protocol::ModelCallRole::Unknown,
        420,
        5_000,
        true,
    ));
    assert!(!text.contains("unknown"), "{text}");
    assert!(text.contains("model · 84 tok/s"), "{text}");
}

/// An unknown tool still renders — the vocabulary is open (MCP, custom
/// tools), and a missing row is the failure this guards against.
#[test]
fn an_unrecognised_tool_still_renders_a_head() {
    let rows = head_rows(
        "mcp__fs__read_file",
        None,
        "apps/page.tsx",
        CallFacts::default(),
        80,
    );
    assert_eq!(rows.len(), 1);
    let text = text_of(&rows[0]);
    // The tool's own name survives; the routing prefix does not. A reader
    // wants to know what happened, and `mcp__fs__` is how the call was
    // addressed rather than what it did.
    assert!(text.contains("read_file"), "{text}");
    assert!(
        !text.contains("mcp__fs__"),
        "the routing prefix leaked into the row: {text}"
    );
    assert!(text.contains("apps/page.tsx"), "{text}");
}

/// The witness for #4699: a delegate's call renders visibly apart from
/// the lead's, and the lead's own renders no tag at all.
#[test]
fn a_delegates_head_names_the_delegate() {
    let delegated = text_of_rows(&head_rows(
        "bash",
        None,
        "ls",
        CallFacts {
            sub_agent_id: Some("d:1".into()),
            ..Default::default()
        },
        80,
    ));
    assert!(
        delegated.contains("d:1"),
        "the delegate's own call did not name it: {delegated}"
    );
    let lead = text_of_rows(&head_rows("bash", None, "ls", CallFacts::default(), 80));
    assert!(
        !lead.contains("d:1"),
        "the lead's own call must carry no delegate tag: {lead}"
    );
}

/// A file tool names its path, not its raw argument blob.
#[test]
fn a_file_tool_names_its_path() {
    let rows = head_rows(
        "read_file",
        Some("src/main.rs"),
        "{\"path\":\"…\"}",
        CallFacts::default(),
        80,
    );
    let text = text_of(&rows[0]);
    assert!(text.contains("read src/main.rs"), "{text}");
    assert!(!text.contains('{'), "the raw argument blob must not leak");
}

/// A dispatched call has measured nothing yet, so its head states no size.
///
/// The zeros this guards against were not cosmetic: `edit <path> +0 -0`
/// rode over every real edit in the deck, and `+0 -0` is a *claim* — that
/// the tool ran and changed nothing — sitting on the one row a reader
/// scans to find out what a turn touched. The same substitution had
/// already shipped once in the files panel and been removed there for this
/// reason ([`crate::deck::FileLedger`]), and `AgentEvent::FileChange`'s own
/// doc names that instance (#2290) while forbidding the repair that looks
/// easiest: deriving the counts from the tool's *input* or its result text.
/// This row therefore states no number at all rather than a wrong one
/// (#4150).
#[test]
fn a_dispatched_head_states_no_size_it_has_not_measured() {
    for (tool, path) in [
        ("edit_file", "crates/stella-tools/Cargo.toml"),
        ("read_file", "src/main.rs"),
        ("write_file", "src/new.rs"),
        ("delete_file", "src/old.rs"),
    ] {
        let text = text_of_rows(&head_rows(
            tool,
            Some(path),
            "{}",
            CallFacts::default(),
            120,
        ));
        for zero in ["+0", "-0", "0 lines"] {
            assert!(
                !text.contains(zero),
                "`{tool}` head fabricates `{zero}` before its result exists: {text}"
            );
        }
        assert!(text.contains(path), "{text}");
    }
}

/// Which half of a measurement a verb states is a property of the verb —
/// an edit's two numbers are one reading, a write states what it wrote, a
/// deletion what it removed.
#[test]
fn a_write_states_what_it_wrote_and_a_delete_what_it_removed() {
    let write = text_of_rows(&head_rows(
        "write_file",
        Some("src/new.rs"),
        "{}",
        CallFacts {
            scope: Some(one_file(42, 0)),

            ..Default::default()
        },
        120,
    ));
    assert!(write.contains("new file"), "{write}");
    assert!(write.contains("42 lines"), "{write}");
    let delete = text_of_rows(&head_rows(
        "delete_file",
        Some("src/old.rs"),
        "{}",
        CallFacts {
            scope: Some(one_file(0, 17)),

            ..Default::default()
        },
        120,
    ));
    assert!(delete.contains("-17 lines"), "{delete}");
    assert!(delete.contains("u undo"), "{delete}");
}

/// Absent counts are not the same as an absent row: `write` still says the
/// file is new and `delete` still says it is recoverable, because neither
/// is a measurement.
#[test]
fn an_unmeasured_head_keeps_the_facts_that_are_not_measurements() {
    let write = text_of_rows(&head_rows(
        "write_file",
        Some("src/new.rs"),
        "{}",
        CallFacts::default(),
        120,
    ));
    assert!(write.contains("new file"), "{write}");
    let delete = text_of_rows(&head_rows(
        "delete_file",
        Some("src/old.rs"),
        "{}",
        CallFacts::default(),
        120,
    ));
    assert!(delete.contains("git-backed"), "{delete}");
    assert!(delete.contains("u undo"), "{delete}");
}

/// The whole chain, driven by the events a real session emits: dispatch,
/// result, and the turn boundary's measurement of the tree.
///
/// `mutations` is `(call_id, path, added, removed)` per `edit_file` call.
/// Every `FileChange` follows every `ToolResult`, which is the **real**
/// producer ordering and not a convenience: `stella-pipeline` emitted one
/// change per adoption during delivery and is deleted (#3865), leaving
/// `stella_cli::turn_files::emit_shared_tree_changes` — one aggregate
/// `--numstat` change per path, at the boundary, after every result of the
/// turn has folded (#4155). A fixture that measured earlier would prove the
/// head fills in under an ordering the product does not produce.
fn edited(mutations: &[(&str, &str, u32, u32)]) -> SessionModel {
    let mut model = SessionModel::new();
    for (call_id, path, ..) in mutations {
        model.apply(&AgentEvent::ToolStart {
            call: ToolCall {
                call_id: (*call_id).into(),
                name: "edit_file".into(),
                input: serde_json::json!({ "path": path }),
            },
            sub_agent_id: None,
            task_id: None,
        });
        model.apply(&AgentEvent::ToolResult {
            call_id: (*call_id).into(),
            output: ToolOutput::Ok {
                content: format!("replaced 1 occurrence(s) in {path}"),
                data: None,
            },
            duration_ms: 2,
            speculated: false,
            sub_agent_id: None,
            task_id: None,
        });
    }
    for (_, path, added, removed) in mutations {
        model.apply(&AgentEvent::FileChange {
            path: (*path).into(),
            kind: FileChangeKind::Modified,
            added: *added,
            removed: *removed,
            diff: Some(format!("@@ -1,1 +1,1 @@\n+{path}")),
            minimal: true,
            task_id: None,
        });
    }
    model
}

/// Render the head at `idx` the way the deck's fold does — the entry, plus
/// everything after it, plus the ledger.
fn head_at(model: &SessionModel, idx: usize) -> String {
    let TranscriptEntry::ToolStart {
        call_id,
        name,
        input,
        path,
        ..
    } = &model.transcript[idx]
    else {
        panic!("entry {idx} is not a head: {:?}", model.transcript[idx]);
    };
    let scope = measured_scope(call_id, &model.transcript[idx + 1..], &model.files);
    let read = read_size(call_id, &model.transcript[idx + 1..]);
    let graph = graph_fact(call_id, &model.transcript[idx + 1..]);
    text_of_rows(&head_rows(
        name,
        path.as_deref(),
        input,
        CallFacts {
            scope,
            read,
            graph,
            ..Default::default()
        },
        120,
    ))
}

/// One mutation driven through the real fold: the announcement, then a
/// result carrying whatever structured `data` the producer published.
fn mutated(name: &str, path: &str, data: Option<serde_json::Value>) -> SessionModel {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: name.into(),
            input: serde_json::json!({ "path": path }),
        },
        sub_agent_id: None,
        task_id: None,
    });
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: ToolOutput::Ok {
            content: format!("did {name} on {path}"),
            data,
        },
        duration_ms: 2,
        speculated: false,
        sub_agent_id: None,
        task_id: None,
    });
    model
}

/// One graph-facts payload for `path`.
fn facts(path: &str, fact: serde_json::Value) -> serde_json::Value {
    let mut fact = fact;
    fact["path"] = serde_json::Value::String(path.to_string());
    serde_json::json!({ "graph_facts": [fact] })
}

/// SPEC 6.3's write footer, end to end: the producer publishes the
/// registration, the fold carries it, and the head states it. The string
/// existed nowhere in the tree before #5034.
#[test]
fn a_write_that_registered_a_node_states_it_in_its_footer() {
    let model = mutated(
        "write_file",
        "src/fresh.rs",
        Some(facts(
            "src/fresh.rs",
            serde_json::json!({ "fact": "registered" }),
        )),
    );
    let head = head_at(&model, 0);
    assert!(
        head.contains("registered in graph as module node"),
        "{head}"
    );
}

/// SPEC 6.3's delete body: the count the pre-execution check measured,
/// tagged `det` because a graph query reaches no model (SPEC §5). The
/// noun agrees with the number — a row that reads `1 inbound refs` is a
/// row a reader stops trusting.
#[test]
fn a_deletion_states_the_inbound_count_its_check_measured() {
    for (inbound, expected) in [
        (0, "graph check: 0 inbound refs · det"),
        (1, "1 inbound ref "),
    ] {
        let model = mutated(
            "delete_file",
            "src/old.rs",
            Some(facts(
                "src/old.rs",
                serde_json::json!({ "fact": "inbound_refs", "inbound": inbound }),
            )),
        );
        let head = head_at(&model, 0);
        assert!(head.contains(expected), "{inbound}: {head}");
    }
}

/// A workspace with no code graph publishes no fact, and the row states
/// nothing rather than a `0 inbound refs` nobody measured.
#[test]
fn a_mutation_with_no_graph_fact_states_no_graph_line() {
    for name in ["write_file", "delete_file"] {
        let head = head_at(&mutated(name, "src/lonely.rs", None), 0);
        assert!(!head.contains("graph check"), "{name}: {head}");
        assert!(!head.contains("registered in graph"), "{name}: {head}");
    }
}

/// A batch publishes one fact per file, so a row takes the one naming its
/// own path and never a neighbour's.
#[test]
fn a_row_takes_the_fact_naming_its_own_path() {
    let payload = serde_json::json!({ "graph_facts": [
        { "fact": "inbound_refs", "path": "src/a.rs", "inbound": 7 },
        { "fact": "inbound_refs", "path": "src/b.rs", "inbound": 2 },
    ]});
    assert_eq!(
        crate::model::GraphFact::from_data(&payload, "src/b.rs"),
        Some(crate::model::GraphFact::InboundRefs(2))
    );
    assert_eq!(
        crate::model::GraphFact::from_data(&payload, "src/c.rs"),
        None
    );
}

/// The witness for #4154: a head that used to be drawn once at dispatch and
/// never revisited now states the size of the change its own call made.
///
/// The number is the **emitter's**, resolved through `FileState::delta_at`
/// — never counted out of the tool's input (`edit_file` with `replace_all`
/// makes that wrong outright) and never out of the rendered diff, which is
/// a bounded view of the changed region. That is the substitution #2290
/// established as the defect and `AgentEvent::FileChange`'s own doc
/// forbids.
#[test]
fn a_returned_edit_head_states_the_delta_the_emitter_measured() {
    let model = edited(&[("c1", "crates/stella-tools/src/lib.rs", 3, 1)]);
    let head = head_at(&model, 0);
    assert!(
        head.contains("+3") && head.contains("-1"),
        "the head must state the measured change: {head}"
    );
}

/// The witness for #4297: a returned read head states the line count the
/// tool reported — through the structured `data` payload `read_file`
/// ships, never by recounting the rendered body, which is capped at
/// `OUTPUT_BUDGET` and was #2290's defect for mutation counts. A whole
/// read states `n lines`; a truncated one states `n of m lines`, so a
/// partial read is visibly partial.
#[test]
fn a_returned_read_head_states_the_line_count_the_tool_reported() {
    let read = |data: serde_json::Value| {
        let mut model = SessionModel::new();
        model.apply(&AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({ "path": "crates/stella-core/src/lifecycle.rs" }),
            },
            sub_agent_id: None,
            task_id: None,
        });
        model.apply(&AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: ToolOutput::ok_with_data("     1\tfn main() {}".to_string(), data),
            duration_ms: 3,
            speculated: false,
            sub_agent_id: None,
            task_id: None,
        });
        head_at(&model, 0)
    };

    let whole = read(serde_json::json!({ "lines_shown": 221, "lines_total": 221 }));
    assert!(
        whole.contains("· 221 lines"),
        "a whole read states its one count: {whole}"
    );
    assert!(
        !whole.contains("of 221"),
        "equal counts must not render as a truncation: {whole}"
    );

    let truncated = read(serde_json::json!({ "lines_shown": 200, "lines_total": 500 }));
    assert!(
        truncated.contains("· 200 of 500 lines"),
        "a truncated read is visibly truncated: {truncated}"
    );
}

/// And the read head stays sizeless until its call returns, exactly as
/// the in-flight edit head below does: nothing has reported a coverage
/// yet, so there is no column, not a zero.
#[test]
fn a_read_head_whose_call_has_not_returned_states_no_count() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "read_file".into(),
            input: serde_json::json!({ "path": "src/x.rs" }),
        },
        sub_agent_id: None,
        task_id: None,
    });
    let head = head_at(&model, 0);
    assert!(!head.contains("lines"), "{head}");
    assert!(head.contains("src/x.rs"), "{head}");
}

/// And it stays silent until then. The in-flight head is the case #4150
/// fixed and this must not undo: a call that has not returned has measured
/// nothing, and `+0 -0` over a real edit is a claim that it changed
/// nothing.
#[test]
fn a_head_whose_call_has_not_returned_still_states_no_size() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "edit_file".into(),
            input: serde_json::json!({ "path": "src/x.rs" }),
        },
        sub_agent_id: None,
        task_id: None,
    });
    let head = head_at(&model, 0);
    for zero in ["+0", "-0", "+", "-"] {
        assert!(
            !head.contains(zero),
            "an in-flight head states `{zero}`: {head}"
        );
    }
    assert!(head.contains("src/x.rs"), "{head}");
}

/// Correlation is by `call_id`, not "the newest change in the ledger": two
/// calls in one turn each state their own path's measurement.
///
/// The failure this pins is the one `resolve_inline_diff`'s doc names —
/// a row wearing a neighbour's numbers — and it is invisible in a
/// single-call fixture, where every wrong join gives the right answer.
#[test]
fn each_head_states_its_own_calls_change_not_a_neighbours() {
    let model = edited(&[("c1", "src/a.rs", 3, 1), ("c2", "src/b.rs", 20, 7)]);
    let first = head_at(&model, 0);
    let second = head_at(&model, 2);
    assert!(
        first.contains("+3") && first.contains("-1"),
        "first head: {first}"
    );
    assert!(
        second.contains("+20") && second.contains("-7"),
        "second head: {second}"
    );
    assert!(
        !first.contains("+20"),
        "the first head wears the second call's numbers: {first}"
    );
}

/// Every span of a head, as `(content, foreground)`.
fn styled_spans(rows: &[Line<'static>]) -> Vec<(String, Option<Color>)> {
    rows.iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| (span.content.to_string(), span.style.fg))
        .collect()
}

/// The witness for #4168: a path subject renders as a dim directory and a
/// bright basename, so a column of calls is scanned by file identity rather
/// than by the `crates/stella-tui/src/…` prefix every one of them shares.
#[test]
fn a_path_subject_splits_dim_directory_from_bright_basename() {
    let spans = styled_spans(&head_rows(
        "read_file",
        Some("a/b/c.rs"),
        "{}",
        CallFacts::default(),
        120,
    ));
    let cut = spans
        .iter()
        .position(|(content, _)| content.ends_with("a/b/"))
        .unwrap_or_else(|| panic!("the path is not split at its last separator: {spans:?}"));
    assert_eq!(
        spans[cut],
        (" a/b/".to_string(), Some(token::DIM)),
        "the directory is context and recedes: {spans:?}"
    );
    assert_eq!(
        spans.get(cut + 1),
        Some(&("c.rs".to_string(), Some(token::TEXT))),
        "the basename is the identity the eye is hunting: {spans:?}"
    );
}

/// And the half that stops the fix over-reaching: a `bash` head's subject is
/// a command line, so a slash inside it names no file this row touched and
/// must not be emphasised. Derived from the call's `path` argument, never
/// from the presence of a separator.
#[test]
fn a_command_subject_stays_one_unemphasised_span() {
    let spans = styled_spans(&head_rows(
        "bash",
        None,
        "grep -r foo/ .",
        CallFacts::default(),
        120,
    ));
    let subject: Vec<_> = spans
        .iter()
        .filter(|(content, _)| content.contains("foo/"))
        .collect();
    assert_eq!(
        subject.len(),
        1,
        "the command was split as though it named a file: {spans:?}"
    );
    assert_eq!(subject[0].0, " grep -r foo/ .", "{spans:?}");
    assert_eq!(
        subject[0].1,
        Some(token::TEXT),
        "a command keeps the text tone: {spans:?}"
    );
}

/// A measured extent still renders its numbers — the fix removes the
/// fabrication, not the column.
#[test]
fn a_measured_edit_still_renders_its_delta() {
    let mut event = Event::new(
        EventKind::Edit {
            extent: Extent::delta(3, 1),
        },
        "src/lib.rs",
    );
    event.collapsed = Some(false);
    let text = text_of_rows(&event_rows(&event, 120));
    assert!(text.contains("+3"), "{text}");
    assert!(text.contains("-1"), "{text}");
}

/// The witness for #4180: a kind carries an [`Extent`] only where
/// [`kind_for`] can fill one.
///
/// Declaration and behaviour are both asserted from the production
/// constructor, so neither can drift into a claim the other does not back.
/// Whether a kind *declares* a size at all is read off the value's `Debug`
/// projection, because field presence is the one property of an enum
/// option a test cannot otherwise observe without naming the field, and
/// naming it is exactly what must stop compiling. The behaviour is that a
/// kind which declares a size renders something different
/// once a measurement exists.
///
/// `read_file` failed the left half for as long as `EventKind::Read` had an
/// `extent`: [`kind_for`] handed it `Extent::default()` unconditionally
/// because only a *mutation* stamps the inline-diff reference
/// [`measured_scope`] resolves through, so the column was expressible,
/// unreachable, and reached by nothing but a fixture.
#[test]
fn a_size_field_exists_only_where_a_producer_fills_it() {
    for (tool, path) in [
        ("read_file", "src/read.rs"),
        ("edit_file", "src/edit.rs"),
        ("write_file", "src/new.rs"),
        ("delete_file", "src/old.rs"),
    ] {
        let declares_a_size = format!("{:?}", kind_for(tool, None, None)).contains("Extent");
        let unmeasured = text_of_rows(&head_rows(
            tool,
            Some(path),
            "{}",
            CallFacts::default(),
            120,
        ));
        let measured = text_of_rows(&head_rows(
            tool,
            Some(path),
            "{}",
            CallFacts {
                scope: Some(one_file(7, 3)),

                ..Default::default()
            },
            120,
        ));
        assert_eq!(
            declares_a_size,
            measured != unmeasured,
            "`{tool}` declares a size column its producer cannot fill \
             (declares: {declares_a_size}), so the row states the same \
             thing measured as unmeasured: {measured}"
        );
    }
}

/// The three calls #4125 named, and the three cells that have to tell them
/// apart.
///
/// `get_state` is an inspection, `mcp__github__create_pull_request` an
/// opaque external call and `delegate` a hand-off. All three are
/// `EventKind::Other`, so all three rail gold — which leaves every one of
/// them drawing the same `●` now that class-coloured tool names are gone.
/// The glyph is the only cell left that
/// can carry the distinction.
const CLASS_CASES: [(&str, char); 4] = [
    ("get_state", glyph::TOOL_INSPECT),
    ("save_state", glyph::TOOL_MUTATE),
    ("mcp__github__create_pull_request", glyph::TOOL_EXECUTE),
    ("delegate", glyph::TOOL_DELEGATE),
];

/// The glyph cell of a head row: the rail is span 0, the glyph span 1
/// (`" x "`), which `head_row` composes in that order.
fn head_glyph_of(name: &str) -> char {
    let rows = head_rows(name, None, "{}", CallFacts::default(), 120);
    let row = rows
        .first()
        .expect("a head always renders at least one row");
    let cell = row.spans[1].content.trim().to_string();
    let mut chars = cell.chars();
    let glyph = chars
        .next()
        .unwrap_or_else(|| panic!("`{name}` drew no glyph"));
    assert_eq!(
        chars.next(),
        None,
        "`{name}` drew more than one glyph: {cell:?}"
    );
    glyph
}

/// **The witness (#4125).** A look, a write, an opaque external call and a
/// hand-off render four *different* glyphs.
///
/// Before this they were four `●`, so a reader scanning the margin could
/// not tell a `get_state` from an MCP pull-request call from a sub-agent
/// delegation — the exact confusion the issue was filed about.
#[test]
fn a_tool_calls_class_is_legible_from_its_glyph_alone() {
    let mut seen: Vec<(char, &str)> = Vec::new();
    for (name, want) in CLASS_CASES {
        let got = head_glyph_of(name);
        assert_eq!(
            got, want,
            "`{name}` drew {got:?} rather than its class glyph {want:?}"
        );
        if let Some((_, other)) = seen.iter().find(|(ch, _)| *ch == got) {
            panic!(
                "`{name}` and `{other}` both drew {got:?} — the class is \
                 illegible in the one cell that carries it (#4125)"
            );
        }
        seen.push((got, name));
    }
}

/// The other half of the law, and the reason this fix is a *glyph* and not
/// a hue: the four classes still share one rail and spend no colour.
///
/// SPEC 2 gives the scheme two metals and spends them on **kind**. #4125
/// declined a third colour channel for class precisely because it would
/// erode that rule, so a future change that re-reaches for
/// `ToolClass::color` — the categorical `data-*` hues, which SPEC 3.2's
/// clamp rejects outright — has to fail here rather than pass quietly.
#[test]
fn a_tool_classs_glyph_spends_no_colour() {
    let banned = [
        ("teal", crate::theme::TEAL),
        ("magenta", crate::theme::MAGENTA),
        ("violet", crate::theme::VIOLET),
        ("orchid", crate::theme::ORCHID),
    ];
    for (name, _) in CLASS_CASES {
        let rows = head_rows(name, None, "{}", CallFacts::default(), 120);
        let row = rows
            .first()
            .expect("a head always renders at least one row");

        // One rail, one metal, for every class alike.
        assert_eq!(
            row.spans[0].style.fg,
            Some(token::GOLD),
            "`{name}` moved off the shared gold rail — class is not a metal"
        );
        // The glyph takes the row's metal and adds none of its own.
        assert_eq!(
            row.spans[1].style.fg,
            Some(token::GOLD),
            "`{name}`'s class glyph wears a colour of its own"
        );
        for span in &row.spans {
            for (hue, colour) in banned {
                assert_ne!(
                    span.style.fg,
                    Some(colour),
                    "`{name}` paints {:?} in the categorical `{hue}` — \
                     class came back as a hue, which #4125 declined and \
                     SPEC 3.2's clamp rejects",
                    span.content
                );
            }
        }
    }
}
