// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a `$` or a `!` at the head of a line means.
//!
//! `$ cmd` runs a shell command now. It skips the queue and the running turn.
//! `! text` stops the running turn at its next step. Your text then runs
//! next. That is the stop the red composer mode asks for. Both doors send
//! [`WorkspaceInput::Interrupt`], so one driver path does the work
//! (`command_deck::steer::interrupt_lead`).
//!
//! The bang ran shell commands before this. The bang family takes it back:
//! `!!` and `!!!` will mean "stop, and keep this rule". So `!ls` still runs
//! `ls` for one more release, and it says where the shell mark went. `! ls`
//! does not. A space after the bang is what the family shares.
//!
//! Split out of [`super`], a god file closed to growth.

use super::*;

/// The mark a line starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// `$` — run the rest as a shell command, now.
    Shell,
    /// `!` with the command against it, as in `!ls`. The old spelling of
    /// [`Mark::Shell`]. It lasts one release and says so.
    LegacyShell,
    /// `!` then a space: stop the running turn and run the rest next.
    Steer,
}

/// The mark `text` carries, and the words after it. `None` when the line
/// carries no mark and is a plain prompt.
///
/// A bang is a [`Mark::Steer`] only when a space follows it. That space is
/// the whole difference between `! use short sentences` and `!ls`. It is also
/// the shape the whole family shares. And it leaves the old shell spelling
/// people's hands already type. Nobody writes `! ls` for a shell command.
fn parse(text: &str) -> Option<(Mark, &str)> {
    let head = text.trim_start();
    if let Some(rest) = head.strip_prefix('$') {
        return Some((Mark::Shell, rest.trim()));
    }
    // Only the first bang is the mark. A command whose own text starts with a
    // bang keeps it: `!important` is typed as `!!important`.
    let rest = head.strip_prefix('!')?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some((Mark::Steer, rest.trim()))
    } else {
        Some((Mark::LegacyShell, rest.trim()))
    }
}

/// Whether a marked line beats a gate that is waiting for an answer.
///
/// A shell command has to run while an `ask_user` question or a hunk card is
/// up. A `!` interrupt is the same claim, one step harder. Neither one is the
/// answer the card asked for.
pub(super) fn claims_submission(text: &str) -> bool {
    parse(text).is_some()
}

/// Send one submission where its mark says, or down the plain prompt route.
pub(super) fn dispatch(ui: &mut DeckUi, model: &WorkspaceModel, text: String) -> DeckAction {
    // Owned before the branch: an unmarked line moves `text` on, and the
    // borrow `parse` takes has to be over by then.
    let marked = parse(&text).map(|(mark, rest)| (mark, rest.to_string()));
    let Some((mark, rest)) = marked else {
        return super::submit_prompt(ui, model, text);
    };
    // A mark with nothing after it is a keystroke spent on nothing. The user
    // is mid-sentence, and a queued bare `!` would be worse than a no-op.
    if rest.is_empty() {
        return DeckAction::Ignored;
    }
    match mark {
        Mark::Shell => DeckAction::Shell(rest),
        Mark::LegacyShell => {
            note(
                ui,
                "shell commands are `$ cmd` now — `!` interrupts the running turn. \
                 The old spelling runs for one more release.",
            );
            DeckAction::Shell(rest)
        }
        Mark::Steer => steer(ui, model, rest),
    }
}

/// `! text` at the focused agent. It interrupts a turn in flight. With no
/// turn to stop, the words are simply the next prompt.
///
/// The interrupt is [`WorkspaceInput::Interrupt`], the message the red
/// composer mode sends. So a lane gets the stop, the front-insert and the
/// resume from one driver path rather than two. The deck says when the mark
/// did not interrupt. A mark that ate a message would cost more than it saves.
fn steer(ui: &mut DeckUi, model: &WorkspaceModel, text: String) -> DeckAction {
    let running = model
        .agents
        .get(ui.focused)
        .filter(|a| a.status == crate::AgentStatus::Running);
    let Some(agent) = running else {
        note(
            ui,
            "nothing is running, so that went out as a prompt — `!` interrupts a turn in flight.",
        );
        return super::submit_prompt(ui, model, text);
    };
    DeckAction::Send(WorkspaceInput::Interrupt {
        agent: agent.meta.id.clone(),
        texts: vec![text],
    })
}

/// One line of chrome about a keystroke that did something other than what
/// the words looked like. `push`, not `demand`: nothing was refused, so a
/// reader who waved the dialog away keeps it away.
fn note(ui: &mut DeckUi, message: &str) {
    ui.notice.push(message);
    ui.scrollback
        .announce(format!("{}{message}", crate::accessible::NOTICE_MARKER));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dollar_mark_takes_the_rest_of_the_line_as_a_command() {
        assert_eq!(parse("$ cargo build"), Some((Mark::Shell, "cargo build")));
        assert_eq!(parse("  $ls"), Some((Mark::Shell, "ls")));
        assert_eq!(parse("$"), Some((Mark::Shell, "")));
    }

    /// One space is the whole difference, so it is asserted here.
    #[test]
    fn a_space_after_the_bang_is_what_separates_the_steer_from_the_old_shell() {
        assert_eq!(
            parse("! use short sentences"),
            Some((Mark::Steer, "use short sentences"))
        );
        assert_eq!(parse("!ls"), Some((Mark::LegacyShell, "ls")));
        assert_eq!(
            parse("!!important"),
            Some((Mark::LegacyShell, "!important"))
        );
        assert_eq!(parse("!"), Some((Mark::Steer, "")));
    }

    #[test]
    fn an_unmarked_line_carries_no_mark() {
        for text in ["and now the tests", "> steer me", "/help", "a $ sign"] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_marked_line_claims_the_submission_a_gate_would_otherwise_answer() {
        assert!(claims_submission("$ ls"));
        assert!(claims_submission("!ls"));
        assert!(claims_submission("! stop that"));
        assert!(!claims_submission("sqlite"));
    }
}
