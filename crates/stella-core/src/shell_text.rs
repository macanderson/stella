//! Reading a shell command as *text* — never running one.
//!
//! Two questions live here, and they are one subject: how a command splits
//! into words and separators, and what the resulting shape says about whether
//! the command does any work at all.
//!
//! It sits in `stella-core` rather than beside the shell tool because **two
//! planes ask the same question of the same string**. The `bash` tool asks it
//! per call, to append an advisory to a result
//! (`stella_tools::bash::sleep_advisory`); the engine asks it per turn, over
//! the calls already sitting in the transcript, to decide whether the turn is
//! stalling ([`crate::driver`]'s stall rung). `stella-tools` depends on
//! `stella-core`, so one implementation can serve both — and the alternative
//! was a second copy of the operator list, which is the failure this splitter
//! was extracted to end in the first place: four near-copies had accumulated
//! inside `bash.rs` and had already diverged (#2301).
//!
//! Pure functions over borrowed text (invariant 2): no process, no clock, no
//! filesystem. In particular the stall classifier is a *static text-shape*
//! check and never a measured elapsed time, so the same transcript always
//! classifies the same way — a timing here would make loop detection
//! nondeterministic.

/// Words the tokenizer emits as command separators, and the one predicate
/// every consumer of [`shell_words`] asks about them.
///
/// Four near-copies of this list had accumulated in `stella-tools`' `bash.rs`
/// — the `cd` skip list, the grep-scan boundary, `segment_args` and
/// [`bare_sleep_seconds`] — and they had already diverged: the grep boundary
/// omitted `&`, so `ls & grep -rn "struct X" .` scanned past the background
/// operator (#2301).
///
/// A newline is an operator here for the same reason `;` is: it ends one
/// command and begins the next. Redirection tokens (`2>&1`, `>>`, `<`) are
/// deliberately **not** operator words — they neither end a command nor start
/// one, and `stella-tools`' `redirect_target` reads them as ordinary words.
pub fn is_operator_word(word: &str) -> bool {
    matches!(word, ";" | "&" | "&&" | "|" | "||" | "\n")
}

/// A quote-aware word split — enough to pull a `cd` target or a `grep`
/// pattern out of the common command shapes, returning each word already
/// unquoted. NOT a shell parser: it respects `'…'` and `"…"` (so a pattern or
/// path with spaces stays one word) and preserves backslash escapes like
/// `\|` (so an alternation survives into `stella-tools`' `is_symbol_shaped`);
/// unquoted operators (`&&`, `||`, `|`, `;`, `&`, and a bare newline) come
/// back as their own words to bound a scan — including when attached to a
/// word, so `cd /app; ls` yields the target `/app`, not the unresolvable
/// `/app;` a paid bench trial saw warned about as an escape from its own
/// session root.
pub fn shell_words(command: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut has_word = false;
    let (mut in_single, mut in_double) = (false, false);
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                has_word = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                has_word = true;
            }
            c @ (';' | '&' | '|') if !in_single && !in_double => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
                let mut op = String::from(c);
                // `&&` / `||` are one operator; `;;` never appears in the
                // shapes this scans, so `;` stays single.
                if c != ';' && chars.peek() == Some(&c) {
                    chars.next();
                    op.push(c);
                }
                words.push(op);
            }
            '\\' if !in_single => {
                // Keep the escape literal (covers `\|`, `\"`, …); we don't
                // interpret it, just preserve it for the symbol test.
                cur.push('\\');
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                has_word = true;
            }
            // A newline separates two commands as surely as `;` does, so it
            // is an operator word rather than plain whitespace. Without it a
            // command-position rule cannot see the second line of
            // `ls\ncd /outside` as a command at all.
            '\n' if !in_single && !in_double => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
                words.push(String::from('\n'));
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
            }
            c => {
                cur.push(c);
                has_word = true;
            }
        }
    }
    if has_word {
        words.push(cur);
    }
    words
}

/// The accumulated seconds a *bare* sleep command blocks for, or `None` if
/// any segment does real work beyond sleeping and a harmless no-op.
///
/// Only a bare sleep is worth naming: `sleep 2 && curl` retry backoffs are
/// ordinary and must stay unflagged, so this requires **every** segment of
/// the command to be `sleep N` or an inert no-op (`echo`, `printf`, `true`)
/// — anything else (a real command sharing the line) disqualifies the whole
/// command. That matches the pathological shape #2022 observed
/// (`sleep 300; echo done`) rather than a compound one that happens to
/// contain a sleep, and it is deliberately biased toward saying nothing: the
/// expensive direction here is the false positive, because clamping or
/// scolding a legitimate retry loop breaks real tasks.
pub fn bare_sleep_seconds(command: &str) -> Option<u64> {
    let words = shell_words(command);
    let mut segments: Vec<&[String]> = Vec::new();
    let mut start = 0;
    for (i, w) in words.iter().enumerate() {
        if is_operator_word(w) {
            segments.push(&words[start..i]);
            start = i + 1;
        }
    }
    segments.push(&words[start..]);

    let mut total_secs = 0u64;
    let mut saw_sleep = false;
    for segment in &segments {
        match segment {
            [] => {}
            [cmd, arg] if cmd == "sleep" => {
                let secs = arg.parse::<f64>().ok()?;
                total_secs += secs.round() as u64;
                saw_sleep = true;
            }
            [cmd, ..] if matches!(cmd.as_str(), "echo" | "printf" | "true") => {}
            _ => return None,
        }
    }
    saw_sleep.then_some(total_secs)
}

#[cfg(test)]
mod tests {
    use super::{bare_sleep_seconds, is_operator_word, shell_words};

    #[test]
    fn shell_words_splits_operators_attached_to_a_word_in_core() {
        assert_eq!(shell_words("cd /app; ls"), ["cd", "/app", ";", "ls"]);
        assert_eq!(shell_words("a&&b"), ["a", "&&", "b"]);
        assert_eq!(shell_words("echo 'a;b'"), ["echo", "a;b"]);
    }

    #[test]
    fn every_separator_the_splitter_emits_is_an_operator_word_in_core() {
        for word in [";", "&", "&&", "|", "||", "\n"] {
            assert!(is_operator_word(word), "{word} should be an operator word");
        }
        for word in [">", ">>", "2>&1", "<", "<<<"] {
            assert!(
                !is_operator_word(word),
                "{word} should not be an operator word"
            );
        }
    }

    /// The shapes #2022 measured, and the accumulation across one command.
    #[test]
    fn a_bare_sleep_is_detected_and_summed() {
        assert_eq!(bare_sleep_seconds("sleep 300; echo done"), Some(300));
        assert_eq!(bare_sleep_seconds("sleep 120"), Some(120));
        assert_eq!(bare_sleep_seconds("sleep 30 && sleep 30"), Some(60));
        assert_eq!(bare_sleep_seconds("sleep 2.5"), Some(3));
    }

    /// The expensive direction: anything doing real work beside the sleep
    /// answers `None`, whatever the sleep is worth.
    #[test]
    fn a_sleep_beside_real_work_is_never_bare() {
        assert_eq!(
            bare_sleep_seconds("sleep 2 && curl -s http://localhost:8080"),
            None
        );
        assert_eq!(bare_sleep_seconds("sleep 5; tail -f build.log"), None);
        assert_eq!(bare_sleep_seconds("echo waiting; sleep 5; ls"), None);
        assert_eq!(bare_sleep_seconds("cargo test"), None);
    }
}
