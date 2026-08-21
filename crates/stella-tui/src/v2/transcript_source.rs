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
//! This module projects the **head**, and [`metal_for`] is how the result row
//! joins it: SPEC 6.2 makes the rail a property of the event, so both entries
//! read one metal from one table.
//!
//! The result's *rows* are deliberately not built here. That row carries syntax
//! highlighting in the file's own language, word-level inline diffs, a
//! line-number gutter and a truncation notice naming the key that reveals the
//! rest (#4019, #4020, #4036); SPEC 6.4 keeps every one of them, and a second
//! renderer written next to this one would have to be taught all four or
//! silently drop them — which is exactly why #4123 left the row alone rather
//! than replacing it. #4127 restyled the renderer that already has them instead
//! (`render::entry::tool`), and this module supplies the metal it wears.
//!
//! ## Open vocabulary
//!
//! An unrecognised tool is [`EventKind::Other`], never a missing row. The deck
//! gains MCP tools and workspace custom tools at runtime, so a renderer with an
//! arm per tool is a renderer that silently drops the ones a user added — the
//! reasoning [`stella_transcript::ToolKind`] already states.

use ratatui::text::Line;

use super::transcript::{Event, EventKind, Extent, Receipt, event_rows, receipt, turn_end};

/// The metal-bearing head of a dispatched call (SPEC 6.2).
///
/// Always at least one row: a tool with no recognised verb still names itself,
/// because a call that rendered nothing would be a call the reader cannot see
/// happened.
#[must_use]
pub fn head_rows(name: &str, path: Option<&str>, input: &str, width: usize) -> Vec<Line<'static>> {
    let kind = kind_for(name);
    let subject = subject_for(name, path, input);
    let mut event = Event::new(kind, subject);
    // The head is drawn the moment the call dispatches, so it is never
    // "collapsed" in the fold sense — there is no body under it yet.
    event.collapsed = Some(false);
    event_rows(&event, width)
}

/// The rail metal a tool's whole block wears (SPEC 6.2).
///
/// The head and the result are two [`crate::TranscriptEntry`]s and one *event*,
/// so they take one metal. Exported rather than folded into [`head_rows`]
/// because the result row is rendered by `render::entry::tool` — it carries
/// syntax highlighting, inline diffs, a gutter and a truncation notice that
/// live there — and it has to reach the same answer from the same table. A
/// second `match` on tool names beside this one is how the block came to draw
/// gold for one row and silver for the rest (#4127).
#[must_use]
pub fn metal_for(name: &str) -> ratatui::style::Color {
    kind_for(name).metal()
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
/// Every file kind takes an **unmeasured** [`Extent`], and that is the whole
/// point of the type. This function runs when the call *dispatches*: the tool
/// has not returned, no `FileChange` has been emitted, and there is nothing to
/// count. Filling the fields with zeros instead put `edit <path> +0 -0` over
/// every real edit in the deck — a row asserting the change was empty, on the
/// one screen a reader consults to find out what changed. The measured numbers
/// live on the result row (`render::entry`'s `resolve_inline_delta`), which is
/// where they can be true; this row states the verb and its subject.
fn kind_for(name: &str) -> EventKind {
    match name {
        "read_file" => EventKind::Read {
            extent: Extent::default(),
        },
        "edit_file" => EventKind::Edit {
            extent: Extent::default(),
        },
        "write_file" => EventKind::Write {
            extent: Extent::default(),
        },
        "delete_file" => EventKind::Delete {
            extent: Extent::default(),
        },
        "bash" => EventKind::Run,
        _ => EventKind::Other,
    }
}

/// The object of the verb: the path for a file tool, the command for `bash`,
/// the raw input for anything else.
fn subject_for(name: &str, path: Option<&str>, input: &str) -> String {
    match (name, path) {
        (_, Some(p)) if !p.is_empty() => p.to_string(),
        ("bash", _) => first_line(input).to_string(),
        _ => {
            let head = first_line(input);
            if head.is_empty() {
                name.to_string()
            } else {
                format!("{name} {head}")
            }
        }
    }
}

/// The first line of a blob, for a row that has exactly one.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim_end()
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
/// one line whose whole job is to be the settled account of a turn.
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
        let rows = head_rows("mcp__fs__read_file", None, "apps/page.tsx", 80);
        assert_eq!(rows.len(), 1);
        assert!(
            text_of(&rows[0]).contains("mcp__fs__read_file"),
            "{}",
            text_of(&rows[0])
        );
    }

    /// A file tool names its path, not its raw argument blob.
    #[test]
    fn a_file_tool_names_its_path() {
        let rows = head_rows("read_file", Some("src/main.rs"), "{\"path\":\"…\"}", 80);
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
            let text = text_of_rows(&head_rows(tool, Some(path), "{}", 120));
            for zero in ["+0", "-0", "0 lines"] {
                assert!(
                    !text.contains(zero),
                    "`{tool}` head fabricates `{zero}` before its result exists: {text}"
                );
            }
            assert!(text.contains(path), "{text}");
        }
    }

    /// Absent counts are not the same as an absent row: `write` still says the
    /// file is new and `delete` still says it is recoverable, because neither
    /// is a measurement.
    #[test]
    fn an_unmeasured_head_keeps_the_facts_that_are_not_measurements() {
        let write = text_of_rows(&head_rows("write_file", Some("src/new.rs"), "{}", 120));
        assert!(write.contains("new file"), "{write}");
        let delete = text_of_rows(&head_rows("delete_file", Some("src/old.rs"), "{}", 120));
        assert!(delete.contains("git-backed"), "{delete}");
        assert!(delete.contains("u undo"), "{delete}");
    }

    /// And a measured extent still renders its numbers — the fix removes the
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
