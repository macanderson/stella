//! Whether an incoming `Error` event is a row the transcript already shows.
//!
//! One failure can be reported twice at the engine/host seam. Whatever decides
//! an abort emits `AgentEvent::Error` for it *and* returns an aborted outcome,
//! which the host maps to a failed turn and emits as an `Error` again. The
//! reader saw the same red row twice and read it, reasonably, as two failures:
//! two aborted attempts, or a retry that failed the same way.
//!
//! The original instance was the staged pipeline's own "aborted at scope
//! review", reported once by the pipeline and once by the host. That crate is
//! deleted now (#3865); the seam it exposed is not, because it was never
//! really about the pipeline — it is about the host re-reporting an outcome
//! whose `Error` already crossed the stream.
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
//!
//! # The host wraps, it does not restate
//!
//! Verbatim equality was not enough, because the seam this module was written
//! for does not produce two identical strings. On a model-call failure the
//! engine emits the provider's own message, and the host then emits that same
//! message prefixed with [`MODEL_CALL_FAILED_PREFIX`] — so a live OpenRouter
//! abort painted (#4146):
//!
//! ```text
//! ✗ error  terminal provider error: OpenRouter stream error: The operation was aborted
//! ✗ error  model call failed: terminal provider error: OpenRouter stream error: The operation was aborted
//! ```
//!
//! Two red rows, one failure — and the reader counts failures. So a wrapped
//! restatement collapses too, by **exactly one named prefix** and no other
//! rule. Deliberately not a substring or similarity test: "looks like the row
//! above" is how a de-duplicator starts eating genuine second failures, and
//! adjacency alone is not enough of a guard for that.

use stella_protocol::MODEL_CALL_FAILED_PREFIX;

use super::TranscriptEntry;

/// Does `(message, retryable)` restate the transcript's last row — verbatim,
/// or wrapped by the host in [`MODEL_CALL_FAILED_PREFIX`]?
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
        }) if *prev_retryable == retryable && restates(prev, message)
    )
}

/// Whether `incoming` says nothing `prev` did not already say.
///
/// The prefix is stripped from `incoming` rather than prepended to `prev` so
/// the comparison stays allocation-free on the common path.
fn restates(prev: &str, incoming: &str) -> bool {
    prev == incoming
        || incoming
            .strip_prefix(MODEL_CALL_FAILED_PREFIX)
            .is_some_and(|unwrapped| unwrapped == prev)
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

    /// The reported panel (#4146): a live OpenRouter stream abort. The engine
    /// emits the provider's message; the host emits it again wrapped. Two red
    /// rows, one failure — and a reader counts failures.
    #[test]
    fn the_hosts_wrapped_restatement_repeats_the_engines_row() {
        let engine = "terminal provider error: OpenRouter stream error: The operation was aborted";
        let rows = vec![err(engine, false)];
        let host = format!("{MODEL_CALL_FAILED_PREFIX}{engine}");
        assert!(repeats_the_last_row(&rows, &host, false));
    }

    /// The wrap is recognised, not merely tolerated: the SAME prefix over a
    /// DIFFERENT failure is a second failure and must still show. This is what
    /// stops the new arm degrading into "starts with the host prefix ⇒ hide".
    #[test]
    fn the_same_wrapper_over_a_different_failure_is_not_a_repeat() {
        let rows = vec![err("terminal provider error: rate limited", false)];
        let host = format!("{MODEL_CALL_FAILED_PREFIX}terminal provider error: bad request");
        assert!(!repeats_the_last_row(&rows, &host, false));
    }

    /// Adjacency still governs the wrapped case too — a wrapped restatement
    /// arriving after other activity is a second event, and hiding it would
    /// hide a loop exactly as the verbatim rule guards against.
    #[test]
    fn a_wrapped_restatement_separated_by_other_activity_is_not_a_repeat() {
        let engine = "terminal provider error: timeout";
        let rows = vec![
            err(engine, false),
            TranscriptEntry::Text("retrying".to_string()),
        ];
        let host = format!("{MODEL_CALL_FAILED_PREFIX}{engine}");
        assert!(!repeats_the_last_row(&rows, &host, false));
    }

    /// The unwrapping is one level, not a general suffix test. A row that
    /// merely *ends with* the previous row's text — without the host's prefix
    /// accounting for the difference — is a different message and stays.
    #[test]
    fn a_bare_suffix_match_is_not_a_repeat() {
        let rows = vec![err("provider timeout", false)];
        assert!(!repeats_the_last_row(
            &rows,
            "upstream reported: provider timeout",
            false
        ));
    }
}
