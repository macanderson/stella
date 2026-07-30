//! Assertion-density screening for authored witness tests (#863, ROADMAP §1).
//!
//! The flip oracle credits a witness that fails on the pre-change tree and
//! passes on the candidate's. That proves the test *reacted* to the change; it
//! does not prove the test *constrains* it. Three shapes flip green while
//! testing nothing:
//!
//! - **No assertion at all.** The test fails now because a symbol is missing
//!   and passes the moment it exists — a compile check wearing a test's name.
//! - **Assertions over constants only.** `assert_eq!(2, 2)` is a tautology; it
//!   holds against every implementation, including one that ignores the goal.
//! - **A catch-all panic check.** A bare `#[should_panic]` or
//!   `pytest.raises(Exception)` is satisfied by ANY panic, so it can flip on a
//!   panic the goal never addressed.
//!
//! This module is the static screen for all three, and it is deliberately
//! **lexical**: a scan over the authored source with no parser, no toolchain,
//! and no process. It runs at the `create_witness_test` boundary — the only way
//! witness bytes reach disk — so a vacuous artifact is refused with a named
//! reason *inside the author's own turn*. The author revises against the tool
//! error, and the run never pays the baseline test run or the bounded repair
//! turn for a test that could not have witnessed anything. That is strictly
//! cheaper than the check ROADMAP §1 sketched, which spent a turn first.
//!
//! # Conservative by construction
//!
//! A false rejection costs far more here than a false acceptance: it discards a
//! legitimate witness and drops the run to the unauthored ladder. So the screen
//! rejects only on **whole-file** evidence — a single substantive assertion
//! anywhere in the artifact clears it. Sloppiness is not the target;
//! vacuousness is. Everything it cannot read confidently (an unrecognized
//! extension, a language whose assertion vocabulary is not listed) passes.
//!
//! # Known limits
//!
//! Lexical means lexical. An assertion assembled at runtime, reached through a
//! macro defined in another file, or hidden behind an interpolated template
//! literal is invisible here — each of those *passes* the screen, per the
//! posture above. The screen's claim is narrow and worth exactly what it says:
//! the three vacuous shapes above never reach the oracle.

/// Why an authored witness source cannot witness anything. Each variant names
/// the shape, because the message is what the author revises against.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VacuousWitness {
    /// No assertion vocabulary appears anywhere in the artifact.
    #[error(
        "the witness test asserts nothing: add an assertion on the behavior the goal changes, \
         or a fail→pass flip proves only that the code compiles"
    )]
    NoAssertions,
    /// Every assertion site compares constants to constants, or a value to
    /// itself.
    #[error(
        "every assertion in the witness test is a tautology over constants (`{example}`): it \
         holds against any implementation, so its flip would prove nothing — assert on a value \
         the goal actually produces"
    )]
    ConstantAssertions {
        /// The first tautological site, collapsed to one line.
        example: String,
    },
    /// The artifact's only failure mechanism is an unnamed panic expectation.
    #[error(
        "the witness test's only failure mechanism is a catch-all panic check (`{example}`): it \
         is satisfied by ANY panic, including one the goal never addresses — name the expected \
         panic or assert on a value instead"
    )]
    CatchAllPanic {
        /// The first catch-all site, collapsed to one line.
        example: String,
    },
}

/// Screen an authored witness artifact's source for the three vacuous shapes.
///
/// `path` selects the assertion vocabulary by extension; `source` is the
/// complete authored file. `Ok(())` means the screen found something that can
/// genuinely fail — not that the test is good, only that it is not vacuous.
///
/// An unrecognized extension is `Ok(())`: [`super::is_witness_test_path`] has
/// already decided which artifacts are admissible at all, and this screen must
/// never be the thing that rejects a language it simply does not read.
pub fn screen_witness_source(path: &str, source: &str) -> Result<(), VacuousWitness> {
    let Some(lang) = Lang::from_path(path) else {
        return Ok(());
    };
    let sanitized = sanitize(source, lang);
    let mut first_tautology: Option<String> = None;
    let mut first_catch_all: Option<String> = None;
    let mut any_site = false;

    for site in sites(&sanitized, lang) {
        any_site = true;
        // The original bytes, not the sanitized ones: offsets are preserved
        // byte-for-byte precisely so the message can quote what was written.
        let example = || excerpt(&source[site.start..site.end]);
        match classify(&sanitized[site.start..site.end], lang) {
            Verdict::Substantive => return Ok(()),
            Verdict::CatchAll => {
                first_catch_all.get_or_insert_with(example);
            }
            Verdict::Tautological => {
                first_tautology.get_or_insert_with(example);
            }
        }
    }

    if !any_site {
        return Err(VacuousWitness::NoAssertions);
    }
    // A catch-all is reported ahead of a tautology when both are present: it is
    // the more specific diagnosis, and the author's fix differs (name the
    // panic, rather than assert on a value).
    if let Some(example) = first_catch_all {
        return Err(VacuousWitness::CatchAllPanic { example });
    }
    if let Some(example) = first_tautology {
        return Err(VacuousWitness::ConstantAssertions { example });
    }
    Ok(())
}

/// The witness languages, keyed off the artifact extension the tool boundary
/// already validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    /// The whole JS/TS family — one vocabulary covers Vitest and Jest.
    Script,
    Go,
    CSharp,
}

impl Lang {
    fn from_path(path: &str) -> Option<Self> {
        let name = path.replace('\\', "/").to_ascii_lowercase();
        match name.rsplit_once('.').map(|(_, ext)| ext)? {
            "rs" => Some(Lang::Rust),
            "py" => Some(Lang::Python),
            "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => Some(Lang::Script),
            "go" => Some(Lang::Go),
            "cs" => Some(Lang::CSharp),
            _ => None,
        }
    }

    /// Whether `'` opens a character literal in this language. Rust is the
    /// exception that forces the question: there, `'` also opens a lifetime,
    /// which has no closing quote at all.
    fn quotes_are_unambiguous(self) -> bool {
        !matches!(self, Lang::Rust)
    }
}

/// One assertion site: byte offsets into the source (and, identically, into its
/// sanitized twin).
struct Site {
    start: usize,
    end: usize,
}

/// How an assertion marker delimits its site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Span {
    /// Forward from the marker through the balanced call it opens, following
    /// any method chain (`expect(x).toBe(y)`).
    Forward,
    /// Also backward over the receiver: `.unwrap()` and `.expect(…)` assert
    /// about the expression to their LEFT, which is the part that decides
    /// whether they assert anything (`Some(1).unwrap()` does not).
    Receiver,
}

/// The assertion vocabulary: a marker token and how far its site reaches.
///
/// Tokens carry their own punctuation on purpose. `.unwrap()` is listed with
/// parentheses so it cannot match `.unwrap_or(`, which is a default rather than
/// an assertion; `assert ` carries its space so it cannot match `assertion`.
fn markers(lang: Lang) -> &'static [(&'static str, Span)] {
    match lang {
        Lang::Rust => &[
            ("assert!", Span::Forward),
            ("assert_eq!", Span::Forward),
            ("assert_ne!", Span::Forward),
            ("assert_matches!", Span::Forward),
            ("debug_assert!", Span::Forward),
            ("debug_assert_eq!", Span::Forward),
            ("debug_assert_ne!", Span::Forward),
            ("#[should_panic", Span::Forward),
            (".unwrap()", Span::Receiver),
            (".expect(", Span::Receiver),
        ],
        Lang::Python => &[
            ("assert ", Span::Forward),
            ("assert(", Span::Forward),
            ("assertEqual", Span::Forward),
            ("assertNotEqual", Span::Forward),
            ("assertAlmostEqual", Span::Forward),
            ("assertTrue", Span::Forward),
            ("assertFalse", Span::Forward),
            ("assertIn", Span::Forward),
            ("assertNotIn", Span::Forward),
            ("assertIs", Span::Forward),
            ("assertIsNot", Span::Forward),
            ("assertIsNone", Span::Forward),
            ("assertIsNotNone", Span::Forward),
            ("assertGreater", Span::Forward),
            ("assertLess", Span::Forward),
            ("assertListEqual", Span::Forward),
            ("assertDictEqual", Span::Forward),
            ("assertCountEqual", Span::Forward),
            ("assertRegex", Span::Forward),
            ("assertRaises", Span::Forward),
            ("raises(", Span::Forward),
        ],
        Lang::Script => &[
            ("expect(", Span::Forward),
            ("assert", Span::Forward),
            ("t.is(", Span::Forward),
            ("t.true(", Span::Forward),
            ("t.false(", Span::Forward),
            ("t.deepEqual(", Span::Forward),
            ("t.throws(", Span::Forward),
        ],
        Lang::Go => &[
            ("t.Error", Span::Forward),
            ("t.Fatal", Span::Forward),
            ("t.Fail", Span::Forward),
            ("require.", Span::Forward),
            ("assert.", Span::Forward),
        ],
        Lang::CSharp => &[
            ("Assert.", Span::Forward),
            (".Should(", Span::Receiver),
        ],
    }
}

/// Identifiers that carry no evidence: the assertion vocabulary itself, the
/// chain methods that spell an assertion out, value-neutral wrappers whose
/// argument is the only thing that matters (`Some(1)` is as constant as `1`),
/// and language keywords.
///
/// Every name here is one the screen must NOT count as "the test named
/// something real". Omitting a wrapper makes the screen miss a tautology
/// (`Some(1).unwrap()`); adding a name that can carry meaning makes it reject a
/// real test, so the list stays short and boring.
fn neutral(lang: Lang) -> &'static [&'static str] {
    const KEYWORDS: &[&str] = &[
        "let", "mut", "const", "var", "return", "if", "else", "for", "while", "match", "as", "in",
        "not", "and", "or", "is", "await", "async", "self", "this", "def", "fn", "func", "with",
        "true", "false", "True", "False", "None", "null", "nil", "undefined", "NULL",
    ];
    // Concatenated at each call rather than stored: the lists are a handful of
    // entries scanned once per assertion site, and a `static` join would need
    // either a build step or a lazy lock to say the same thing.
    macro_rules! joined {
        ($($extra:expr),* $(,)?) => {{
            static ONCE: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
            ONCE.get_or_init(|| {
                let mut all: Vec<&'static str> = KEYWORDS.to_vec();
                $(all.extend_from_slice($extra);)*
                all
            })
            .as_slice()
        }};
    }
    match lang {
        Lang::Rust => joined!(&[
            "assert",
            "assert_eq",
            "assert_ne",
            "assert_matches",
            "debug_assert",
            "debug_assert_eq",
            "debug_assert_ne",
            "should_panic",
            "expected",
            "panic",
            "unwrap",
            "expect",
            "Some",
            "Ok",
            "Err",
            "Box",
            "String",
            "new",
            "from",
            "to_string",
            "to_owned",
            "into",
            "vec",
        ]),
        Lang::Python => joined!(&[
            "assert",
            "assertEqual",
            "assertNotEqual",
            "assertAlmostEqual",
            "assertTrue",
            "assertFalse",
            "assertIn",
            "assertNotIn",
            "assertIs",
            "assertIsNot",
            "assertIsNone",
            "assertIsNotNone",
            "assertGreater",
            "assertLess",
            "assertListEqual",
            "assertDictEqual",
            "assertCountEqual",
            "assertRegex",
            "assertRaises",
            "pytest",
            "raises",
            "match",
            "len",
            "str",
            "int",
            "float",
            "bool",
            "list",
            "dict",
            "set",
            "tuple",
        ]),
        Lang::Script => joined!(&[
            "expect",
            "assert",
            "strictEqual",
            "deepStrictEqual",
            "notStrictEqual",
            "equal",
            "deepEqual",
            "ok",
            "throws",
            "toBe",
            "toEqual",
            "toStrictEqual",
            "toBeTruthy",
            "toBeFalsy",
            "toBeNull",
            "toBeUndefined",
            "toBeDefined",
            "toBeNaN",
            "toContain",
            "toHaveLength",
            "toThrow",
            "toThrowError",
            "toMatch",
            "toMatchObject",
            "toBeGreaterThan",
            "toBeLessThan",
            "toBeCloseTo",
            "resolves",
            "rejects",
            "length",
            "Number",
            "String",
            "Boolean",
            "t",
        ]),
        Lang::Go => joined!(&[
            "t",
            "Error",
            "Errorf",
            "Fatal",
            "Fatalf",
            "Fail",
            "FailNow",
            "Helper",
            "Log",
            "Logf",
            "require",
            "assert",
            "Equal",
            "NotEqual",
            "NoError",
            "True",
            "False",
            "Nil",
            "NotNil",
            "len",
        ]),
        Lang::CSharp => joined!(&[
            "Assert",
            "Equal",
            "NotEqual",
            "True",
            "False",
            "Null",
            "NotNull",
            "Throws",
            "Should",
            "Be",
            "BeTrue",
            "BeFalse",
            "BeNull",
            "Contain",
            "HaveCount",
            "BeEquivalentTo",
        ]),
    }
}

/// Every assertion site in the sanitized source, in order of appearance.
///
/// Overlaps are dropped: `.expect(` inside a site already claimed by an
/// enclosing `assert!` would otherwise be counted twice, and a duplicate site
/// cannot change the verdict but can change which excerpt the message quotes.
fn sites(sanitized: &str, lang: Lang) -> Vec<Site> {
    let bytes = sanitized.as_bytes();
    let mut found: Vec<Site> = Vec::new();
    for (token, span) in markers(lang) {
        let mut from = 0usize;
        while let Some(offset) = sanitized[from..].find(token) {
            let at = from + offset;
            from = at + 1;
            // An identifier boundary on the left, so `assert ` cannot be found
            // inside `reassert ` and `Assert.` not inside `MyAssert.`.
            if token.starts_with(|c: char| c.is_alphanumeric() || c == '_')
                && at > 0
                && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_')
            {
                continue;
            }
            let end = forward_end(bytes, at);
            let start = match span {
                Span::Forward => at,
                Span::Receiver => receiver_start(bytes, at),
            };
            found.push(Site { start, end });
        }
    }
    found.sort_by_key(|site| (site.start, std::cmp::Reverse(site.end)));
    let mut kept: Vec<Site> = Vec::new();
    for site in found {
        if kept.last().is_some_and(|last| site.end <= last.end) {
            continue;
        }
        kept.push(site);
    }
    kept
}

/// Walk forward from a marker to the end of its site: through the balanced
/// bracket group it opens, then across any `.`/`?` chain that continues from
/// there. A marker that opens no group (Python's bare `assert x == y`) ends at
/// the line.
fn forward_end(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    let mut depth = 0usize;
    let mut opened = false;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => {
                depth += 1;
                opened = true;
            }
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b';' | b'\n' if depth == 0 => break,
            _ => {}
        }
        i += 1;
        if opened && depth == 0 {
            // A chain continues the same assertion; anything else ends it.
            let mut j = i;
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
                j += 1;
            }
            if j < bytes.len() && matches!(bytes[j], b'.' | b'?') {
                // Re-arm: the chain's next call has not opened yet, and leaving
                // `opened` set would end the site on the very next byte —
                // clipping `expect(1).toBe(1)` down to `expect(1).`, which
                // hides the half that makes it a tautology.
                i = j;
                opened = false;
                continue;
            }
            break;
        }
    }
    i
}

/// Walk backward from a `.unwrap()`/`.expect(` marker to the start of the
/// expression it asserts about, so the receiver is part of the site.
///
/// Stops at an assignment as well as at a statement boundary: in
/// `let v = Some(1).unwrap();` the evidence is `Some(1)`, and dragging `let v`
/// into the site would make the binding's own name read as something the test
/// named.
fn receiver_start(bytes: &[u8], marker: usize) -> usize {
    let mut i = marker;
    let mut depth = 0usize;
    while i > 0 {
        let c = bytes[i - 1];
        match c {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            _ if depth > 0 => {}
            b'"' | b'\'' => {}
            c if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'?' | b'!') => {}
            _ => break,
        }
        i -= 1;
    }
    i
}

/// What one assertion site is worth.
enum Verdict {
    /// It names something the implementation decides.
    Substantive,
    /// Constants only, or a value compared to itself.
    Tautological,
    /// An unnamed panic expectation: any panic satisfies it.
    CatchAll,
}

fn classify(site: &str, lang: Lang) -> Verdict {
    if is_catch_all(site, lang) {
        return Verdict::CatchAll;
    }
    // A NAMED panic expectation is itself the assertion: the test passes only
    // on a panic carrying that message. Its identifiers are all vocabulary, so
    // the scan below would read it as a tautology and reject a witness that
    // constrains its failure precisely.
    if lang == Lang::Rust && site.contains("#[should_panic") {
        return Verdict::Substantive;
    }
    // Self-comparison is checked before the identifier scan, because both
    // sides of `assert_eq!(a, a)` name something real and would otherwise pass.
    if compares_a_value_to_itself(site) {
        return Verdict::Tautological;
    }
    let neutral = neutral(lang);
    for identifier in identifiers(site) {
        if !neutral.contains(&identifier) {
            return Verdict::Substantive;
        }
    }
    Verdict::Tautological
}

/// A panic expectation with nothing named: bare `#[should_panic]`,
/// `pytest.raises(Exception)` with no `match=`, or `.toThrow()` with no
/// argument. The *named* forms — `#[should_panic(expected = "…")]`,
/// `raises(ValueError)`, `toThrow("…")` — constrain the failure and are
/// classified normally.
fn is_catch_all(site: &str, lang: Lang) -> bool {
    let squeezed: String = site.chars().filter(|c| !c.is_whitespace()).collect();
    match lang {
        Lang::Rust => site.contains("#[should_panic") && !site.contains("expected"),
        Lang::Python => {
            (squeezed.contains("raises(Exception")
                || squeezed.contains("raises(BaseException")
                || squeezed.contains("assertRaises(Exception")
                || squeezed.contains("assertRaises(BaseException"))
                && !site.contains("match")
        }
        Lang::Script => squeezed.contains("toThrow()") || squeezed.contains("toThrowError()"),
        Lang::Go | Lang::CSharp => false,
    }
}

/// Whether every operand in the site is the same text — `assert_eq!(a, a)`,
/// `assert!(x == x)`, `expect(f(1)).toBe(f(1))`. Each proves that a value
/// equals itself, which no implementation can falsify.
///
/// Operands are the top-level comma- and `==`-separated pieces of every
/// bracket group in the site, so the rule reads a macro's argument list and a
/// method chain's arguments the same way.
fn compares_a_value_to_itself(site: &str) -> bool {
    let mut operands: Vec<String> = Vec::new();
    let bytes = site.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if matches!(bytes[i], b'(' | b'[') {
            let Some(close) = balanced_close(bytes, i) else {
                break;
            };
            operands.extend(split_operands(&site[i + 1..close]));
            i = close + 1;
            continue;
        }
        i += 1;
    }
    operands.len() >= 2 && operands.windows(2).all(|pair| pair[0] == pair[1])
}

/// The index of the bracket closing the one at `open`, if it is balanced.
fn balanced_close(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split one bracket group's contents on top-level `,` and `==`, normalized to
/// single spaces so formatting differences do not hide a self-comparison.
fn split_operands(inner: &str) -> Vec<String> {
    let bytes = inner.as_bytes();
    let mut parts: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(inner[start..i].to_string());
                start = i + 1;
            }
            b'=' if depth == 0 && bytes.get(i + 1) == Some(&b'=') => {
                parts.push(inner[start..i].to_string());
                i += 2;
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(inner[start..].to_string());
    parts
        .into_iter()
        .map(|part| part.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|part| !part.is_empty())
        .collect()
}

/// The identifier-shaped tokens in a site, with quoted regions removed.
///
/// Quotes are still delimited in the sanitized text (only their *contents* were
/// blanked), so dropping them here is what stops `assert_eq!("foo", "foo")`
/// from reading as a test that named `foo`.
fn identifiers(site: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = site.as_bytes();
    let mut i = 0usize;
    let mut start: Option<usize> = None;
    while i < bytes.len() {
        let c = bytes[i];
        if matches!(c, b'"' | b'\'' | b'`') {
            if let Some(from) = start.take() {
                out.push(&site[from..i]);
            }
            i += 1;
            while i < bytes.len() && bytes[i] != c {
                i += 1;
            }
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' || (start.is_some() && c.is_ascii_digit()) {
            start.get_or_insert(i);
        } else if let Some(from) = start.take() {
            out.push(&site[from..i]);
        }
        i += 1;
    }
    if let Some(from) = start {
        out.push(&site[from..]);
    }
    out
}

/// Blank every comment byte and every string/character-literal *content* byte,
/// preserving length exactly so offsets into the result index the original
/// source unchanged. That byte-for-byte property is what lets the verdict be
/// computed on sanitized text while the message quotes what the author wrote.
///
/// Comments must go or a commented-out `assert_eq!` counts as an assertion;
/// literal contents must go or the words inside a string count as identifiers.
/// Delimiters stay, so a blanked literal is still recognizably a literal.
fn sanitize(source: &str, lang: Lang) -> String {
    let mut out = source.as_bytes().to_vec();
    let bytes = source.as_bytes();
    let mut i = 0usize;
    let blank = |out: &mut Vec<u8>, range: std::ops::Range<usize>, fill: u8| {
        for byte in &mut out[range] {
            *byte = fill;
        }
    };
    while i < bytes.len() {
        // Line comments.
        let line_comment = match lang {
            Lang::Python => bytes[i] == b'#',
            _ => bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/'),
        };
        if line_comment {
            let end = source[i..]
                .find('\n')
                .map_or(bytes.len(), |offset| i + offset);
            blank(&mut out, i..end, b' ');
            i = end;
            continue;
        }
        // Block comments (none in Python).
        if lang != Lang::Python && bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let end = source[i + 2..]
                .find("*/")
                .map_or(bytes.len(), |offset| i + 2 + offset + 2);
            blank(&mut out, i..end, b' ');
            i = end;
            continue;
        }
        // Rust raw strings: `r"…"`, `r#"…"#`, `br##"…"##`. The hash count is
        // part of the terminator, so it has to be counted going in.
        if lang == Lang::Rust
            && let Some(after) = raw_string_open(bytes, i)
        {
            let (content, end) = after;
            blank(&mut out, content, b'x');
            i = end;
            continue;
        }
        if matches!(bytes[i], b'"' | b'\'' | b'`') {
            let quote = bytes[i];
            // Rust's `'` is also a lifetime, which never closes. A character
            // literal is `'\…'` or `'c'`; anything else beginning with `'` is
            // a lifetime and must be left alone, or the scan swallows the rest
            // of the file looking for a quote that is not there.
            if quote == b'\''
                && !lang.quotes_are_unambiguous()
                && !(bytes.get(i + 1) == Some(&b'\\')
                    || bytes.get(i + 2) == Some(&b'\'')
                    || bytes.get(i + 3) == Some(&b'\''))
            {
                i += 1;
                continue;
            }
            // Python triple quotes: a docstring holding an `assert` example is
            // exactly the false positive this exists to avoid.
            let triple = lang == Lang::Python
                && bytes.get(i + 1) == Some(&quote)
                && bytes.get(i + 2) == Some(&quote);
            let (content_start, terminator) = if triple {
                (i + 3, 3usize)
            } else {
                (i + 1, 1usize)
            };
            let mut j = content_start;
            let end = loop {
                if j >= bytes.len() {
                    break bytes.len();
                }
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == quote
                    && (terminator == 1
                        || (bytes.get(j + 1) == Some(&quote) && bytes.get(j + 2) == Some(&quote)))
                {
                    break j + terminator;
                }
                j += 1;
            };
            let content_end = end.saturating_sub(terminator).max(content_start);
            blank(&mut out, content_start..content_end, b'x');
            i = end;
            continue;
        }
        i += 1;
    }
    // Every replacement is one ASCII byte for one byte of an already-valid
    // UTF-8 string, so the result cannot be invalid; the fallback keeps the
    // screen from being the thing that panics on odd input.
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

/// If a Rust raw string opens at `i`, its content range and the index just past
/// its terminator.
fn raw_string_open(bytes: &[u8], i: usize) -> Option<(std::ops::Range<usize>, usize)> {
    let mut cursor = i;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hashes = bytes[cursor..]
        .iter()
        .take_while(|byte| **byte == b'#')
        .count();
    cursor += hashes;
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    let content_start = cursor + 1;
    let mut j = content_start;
    while j < bytes.len() {
        if bytes[j] == b'"' && bytes[j + 1..].iter().take(hashes).all(|byte| *byte == b'#') {
            let end = j + 1 + hashes;
            if end <= bytes.len() {
                return Some((content_start..j, end));
            }
        }
        j += 1;
    }
    Some((content_start..bytes.len(), bytes.len()))
}

/// One assertion site collapsed to a single quotable line.
fn excerpt(site: &str) -> String {
    let one_line = site.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 96;
    if one_line.chars().count() <= MAX {
        return one_line;
    }
    let kept: String = one_line.chars().take(MAX).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(path: &str, source: &str) -> Result<(), VacuousWitness> {
        screen_witness_source(path, source)
    }

    /// Acceptance, first clause: an authored witness with zero assertions is
    /// rejected before it can be run.
    #[test]
    fn a_witness_with_no_assertions_is_rejected() {
        for (path, source) in [
            (
                "tests/witness.rs",
                "#[test]\nfn witness_retry() {\n    let _ = stella::retry();\n}\n",
            ),
            (
                "tests/test_witness.py",
                "def test_witness():\n    result = retry()\n    print(result)\n",
            ),
            (
                "src/witness.test.ts",
                "import { retry } from './retry';\ntest('retry', () => {\n  retry();\n});\n",
            ),
            (
                "pkg/witness_test.go",
                "func TestRetry(t *testing.T) {\n\tgot := Retry()\n\t_ = got\n}\n",
            ),
        ] {
            assert_eq!(
                screen(path, source),
                Err(VacuousWitness::NoAssertions),
                "{path} asserts nothing"
            );
        }
    }

    /// Acceptance, second clause: a witness asserting only on constants is
    /// rejected, with the offending site named.
    #[test]
    fn a_witness_asserting_only_constants_is_rejected() {
        for (path, source, needle) in [
            (
                "tests/witness.rs",
                "#[test]\nfn witness() {\n    assert!(true);\n    assert_eq!(2, 2);\n}\n",
                "assert!(true)",
            ),
            (
                "tests/witness.rs",
                "#[test]\nfn witness() {\n    assert_eq!(\"same\", \"same\");\n}\n",
                "assert_eq!",
            ),
            (
                "tests/test_witness.py",
                "def test_witness():\n    assert True\n",
                "assert True",
            ),
            (
                "src/witness.test.ts",
                "test('x', () => {\n  expect(1).toBe(1);\n});\n",
                "expect(1).toBe(1)",
            ),
            (
                "tests/Witness/WitnessTests.cs",
                "public void Witness() { Assert.Equal(1, 1); }\n",
                "Assert.Equal(1, 1)",
            ),
        ] {
            match screen(path, source) {
                Err(VacuousWitness::ConstantAssertions { example }) => assert!(
                    example.contains(needle),
                    "{path}: expected the site named, got `{example}`"
                ),
                other => panic!("{path} asserts only constants, got {other:?}"),
            }
        }
    }

    /// `unwrap()`/`expect()` on a constant is the third catch-all ROADMAP §1
    /// names: it cannot fail, so it cannot witness.
    #[test]
    fn unwrap_on_a_constant_is_not_an_assertion() {
        for source in [
            "#[test]\nfn witness() {\n    Some(1).unwrap();\n}\n",
            "#[test]\nfn witness() {\n    let value = Some(42).unwrap();\n}\n",
            "#[test]\nfn witness() {\n    Ok::<_, ()>(1).expect(\"never\");\n}\n",
        ] {
            assert!(
                matches!(
                    screen("tests/witness.rs", source),
                    Err(VacuousWitness::ConstantAssertions { .. })
                ),
                "unwrapping a constant witnesses nothing: {source}"
            );
        }
        // The same shape over a real call is the ordinary way a Rust witness
        // asserts success, and must survive.
        assert_eq!(
            screen(
                "tests/witness.rs",
                "#[test]\nfn witness() {\n    stella::retry().unwrap();\n}\n"
            ),
            Ok(())
        );
    }

    /// A bare panic expectation is satisfied by any panic; a named one is not.
    #[test]
    fn a_catch_all_panic_check_is_rejected_but_a_named_one_is_kept() {
        match screen(
            "tests/witness.rs",
            "#[test]\n#[should_panic]\nfn witness() {\n    stella::retry();\n}\n",
        ) {
            Err(VacuousWitness::CatchAllPanic { example }) => {
                assert!(example.contains("should_panic"), "{example}");
            }
            other => panic!("a bare should_panic is a catch-all, got {other:?}"),
        }
        assert_eq!(
            screen(
                "tests/witness.rs",
                "#[test]\n#[should_panic(expected = \"budget exhausted\")]\n\
                 fn witness() {\n    stella::retry();\n}\n"
            ),
            Ok(()),
            "a named panic constrains the failure"
        );
        match screen(
            "tests/test_witness.py",
            "def test_witness():\n    with pytest.raises(Exception):\n        retry()\n",
        ) {
            Err(VacuousWitness::CatchAllPanic { .. }) => {}
            other => panic!("raises(Exception) is a catch-all, got {other:?}"),
        }
        assert_eq!(
            screen(
                "tests/test_witness.py",
                "def test_witness():\n    with pytest.raises(ValueError):\n        retry()\n"
            ),
            Ok(()),
            "a specific exception constrains the failure"
        );
        match screen(
            "src/witness.test.ts",
            "test('x', () => {\n  expect(() => retry()).toThrow();\n});\n",
        ) {
            Err(VacuousWitness::CatchAllPanic { .. }) => {}
            other => panic!("toThrow() with no argument is a catch-all, got {other:?}"),
        }
    }

    /// A value compared to itself names something real on both sides, so the
    /// identifier scan cannot catch it — the self-comparison rule must.
    #[test]
    fn comparing_a_value_to_itself_is_a_tautology() {
        for source in [
            "#[test]\nfn witness() {\n    assert_eq!(actual, actual);\n}\n",
            "#[test]\nfn witness() {\n    assert!(compute(3) == compute(3));\n}\n",
        ] {
            assert!(
                matches!(
                    screen("tests/witness.rs", source),
                    Err(VacuousWitness::ConstantAssertions { .. })
                ),
                "self-comparison witnesses nothing: {source}"
            );
        }
        assert!(
            matches!(
                screen(
                    "src/witness.test.ts",
                    "test('x', () => {\n  expect(sum(1)).toBe(sum(1));\n});\n"
                ),
                Err(VacuousWitness::ConstantAssertions { .. })
            ),
            "the chained form is the same tautology"
        );
    }

    /// The whole point of the conservative posture: real witnesses pass. Each
    /// of these is a shape an author legitimately writes, and a screen that
    /// rejected any of them would cost a run its verification.
    #[test]
    fn substantive_witnesses_are_not_rejected() {
        for (path, source) in [
            (
                "tests/witness.rs",
                "#[test]\nfn witness_retry_backs_off() {\n    \
                 let delays = stella::retry_delays(3);\n    \
                 assert_eq!(delays, vec![1, 2, 4]);\n}\n",
            ),
            (
                "tests/witness.rs",
                "#[test]\nfn witness() {\n    assert!(!stella::plan(\"x\").is_empty());\n}\n",
            ),
            (
                // The assertion lives in a helper the test calls — a file-level
                // screen must see it.
                "tests/witness.rs",
                "fn check(v: u32) {\n    assert_eq!(v, stella::expected());\n}\n\
                 #[test]\nfn witness() {\n    check(stella::compute());\n}\n",
            ),
            (
                "tests/witness.rs",
                "#[test]\nfn witness() {\n    \
                 assert!(matches!(stella::parse(\"a\"), Ok(_)));\n}\n",
            ),
            (
                "tests/test_witness.py",
                "def test_witness():\n    assert retry_delays(3) == [1, 2, 4]\n",
            ),
            (
                "tests/test_witness.py",
                "class T(unittest.TestCase):\n    def test_x(self):\n        \
                 self.assertEqual(retry_delays(3), [1, 2, 4])\n",
            ),
            (
                "src/witness.test.ts",
                "test('backs off', () => {\n  expect(retryDelays(3)).toEqual([1, 2, 4]);\n});\n",
            ),
            (
                "pkg/witness_test.go",
                "func TestRetry(t *testing.T) {\n\tgot := RetryDelays(3)\n\t\
                 if got != 7 {\n\t\tt.Fatalf(\"got %d\", got)\n\t}\n}\n",
            ),
            (
                "tests/Witness/WitnessTests.cs",
                "public void Witness() { Assert.Equal(7, Retry.Delays(3)); }\n",
            ),
        ] {
            assert_eq!(screen(path, source), Ok(()), "must be accepted: {path}\n{source}");
        }
    }

    /// One real assertion clears the file even beside vacuous ones: the screen
    /// rejects vacuousness, not sloppiness.
    #[test]
    fn one_substantive_assertion_clears_the_file() {
        assert_eq!(
            screen(
                "tests/witness.rs",
                "#[test]\nfn witness() {\n    assert!(true);\n    \
                 assert_eq!(stella::retry_delays(3), vec![1, 2, 4]);\n}\n"
            ),
            Ok(())
        );
    }

    /// A commented-out or quoted assertion is not an assertion — without
    /// stripping both, either one lets a vacuous witness through.
    #[test]
    fn assertions_in_comments_and_strings_do_not_count() {
        assert_eq!(
            screen(
                "tests/witness.rs",
                "#[test]\nfn witness() {\n    // assert_eq!(stella::compute(), 4);\n    \
                 /* assert!(stella::ready()); */\n    let _ = 1;\n}\n"
            ),
            Err(VacuousWitness::NoAssertions),
            "a commented assertion asserts nothing"
        );
        assert_eq!(
            screen(
                "tests/test_witness.py",
                "def test_witness():\n    \"\"\"Example: assert compute() == 4\"\"\"\n    \
                 value = 1\n"
            ),
            Err(VacuousWitness::NoAssertions),
            "a docstring example asserts nothing"
        );
        assert_eq!(
            screen(
                "tests/witness.rs",
                "#[test]\nfn witness() {\n    assert_eq!(\"expected\", \"expected\");\n}\n"
            )
            .is_err(),
            true,
            "words inside a string are not identifiers the test named"
        );
    }

    /// Rust `'` is a lifetime as often as a character literal. Mistaking one
    /// for the other blanks the rest of the file, which turns every real
    /// assertion after it into no assertion at all.
    #[test]
    fn rust_lifetimes_do_not_swallow_the_file() {
        assert_eq!(
            screen(
                "tests/witness.rs",
                "fn borrow<'a>(s: &'a str) -> &'a str { s }\n\
                 #[test]\nfn witness() {\n    \
                 assert_eq!(borrow(stella::name()), \"stella\");\n}\n"
            ),
            Ok(())
        );
        // A real character literal is still a literal, so this stays vacuous.
        assert!(
            screen(
                "tests/witness.rs",
                "#[test]\nfn witness() {\n    assert_eq!('a', 'a');\n}\n"
            )
            .is_err()
        );
    }

    /// A raw string's hash count is part of its terminator; reading it wrong
    /// blanks past the literal and hides the assertions that follow.
    #[test]
    fn rust_raw_strings_are_blanked_without_overrunning() {
        assert_eq!(
            screen(
                "tests/witness.rs",
                "#[test]\nfn witness() {\n    let json = r#\"{\"a\": 1}\"#;\n    \
                 assert_eq!(stella::parse(json), 1);\n}\n"
            ),
            Ok(())
        );
    }

    /// An extension the screen does not read is never the reason a witness is
    /// refused — `is_witness_test_path` owns that decision.
    #[test]
    fn an_unreadable_extension_passes() {
        assert_eq!(screen("tests/fixture.txt", "nothing here"), Ok(()));
        assert_eq!(screen("noextension", "nothing here"), Ok(()));
    }

    /// `.unwrap_or(…)` supplies a default rather than asserting, so it must not
    /// register as the file's one assertion.
    #[test]
    fn unwrap_or_is_not_an_assertion() {
        assert_eq!(
            screen(
                "tests/witness.rs",
                "#[test]\nfn witness() {\n    let v = stella::compute().unwrap_or(0);\n    \
                 let _ = v;\n}\n"
            ),
            Err(VacuousWitness::NoAssertions)
        );
    }

    /// The excerpt is quotable: one line, bounded, and drawn from what the
    /// author actually wrote rather than from the blanked twin.
    #[test]
    fn the_excerpt_quotes_the_original_source_on_one_line() {
        let source = "#[test]\nfn witness() {\n    assert_eq!(\n        \"same\",\n        \
                      \"same\",\n    );\n}\n";
        match screen("tests/witness.rs", source) {
            Err(VacuousWitness::ConstantAssertions { example }) => {
                assert!(!example.contains('\n'), "collapsed to one line: {example}");
                assert!(
                    example.contains("\"same\""),
                    "quotes the original bytes, not the blanked ones: {example}"
                );
            }
            other => panic!("expected a tautology, got {other:?}"),
        }
        assert_eq!(excerpt(&"x".repeat(200)).chars().count(), 97);
    }
}
