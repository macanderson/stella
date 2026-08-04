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
        out.extend((from..=to).filter(|i| *i <= total).map(|i| i - 1));
    }
    out.sort_unstable();
    out.dedup();
    Some(out)
}

/// The answers a scope-review card offers. Three are keystrokes (`a`/`t`/`x`);
/// the fourth is whatever the reviewer typed instead.
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

impl ScopeDecision {
    /// Read a line typed at the scope card as a decision.
    ///
    /// A *bare* word that means yes/trim/no is that decision — a reviewer who
    /// types "ok" and hits ⏎ means approve, and making them find the `a` key
    /// instead is the kind of small refusal that teaches people the card is
    /// broken. Anything longer is a [`ScopeDecision::Revise`] note, because a
    /// sentence is an instruction: "no, don't touch the tests" starts with
    /// "no" but reading it as `Abort` would throw away the half that says what
    /// to do instead.
    ///
    /// Trailing `.`/`!`/`,` are stripped before matching, so "yes!" is still
    /// bare. Case-insensitive. A blank line yields `Revise` with an empty note,
    /// which callers reject before it reaches the engine — a blank submission
    /// is not an answer.
    ///
    /// A single leading `>` is dropped first. Elsewhere that marker means
    /// "steer the running turn", and a turn parked at a scope card *is* steered
    /// by answering the card — so `> narrow it down` is this note, not a note
    /// whose first word is a punctuation mark the planner has to guess at.
    pub fn from_typed(text: &str) -> Self {
        let text = text
            .trim()
            .strip_prefix('>')
            .map(str::trim_start)
            .unwrap_or_else(|| text.trim());
        let bare = text
            .trim()
            .trim_end_matches(['.', '!', ','])
            .trim()
            .to_ascii_lowercase();
        match bare.as_str() {
            "a" | "y" | "yes" | "yep" | "yeah" | "ok" | "okay" | "approve" | "approved" | "go"
            | "go ahead" | "do it" | "ship it" | "lgtm" | "sure" | "proceed" | "continue" => {
                Self::Approve
            }
            "t" | "trim" | "smaller" | "less" | "fewer" | "trim it" => Self::Trim,
            "x" | "n" | "no" | "nope" | "abort" | "cancel" | "stop" | "quit" | "nevermind"
            | "never mind" => Self::Abort,
            _ => Self::Revise {
                note: text.trim().to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported case: the user typed "ok" at a scope card and it became a
    /// sidecar sub-session's prompt instead of an approval.
    #[test]
    fn a_bare_yes_word_approves() {
        for word in ["ok", "OK", "yes", "y", "sure", "do it", "lgtm", "yes!"] {
            assert_eq!(
                ScopeDecision::from_typed(word),
                ScopeDecision::Approve,
                "{word:?} should approve"
            );
        }
    }

    #[test]
    fn a_bare_no_word_aborts() {
        for word in ["no", "n", "x", "abort", "cancel", "stop", "never mind"] {
            assert_eq!(
                ScopeDecision::from_typed(word),
                ScopeDecision::Abort,
                "{word:?} should abort"
            );
        }
    }

    #[test]
    fn a_bare_trim_word_trims() {
        for word in ["t", "trim", "smaller", "fewer"] {
            assert_eq!(
                ScopeDecision::from_typed(word),
                ScopeDecision::Trim,
                "{word:?} should trim"
            );
        }
    }

    /// The distinction that matters: a sentence beginning with a decision word
    /// is a note, not the decision. Reading "no, just the dialog" as `Abort`
    /// would keep the half the user cared about and discard the rest.
    #[test]
    fn a_sentence_is_a_note_even_when_it_starts_with_a_decision_word() {
        assert_eq!(
            ScopeDecision::from_typed("no, just the ctrl+O dialog"),
            ScopeDecision::Revise {
                note: "no, just the ctrl+O dialog".to_string()
            }
        );
        assert_eq!(
            ScopeDecision::from_typed("yes but skip the tests"),
            ScopeDecision::Revise {
                note: "yes but skip the tests".to_string()
            }
        );
    }

    /// The note keeps the user's own casing and punctuation — it is going into
    /// a planner prompt, not a keyword match.
    #[test]
    fn a_note_is_preserved_verbatim_apart_from_surrounding_whitespace() {
        assert_eq!(
            ScopeDecision::from_typed("  Only Ctrl+O. Skip Ctrl+W!  \n"),
            ScopeDecision::Revise {
                note: "Only Ctrl+O. Skip Ctrl+W!".to_string()
            }
        );
    }

    /// `>` steers the running turn everywhere else. A turn parked at a card is
    /// steered by answering the card, so the marker is dropped rather than
    /// carried into the planner prompt.
    #[test]
    fn a_leading_steer_marker_is_dropped_from_the_note() {
        assert_eq!(
            ScopeDecision::from_typed("> narrow it down"),
            ScopeDecision::Revise {
                note: "narrow it down".to_string()
            }
        );
        assert_eq!(ScopeDecision::from_typed("> ok"), ScopeDecision::Approve);
        // Only the first one: a note that is *about* a `>` keeps it.
        assert_eq!(
            ScopeDecision::from_typed(">> use >> for redirects"),
            ScopeDecision::Revise {
                note: "> use >> for redirects".to_string()
            }
        );
    }

    #[test]
    fn a_blank_line_is_an_empty_note_for_the_caller_to_reject() {
        assert_eq!(
            ScopeDecision::from_typed("   "),
            ScopeDecision::Revise {
                note: String::new()
            }
        );
    }

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
        assert_eq!(hunk_selection_from_typed("1, 2-3, 3", 4), Some(vec![0, 1, 2]));
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
}
