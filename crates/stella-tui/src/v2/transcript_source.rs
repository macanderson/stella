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

use ratatui::text::Line;

use super::transcript::{Event, EventKind, Receipt, event_rows, receipt, turn_end};

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
fn kind_for(name: &str) -> EventKind {
    match name {
        "read_file" => EventKind::Read { lines: 0 },
        "edit_file" => EventKind::Edit {
            added: 0,
            removed: 0,
        },
        "write_file" => EventKind::Write { lines: 0 },
        "delete_file" => EventKind::Delete { lines: 0 },
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
}
