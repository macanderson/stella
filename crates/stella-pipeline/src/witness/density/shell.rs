//! The shell arm of the assertion-density screen (#2207).
//!
//! Every other language the screen reads has a *test-framework* assertion
//! vocabulary — `assert_eq!`, `assertEqual`, `expect(…).toBe(…)`. Shell has
//! none. A shell witness asserts with the exit status of `test`/`[`/`grep -q`
//! and propagates it with `||`, `&&` or `set -e`, so the three vacuous shapes
//! [`super`] refuses look completely different here:
//!
//! - **No assertion at all** is a script that never *examines* anything —
//!   `exit 1` on its own line is the whole failure mechanism.
//! - **Assertions over constants** is `test "1" = "1"`: a comparison whose
//!   operands are literals the author wrote, not values the run produced.
//! - **A catch-all** is `some_command || exit 1`: the shell analogue of a bare
//!   `#[should_panic]`, satisfied by *any* non-zero status from anything.
//!
//! This arm existed nowhere until #2207. `Lang::from_path` returned `None` for
//! `.sh`, so [`super::screen_witness_source`] returned `Ok(())` for every shell
//! artifact — while #2064 had made `sh` the *floor runner* and
//! [`crate::witness::RUNNER_VOCABULARY`] the boundary actively recommends when
//! no language toolchain is present. The screen was off for exactly the corpus
//! that needs it most.
//!
//! # Statements, not calls
//!
//! The one structural difference from the other arms: a shell site spans a
//! whole *statement* ([`Span::Statement`], resolved by [`statement_start`]),
//! not a balanced call. A shell check has no parentheses to walk, and its
//! evidence sits on both sides of the marker — `openssl … | grep -q "CN=…"`
//! puts the subject to the LEFT of the assertion and `test -f x || exit 1`
//! puts the consequence to the right. Screening either half alone reads the
//! other's absence as vacuousness.
//!
//! # Known limits
//!
//! `set -e` propagation is not modelled: a script whose only failure mechanism
//! is a bare command under `set -e` produces no site and is refused as
//! [`super::VacuousWitness::NoAssertions`] rather than the more precise
//! catch-all diagnosis (#2821). Heredocs, `case` arms and ANSI-C `$'…'`
//! quoting are read as ordinary text — lexical means lexical, per [`super`]'s
//! conservative posture, and every one of them errs toward acceptance.

use super::{Lang, Span, Verdict};

/// The shell assertion vocabulary: the commands that *examine* something, plus
/// the two guards that turn a bare command's status into the script's verdict.
///
/// Every marker spans its whole statement, for the reason in the module doc.
/// `exit` appears only in its **guarded** forms. A bare `exit 1` is not a site
/// at all: it examines nothing, so a script containing only that must read as
/// "asserts nothing" rather than as a catch-all that at least ran something.
pub(super) const MARKERS: &[(&str, Span)] = &[
    ("test ", Span::Statement),
    ("[ ", Span::Statement),
    ("[[ ", Span::Statement),
    ("grep -q", Span::Statement),
    ("cmp ", Span::Statement),
    ("diff ", Span::Statement),
    ("|| exit", Span::Statement),
    ("&& exit", Span::Statement),
];

/// Shell words that carry no evidence: the vocabulary above, the control-flow
/// keywords, and the builtins a script uses to arrange a check rather than to
/// make one. Concatenated with [`super::neutral`]'s shared `KEYWORDS`.
///
/// Deliberately absent: single-letter flag names (`e`, `x`, `f`, `q`, `z`).
/// [`names_something_real`] discards any word beginning with `-` outright, so
/// listing them here would only cost the screen a file genuinely named `x`.
pub(super) const NEUTRAL: &[&str] = &[
    "test", "exit", "then", "fi", "elif", "do", "done", "echo", "printf", "cd", "set", "grep",
    "cmp", "diff", "until", "case", "esac", "function", "shift", "local", "readonly", "unset",
    "export", "eval", "exec", "source", "command", "pipefail",
];

/// The words that make a statement an examination rather than an invocation.
fn is_check_word(word: &str) -> bool {
    matches!(word, "test" | "[" | "[[" | "grep" | "cmp" | "diff")
}

/// The binary comparison operators of `test`/`[`/`[[`. Redirections (`<`, `>`)
/// are deliberately excluded: they move bytes, they do not compare them.
fn is_comparison(word: &str) -> bool {
    matches!(
        word,
        "=" | "=="
            | "!="
            | "=~"
            | "-eq"
            | "-ne"
            | "-lt"
            | "-le"
            | "-gt"
            | "-ge"
            | "-ef"
            | "-nt"
            | "-ot"
    )
}

/// Whether `#` at `at` opens a comment, rather than being part of `$#`,
/// `${#var}` or `#!`-after-text.
///
/// This is the whole reason the shell sanitizer cannot reuse Python's rule: a
/// Python `#` is always a comment, a shell `#` is a comment only at a word
/// boundary. Treating `$#` as a comment blanks the rest of the line and turns
/// an argument-count guard into no assertion at all.
pub(super) fn starts_comment(bytes: &[u8], at: usize) -> bool {
    match at.checked_sub(1).map(|prev| bytes[prev]) {
        // A `#` opening the file is the shebang. Blanking it is harmless: it
        // names an interpreter, never a value under test.
        None => true,
        Some(prev) => prev.is_ascii_whitespace() || matches!(prev, b';' | b'|' | b'&' | b'('),
    }
}

/// Walk back from a marker to the start of the shell statement it sits in: the
/// previous newline, `;` or `&`, with leading blanks skipped.
///
/// `|` is deliberately *not* a boundary — a pipeline is one statement, and the
/// subject of `… | grep -q …` lives on its left. `&` is, because `&&`
/// sequences two independent statements (and `cmd &` backgrounds one).
pub(super) fn statement_start(bytes: &[u8], marker: usize) -> usize {
    let mut i = marker;
    while i > 0 && !matches!(bytes[i - 1], b'\n' | b';' | b'&') {
        i -= 1;
    }
    while i < marker && matches!(bytes[i], b' ' | b'\t' | b'\r') {
        i += 1;
    }
    i
}

/// Blank a double-quoted string's literal bytes while leaving its expansions
/// intact, preserving length exactly like [`super::sanitize`]'s plain fill.
///
/// Double quotes interpolate, so `"$1"` is a value the run produces and `"ok"`
/// is a constant the author typed. Filling both with `x` erases that
/// distinction — and worse, makes any two same-length strings compare equal,
/// so `test "ab" = "$a"` would read as a self-comparison and a real assertion
/// would be refused.
pub(super) fn blank_interpolated(out: &mut [u8], bytes: &[u8], range: std::ops::Range<usize>) {
    let end = range.end;
    let mut i = range.start;
    while i < end {
        if bytes[i] == b'$' && i + 1 < end {
            i = expansion_end(bytes, i, end);
            continue;
        }
        out[i] = b'x';
        i += 1;
    }
}

/// The index just past the parameter or command expansion opening at `at`
/// (which holds the `$`), bounded by `end`.
fn expansion_end(bytes: &[u8], at: usize, end: usize) -> usize {
    let mut i = at + 1;
    match bytes.get(i).copied() {
        Some(opener @ (b'{' | b'(')) => {
            let closer = if opener == b'{' { b'}' } else { b')' };
            let mut depth = 0usize;
            while i < end {
                if bytes[i] == opener {
                    depth += 1;
                } else if bytes[i] == closer {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return i + 1;
                    }
                }
                i += 1;
            }
            end
        }
        // `$1`, `$name`, and the special parameters `$#`, `$?`, `$@`, `$*`,
        // `$!`, `$$` — all of them values the run decides.
        Some(c)
            if c.is_ascii_alphanumeric()
                || matches!(c, b'_' | b'#' | b'?' | b'@' | b'*' | b'!' | b'$') =>
        {
            i += 1;
            while i < end && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            i
        }
        // A lone `$` is an ordinary character.
        _ => at + 1,
    }
}

/// Whether a shell site's only failure mechanism is a bare command's exit
/// status — the shell analogue of a bare `#[should_panic]`.
///
/// A site exists only because one of [`MARKERS`] matched, so a site with no
/// check word and no comparison in it is exactly a `cmd || exit 1`: whatever
/// non-zero status that command produces satisfies it, including one the goal
/// never addressed.
pub(super) fn is_catch_all(site: &str) -> bool {
    !words(site)
        .iter()
        .any(|word| is_check_word(word) || is_comparison(word))
}

/// What one shell statement is worth.
pub(super) fn classify(site: &str) -> Verdict {
    if is_catch_all(site) {
        return Verdict::CatchAll;
    }
    let words = words(site);
    let comparisons: Vec<usize> = (0..words.len())
        .filter(|index| is_comparison(words[*index]))
        .collect();
    if !comparisons.is_empty() {
        // A comparison names its own operands, so they — not the surrounding
        // command line — decide the verdict.
        let operands = |index: usize| {
            (
                index.checked_sub(1).and_then(|i| words.get(i)).copied(),
                words.get(index + 1).copied(),
            )
        };
        if comparisons.iter().all(|index| {
            let (left, right) = operands(*index);
            left.is_some() && left == right
        }) {
            return Verdict::Tautological;
        }
        let names_a_value = comparisons.iter().any(|index| {
            let (left, right) = operands(*index);
            [left, right]
                .into_iter()
                .flatten()
                .any(operand_names_a_value)
        });
        return if names_a_value {
            Verdict::Substantive
        } else {
            Verdict::Tautological
        };
    }
    // A unary check (`test -f p`, `grep -q pat file`) has no operand pair: the
    // subject is whatever the statement names.
    let neutral = super::neutral(Lang::Shell);
    if words.iter().any(|word| names_something_real(word, neutral)) {
        Verdict::Substantive
    } else {
        Verdict::Tautological
    }
}

/// Whether a comparison operand is a value the run produced rather than a
/// literal the author typed. Only an expansion or a command substitution
/// qualifies: in `test a = b` both sides are constants, however they are
/// spelled.
fn operand_names_a_value(word: &str) -> bool {
    word.contains('$') || word.contains('`')
}

/// Whether a word in a unary check names something the implementation decides.
///
/// Quoted words are literals — [`super::sanitize`] has already blanked their
/// contents, and what survives is the author's constant. Flags, operators and
/// numbers carry no evidence; anything else is a path, a program, or a name,
/// and the screen's conservative posture credits it.
fn names_something_real(word: &str, neutral: &[&str]) -> bool {
    if operand_names_a_value(word) {
        return true;
    }
    let Some(&first) = word.as_bytes().first() else {
        return false;
    };
    if matches!(
        first,
        b'"' | b'\'' | b'|' | b'&' | b'>' | b'<' | b'(' | b')' | b';' | b'=' | b'!' | b'[' | b']'
    ) {
        return false;
    }
    // A flag selects a mode; the thing being examined is somewhere else.
    if first == b'-' {
        return false;
    }
    if !word.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    !neutral.contains(&word)
}

/// Split a statement into shell words, keeping a quoted string or a `$(…)` /
/// `${…}` expansion together however much whitespace it contains.
///
/// A plain `split_whitespace` breaks `"$(cat f)"` into two halves, neither of
/// which looks like an expansion — which reads a real assertion as a
/// comparison of two constants and refuses it.
fn words(site: &str) -> Vec<&str> {
    let bytes = site.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        let mut depth = 0usize;
        let mut quote: Option<u8> = None;
        while i < bytes.len() {
            let c = bytes[i];
            match quote {
                Some(open) => {
                    if c == open {
                        quote = None;
                    }
                }
                None => match c {
                    b'"' | b'\'' | b'`' => quote = Some(c),
                    b'(' | b'{' => depth += 1,
                    b')' | b'}' => depth = depth.saturating_sub(1),
                    _ if depth == 0 && c.is_ascii_whitespace() => break,
                    _ => {}
                },
            }
            i += 1;
        }
        out.push(&site[start..i]);
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::witness::density::{VacuousWitness, screen_witness_source};

    /// Every case here is authored at the path the boundary itself recommends
    /// (`is_witness_test_path` accepts `witness_check.sh`, and #2064 made `sh`
    /// the floor runner), so these are the shapes the screen actually meets.
    fn screen(source: &str) -> Result<(), VacuousWitness> {
        screen_witness_source("witness_check.sh", source)
    }

    /// #2207 clause one. Before the shell arm existed this returned `Ok(())`:
    /// a script that examines nothing armed the flip oracle and turned green
    /// the moment anyone edited it.
    #[test]
    fn a_shell_witness_that_asserts_nothing_is_rejected() {
        assert_eq!(
            screen("#!/bin/sh\nexit 1\n"),
            Err(VacuousWitness::NoAssertions)
        );
    }

    /// #2207 clause two: a comparison whose operands are both literals holds
    /// against every implementation.
    #[test]
    fn a_shell_witness_comparing_constants_is_rejected() {
        match screen("#!/bin/sh\ntest \"1\" = \"1\" || exit 1\n") {
            Err(VacuousWitness::ConstantAssertions { example }) => assert!(
                example.contains("test \"1\" = \"1\""),
                "the site is quoted from the original bytes: {example}"
            ),
            other => panic!("a constant comparison witnesses nothing, got {other:?}"),
        }
    }

    /// #2207 clause three: a guarded bare command is the shell analogue of a
    /// bare `#[should_panic]` — any non-zero status from anything satisfies it.
    #[test]
    fn a_shell_witness_guarding_a_bare_command_is_rejected() {
        match screen("#!/bin/sh\nsome_command 2>/dev/null || exit 1\n") {
            Err(VacuousWitness::CatchAllPanic { example }) => assert!(
                example.contains("some_command"),
                "the site names the command whose status is the whole check: {example}"
            ),
            other => panic!("a bare command's status is a catch-all, got {other:?}"),
        }
    }

    /// The conservative posture, which costs a run its verification when it is
    /// wrong: a real shell witness must pass. The subject of the `grep -q` is
    /// to its LEFT, which is why a shell site spans the whole statement.
    #[test]
    fn a_substantive_shell_witness_is_accepted() {
        assert_eq!(
            screen(
                "#!/bin/sh\nset -e\ntest -f ssl/server.crt\n\
                 openssl x509 -noout -subject -in ssl/server.crt | grep -q \"CN=example.com\"\n"
            ),
            Ok(())
        );
    }

    /// `#` opens a comment only at a word boundary. A Python-style rule blanks
    /// from `$#` to end of line, which erases the guard and then the assertion
    /// that follows it — the screen would refuse a witness for a defect of its
    /// own sanitizer.
    #[test]
    fn a_parameter_count_is_not_a_shell_comment() {
        assert_eq!(
            screen("#!/bin/sh\nif [ $# -ne 1 ]; then exit 2; fi\ntest -f \"$1\"\n"),
            Ok(())
        );
        assert_eq!(
            screen("#!/bin/sh\nif [ ${#PATH} -eq 0 ]; then exit 2; fi\ntest -f \"$1\"\n"),
            Ok(())
        );
        // A `#` that *is* a comment still goes, or a commented-out check
        // counts as the file's one assertion.
        assert_eq!(
            screen("#!/bin/sh\n# test -f ssl/server.crt\nexit 1\n"),
            Err(VacuousWitness::NoAssertions)
        );
    }

    /// A single-quoted string is fully literal — `\` inside one is an ordinary
    /// character, so the escape skip every other language needs would run the
    /// scan past the closing quote and swallow the rest of the file.
    #[test]
    fn a_backslash_does_not_escape_inside_single_quotes() {
        assert_eq!(
            screen("#!/bin/sh\necho 'a\\'\ntest -f ssl/server.crt\n"),
            Ok(())
        );
    }

    /// Double quotes interpolate. Blanking their contents wholesale makes any
    /// two same-length strings compare equal, so a real assertion against a
    /// captured value would read as a self-comparison.
    #[test]
    fn an_expansion_survives_double_quote_sanitizing() {
        assert_eq!(
            screen("#!/bin/sh\nout=$(stella --version)\ntest \"ab\" = \"$o\" || exit 1\n"),
            Ok(())
        );
        // The genuine self-comparison is still caught.
        assert!(matches!(
            screen("#!/bin/sh\nout=$(stella --version)\ntest \"$out\" = \"$out\" || exit 1\n"),
            Err(VacuousWitness::ConstantAssertions { .. })
        ));
    }

    /// A command substitution split on whitespace looks like two constants.
    #[test]
    fn a_command_substitution_is_one_operand() {
        assert_eq!(
            screen("#!/bin/sh\ntest \"$(cat out.txt)\" = \"expected\" || exit 1\n"),
            Ok(())
        );
    }

    /// The shapes the `create_witness_test` boundary already accepts at the
    /// other end of this call (`stella-cli`'s `witness_tools` tests author both
    /// verbatim). Turning the screen on for `.sh` must not turn it on against
    /// them: a machine path and a `grep -q` over a config file are exactly the
    /// substantive shell witness the floor runner exists to allow.
    #[test]
    fn the_shapes_the_authoring_boundary_already_accepts_still_pass() {
        assert_eq!(
            screen("#!/bin/sh\nset -e\ntest -f ssl/server.crt\ntest -f ssl/server.key\n"),
            Ok(())
        );
        assert_eq!(
            screen(
                "#!/bin/sh\nset -e\ntest -x /usr/sbin/nginx\n\
                 grep -q 'ssl_protocols TLSv1.3' /etc/nginx/nginx.conf\n"
            ),
            Ok(())
        );
    }

    /// `.bash` reaches the same screen: `is_witness_test_path` keys on `.sh`,
    /// but the vocabulary is the same and a `bash` artifact must not be the
    /// one shape that slips through.
    #[test]
    fn bash_artifacts_are_screened_too() {
        assert_eq!(
            screen_witness_source("tests/witness_check.bash", "#!/bin/bash\nexit 1\n"),
            Err(VacuousWitness::NoAssertions)
        );
    }
}
