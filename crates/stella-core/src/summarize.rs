//! The overflow summarizer's prompt and span rendering — the pure half of
//! `driver.rs`'s summarize-on-overflow fallback, split out so the driver
//! stays small and the render logic is testable alone.

use stella_protocol::{CompletionMessage, MessageRole, ToolOutput};

/// System prompt of the overflow summarizer. Byte-stable const: the
/// summarizer's own request is tiny, but stability costs nothing and keeps
/// its prefix cacheable across repeated overflow events in one session.
pub(crate) const SUMMARIZE_SYSTEM: &str = "You are compacting an agent work log. Write a dense summary of \
    the work so far that a coding agent can resume from: the goal, key decisions and why, files \
    touched (exact paths) and what changed in each, commands run with outcomes, errors seen and \
    how they were resolved, and anything explicitly left unresolved. Short bullet lines. No \
    preamble — the summary text only.";

/// Per-item caps for [`render_span_for_summary`]. The summarizer needs the
/// shape of the work, not the bytes: full file dumps in tool results are
/// exactly what overflowed in the first place.
const SUMMARY_TEXT_CAP: usize = 600;
const SUMMARY_RESULT_CAP: usize = 300;
/// Whole-render cap — half of a typical small-model context, leaving room
/// for the summarizer's own output.
const SUMMARY_RENDER_CAP: usize = 60_000;

/// Truncate `s` to `cap` UTF-8 **bytes**, walked back to a char boundary, with
/// an elision marker appended when it was cut. Bytes (not chars) because every
/// cap in this module is a proxy for request size; the name is historical and
/// the boundary walk is what keeps it safe on multi-byte input.
pub(crate) fn cap_chars(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let mut end = cap;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}[…]", &s[..end])
}

/// What a dropped head is replaced by, so the summarizer is told it is
/// looking at the newer part of a longer log rather than the whole of a
/// short one — and so a reader of the request can tell the two apart.
const DROPPED_HEAD_MARKER: &str = "[older history dropped: it exceeded the summarizer's own \
                                   context window and was never summarized]\n";

/// The newer half of an already-rendered span, with its older head dropped.
///
/// The one branch reactive overflow recovery could not repair is a
/// *summarizer request* that itself overflows: the call that exists to shrink
/// the transcript is rejected for being too large, so the span is left intact
/// and the turn aborts (#2751). Dropping the head is the only lever left — the
/// content is lost unsummarized, which is why the splice marker says so.
///
/// The cut lands on the next line boundary at or after the halfway byte, so
/// the tail opens on a whole rendered record rather than mid-way through one;
/// falling back to the next char boundary when the half has no newline left
/// in it. `None` when the drop would not actually shrink the request — an
/// empty render, a half that leaves no tail, or one so short that the marker
/// costs more than the head saved — so a caller looping on this always
/// terminates.
pub(crate) fn drop_span_head(rendered: &str) -> Option<String> {
    let half = rendered.len() / 2;
    if half == 0 {
        return None;
    }
    let cut = match rendered[half..].find('\n') {
        Some(offset) => half + offset + 1,
        None => {
            let mut cut = half;
            while !rendered.is_char_boundary(cut) {
                cut += 1;
            }
            cut
        }
    };
    let tail = &rendered[cut..];
    if tail.is_empty() {
        return None;
    }
    let shorter = format!("{DROPPED_HEAD_MARKER}{tail}");
    (shorter.len() < rendered.len()).then_some(shorter)
}

/// Flatten a message span into the summarizer's input: roles, text, tool
/// calls with their inputs, and truncated results — enough to reconstruct
/// WHAT happened without re-shipping the content that overflowed.
pub(crate) fn render_span_for_summary(span: &[CompletionMessage]) -> String {
    let mut out = String::new();
    for message in span {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        if !message.content.trim().is_empty() {
            out.push_str(&format!(
                "{role}: {}\n",
                cap_chars(message.content.trim(), SUMMARY_TEXT_CAP)
            ));
        }
        for call in &message.tool_calls {
            out.push_str(&format!(
                "{role} → {}({})\n",
                call.name,
                cap_chars(&call.input.to_string(), SUMMARY_RESULT_CAP)
            ));
        }
        for result in &message.tool_results {
            let (tag, body) = match &result.output {
                ToolOutput::Ok { content, .. } => ("ok", content),
                ToolOutput::Error { message, .. } => ("error", message),
            };
            out.push_str(&format!(
                "  ← {tag}: {}\n",
                cap_chars(body.trim(), SUMMARY_RESULT_CAP)
            ));
        }
        if out.len() > SUMMARY_RENDER_CAP {
            out = cap_chars(&out, SUMMARY_RENDER_CAP);
            out.push_str("\n[span truncated]");
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{DROPPED_HEAD_MARKER, drop_span_head};

    /// The cut keeps the newer tail, opens it on a whole rendered record,
    /// and names what went — and it converges, which is what lets the caller
    /// loop on it (#2751).
    #[test]
    fn dropping_a_head_keeps_the_newer_tail_and_converges() {
        let rendered: String = (0..40).map(|i| format!("assistant: line {i}\n")).collect();
        let once = drop_span_head(&rendered).expect("a long render has a head to drop");
        assert!(once.starts_with(DROPPED_HEAD_MARKER));
        assert!(once.len() < rendered.len());
        // The tail opens on a whole record and still carries the newest one.
        let tail = once.strip_prefix(DROPPED_HEAD_MARKER).unwrap();
        assert!(tail.starts_with("assistant: line "), "{tail}");
        assert!(tail.ends_with("assistant: line 39\n"), "{tail}");
        assert!(!once.contains("line 0\n"), "the oldest record is gone");

        // Repeated dropping terminates rather than growing on the marker.
        let mut current = once;
        while let Some(shorter) = drop_span_head(&current) {
            assert!(shorter.len() < current.len());
            current = shorter;
        }
    }

    /// Nothing worth dropping answers `None`, so a caller looping on this
    /// never spins on a render it cannot shrink.
    #[test]
    fn a_render_with_no_head_to_drop_is_refused() {
        assert_eq!(drop_span_head(""), None);
        assert_eq!(drop_span_head("assistant: hi\n"), None);
    }

    /// The halfway byte can land inside a multi-byte char; the cut walks to
    /// a boundary rather than panicking on a slice.
    #[test]
    fn a_multibyte_render_cuts_on_a_char_boundary() {
        let rendered = "форматировать".repeat(40);
        let shorter = drop_span_head(&rendered).expect("a long render can shed a head");
        assert!(shorter.len() < rendered.len());
    }
}
