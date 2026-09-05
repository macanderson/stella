// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a broadcast line means, and to whom.
//!
//! Two forms in the composer speak to other sessions on this machine:
//!
//! | Line | Who hears it | What it does there |
//! |---|---|---|
//! | `>@all <text>` | every other live session | the words land at the next step boundary |
//! | `>@<id> --deep <text>` | that session, worker lanes too | the same |
//! | `>>> @agents <text>` | every live session, lanes too | the same |
//! | `>>> @agents ! <text>` | the same | the turn stops, the words run next |
//! | `>>> @agents !! <text>` | the same | and the words are kept as guidance |
//! | `>>> @agents !!! <text>` | the same | and the words are kept as a rule |
//!
//! The three arrows are deep by construction, so there is no flag to
//! remember. The bang run says how hard the words push, and it is the run
//! `deck_ui::sigil` reads at the head of a plain line. One grammar, so `!!!`
//! cannot mean two things in one composer.
//!
//! `@agents` and `@all` both name the room. Any other word after the `@` is
//! one session id.
//!
//! What is kept is written once, by the session that sent the line, and it is
//! written for the workspace. Every target gets the stop and the words, and
//! none of them writes a record — see `stella-cli`'s
//! `command_deck::keep_record`.

use super::KeepStrength;

/// One broadcast, read from the line a person typed.
///
/// The stop and the save travel in one value on purpose. A save whose stop
/// was dropped, or a stop whose save was, is a failure the person who typed
/// the line never sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Broadcast {
    /// The words to send. Never empty.
    pub message: String,
    /// One session id, or `None` for every live session.
    pub session: Option<String>,
    /// Reach each target's worker lanes as well as its lead.
    pub deep: bool,
    /// Stop each target's running turn and run the words next, rather than
    /// only adding them at the next boundary.
    pub interrupt: bool,
    /// Keep the words for this workspace at this strength. `None` leaves
    /// nothing behind.
    pub keep: Option<KeepStrength>,
}

/// The bang run at the head of a line.
///
/// A bang run is a sigil only when a space follows it. That space is the
/// whole difference between `! use short sentences` and `!ls`.
pub(crate) enum Bangs<'a> {
    /// The line opens with no bang.
    Absent,
    /// A bang run with text pressed against it, as in `!ls`. Not a sigil.
    Attached,
    /// A sigil, and the words after it. `keep` is `None` for a lone `!`,
    /// which stops the turn and keeps nothing.
    Sigil {
        keep: Option<KeepStrength>,
        rest: &'a str,
    },
    /// More bangs than the family has, as in `!!!!`. Refused rather than read
    /// as the strongest one: guessing what a stray keystroke meant is how a
    /// typo publishes a rule.
    TooMany,
}

/// Read the bang run at the head of `text`.
pub(crate) fn bangs(text: &str) -> Bangs<'_> {
    let run = text.chars().take_while(|c| *c == '!').count();
    if run == 0 {
        return Bangs::Absent;
    }
    // Every counted char is one byte, so the run is also a byte offset.
    let rest = &text[run..];
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return Bangs::Attached;
    }
    let keep = match run {
        1 => None,
        2 => Some(KeepStrength::Guidance),
        3 => Some(KeepStrength::Rule),
        _ => return Bangs::TooMany,
    };
    Bangs::Sigil {
        keep,
        rest: rest.trim(),
    }
}

/// Read one composer line as a broadcast.
///
/// `None` for every line that is not one, which includes an address with
/// nothing to say. Those words keep their ordinary route, and the person sees
/// what they typed.
pub(crate) fn parse(text: &str) -> Option<Broadcast> {
    let head = text.trim_start();
    match head.strip_prefix(">>>") {
        Some(rest) => deep_form(rest),
        None => flat_form(head),
    }
}

/// `>>> @<address> [bangs] <message>`: deep always, with the bang run saying
/// whether the turn stops and what is kept.
fn deep_form(text: &str) -> Option<Broadcast> {
    let (address, rest) = address_of(text.trim_start())?;
    let (interrupt, keep, message) = match bangs(rest.trim_start()) {
        Bangs::Absent => (false, None, rest.trim()),
        Bangs::Sigil { keep, rest } => (true, keep, rest),
        // A run this form cannot read is not a weaker form of it. The line
        // goes out as the prompt it reads as.
        Bangs::Attached | Bangs::TooMany => return None,
    };
    build(address, message, true, interrupt, keep)
}

/// `>@<address> [--deep] <message>`: the older address, which never stops a
/// turn and keeps nothing.
fn flat_form(text: &str) -> Option<Broadcast> {
    let (address, rest) = address_of(text.strip_prefix('>')?)?;
    let (deep, message) = match rest.trim_start().strip_prefix("--deep") {
        Some(after) if after.is_empty() || after.starts_with(char::is_whitespace) => {
            (true, after.trim())
        }
        _ => (false, rest.trim()),
    };
    build(address, message, deep, false, None)
}

/// The `@name` at the head of `text`, and whatever follows it.
fn address_of(text: &str) -> Option<(&str, &str)> {
    text.strip_prefix('@')?.split_once(char::is_whitespace)
}

/// One broadcast, or `None` when the line names nobody or says nothing.
fn build(
    address: &str,
    message: &str,
    deep: bool,
    interrupt: bool,
    keep: Option<KeepStrength>,
) -> Option<Broadcast> {
    if address.is_empty() || message.is_empty() {
        return None;
    }
    Some(Broadcast {
        message: message.to_string(),
        session: (address != "all" && address != "agents").then(|| address.to_string()),
        deep,
        interrupt,
        keep,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(message: &str, deep: bool, interrupt: bool, keep: Option<KeepStrength>) -> Broadcast {
        Broadcast {
            message: message.to_string(),
            session: None,
            deep,
            interrupt,
            keep,
        }
    }

    /// **The witness for the address.** `>@all` reaches every other session,
    /// `>@<id>` one of them, `--deep` reaches their lanes, and an address
    /// with nothing to say keeps its ordinary route.
    #[test]
    fn the_broadcast_address_parses_its_target_depth_and_message() {
        assert_eq!(
            parse(">@all stop touching the release branch"),
            Some(room("stop touching the release branch", false, false, None))
        );
        assert_eq!(
            parse("  >@ses-17-9 --deep run the tests first"),
            Some(Broadcast {
                message: "run the tests first".into(),
                session: Some("ses-17-9".into()),
                deep: true,
                interrupt: false,
                keep: None,
            })
        );
        assert_eq!(
            parse(">@all --deeper is a word"),
            Some(room("--deeper is a word", false, false, None)),
            "only the exact flag is a flag"
        );
        for text in [
            ">@all",
            ">@all   ",
            ">@ --deep",
            "> @all hello",
            "@all hello",
        ] {
            assert_eq!(parse(text), None, "{text:?} is not a broadcast");
        }
    }

    /// **The witness for the broadcast sigils.** Three arrows plus a
    /// bang run stop every live session and keep the words for the workspace.
    /// The room is `@agents` or `@all`; one id addresses one session; the
    /// arrows are deep with no flag.
    #[test]
    fn three_arrows_and_a_bang_run_stop_the_room_and_keep_the_words() {
        assert_eq!(
            parse(">>> @agents !!! do not force-push"),
            Some(room(
                "do not force-push",
                true,
                true,
                Some(KeepStrength::Rule)
            ))
        );
        assert_eq!(
            parse(">>> @all !! use short sentences"),
            Some(room(
                "use short sentences",
                true,
                true,
                Some(KeepStrength::Guidance)
            ))
        );
        assert_eq!(
            parse(">>> @agents ! stop the compile"),
            Some(room("stop the compile", true, true, None)),
            "one bang stops the turn and keeps nothing"
        );
        assert_eq!(
            parse(">>>@agents read the failing test"),
            Some(room("read the failing test", true, false, None)),
            "no bang is a deep steer, and the space after the arrows is optional"
        );
        assert_eq!(
            parse(">>> @ses-17-9 !!! do not force-push"),
            Some(Broadcast {
                message: "do not force-push".into(),
                session: Some("ses-17-9".into()),
                deep: true,
                interrupt: true,
                keep: Some(KeepStrength::Rule),
            }),
            "one id is the same grammar aimed at one session"
        );
    }

    /// A typo must not publish a rule. A fourth bang, and a bang with the
    /// text pressed against it, are both refused. The line then goes out as
    /// the prompt it reads as.
    #[test]
    fn a_mistyped_sigil_broadcasts_nothing() {
        for text in [
            ">>> @agents !!!! do not force-push",
            ">>> @agents !!!urgent",
            ">>> @agents !!!",
            ">>> @agents",
            ">>> !!! nobody is addressed",
        ] {
            assert_eq!(parse(text), None, "{text:?} is not a broadcast");
        }
    }
}
