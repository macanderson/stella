//! The JSON lexer every transcript surface colours with.
//!
//! ## Why this lives here and not in `stella-tui`
//!
//! It used to live there, beside the deck's Rust/Python/Markdown/TOML lexers,
//! and it was the only one of them the *export* surfaces also needed: a tool's
//! arguments and most tool results are JSON, and JSON is the one language in
//! that set which is read far more than it is edited. So the Observatory and an
//! exported transcript rendered it as flat grey while the deck coloured it —
//! two renderings of one information model, which is the drift this crate
//! exists to prevent (#3644).
//!
//! `stella-transcript` is a near-leaf by contract and could not take an edge on
//! `stella-tui` to fix that; the edge had to go the other way. So the *lexing*
//! moved down here, where both the grid and the HTML renderer can reach it, and
//! `stella-tui` now depends on this crate and re-exports [`Tok`] and [`Runs`]
//! so its other lexers keep compiling unchanged. What stayed up there is
//! [`tok_style`-shaped colour resolution][crate::grid::Color]: turning a token
//! class into a `ratatui::Style` needs ratatui and the terminal theme, and this
//! crate has neither and wants neither. One lexer, three palettes.
//!
//! ## Contract
//!
//! Carried over verbatim from the original, because two surfaces now depend on
//! it and a middle-elided tool result is the normal input, not the edge case:
//!
//! - **Lossless.** Concatenating the run texts reproduces the input exactly.
//! - **Stateless.** One left-to-right scan per line, no carry between lines, so
//!   a line sliced out of a larger document classifies what it can see rather
//!   than mis-colouring the rest of the render.
//! - **Panic-free.** An unterminated string runs to end of line; a trailing
//!   lone backslash cannot step past the end.

/// A token class given a syntax colour; `None` runs stay the base colour.
///
/// Shared by this crate's two renderers and by `stella-tui`'s lexers for every
/// other language, which is why the variants are named for what they *are*
/// rather than for one language's use of them — the deck also spends
/// [`Tok::Keyword`] on markdown headings and TOML table headers, and
/// [`Tok::Number`] on markdown list markers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    /// Language keywords; also structural markup — the "shape" of a document.
    /// In JSON, an object key.
    Keyword,
    /// String literals. In JSON, a string *value* as opposed to a key.
    Str,
    /// Numeric literals, and the scalar constants that stand where a number
    /// stands (`true`, `false`, `null`).
    Number,
    /// Line comments. JSON has none; the other lexers do.
    Comment,
}

/// The tagged runs for one source line: consecutive text slices, each with an
/// optional token class. Concatenating the texts reproduces the line.
pub type Runs = Vec<(String, Option<Tok>)>;

/// Whether a body should be read as JSON.
///
/// A delimiter sniff, deliberately not a parse. The bodies this is asked about
/// are already middle-elided to fit an output budget, so a real parse would say
/// "no" to exactly the large pretty-printed objects worth colouring. Being
/// wrong costs a few mis-tinted punctuation runs and nothing else, because the
/// lexer is lossless either way.
///
/// Shared so the deck and the export cannot disagree about *which* bodies get
/// coloured, having already disagreed about whether any of them do.
#[must_use]
pub fn reads_as_json(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('{') || t.starts_with('[')
}

/// Whether an already-split body should be read as JSON.
///
/// The line-oriented form of [`reads_as_json`], for the surfaces that hold a
/// result as `Vec<String>` rather than as one string. Equivalent by
/// construction: `trim_start` on the joined text eats leading blank lines and
/// indentation alike, so the question is what the first non-blank line starts
/// with. Kept beside its sibling so the two cannot drift into disagreeing about
/// which bodies are JSON.
#[must_use]
pub fn body_reads_as_json(lines: &[String]) -> bool {
    lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| reads_as_json(l))
}

/// One line of JSON, tokenized left to right.
///
/// A key and a string value are the same lexical thing — a quoted string — and
/// only what follows separates them, so a closed string looks ahead to the next
/// non-space character and takes [`Tok::Keyword`] when that is a `:`. That is
/// the same call `stella-tui`'s TOML lexer makes for a bare key, and it keeps
/// the two object-shaped formats reading alike: the shape in one hue, the data
/// in another.
///
/// `true`/`false`/`null` take [`Tok::Number`] rather than a keyword hue: they
/// are scalar constants standing where a number could stand, and colouring them
/// like keys would make an object's shape unreadable at a glance — which is the
/// whole reason this line is coloured at all.
#[must_use]
pub fn json_runs(code: &str) -> Runs {
    let chars: Vec<char> = code.chars().collect();
    let mut runs: Runs = Vec::new();
    let mut plain = String::new();
    let mut i = 0;

    // Punctuation and whitespace accumulate untagged until a classified token
    // interrupts them, so the run list stays as short as the colouring needs.
    fn flush(plain: &mut String, runs: &mut Runs) {
        if !plain.is_empty() {
            runs.push((std::mem::take(plain), None));
        }
    }

    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            let start = i;
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    // A backslash escapes the next char, including a quote —
                    // clamp so a trailing lone backslash cannot step past the
                    // end.
                    '\\' => i = (i + 2).min(chars.len()),
                    '"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            let key = chars[i..]
                .iter()
                .find(|c| !c.is_whitespace())
                .is_some_and(|c| *c == ':');
            flush(&mut plain, &mut runs);
            runs.push((
                chars[start..i].iter().collect(),
                Some(if key { Tok::Keyword } else { Tok::Str }),
            ));
            continue;
        }
        if c.is_ascii_digit() || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let start = i;
            i += 1;
            while i < chars.len() {
                let d = chars[i];
                if d.is_ascii_digit() || d == '.' {
                    i += 1;
                } else if matches!(d, 'e' | 'E') {
                    // Only an exponent may carry a sign, and only right after
                    // the `e` — otherwise `1-2` would lex as one number.
                    i += 1;
                    if chars.get(i).is_some_and(|s| matches!(s, '+' | '-')) {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            flush(&mut plain, &mut runs);
            runs.push((chars[start..i].iter().collect(), Some(Tok::Number)));
            continue;
        }
        if c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let tok = matches!(word.as_str(), "true" | "false" | "null").then_some(Tok::Number);
            flush(&mut plain, &mut runs);
            runs.push((word, tok));
            continue;
        }
        plain.push(c);
        i += 1;
    }

    flush(&mut plain, &mut runs);
    runs
}
