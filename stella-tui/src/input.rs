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
    /// A clean cancellation request (`q` / Ctrl-C). The engine should abort
    /// the current turn cleanly — never a mid-tool kill.
    Cancel,
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
}
