//! The lexers every transcript surface colours with, and the one decision about
//! *which* lexer a tool-result body earns.
//!
//! ## Why this lives here and not in `stella-tui`
//!
//! The JSON lexer came down first, in #3644. It lived beside the deck's
//! Rust/Python/Markdown/TOML lexers and was the only one of them the *export*
//! surfaces also needed, so the Observatory and an exported transcript rendered
//! JSON as flat grey while the deck coloured it — two renderings of one
//! information model, which is the drift this crate exists to prevent.
//!
//! The others followed in #4036, for the same reason one step later. They
//! stayed behind on the argument that "only the deck reads them", and that
//! stopped being true the moment a `read_file` result had to render as *source*
//! rather than as flat grey: #4019 taught the deck to do it, and the export
//! surfaces had no way to follow without reaching up into `stella-tui`.
//! `stella-transcript` is a near-leaf by contract and cannot take that edge; it
//! had to go the other way, so those lexers now sit at the bottom with the
//! JSON one, behind [`tokenize`].
//!
//! What stayed up in `stella-tui` is the half that genuinely needs ratatui and
//! the terminal theme: `tok_style`, turning a [`Tok`] into a `Style`. The grid
//! and HTML renderers keep their own `tok_color` / `tok_class` mappings. One
//! lexer, three palettes — and no surface can differ on *classification* any
//! more, only on hue.
//!
//! ## The grammar, and why it landed here too (#4283)
//!
//! The lexer that arrived by those two moves was hand-written, knew six
//! languages, and guessed inside them. #4283 asked whether to replace it with a
//! grammar-backed one and — the part that mattered — *where* such a thing may
//! land. The answer was the same as above, for the same reason: here, so all
//! three surfaces gain it at once. A deck-local highlighting crate would have
//! been a fourth copy of a rendering decision this tree has already
//! de-duplicated twice.
//!
//! What shipped is tree-sitter in the private `grammar` module, covering the
//! eight languages whose grammars this workspace already compiles for
//! `stella-graph`, and the hand-written scans in `lang` surviving for the
//! three with no resident
//! grammar (Markdown, TOML, JSON). syntect was declined: it loads grammar and
//! theme assets at runtime, which invariant #2 forbids in this layer. The two
//! token classes only a grammar can produce — [`Tok::Type`] and
//! [`Tok::Function`] — grew the shared vocabulary, so all three palettes moved
//! in that same change.
//!
//! ## Contract
//!
//! Carried over verbatim from the original, because three surfaces now depend
//! on it and a middle-elided tool result is the normal input, not the edge case:
//!
//! - **Lossless.** Concatenating the run texts reproduces the input exactly.
//! - **Stateless.** One left-to-right scan per line, no carry between lines, so
//!   a line sliced out of a larger document classifies what it can see rather
//!   than mis-colouring the rest of the render.
//! - **Panic-free.** An unterminated string runs to end of line; a trailing
//!   lone backslash cannot step past the end.

mod grammar;
mod lang;

pub use lang::{Highlighter, Lang, lang_from_ext, lang_from_fence, lang_from_path, tokenize};

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
    /// Type positions — a declared type, a primitive, a type parameter.
    ///
    /// Grammar-only, and one of the two reasons the grammar was worth taking
    /// (#4283): a keyword table sees `Duration` and an ordinary variable as the
    /// same identifier, so the hand-written scan could never have produced this
    /// class. Languages still lexed by hand (Markdown, TOML, JSON) never emit
    /// it, which is not a gap — none of the three has type syntax.
    Type,
    /// A function or method *name* — the one being declared, or the one being
    /// called.
    ///
    /// The other grammar-only class. It earns a palette entry because in Java,
    /// C and Go most lines are declarations or calls, and without it the name
    /// that says what the line *does* wears the same tone as every local around
    /// it.
    Function,
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

/// [`reindent_json`], but only for a body that [`reads_as_json`] — `None` when
/// the body is anything else and must be rendered exactly as the tool emitted
/// it.
///
/// The pairing is the whole point: the *sniff* and the *transform* have to be
/// asked in one place, or a surface will re-indent a `read_file` listing whose
/// line-number gutter makes it look like nothing of the sort.
#[must_use]
pub fn reindent_json_body(text: &str) -> Option<String> {
    reads_as_json(text).then(|| reindent_json(text))
}

/// Cells one nesting level indents by in [`reindent_json`].
const JSON_INDENT: &str = "  ";

/// Re-lay a JSON body one member to a line, so a document that arrived as a
/// single eight-thousand-character line can be read.
///
/// A **whitespace** transform, not a parse and not a pretty-printer. Every
/// non-whitespace byte survives, in order; the only decisions are where a
/// newline goes and how far the next line indents. Two consequences, and both
/// are the point:
///
/// - **It works on the bodies these surfaces actually hold.** A tool result is
///   middle-elided to a char budget before it is ever rendered, so a large `gh
///   api` response is *not valid JSON* by the time anything looks at it, and a
///   `serde_json` round trip refuses precisely the body this exists for — the
///   reader gets the wall of text back. A scan re-indents what it can and
///   leaves the elision marker sitting between the two halves.
/// - **It is idempotent on an already-indented document**, because the layout
///   it produces is the layout it recognises: two spaces per level, a line per
///   comma, closers on their own line. So applying it unconditionally to a
///   body that [`reads_as_json`] costs nothing and needs no width threshold to
///   decide it is "long enough" — a threshold would be one more number two
///   surfaces could disagree about.
///
/// Whitespace *inside* a string literal is untouched, and so is whitespace in a
/// run that is not JSON at all — otherwise the elision marker's own words would
/// be glued together by the transform meant to make them legible.
#[must_use]
pub fn reindent_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut depth = 0usize;
    // A newline is *owed* after an opener or a comma, and paid at the next
    // non-whitespace character — never before it, or an empty `{}` would be
    // broken across three lines to say nothing.
    let mut owed = false;
    let mut in_string = false;
    let mut escaped = false;

    for ch in text.chars() {
        if in_string {
            out.push(ch);
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '{' | '[' => {
                pay(&mut out, owed, depth);
                out.push(ch);
                depth += 1;
                owed = true;
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                trim_trailing_blanks(&mut out);
                // An empty container keeps its two glyphs together: the newline
                // owed by the opener is simply forgiven.
                if !ends_with_opener(&out) {
                    pay(&mut out, true, depth);
                }
                out.push(ch);
                owed = false;
            }
            ',' => {
                trim_trailing_blanks(&mut out);
                out.push(ch);
                owed = true;
            }
            '"' => {
                pay(&mut out, owed, depth);
                owed = false;
                in_string = true;
                escaped = false;
                out.push(ch);
            }
            // Whitespace an owed newline is about to replace is dropped; every
            // other run of it is the input's own and is left alone.
            c if c.is_whitespace() => {
                if !owed {
                    out.push(c);
                }
            }
            c => {
                pay(&mut out, owed, depth);
                owed = false;
                out.push(c);
            }
        }
    }
    out
}

/// Emit an owed newline and the indent for `depth`.
fn pay(out: &mut String, owed: bool, depth: usize) {
    if !owed {
        return;
    }
    trim_trailing_blanks(out);
    if !out.is_empty() {
        out.push('\n');
    }
    for _ in 0..depth {
        out.push_str(JSON_INDENT);
    }
}

/// Drop the whitespace already written at the end of `out`, so a newline this
/// transform is about to emit does not land after the input's own.
fn trim_trailing_blanks(out: &mut String) {
    while out.ends_with(|c: char| c.is_whitespace()) {
        out.pop();
    }
}

/// Whether the last thing written was a container opener — the one case where
/// the newline it owes is forgiven rather than paid.
fn ends_with_opener(out: &str) -> bool {
    out.ends_with('{') || out.ends_with('[')
}

/// `read_file`'s line-number gutter, split off one body line.
///
/// The emitter's shape is `stella-tools`' `{line_num:>6}\t{line}`: padding
/// spaces, ASCII digits, one tab. Parsed rather than assumed, so the lines of
/// the same body that carry no gutter — the read footer, a payload-cap notice —
/// say so by yielding `None` instead of having six characters of their own text
/// amputated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gutter<'a> {
    /// The gutter exactly as it appeared, its tab included. What a surface
    /// re-renders when it wants the emitter's own column back.
    pub text: &'a str,
    /// The line number the gutter states — the number *in the file*, which is
    /// not the line's index in the body whenever the read carried an offset.
    pub number: usize,
    /// Everything after the tab: the file's actual source for this line.
    pub source: &'a str,
}

/// Split [`Gutter`] off a body line, or `None` when the line carries none.
///
/// The *first* tab is the separator; a source line may contain more, and those
/// belong to the source.
#[must_use]
pub fn split_gutter(line: &str) -> Option<Gutter<'_>> {
    let tab = line.find('\t')?;
    let digits = line[..tab].trim_start_matches(' ');
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (text, source) = line.split_at(tab + 1);
    Some(Gutter {
        text,
        // A gutter too long to be a line number is not a gutter. Refusing here
        // rather than saturating keeps the line whole: it renders plain, which
        // is right for whatever it actually was.
        number: digits.parse().ok()?,
        source,
    })
}

/// How a tool-result body is coloured, and whether it is a numbered listing.
///
/// This replaces the bare "is it JSON" bool all three surfaces used to ask.
/// That question was the one they could answer cheaply rather than the one they
/// wanted: a body earned colouring only when it opened with `{` or `[`, and a
/// `read_file` result never does, because every line starts with a digit from
/// the gutter. The consequence was wider than "source is not highlighted" —
/// reading a `.json` file through `read_file` was not highlighted either
/// (#4019).
///
/// Deciding it once, here, is the point: the deck, the grid and the HTML
/// renderer previously each held their own copy of the predicate, and #3644
/// exists because copies of a rendering decision drift.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BodyPaint {
    /// The language each source line lexes as, or `None` for one flat tone.
    ///
    /// `None` with `numbered` set is a real combination and not a
    /// contradiction: a read of an extension no lexer covers still has a gutter
    /// to separate from its text, it just has no colouring to apply to the rest.
    pub lang: Option<Lang>,
    /// Whether this body's lines carry [`Gutter`]s.
    ///
    /// A surface that draws its own line numbers must consult this before doing
    /// so, or it stacks a second gutter on the emitter's — which is exactly what
    /// both export renderers did, and with the *wrong* number: they counted
    /// position in the fold, so a read at offset 780 was labelled line 1.
    pub numbered: bool,
}

impl BodyPaint {
    /// The paint for a body the caller already knows is a JSON document.
    ///
    /// For the one input that needs no sniffing: a tool call's *arguments*,
    /// which the deck pretty-prints from a `serde_json::Value` and so cannot be
    /// anything else. Asking [`body_paint`] would work and would be a lie about
    /// where the knowledge came from.
    #[must_use]
    pub fn json() -> Self {
        Self {
            lang: Some(Lang::Json),
            numbered: false,
        }
    }

    /// Whether this body carries syntax colour.
    ///
    /// A surface that flattens a line into a single style — the deck promoting
    /// a body's first line onto its result row — must not do so for a coloured
    /// body, or it strips exactly the colouring this exists to produce.
    #[must_use]
    pub fn colored(self) -> bool {
        self.lang.is_some()
    }
}

/// Decide how to paint a body held as one string (the deck's shape).
///
/// Order matters. A body that opens as a JSON document is JSON whatever tool
/// produced it — that is a fact about the bytes, and it settles the question
/// before the path is consulted. Only then does the path get a say, and only
/// for a body that actually *is* a numbered listing: the same `read_file` can
/// answer `(file has 3 lines, offset 9 is past end)`, and lexing that as Rust
/// because the path ended in `.rs` would colour a sentence.
#[must_use]
pub fn body_paint(path: Option<&str>, text: &str) -> BodyPaint {
    paint(path, text.lines().find(|l| !l.trim().is_empty()))
}

/// [`body_paint`] for a body already split into lines (the export's shape).
///
/// Equivalent by construction — both hand the same first non-blank line to the
/// same decision — and kept beside its sibling for the reason
/// [`body_reads_as_json`] is kept beside [`reads_as_json`]: two surfaces asking
/// one question must not be able to drift into two answers.
#[must_use]
pub fn lines_body_paint(path: Option<&str>, lines: &[String]) -> BodyPaint {
    paint(
        path,
        lines
            .iter()
            .map(String::as_str)
            .find(|l| !l.trim().is_empty()),
    )
}

/// One body line, split into the parts a renderer paints separately.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PaintedLine<'a> {
    /// The emitter's line-number gutter, when this line carries one. A surface
    /// draws it as its own number column and never lexes it — otherwise every
    /// line number is painted as a numeric literal, the brightest column on
    /// screen and the one saying least.
    pub gutter: Option<Gutter<'a>>,
    /// The text to lex: the source after the gutter, or the whole line.
    pub source: &'a str,
    /// The language to lex `source` as, or `None` for one flat tone.
    pub lang: Option<Lang>,
}

/// Split one body line the way every surface must split it.
///
/// The third and last thing that was about to exist in three copies. The deck,
/// the grid and the HTML renderer each need the same three-way answer — is
/// there a gutter, what is the source, what lexes it — and each had its own
/// reason to get it subtly wrong: the deck by lexing a footer as Rust, the two
/// export renderers by stacking their own line numbers on top of the emitter's.
///
/// The rule: a `numbered` body's line is source *only* when it actually carries
/// a gutter. A line without one is the read footer or a payload-cap notice —
/// prose, not code — so it lexes as nothing at all rather than as the file's
/// language.
#[must_use]
pub fn paint_line(paint: BodyPaint, line: &str) -> PaintedLine<'_> {
    if !paint.numbered {
        return PaintedLine {
            gutter: None,
            source: line,
            lang: paint.lang,
        };
    }
    match split_gutter(line) {
        Some(gutter) => PaintedLine {
            source: gutter.source,
            gutter: Some(gutter),
            lang: paint.lang,
        },
        None => PaintedLine {
            gutter: None,
            source: line,
            lang: None,
        },
    }
}

/// The shared decision, over whichever line came first.
fn paint(path: Option<&str>, first: Option<&str>) -> BodyPaint {
    let Some(first) = first else {
        return BodyPaint::default();
    };
    if reads_as_json(first) {
        return BodyPaint {
            lang: Some(Lang::Json),
            numbered: false,
        };
    }
    if split_gutter(first).is_none() {
        return BodyPaint::default();
    }
    BodyPaint {
        lang: path.and_then(lang_from_path),
        numbered: true,
    }
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
