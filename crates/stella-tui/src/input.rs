//! The messages the TUI sends *back* to the engine over the submissions
//! channel. This is the other half of the `run(...)` contract: `AgentEvent`s
//! flow in, [`UserInput`] flows out. Kept in its own tiny module so both the
//! pure key-handling layer ([`crate::deck_ui`]) and the interactive shell
//! ([`crate::deck_shell`]) can depend on it without a cycle.

use stella_protocol::Attachment;

/// A message from the user to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserInput {
    /// A prompt to run. `text` is the fully-expanded message — paste chips
    /// have already been expanded to their payloads (L-T3). `attachments`
    /// carries any multimodal inputs (pasted images, attached files) the
    /// composer collected alongside the text.
    Prompt {
        text: String,
        attachments: Vec<Attachment>,
    },
    /// The user's answer to a pending scope-review gate (L-E5).
    ScopeDecision(ScopeDecision),
    /// The user's answer to a pending `ask_user` question. `id` correlates it
    /// back to the question (and, downstream, to the `ask_user` tool call's
    /// `ToolResult`); `answer` is either a chosen option's text or the user's
    /// own free-text reply — the always-available affordance the `AskUser`
    /// renderer contract mandates.
    AskUserAnswer { id: String, answer: String },
    /// The user's answer to a pending per-hunk approval gate (#1265). `id`
    /// correlates it back to the `HunkReview` proposal; `accepted` holds the
    /// indices of the hunks to APPLY, so an empty vector is a full refusal
    /// rather than a missing answer.
    HunkDecision { id: String, accepted: Vec<usize> },
    /// A clean cancellation request (`q` / Ctrl-C). The engine should abort
    /// the current turn cleanly — never a mid-tool kill.
    Cancel,
}

/// Read a line typed at a hunk-review card as a set of hunks to keep.
///
/// `total` is how many hunks the card offers; indices the reviewer types are
/// **1-based** (what the card shows) and come back 0-based. Accepted forms:
///
/// - a bare yes-word — keep everything;
/// - a bare no-word — keep nothing, which is a real answer, not a non-answer;
/// - a list of numbers and ranges (`1 3`, `1,3`, `2-4`, `1, 3-5`).
///
/// `None` means "that was not an answer" and the caller keeps the card up.
/// Unlike the scope card there is no revise channel to absorb prose here, and
/// this is the gate whose wrong answer edits somebody's files — so an
/// unrecognized line must do nothing at all rather than be interpreted
/// generously. An out-of-range number is dropped rather than failing the whole
/// line, matching how `ScopeDecision::Trim` indices are read.
#[must_use]
pub fn hunk_selection_from_typed(text: &str, total: usize) -> Option<Vec<usize>> {
    let text = text.trim().trim_end_matches(['.', '!', ',']).trim();
    match text.to_ascii_lowercase().as_str() {
        "" => return None,
        "a" | "all" | "y" | "yes" | "ok" | "okay" | "approve" | "apply" | "lgtm" => {
            return Some((0..total).collect());
        }
        "n" | "no" | "none" | "x" | "abort" | "cancel" | "skip" | "reject" | "stop" => {
            return Some(Vec::new());
        }
        _ => {}
    }
    let mut out = Vec::new();
    for token in text.split([',', ' ', '\t', '\n']).filter(|t| !t.is_empty()) {
        let bounds: Vec<&str> = token.splitn(2, '-').collect();
        let (from, to) = match bounds.as_slice() {
            [one] => (one.parse::<usize>().ok()?, one.parse::<usize>().ok()?),
            [from, to] => (from.parse::<usize>().ok()?, to.parse::<usize>().ok()?),
            _ => return None,
        };
        if from == 0 || from > to {
            return None;
        }
        // Clamp the range before walking it, never after. This parse runs
        // inline in the key handler, so a typed `1-9999999999` filtered
        // one value at a time would stop the deck redrawing and reading
        // keys for as long as it counted. Bounding the range costs the
        // same answer in as many steps as the card has hunks.
        out.extend((from..=to.min(total)).map(|i| i - 1));
    }
    out.sort_unstable();
    out.dedup();
    Some(out)
}

/// The answers the plan-review dialog offers. Three are single keystrokes
/// (`a`/`t`/`x`); the fourth carries the refine note typed after `r`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeDecision {
    /// Run the plan as proposed.
    Approve,
    /// Approve, but trim the plan down first.
    Trim,
    /// Re-plan with this note — the reviewer wants a different scope.
    Revise { note: String },
    /// Abort the plan.
    Abort,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_words_select_everything_or_nothing() {
        for word in ["a", "all", "yes", "OK", "apply", "lgtm"] {
            assert_eq!(
                hunk_selection_from_typed(word, 3),
                Some(vec![0, 1, 2]),
                "{word:?}"
            );
        }
        for word in ["n", "none", "no", "x", "reject", "skip"] {
            assert_eq!(hunk_selection_from_typed(word, 3), Some(vec![]), "{word:?}");
        }
    }

    #[test]
    fn hunk_numbers_and_ranges_are_one_based_and_deduped() {
        assert_eq!(hunk_selection_from_typed("1 3", 4), Some(vec![0, 2]));
        assert_eq!(hunk_selection_from_typed("1,3", 4), Some(vec![0, 2]));
        assert_eq!(hunk_selection_from_typed("2-4", 4), Some(vec![1, 2, 3]));
        assert_eq!(
            hunk_selection_from_typed("1, 2-3, 3", 4),
            Some(vec![0, 1, 2])
        );
    }

    /// Out of range is dropped (the same forgiveness `Trim` indices get), but a
    /// line that is not a selection at all is NOT an answer — this is the gate
    /// whose wrong reading edits somebody's files.
    #[test]
    fn hunk_selection_drops_out_of_range_and_refuses_prose() {
        assert_eq!(hunk_selection_from_typed("1 99", 2), Some(vec![0]));
        assert_eq!(hunk_selection_from_typed("99", 2), Some(vec![]));
        for prose in ["", "  ", "keep the first one", "1 or 2", "-1", "3-1", "0"] {
            assert_eq!(hunk_selection_from_typed(prose, 4), None, "{prose:?}");
        }
    }

    /// A typed upper bound is a bound, not a work list. `1-99` on a 2-hunk card
    /// already had to answer `[0, 1]`; what this pins is the *cost* of getting
    /// there, because the parse runs inline in the key handler and a range
    /// walked one value at a time would stop the deck redrawing and reading
    /// keys. Answered on a worker so reintroducing the walk fails in seconds
    /// rather than hanging the suite until CI's timeout kills it.
    #[test]
    fn a_typed_upper_bound_costs_the_card_not_the_number() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(hunk_selection_from_typed(&format!("1-{}", usize::MAX), 3));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(selection) => assert_eq!(selection, Some(vec![0, 1, 2])),
            Err(_) => panic!(
                "`1-{}` walked the typed range instead of clamping it",
                usize::MAX
            ),
        }
    }
}
