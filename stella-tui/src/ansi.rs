// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The one shared ANSI escape-sequence stripper (#934).
//!
//! Tool output reaches the transcript verbatim, and any child process that
//! detects a TTY colourises it. `ratatui`'s `styled_graphemes` drops the ESC
//! control byte but renders the rest of the sequence (`[0;32m`) literally, so
//! unstripped output turns into visible garbage that eats the reader's line
//! budget. The strip is applied **once, where output folds into the
//! transcript** — never per frame, because the fold cache would retain the
//! escapes and re-stripping every paint would be wasted work.
//!
//! This module exists so callers stop growing private copies (three grew in
//! `stella-cli` alone before it was extracted).

use std::borrow::Cow;

/// Remove ANSI escape sequences, leaving the visible text.
///
/// Handles the three shapes that occur in practice: CSI (`ESC [` through a
/// final byte in `@..=~`, which covers all SGR colour codes), OSC (`ESC ]`
/// through BEL or ST, as emitted for hyperlinks and titles), and the residual
/// two-byte `ESC x` escapes (charset selects, keypad modes). Text without an
/// ESC byte — the common case — is returned borrowed, unmodified. In
/// particular a bare `[` that is not preceded by ESC is legitimate content
/// and always survives.
pub fn strip_ansi(s: &str) -> Cow<'_, str> {
    if !s.contains('\u{1b}') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: params/intermediates, then a final byte in `@..=~`.
            Some('[') => {
                for n in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            // OSC: terminated by BEL or ST (`ESC \`).
            Some(']') => {
                while let Some(n) = chars.next() {
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // nF escapes (charset selects): intermediates in `0x20..=0x2f`,
            // then one final byte. Everything else is a two-byte escape
            // (keypad modes, `ESC 7`/`ESC 8`) whose second byte was just
            // consumed by the match.
            Some(c2) if ('\u{20}'..='\u{2f}').contains(&c2) => {
                for n in chars.by_ref() {
                    if !('\u{20}'..='\u{2f}').contains(&n) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colourised_cargo_build_line_renders_clean() {
        let line = "\u{1b}[0m\u{1b}[0m\u{1b}[1m\u{1b}[32m    Finished\u{1b}[0m \
                    `dev` profile [unoptimized + debuginfo] target(s) in 2.41s";
        assert_eq!(
            strip_ansi(line),
            "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.41s"
        );
    }

    #[test]
    fn bare_brackets_without_esc_survive_unmodified() {
        // Over-eager stripping of legitimate bracket text would be a worse
        // bug than the escape residue this module removes.
        let line = "error[E0308]: mismatched types in [u8; 4] at src/lib.rs:1";
        assert!(matches!(strip_ansi(line), Cow::Borrowed(s) if s == line));
    }

    #[test]
    fn osc_hyperlink_and_bel_title_sequences_are_removed() {
        let bel = "\u{1b}]0;window title\u{7}visible";
        assert_eq!(strip_ansi(bel), "visible");
        let st = "\u{1b}]8;;https://example.com\u{1b}\\link text\u{1b}]8;;\u{1b}\\ after";
        assert_eq!(strip_ansi(st), "link text after");
    }

    #[test]
    fn two_byte_escapes_and_truncated_sequences_do_not_leak() {
        // A charset select is ESC + intermediate + final: all three go.
        assert_eq!(strip_ansi("a\u{1b}(Bb"), "ab");
        // A sequence cut off mid-stream (capped output) swallows to the end
        // rather than leaking a partial `[1;3` into the transcript.
        assert_eq!(strip_ansi("done\u{1b}[1;3"), "done");
        // A trailing lone ESC is dropped.
        assert_eq!(strip_ansi("done\u{1b}"), "done");
    }
}
