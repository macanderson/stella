//! The overflow summarizer's prompt and span rendering — the pure half of
//! `driver.rs`'s summarize-on-overflow fallback, split out so the driver
//! stays within the size ratchet and the render logic is testable alone.

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

/// Truncate `s` to `cap` chars on a char boundary with an elision marker.
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
                ToolOutput::Ok { content } => ("ok", content),
                ToolOutput::Error { message } => ("error", message),
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
