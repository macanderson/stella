//! Whether an incoming `Error` event is a row the transcript already shows.
//!
//! One failure can be reported twice at the engine/host seam. The pipeline
//! emits `AgentEvent::Error` for an abort it decided itself — "aborted at scope
//! review" — *and* returns `PipelineStatus::Aborted`, which the host maps to a
//! failed turn and emits as an `Error` again. The reader saw the same red row
//! twice and read it, reasonably, as two failures: two aborted attempts, or a
//! retry that failed the same way.
//!
//! The rule is deliberately narrow. Only an *adjacent* duplicate collapses, and
//! only when the retryable flag matches too:
//!
//!   - a repeat with anything in between is a genuinely separate event, and
//!     collapsing those would hide a loop — the exact failure mode a reader
//!     most needs the transcript to show;
//!   - a retryable warning and a terminal failure with the same text are
//!     different events, so the flag is part of a row's identity.
//!
//! Nothing is lost: an identical row sitting directly under its twin carries no
//! information the first one does not.

use super::TranscriptEntry;

/// Does `(message, retryable)` restate the transcript's last row verbatim?
pub(super) fn repeats_the_last_row(
    transcript: &[TranscriptEntry],
    message: &str,
    retryable: bool,
) -> bool {
    matches!(
        transcript.last(),
        Some(TranscriptEntry::Error {
            message: prev,
            retryable: prev_retryable,
        }) if prev == message && *prev_retryable == retryable
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(message: &str, retryable: bool) -> TranscriptEntry {
        TranscriptEntry::Error {
            message: message.to_string(),
            retryable,
        }
    }

    /// The reported screenshot: "aborted at scope review" twice, which read as
    /// two failed attempts at the review.
    #[test]
    fn the_identical_error_immediately_after_itself_repeats() {
        let rows = vec![err("aborted at scope review", false)];
        assert!(repeats_the_last_row(
            &rows,
            "aborted at scope review",
            false
        ));
    }

    #[test]
    fn a_different_message_is_not_a_repeat() {
        let rows = vec![err("aborted at scope review", false)];
        assert!(!repeats_the_last_row(&rows, "provider timeout", false));
    }

    /// A retryable warning and a terminal failure with the same text are
    /// different events.
    #[test]
    fn the_same_message_with_a_different_retryable_flag_is_not_a_repeat() {
        let rows = vec![err("rate limited", true)];
        assert!(!repeats_the_last_row(&rows, "rate limited", false));
    }

    /// Only adjacency collapses. The same failure recurring after other
    /// activity is a second event, and hiding it would hide a loop.
    #[test]
    fn an_error_separated_by_other_activity_is_not_a_repeat() {
        let rows = vec![
            err("provider timeout", true),
            TranscriptEntry::Text("retrying".to_string()),
        ];
        assert!(!repeats_the_last_row(&rows, "provider timeout", true));
    }

    #[test]
    fn an_empty_transcript_never_repeats() {
        assert!(!repeats_the_last_row(&[], "anything", false));
    }
}
