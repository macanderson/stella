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
//! `!!` and `!!!` mean "stop, and keep this". So `!ls` still runs `ls` for one
//! more release, and it says where the shell mark went. `! ls` does not. A
//! space after the last bang is what the family shares.
//!
//! Counting the bangs is how hard the words push:
//!
//! | Input | Meaning |
//! |---|---|
//! | `! text` | stop the turn, run `text` next |
//! | `!! text` | the same stop, and keep `text` as guidance the agent should follow |
//! | `!!! text` | the same stop, and keep `text` as a rule it must follow |
//!
//! What "keep" writes is a context record, which every later session in this
//! workspace loads through the ordinary record plane. The deck only says which
//! strength was asked for; the driver owns the write
//! (`command_deck::keep_record`).
//!
//! Split out of [`super`], a god file closed to growth.

use super::*;
use crate::envelope::KeepStrength;

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
    /// `!!` or `!!!` then a space: the same stop, and keep the rest as a
    /// context record at this strength.
    Keep(KeepStrength),
}

/// The mark `text` carries, and the words after it. `None` when the line
/// carries no mark and is a plain prompt.
///
/// A bang run is a mark only when a space follows it. That space is the whole
/// difference between `! use short sentences` and `!ls`, and it is what the
/// whole family shares. It also leaves the old shell spelling people's hands
/// already type: nobody writes `! ls` for a shell command, and `!!important`
/// still reaches the shell as `!important`.
///
/// Bangs past the third are not a fourth strength — `!!!!` is refused as a
/// mark rather than read as the strongest one, because guessing which of two
/// meanings a typo intended is how a stray keystroke publishes a rule.
fn parse(text: &str) -> Option<(Mark, &str)> {
    let head = text.trim_start();
    if let Some(rest) = head.strip_prefix('$') {
        return Some((Mark::Shell, rest.trim()));
    }
    let bangs = head.chars().take_while(|c| *c == '!').count();
    if bangs == 0 {
        return None;
    }
    let rest = &head[bangs..];
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        // The old spelling, which owns only the first bang: the rest of the
        // run belongs to the command's own text.
        return Some((Mark::LegacyShell, head[1..].trim()));
    }
    let mark = match bangs {
        1 => Mark::Steer,
        2 => Mark::Keep(KeepStrength::Guidance),
        3 => Mark::Keep(KeepStrength::Rule),
        _ => return None,
    };
    Some((mark, rest.trim()))
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
        Mark::Keep(strength) => keep(ui, model, rest, strength),
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
        keep: None,
    })
}

/// `!! text` and `!!! text`: the interrupt above, with the words kept.
///
/// The save must not depend on what the session happens to be doing, so this
/// sends the interrupt whether or not a turn is running. With nothing to stop,
/// the driver reads that message as "run this now" — what a bang has always
/// meant at rest — and writes the record either way. A deck with no agent at
/// all has no interrupt to send and falls back to the plain prompt route; the
/// composer is not reachable before the lead registers, so that arm is
/// totality rather than a case anyone sees.
///
/// The note says what is being kept, not that it was kept: the write happens
/// driver-side, and only the driver can report how it went.
fn keep(
    ui: &mut DeckUi,
    model: &WorkspaceModel,
    text: String,
    strength: KeepStrength,
) -> DeckAction {
    let Some(agent) = model
        .agents
        .get(ui.focused)
        .or_else(|| model.agents.first())
    else {
        return super::submit_prompt(ui, model, text);
    };
    let running = agent.status == crate::AgentStatus::Running;
    let keeping = match strength {
        KeepStrength::Guidance => "keeping that as guidance for this workspace",
        KeepStrength::Rule => "keeping that as a rule for this workspace",
    };
    let tail = if running {
        "stopping at the next step boundary"
    } else {
        "nothing was running, so it also went out as a prompt"
    };
    note(ui, &format!("{keeping} — {tail}"));
    DeckAction::Send(WorkspaceInput::Interrupt {
        agent: agent.meta.id.clone(),
        texts: vec![text],
        keep: Some(strength),
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

    /// Counting the bangs is the strength, and the space still separates the
    /// family from the old shell spelling.
    #[test]
    fn the_bang_count_says_how_hard_the_words_push() {
        assert_eq!(
            parse("!! use short sentences"),
            Some((Mark::Keep(KeepStrength::Guidance), "use short sentences"))
        );
        assert_eq!(
            parse("!!! do not force-push"),
            Some((Mark::Keep(KeepStrength::Rule), "do not force-push"))
        );
        assert_eq!(parse("!!ls"), Some((Mark::LegacyShell, "!ls")));
        assert_eq!(parse("!!"), Some((Mark::Keep(KeepStrength::Guidance), "")));
    }

    /// A fourth bang is a typo, not a fourth strength. It carries no mark, so
    /// the line goes out as the prompt it reads as.
    #[test]
    fn a_fourth_bang_is_not_a_stronger_rule() {
        assert_eq!(parse("!!!! do not force-push"), None);
        assert!(!claims_submission("!!!! do not force-push"));
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
