//! The `/inbox` note a settled worker leaves behind.
//!
//! A worker runs on its own lane, and the user may never watch it. The note
//! is how the work reaches them: it is stored until read, and it is the only
//! place a finished worker speaks to a reader who looked away.
//!
//! So the note carries the worker's **answer**. `task_assign` tells the
//! model that a sub-agent's results land back in this session; this is the
//! surface that keeps that true for a worker that finished, the way the
//! failure arm below keeps it true for one that did not.
//!
//! A module of its own for the reason [`super::closeout`] is one:
//! `subsession.rs` sits just under the file-size ceiling, and this needs a
//! witness.

use super::{SubSessionSpec, WorkerEnd};
use crate::command_deck::prompt_line;

/// How much of the worker's answer the note carries. Long enough for a real
/// answer, short enough that the inbox stays a list.
const BODY_CHARS: usize = 160;

/// The title and body of the note, or `None` when no note is owed.
///
/// A user-initiated stop leaves none. The user was there; telling them what
/// they just did is noise.
pub(super) fn worker_notification(
    workspace_name: &str,
    spec: &SubSessionSpec,
    end: &WorkerEnd,
) -> Option<(String, String)> {
    match end {
        WorkerEnd::Done(answer) => Some((
            format!("{workspace_name}: {}", spec.notify_title),
            // A turn can end with tool work and no closing words. The prompt
            // is the fallback then, because a blank note names nothing at all.
            if answer.trim().is_empty() {
                prompt_line(&spec.prompt, BODY_CHARS)
            } else {
                prompt_line(answer, BODY_CHARS)
            },
        )),
        WorkerEnd::Failed(reason) => Some((
            format!("{workspace_name}: {} — FAILED", spec.notify_title),
            format!("{} — {reason}", prompt_line(&spec.prompt, 80)),
        )),
        WorkerEnd::Stopped => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SubSessionSpec {
        SubSessionSpec {
            lane: "sub:1".into(),
            title: "task #1".into(),
            purpose: String::new(),
            prompt: "count the rows in the ledger".into(),
            notify_title: "task #1 done".into(),
            dispatched_by: None,
        }
    }

    /// The witness: the note carries what the worker said. Echoing the
    /// prompt tells the reader only what they already asked, which is the
    /// one thing they cannot have forgotten.
    ///
    /// Fails before this change: `WorkerEnd::Done` held no answer to report.
    #[test]
    fn a_finished_worker_reports_its_answer_not_a_copy_of_the_prompt() {
        let end = WorkerEnd::Done("the ledger holds 412 rows".into());
        let (title, body) = worker_notification("stella", &spec(), &end).expect("a note is owed");

        assert!(title.contains("task #1 done"), "{title}");
        assert!(
            body.contains("412 rows"),
            "the note must carry the worker's answer: {body}"
        );
        assert!(
            !body.contains("count the rows"),
            "the prompt is what was asked, not what came back: {body}"
        );
    }

    /// A long answer is trimmed. The inbox is a list of lines.
    #[test]
    fn a_long_answer_is_trimmed_to_one_line() {
        let end = WorkerEnd::Done(format!("start {}", "x".repeat(500)));
        let (_, body) = worker_notification("stella", &spec(), &end).expect("a note is owed");

        assert!(body.starts_with("start "), "{body}");
        assert!(body.chars().count() <= BODY_CHARS, "{body}");
    }

    /// A turn that ends with no closing words still names its work.
    #[test]
    fn an_empty_answer_falls_back_to_the_prompt() {
        let end = WorkerEnd::Done("   ".into());
        let (_, body) = worker_notification("stella", &spec(), &end).expect("a note is owed");

        assert!(body.contains("count the rows"), "{body}");
    }

    /// Failure keeps its reason, and a stop still says nothing.
    #[test]
    fn failure_names_the_reason_and_a_stop_leaves_no_note() {
        let failed = WorkerEnd::Failed("provider refused the request".into());
        let (title, body) =
            worker_notification("stella", &spec(), &failed).expect("a note is owed");
        assert!(title.contains("FAILED"), "{title}");
        assert!(body.contains("provider refused"), "{body}");

        assert!(
            worker_notification("stella", &spec(), &WorkerEnd::Stopped).is_none(),
            "a user-initiated stop owes no note"
        );
    }
}
