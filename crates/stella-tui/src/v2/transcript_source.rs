//! The live projection behind [`super::transcript`] — SPEC 6 over the deck's
//! own [`crate::TranscriptEntry`] stream.
//!
//! ## Why the pair maps to two calls, not one
//!
//! SPEC 6.2 describes an event as one thing: a head, a body, a footer, all
//! sharing a rail. The deck's transcript records it as two entries — a
//! [`crate::TranscriptEntry::ToolStart`] when the call is dispatched and a
//! [`crate::TranscriptEntry::ToolResult`] when it returns — because the head has to
//! render before the result exists, and a transcript that waited for the
//! result would show nothing at all while a two-minute `cargo test` ran.
//!
//! This module projects the **head** only. The result row stays on the v1
//! renderer until P2, because that row already carries syntax highlighting in
//! the file's own language, word-level inline diffs, a line-number gutter and a
//! truncation notice naming the key that reveals the rest (#4019, #4020,
//! #4036). SPEC 6.4 keeps every one of them, and a v2 result renderer that has
//! not been taught them yet would delete working features to make the screen
//! look newer. P2 builds the highlighter; the row is restyled there.
//!
//! ## Open vocabulary
//!
//! An unrecognised tool is [`EventKind::Other`], never a missing row. The deck
//! gains MCP tools and workspace custom tools at runtime, so a renderer with an
//! arm per tool is a renderer that silently drops the ones a user added — the
//! reasoning [`stella_transcript::ToolKind`] already states.

use ratatui::style::Color;
use ratatui::text::Line;

use super::transcript::{
    Event, EventKind, Extent, Receipt, Subject, TurnHead, event_rows, receipt, turn_begin, turn_end,
};
use crate::model::{FileState, TranscriptEntry};

/// The metal-bearing head of a dispatched call (SPEC 6.2).
///
/// `measured` is the `(added, removed)` the emitter reported for this call, or
/// `None` while it is still in flight — [`measured_delta`] is what resolves it,
/// and a head row is never asked to guess.
///
/// Always at least one row: a tool with no recognised verb still names itself,
/// because a call that rendered nothing would be a call the reader cannot see
/// happened.
#[must_use]
pub fn head_rows(
    name: &str,
    path: Option<&str>,
    input: &str,
    measured: Option<(u32, u32)>,
    width: usize,
) -> Vec<Line<'static>> {
    let kind = kind_for(name, measured);
    let subject = subject_for(name, path, input);
    let mut event = Event::new(kind, subject);
    // The head is drawn the moment the call dispatches, so it is never
    // "collapsed" in the fold sense — there is no body under it yet.
    event.collapsed = Some(false);
    event_rows(&event, width)
}

/// The rail metal [`head_rows`] draws this call in (SPEC 6.2) — silver-dim for
/// a read, gold for anything that acts, red for a delete.
///
/// Exposed because the head is only the first row of a call's block: the rows
/// the deck hangs *under* it — the expanded argument object — have to reproduce
/// the same rail, and deriving the colour a second time from the tool name is
/// how the two ends of one block come to disagree about what metal it is. One
/// derivation, read by both.
#[must_use]
pub fn head_metal(name: &str) -> Color {
    // The metal is a fact about the verb, so the extent is irrelevant here and
    // an unmeasured one is the honest argument rather than a placeholder: a
    // block's rail must not change colour when its size arrives.
    kind_for(name, None).metal()
}

/// The `(added, removed)` the emitter measured for the call `call_id`
/// dispatched, or `None` while nothing has measured it.
///
/// `following` is the rest of the lane's transcript *after* the head, and
/// `files` the draw-side ledger. Two hops, both deliberate:
///
/// 1. **The pair, by `call_id`.** A [`TranscriptEntry::ToolResult`] carries the
///    [`crate::model::InlineDiffRef`] naming the change its own call produced,
///    and shares its `call_id` with the head. The scan stops at the turn's
///    closing entry: a result cannot land after the turn that dispatched it
///    completed, so this bounds the walk by turn length rather than by session
///    length, and an unanswered head (a cancelled call) costs one turn's scan.
/// 2. **The measurement, through that reference.** `render::resolve_inline_delta`
///    (crate-private, so named rather than linked) reads
///    [`crate::model::FileState::delta_at`] — the counts the *emitter*
///    measured. Never the tool's input, never a recount of the rendered diff:
///    the first is what #2290 established as the defect (`edit_file` with
///    `replace_all` makes an input-derived number wrong outright), and the
///    second counts a bounded rendering of the changed region rather than the
///    change.
///
/// `None` therefore covers three honest cases — the call has not returned, it
/// failed (a failed mutation stamps no reference), or the turn boundary has not
/// measured the tree yet — and each of them renders as no column at all.
#[must_use]
pub fn measured_delta(
    call_id: &str,
    following: &[TranscriptEntry],
    files: &[FileState],
) -> Option<(u32, u32)> {
    following
        .iter()
        .take_while(|e| !matches!(e, TranscriptEntry::Complete { .. }))
        .find_map(|e| match e {
            TranscriptEntry::ToolResult {
                call_id: cid, diff, ..
            } if cid == call_id => diff.as_ref(),
            _ => None,
        })
        .and_then(|dref| crate::render::resolve_inline_delta(dref, files))
}

/// One dim line, no rail (SPEC 6.3).
#[must_use]
pub fn compaction_rows(
    before: u64,
    after: u64,
    evicted: usize,
    deduped: usize,
) -> Vec<Line<'static>> {
    event_rows(
        &Event::new(
            EventKind::Compaction {
                from_tokens: before,
                to_tokens: after,
                evicted: evicted as u32,
                deduped: deduped as u32,
            },
            String::new(),
        ),
        0,
    )
}

/// Map a wire tool name onto SPEC 6.3's event vocabulary.
///
/// Deliberately the same six names [`stella_transcript::ToolKind::from_name`]
/// recognises, so the two renderers of the same transcript cannot disagree
/// about what a `bash` row is.
///
/// `measured` is the emitter's `(added, removed)` for this call once
/// [`measured_delta`] has resolved one, and `None` for as long as nothing has
/// measured it — which is every head at the moment it dispatches, because the
/// tool has not returned and no `FileChange` has been emitted. An unmeasured
/// kind renders **no size column at all**: filling the fields with zeros
/// instead put `edit <path> +0 -0` over every real edit in the deck — a row
/// asserting the change was empty, on the one screen a reader consults to find
/// out what changed (#4150).
///
/// Which half of the pair a kind states is a property of the verb, and lives
/// here so one measurement cannot be read two ways: an edit states both sides
/// (they are one reading), a write states what it wrote, a deletion what it
/// removed. A read never resolves one at all — only a *mutation* stamps an
/// inline-diff reference, so a read's line count has no source on this path and
/// stays honestly absent (#4180).
fn kind_for(name: &str, measured: Option<(u32, u32)>) -> EventKind {
    match name {
        "read_file" => EventKind::Read {
            extent: Extent::default(),
        },
        "edit_file" => EventKind::Edit {
            extent: measured.map_or_else(Extent::default, |(added, removed)| {
                Extent::delta(added, removed)
            }),
        },
        "write_file" => EventKind::Write {
            extent: measured.map_or_else(Extent::default, |(added, _)| Extent::added(added)),
        },
        "delete_file" => EventKind::Delete {
            extent: measured.map_or_else(Extent::default, |(_, removed)| Extent::removed(removed)),
        },
        "bash" => EventKind::Run,
        // Never mapped onto one of the five above: a server's `read_file` is
        // not necessarily this workspace's `read`, and a familiar verb on a row
        // that did something else is the one error a transcript cannot afford.
        // It keeps its name as the subject instead — see `subject_for`.
        _ => EventKind::Other,
    }
}

/// The object of the verb: the path for a file tool, the command for `bash`,
/// the raw input for anything else.
fn subject_for(name: &str, path: Option<&str>, input: &str) -> Subject {
    match (name, path) {
        // The one arm that yields a path, and the only one that may: the caller
        // handed us `path`, so this is not a guess about the string's shape.
        (_, Some(p)) if !p.is_empty() => Subject::path(p),
        // A command line, not a file — even when it contains a slash.
        ("bash", _) => first_line(input).into(),
        _ => {
            // A tool this host has no verb for is named by its own label, which
            // for an MCP tool is its trailing segment: the row read
            // `mcp__fs__read_file apps/page.tsx`, and `mcp__fs__` is how the
            // call was *addressed*, not what it did.
            //
            // Stripped rather than translated. A server's `read_file` is not
            // necessarily this workspace's `read`, so the name survives and
            // only the routing goes — the most a caller can honestly do
            // without asking the server.
            let label = stella_tools::catalog::label_for(name);
            let head = first_line(input);
            if head.is_empty() {
                label.into()
            } else {
                format!("{label} {head}").into()
            }
        }
    }
}

/// The first line of a blob, for a row that has exactly one.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim_end()
}

/// A turn's opening rule (SPEC 6.1).
///
/// One row, and only on the stage boundary that **opens** the turn — see
/// [`crate::model::TurnOpening`] for why a wrapped run's four inner stages do
/// not each announce the same turn number.
///
/// `queued_steer` is deliberately never fed here. A steer that landed is
/// already its own transcript row ([`crate::TranscriptEntry::User`], prefixed
/// `(steered mid-turn)`), so naming it a second time on the rule above it would
/// report one interruption twice; and a steer *queued before* the turn opened
/// arrives as an ordinary prompt, which the deck cannot tell apart from any
/// other. The label stays unfed rather than fed something that is not it
/// (#4185).
#[must_use]
pub fn turn_begin_rows(
    turn: u32,
    stage: &str,
    model: Option<&str>,
    budget_usd: Option<f64>,
    width: usize,
) -> Vec<Line<'static>> {
    vec![turn_begin(
        &TurnHead {
            number: turn,
            stage: stage.to_string(),
            model: model.map(str::to_string),
            budget_usd,
            queued_steer: None,
        },
        width,
    )]
}

/// A turn's closing rule and its receipt (SPEC 6.1).
///
/// Two rows, always: the boundary is the transcript's rhythm — SPEC 2 makes the
/// turn its unit — so it renders whether or not the receipt has anything but
/// money to say.
///
/// Everything the receipt cannot source is elided rather than zeroed. Only
/// `spend_usd` is fed today: the deck does not fold `StepUsage` (it would
/// double-count the spend the budget gauge tracks), keeps no per-turn clock,
/// and counts no per-turn files, tests or memories. A receipt reading
/// `0 tok · 0 files · 0/0 tests` would be four measurements nobody took, on the
/// one line whose whole job is to be the settled account of a turn. Sourcing
/// the other five — `det %` above all, which SPEC 5 moved off the status bar
/// and re-homed here — is #4184.
#[must_use]
pub fn turn_end_rows(turn: u32, cost_usd: f64, width: usize) -> Vec<Line<'static>> {
    vec![
        turn_end(turn, None, width),
        receipt(&Receipt {
            spend_usd: cost_usd,
            ..Receipt::default()
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SessionModel;
    use stella_protocol::{AgentEvent, FileChangeKind, ToolCall, ToolOutput};

    fn text_of(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.clone()).collect()
    }

    /// Every row of a head, joined — a head is one row today, but an assertion
    /// that a number is absent must not pass merely by looking at the wrong
    /// row if that ever changes.
    fn text_of_rows(rows: &[Line<'static>]) -> String {
        rows.iter().map(text_of).collect::<Vec<_>>().join("\n")
    }

    /// An unknown tool still renders — the vocabulary is open (MCP, custom
    /// tools), and a missing row is the failure this guards against.
    #[test]
    fn an_unrecognised_tool_still_renders_a_head() {
        let rows = head_rows("mcp__fs__read_file", None, "apps/page.tsx", None, 80);
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

    /// A file tool names its path, not its raw argument blob.
    #[test]
    fn a_file_tool_names_its_path() {
        let rows = head_rows(
            "read_file",
            Some("src/main.rs"),
            "{\"path\":\"…\"}",
            None,
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
            let text = text_of_rows(&head_rows(tool, Some(path), "{}", None, 120));
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
            Some((42, 0)),
            120,
        ));
        assert!(write.contains("new file"), "{write}");
        assert!(write.contains("42 lines"), "{write}");
        let delete = text_of_rows(&head_rows(
            "delete_file",
            Some("src/old.rs"),
            "{}",
            Some((0, 17)),
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
            None,
            120,
        ));
        assert!(write.contains("new file"), "{write}");
        let delete = text_of_rows(&head_rows(
            "delete_file",
            Some("src/old.rs"),
            "{}",
            None,
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
            });
            model.apply(&AgentEvent::ToolResult {
                call_id: (*call_id).into(),
                output: ToolOutput::Ok {
                    content: format!("replaced 1 occurrence(s) in {path}"),
                    data: None,
                },
                duration_ms: 2,
                speculated: false,
            });
        }
        for (_, path, added, removed) in mutations {
            model.apply(&AgentEvent::FileChange {
                path: (*path).into(),
                kind: FileChangeKind::Modified,
                added: *added,
                removed: *removed,
                diff: Some(format!("@@ -1,1 +1,1 @@\n+{path}")),
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
        let measured = measured_delta(call_id, &model.transcript[idx + 1..], &model.files);
        text_of_rows(&head_rows(name, path.as_deref(), input, measured, 120))
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
}
