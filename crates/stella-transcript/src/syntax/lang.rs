//! Syntax highlighting — the one source-coloring engine every transcript
//! surface lexes with.
//!
//! **Two engines, and the line between them is which grammars are resident.**
//! Eleven languages are covered. Eight of them — Rust, TypeScript/JavaScript,
//! Python, Go, Java, C, SQL, PHP — go through tree-sitter in [`super::grammar`],
//! which is where the accuracy and every language added after #4283 lives. The
//! remaining three — Markdown, TOML, JSON — have no grammar in this workspace
//! and are lexed by the hand-written scans below; adding a grammar for them
//! would be new supply chain, which is exactly what the resident-grammar
//! argument in `Cargo.toml` does not license.
//!
//! That split is a fact about the dependency tree, not a taste: a language is
//! grammar-backed here if and only if `stella-graph` already compiles its
//! grammar. See [`super::grammar`] for why tree-sitter rather than syntect, and
//! for how per-line parsing stays cheap.
//!
//! Both engines keep the same contract: one line at a time, no state carried
//! between lines, so a line sliced out of context (a diff hunk cutting a block
//! comment in half) degrades to slightly-off coloring, never a wrong render or
//! a panic.
//!
//! ## Why this lives here and not in `stella-tui`
//!
//! It used to live there, and [`super::json_runs`] was extracted out of it in
//! #3644 for one reason: the export surfaces needed JSON and could not reach up
//! into the deck. The other five languages stayed behind on the argument that
//! "only the deck reads them" — which stopped being true the moment a
//! `read_file` result had to render as *source* on every surface rather than as
//! flat grey (#4019 fixed the deck; #4036 is the rest of it). The same edge
//! argument applies to all six, so all six now live at the bottom.
//!
//! What stays in `stella-tui` is the half that needs ratatui and the terminal
//! theme: `tok_style`, turning a [`Tok`] into a `Style`. One lexer, three
//! palettes — the grid renderer and the HTML renderer keep their own
//! `tok_color` / `tok_class` mappings, and none of the three can drift on
//! *classification* any more, only on hue.
//!
//! Consumers: the deck's diff bodies and rendered-markdown fences, the
//! skills / agents definition editors, and both export renderers' tool-result
//! bodies. The editors hold whole files, where cross-line facts (YAML
//! frontmatter, fenced code) are knowable — they feed lines through a
//! [`Highlighter`], which tracks that state and lights fence interiors up in
//! their own language.

use super::grammar::{self, Grammar};
use super::{Runs, Tok};

/// A language we can syntax-highlight.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    /// Rust.
    Rust,
    /// TypeScript / JavaScript (and their `x`/module variants).
    TsJs,
    /// Python.
    Python,
    /// Go.
    Go,
    /// Java.
    Java,
    /// C (and the C-family headers a `read_file` most often lands on).
    C,
    /// SQL.
    Sql,
    /// PHP.
    Php,
    /// Markdown *source* (headings, list markers, fences, inline code) — the
    /// skills/agents definition format.
    Markdown,
    /// TOML — settings, custom tool definitions, context records.
    Toml,
    /// JSON — tool-call arguments and tool results, which every transcript
    /// surface renders verbatim. Unlike the other languages here this one is
    /// read far more than it is edited, and it arrives already pretty-printed.
    Json,
}

/// Map a file extension to a language, or `None` if we don't highlight it.
#[must_use]
pub fn lang_from_ext(ext: &str) -> Option<Lang> {
    match ext {
        "rs" => Some(Lang::Rust),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => Some(Lang::TsJs),
        "py" | "pyi" => Some(Lang::Python),
        "go" => Some(Lang::Go),
        "java" => Some(Lang::Java),
        // The C-family extensions `stella-graph` routes to the same grammar.
        // C++ is a genuine approximation rather than a match — the C grammar
        // recovers through what it cannot parse, so a `.cpp` file lexes its
        // strings, comments, numbers and keywords and mis-structures the rest,
        // which is strictly more than the flat text it rendered as before.
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Lang::C),
        "sql" => Some(Lang::Sql),
        "php" => Some(Lang::Php),
        "md" | "markdown" => Some(Lang::Markdown),
        "toml" => Some(Lang::Toml),
        "json" | "jsonl" | "ndjson" => Some(Lang::Json),
        _ => None,
    }
}

/// The language for a file path via its extension (the segment after the last
/// `.`), guarding against a dot that lives in a parent directory rather than
/// the filename.
pub fn lang_from_path(path: &str) -> Option<Lang> {
    let (_, ext) = path.rsplit_once('.')?;
    if ext.contains('/') {
        return None;
    }
    lang_from_ext(ext)
}

/// Map a fenced-code info string (the word after the opening ```) to a
/// language. Unknown tags (`sh`, `json`, …) render plain rather than wrong.
pub fn lang_from_fence(tag: &str) -> Option<Lang> {
    let tag = tag
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match tag.as_str() {
        "rust" | "rs" => Some(Lang::Rust),
        "ts" | "tsx" | "typescript" | "js" | "jsx" | "javascript" | "mjs" | "cjs" => {
            Some(Lang::TsJs)
        }
        "py" | "python" | "python3" => Some(Lang::Python),
        "go" | "golang" => Some(Lang::Go),
        "java" => Some(Lang::Java),
        "c" | "h" | "cpp" | "c++" | "cc" => Some(Lang::C),
        "sql" | "postgres" | "postgresql" | "mysql" | "sqlite" => Some(Lang::Sql),
        "php" => Some(Lang::Php),
        "md" | "markdown" => Some(Lang::Markdown),
        "toml" => Some(Lang::Toml),
        "json" | "jsonl" | "ndjson" => Some(Lang::Json),
        _ => None,
    }
}

impl Lang {
    /// The grammar that lexes this language, or `None` when it is one of the
    /// three lexed by hand.
    ///
    /// The whole of the two-engine split, in one function. `None` is a claim
    /// about this workspace's dependency tree — no resident grammar — and not
    /// about the language being simple.
    fn grammar(self) -> Option<Grammar> {
        match self {
            Lang::Rust => Some(Grammar::Rust),
            Lang::TsJs => Some(Grammar::Tsx),
            Lang::Python => Some(Grammar::Python),
            Lang::Go => Some(Grammar::Go),
            Lang::Java => Some(Grammar::Java),
            Lang::C => Some(Grammar::C),
            Lang::Sql => Some(Grammar::Sql),
            Lang::Php => Some(Grammar::Php),
            Lang::Markdown | Lang::Toml | Lang::Json => None,
        }
    }
}

/// Split one line of source `code` into consecutive runs, each tagged with an
/// optional token class (`None` = punctuation/whitespace/plain text).
/// Lossless — concatenating the run texts reproduces `code` exactly — and
/// panic-free. Stateless: markdown scans as body prose (fence lines read as
/// markers, but their interiors are unknown here — [`Highlighter`] knows).
pub fn tokenize(code: &str, lang: Lang) -> Runs {
    if let Some(g) = lang.grammar() {
        // `None` only when a grammar will not install, which is a build-time
        // version skew rather than anything about this line. One untinted line
        // is the right degradation for it, and `every_grammar_arms` is what
        // stops it being discovered on a user's screen.
        return grammar::runs(code, g).unwrap_or_else(|| vec![(code.to_string(), None)]);
    }
    match lang {
        Lang::Markdown => md_line(code).0,
        Lang::Toml => toml_runs(code),
        Lang::Json => super::json_runs(code),
        // Unreachable: every remaining variant answered `Some` above. Stated as
        // a plain fallback rather than an `unreachable!` because this is a
        // rendering path, and a panic here would take the deck down over a
        // token colour.
        _ => vec![(code.to_string(), None)],
    }
}

// ── Whole-buffer highlighting (the edit surfaces) ───────────────────────────

/// Markdown cross-line position: only meaningful when the language is
/// [`Lang::Markdown`]; every other language scans line-by-line stateless.
#[derive(Clone, Copy)]
enum MdState {
    /// Before the first line — only there can `---` open YAML frontmatter.
    Lead,
    Frontmatter,
    Body,
    /// Inside a fenced code block, with the fence tag's language (if any).
    Fence(Option<Lang>),
}

/// Cross-line highlighting for a whole buffer, fed top to bottom.
///
/// The per-line [`tokenize`] deliberately keeps no state (a diff hunk can
/// slice a file anywhere); an *editor* holds the entire file, where YAML
/// frontmatter and fenced code blocks are knowable. Feeding each line through
/// one of these colors frontmatter keys, tracks fences, and highlights fence
/// interiors in their own language. `None` passes lines through unstyled, so
/// callers need no unknown-language special case.
pub struct Highlighter {
    lang: Option<Lang>,
    md: MdState,
}

impl Highlighter {
    /// A highlighter positioned before the first line of a buffer in `lang`.
    /// `None` passes every line through untagged.
    #[must_use]
    pub fn new(lang: Option<Lang>) -> Self {
        Self {
            lang,
            md: MdState::Lead,
        }
    }

    /// Tokenize the next line. Lines must arrive in buffer order.
    pub fn runs(&mut self, line: &str) -> Runs {
        let Some(lang) = self.lang else {
            return vec![(line.to_string(), None)];
        };
        if lang != Lang::Markdown {
            return tokenize(line, lang);
        }
        match self.md {
            MdState::Lead if line.trim() == "---" => {
                self.md = MdState::Frontmatter;
                vec![(line.to_string(), Some(Tok::Comment))]
            }
            MdState::Lead => {
                self.md = MdState::Body;
                self.body_runs(line)
            }
            MdState::Frontmatter if line.trim() == "---" || line.trim() == "..." => {
                self.md = MdState::Body;
                vec![(line.to_string(), Some(Tok::Comment))]
            }
            MdState::Frontmatter => frontmatter_runs(line),
            MdState::Fence(inner) if !line.trim_start().starts_with("```") => match inner {
                Some(l) => tokenize(line, l),
                None => vec![(line.to_string(), None)],
            },
            MdState::Fence(_) => {
                self.md = MdState::Body;
                vec![(line.to_string(), Some(Tok::Comment))]
            }
            MdState::Body => self.body_runs(line),
        }
    }

    fn body_runs(&mut self, line: &str) -> Runs {
        let (runs, fence) = md_line(line);
        if let Some(inner) = fence {
            self.md = MdState::Fence(inner);
        }
        runs
    }
}

// ── Markdown source ─────────────────────────────────────────────────────────

/// One markdown source line without cross-line context. Returns the runs plus
/// the fence language when the line opens (or closes — the caller's state
/// decides which) a fenced block; the stateless [`tokenize`] path ignores it.
fn md_line(line: &str) -> (Runs, Option<Option<Lang>>) {
    let lead = line.trim_start();
    let indent = &line[..line.len() - lead.len()];
    if let Some(rest) = lead.strip_prefix("```") {
        let tag = rest.trim_start_matches('`');
        return (
            vec![(line.to_string(), Some(Tok::Comment))],
            Some(lang_from_fence(tag)),
        );
    }
    if is_md_hr(lead) {
        return (vec![(line.to_string(), Some(Tok::Comment))], None);
    }
    if is_md_heading(lead) {
        return (vec![(line.to_string(), Some(Tok::Keyword))], None);
    }
    if lead.starts_with('>') {
        // The `>` marker(s) dim; the quoted text scans as prose.
        let rest = line.trim_start_matches([' ', '>']);
        let marker = &line[..line.len() - rest.len()];
        let mut runs = vec![(marker.to_string(), Some(Tok::Comment))];
        runs.extend(md_inline_runs(rest));
        return (runs, None);
    }
    if let Some(rest) = lead
        .strip_prefix("- ")
        .or_else(|| lead.strip_prefix("* "))
        .or_else(|| lead.strip_prefix("+ "))
    {
        return (bullet_runs(indent, &lead[..2], rest), None);
    }
    if let Some((marker, rest)) = split_ordered_marker(lead) {
        return (bullet_runs(indent, marker, rest), None);
    }
    (md_inline_runs(line), None)
}

/// Runs for a list line: plain indent, the marker in the list-marker color,
/// then the item text as prose.
fn bullet_runs(indent: &str, marker: &str, rest: &str) -> Runs {
    let mut runs = Vec::new();
    if !indent.is_empty() {
        runs.push((indent.to_string(), None));
    }
    runs.push((marker.to_string(), Some(Tok::Number)));
    runs.extend(md_inline_runs(rest));
    runs
}

/// Inline markdown prose: `` `code` `` spans (backticks included) and the
/// `(url)` of a `[text](url)` link get color; everything else stays plain —
/// prose should read calm in an editor, not light up like source code.
fn md_inline_runs(text: &str) -> Runs {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut runs: Runs = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < n {
        // An unterminated backtick is prose, not a code span that swallows
        // the rest of the line.
        if chars[i] == '`'
            && let Some(close) = chars[i + 1..].iter().position(|&c| c == '`')
        {
            flush(&mut plain, &mut runs);
            let end = i + 1 + close;
            runs.push((chars[i..=end].iter().collect(), Some(Tok::Str)));
            i = end + 1;
            continue;
        }
        if chars[i] == '['
            && let Some(close) = chars[i + 1..].iter().position(|&c| c == ']')
            && let bracket = i + 1 + close
            && chars.get(bracket + 1) == Some(&'(')
            && let Some(paren) = chars[bracket + 2..].iter().position(|&c| c == ')')
        {
            let end = bracket + 2 + paren;
            plain.extend(chars[i..=bracket].iter()); // `[text]` stays prose
            flush(&mut plain, &mut runs);
            runs.push((chars[bracket + 1..=end].iter().collect(), Some(Tok::Number)));
            i = end + 1;
            continue;
        }
        plain.push(chars[i]);
        i += 1;
    }
    flush(&mut plain, &mut runs);
    runs
}

/// A frontmatter (YAML) line: `key:` colors as structure; the value scans as
/// a config value — quoted strings, numbers, booleans, `#` comments — via the
/// TOML value rules, which YAML scalars share closely enough.
fn frontmatter_runs(line: &str) -> Runs {
    let lead = line.trim_start();
    let indent = &line[..line.len() - lead.len()];
    if let Some((key, rest)) = lead.split_once(':')
        && !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        let mut runs = Vec::new();
        if !indent.is_empty() {
            runs.push((indent.to_string(), None));
        }
        runs.push((key.to_string(), Some(Tok::Keyword)));
        runs.push((":".to_string(), None));
        runs.extend(value_runs(rest));
        return runs;
    }
    value_runs(line)
}

/// A horizontal rule: 3+ of the same `-`/`*`/`_` (spaces allowed between).
fn is_md_hr(lead: &str) -> bool {
    let t = lead.trim_end();
    let Some(first) = t.chars().next() else {
        return false;
    };
    t.len() >= 3 && matches!(first, '-' | '*' | '_') && t.chars().all(|c| c == first || c == ' ')
}

/// An ATX heading: 1–6 `#`s followed by a space.
fn is_md_heading(lead: &str) -> bool {
    let rest = lead.trim_start_matches('#');
    let level = lead.len() - rest.len();
    (1..=6).contains(&level) && rest.starts_with(' ')
}

/// Split an ordered-list marker (`1. `, `42) `) off `lead`, if present.
fn split_ordered_marker(lead: &str) -> Option<(&str, &str)> {
    let digits = lead.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let rest = lead[digits..]
        .strip_prefix(". ")
        .or_else(|| lead[digits..].strip_prefix(") "))?;
    Some((&lead[..digits + 2], rest))
}

// ── TOML source ─────────────────────────────────────────────────────────────

/// One TOML line: full-line and inline `#` comments, `[table]` / `[[array]]`
/// headers and the key of a `key = value` pair as structure, and values via
/// the generic scan (strings, numbers, booleans). Array/inline-table
/// continuation lines fall through to the generic scan.
fn toml_runs(code: &str) -> Runs {
    let lead = code.trim_start();
    let indent = &code[..code.len() - lead.len()];
    if lead.starts_with('#') {
        return vec![(code.to_string(), Some(Tok::Comment))];
    }
    if lead.starts_with('[') {
        // A table header runs to its matching close bracket; a comma inside
        // means this is really an array value line (`[1, 2],`), not a header.
        let chars: Vec<char> = lead.chars().collect();
        // `depth` cannot underflow: the branch is gated on `lead` starting
        // with `[`, so the first iteration always increments, and the scan
        // breaks the moment depth returns to 0. Keep that gate if this moves.
        let mut depth = 0usize;
        let mut end = chars.len();
        for (i, c) in chars.iter().enumerate() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !chars[..end].contains(&',') {
            let mut runs = Vec::new();
            if !indent.is_empty() {
                runs.push((indent.to_string(), None));
            }
            runs.push((chars[..end].iter().collect(), Some(Tok::Keyword)));
            let rest: String = chars[end..].iter().collect();
            if !rest.is_empty() {
                runs.extend(value_runs(&rest));
            }
            return runs;
        }
    }
    if let Some(eq) = lead.find('=') {
        let key_part = &lead[..eq];
        let key = key_part.trim_end();
        // Bare, dotted, or quoted keys only — anything else (a `=` inside an
        // array continuation, say) is not a key/value line.
        if !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '"' | '\'' | ' '))
        {
            let mut runs = Vec::new();
            if !indent.is_empty() {
                runs.push((indent.to_string(), None));
            }
            runs.push((key.to_string(), Some(Tok::Keyword)));
            runs.push((format!("{}=", &key_part[key.len()..]), None));
            runs.extend(value_runs(&lead[eq + 1..]));
            return runs;
        }
    }
    value_runs(code)
}

// ── The value scan ──────────────────────────────────────────────────────────

/// The left-to-right run scan for a **config value**: `#` line comments,
/// quoted strings, numbers, and the four TOML scalar constants.
///
/// This is all that is left of the hand-written scan that once served Rust,
/// TS/JS and Python too. Those three are grammar-backed now (#4283), and their
/// keyword tables went with them — a keyword table is precisely what a grammar
/// replaces. What survives is the half no grammar in this workspace covers:
/// TOML values, and the YAML frontmatter values [`frontmatter_runs`] lexes by
/// the same rules because the two scalar syntaxes agree closely enough.
fn value_runs(code: &str) -> Runs {
    let chars: Vec<char> = code.chars().collect();
    let n = chars.len();
    let mut runs: Runs = Vec::new();
    let mut plain = String::new();
    let mut i = 0;

    while i < n {
        let c = chars[i];

        // Line comment: `#` to end of line, in TOML and YAML alike.
        if c == '#' {
            flush(&mut plain, &mut runs);
            runs.push((chars[i..].iter().collect(), Some(Tok::Comment)));
            return runs;
        }

        // String literal.
        if matches!(c, '"' | '\'') {
            let (end, closed) = scan_string(&chars, i);
            // An unterminated single quote is far more often an apostrophe in
            // a prose value than a string a hunk cut in half — leave it plain
            // instead of swallowing the rest of the line. A double quote keeps
            // the string-to-end-of-line reading (a cut hunk is the likely
            // cause).
            if closed || c != '\'' {
                flush(&mut plain, &mut runs);
                runs.push((chars[i..end].iter().collect(), Some(Tok::Str)));
                i = end;
                continue;
            }
            plain.push(c);
            i += 1;
            continue;
        }

        // Number literal (only at a run boundary — identifiers below consume
        // their own trailing digits, so a leading digit here starts a number).
        if c.is_ascii_digit() {
            flush(&mut plain, &mut runs);
            let end = scan_number(&chars, i);
            runs.push((chars[i..end].iter().collect(), Some(Tok::Number)));
            i = end;
            continue;
        }

        // Identifier / keyword.
        if is_ident_start(c) {
            let mut j = i + 1;
            while j < n && is_ident_continue(chars[j]) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            if TOML_KEYWORDS.contains(&word.as_str()) {
                flush(&mut plain, &mut runs);
                runs.push((word, Some(Tok::Keyword)));
            } else {
                plain.push_str(&word);
            }
            i = j;
            continue;
        }

        // Anything else accumulates into the current plain run.
        plain.push(c);
        i += 1;
    }
    flush(&mut plain, &mut runs);
    runs
}

/// Push the accumulated plain run (if any) and clear the buffer.
fn flush(plain: &mut String, runs: &mut Runs) {
    if !plain.is_empty() {
        runs.push((std::mem::take(plain), None));
    }
}

/// Scan a string opened at `i`, honoring backslash escapes. Returns the end
/// index (just past the closing quote, or end of line when unterminated) and
/// whether the closing quote was actually found — the caller uses that to
/// tell a real string from a lone apostrophe in prose.
fn scan_string(chars: &[char], i: usize) -> (usize, bool) {
    let quote = chars[i];
    let n = chars.len();
    let mut j = i + 1;
    while j < n {
        match chars[j] {
            '\\' => j = (j + 2).min(n),
            c if c == quote => return (j + 1, true),
            _ => j += 1,
        }
    }
    (n, false)
}

/// Scan a number opened at `i`: a run of alphanumerics/underscores (covering
/// hex `0xFF`, suffixes `10u64`, separators `1_000`), plus one embedded
/// decimal point followed by more digits (`1.5`), so a `1..2` range keeps its
/// `..` intact. Returns the end index.
fn scan_number(chars: &[char], i: usize) -> usize {
    let n = chars.len();
    let mut j = i;
    while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
        j += 1;
    }
    if j < n && chars[j] == '.' && chars.get(j + 1).is_some_and(|c| c.is_ascii_digit()) {
        j += 1;
        while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
            j += 1;
        }
    }
    j
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// TOML value constants (booleans and the special floats).
const TOML_KEYWORDS: [&str; 4] = ["true", "false", "inf", "nan"];

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Flatten runs back to their source text.
    fn rebuilt(runs: &[(String, Option<Tok>)]) -> String {
        runs.iter().map(|(t, _)| t.clone()).collect()
    }

    /// The first run whose exact text is `text`, if any.
    fn run_tok<'a>(runs: &'a [(String, Option<Tok>)], text: &str) -> Option<&'a Option<Tok>> {
        runs.iter().find(|(t, _)| t == text).map(|(_, tok)| tok)
    }

    #[test]
    fn tokenizer_is_lossless_across_languages() {
        for (code, lang) in [
            ("let x = \"a\\\"b\"; // c", Lang::TsJs),
            ("fn main() { 0xFF_u8; 'z' }", Lang::Rust),
            ("def f(): return 1.5 # x", Lang::Python),
            ("<p>Don't have an account?</p>", Lang::TsJs),
            ("", Lang::Rust),
            ("# Heading with `code` and [a](b)", Lang::Markdown),
            ("  - item with `span`", Lang::Markdown),
            ("port = 8080 # local", Lang::Toml),
            ("  [server.tls]  # section", Lang::Toml),
        ] {
            let rebuilt: String = tokenize(code, lang).into_iter().map(|(t, _)| t).collect();
            assert_eq!(rebuilt, code, "tokenizer dropped/added chars for {code:?}");
        }
    }

    /// Every language, both engines. The grammar-backed ones joined this list
    /// with #4283, which matters more than it looks: the losslessness property
    /// below is the one contract a tree-sitter walk could plausibly break, and
    /// it now runs over arbitrary text through all eight grammars.
    const ALL_LANGS: [Lang; 11] = [
        Lang::Rust,
        Lang::TsJs,
        Lang::Python,
        Lang::Go,
        Lang::Java,
        Lang::C,
        Lang::Sql,
        Lang::Php,
        Lang::Markdown,
        Lang::Toml,
        Lang::Json,
    ];

    // Losslessness as a property, not a list of examples.
    //
    // `tokenizer_is_lossless_across_languages` above picks nine strings, and
    // not one of them holds a `snake_case` identifier — so an underscore bug in
    // here would have passed it. That is not hypothetical: the transcript's
    // markdown renderer had exactly that bug, deleting the `_` from
    // `tool_use_id` (see `markdown::parse_inline_spans`). A highlighter is the
    // same shape of code — it walks characters and re-emits them — so it gets
    // the same guarantee, stated over arbitrary input rather than a sample.
    proptest! {
        #[test]
        fn tokenize_re_emits_every_character_unchanged(text in ".{0,120}") {
            for lang in ALL_LANGS {
                prop_assert_eq!(
                    rebuilt(&tokenize(&text, lang)),
                    text.clone(),
                    "{:?} altered the text",
                    lang
                );
            }
        }
    }

    /// The identifier shapes that broke the markdown renderer, run past every
    /// highlighter. Cheap, and it names the regression in the failure output.
    #[test]
    fn identifier_heavy_lines_survive_every_highlighter() {
        for text in [
            "let tool_use_id = cache_creation_input_tokens;",
            "def __init__(self, _private, stop_reason): pass",
            "stop_reason = \"end_turn\"  # __all__",
            "prose with _emphasis_ and __bold__ and snake_case_name",
            "MAX__DOUBLE__UNDERSCORE and a_b_c_d_e",
        ] {
            for lang in ALL_LANGS {
                assert_eq!(
                    rebuilt(&tokenize(text, lang)),
                    text,
                    "{lang:?} altered {text:?}"
                );
            }
        }
    }

    #[test]
    fn a_contraction_apostrophe_does_not_swallow_the_line_as_a_string() {
        // An unpaired apostrophe in JSX prose must not open a string run —
        // the old behavior painted everything after "Don" in string sand.
        let runs = tokenize("<p>Don't have an account?</p>", Lang::TsJs);
        assert!(
            runs.iter().all(|(_, tok)| *tok != Some(Tok::Str)),
            "no string run in prose: {runs:?}"
        );
        // A real single-quoted string on the same language still highlights.
        let runs = tokenize("const x = 'ok';", Lang::TsJs);
        assert!(
            runs.iter()
                .any(|(t, tok)| t == "'ok'" && *tok == Some(Tok::Str)),
            "terminated strings keep their color: {runs:?}"
        );
    }

    #[test]
    fn json_separates_keys_from_string_values() {
        let runs = tokenize(r#"  "query": "needle","#, Lang::Json);
        assert_eq!(
            runs.iter().find(|(t, _)| t == "\"query\"").unwrap().1,
            Some(Tok::Keyword)
        );
        assert_eq!(
            runs.iter().find(|(t, _)| t == "\"needle\"").unwrap().1,
            Some(Tok::Str)
        );
    }

    #[test]
    fn json_scalars_are_numbers_not_structure() {
        for (src, lit) in [
            ("{\"n\": -12.5e+3}", "-12.5e+3"),
            ("{\"n\": true}", "true"),
            ("{\"n\": null}", "null"),
        ] {
            let runs = tokenize(src, Lang::Json);
            assert_eq!(
                runs.iter().find(|(t, _)| t == lit).map(|r| r.1),
                Some(Some(Tok::Number)),
                "{src} mis-classified {lit}: {runs:?}"
            );
        }
    }

    /// The module's stated contract: concatenating the runs reproduces the
    /// input exactly, for any line, including malformed ones.
    #[test]
    fn json_tokenizing_is_lossless_and_panic_free() {
        for src in [
            r#"{"a": 1, "b": [true, null], "c": "x\"y"}"#,
            r#"  "unterminated"#,
            r#""trailing backslash\"#,
            "",
            "   ",
            "}}}],,,",
            "{\"emoji\": \"⋯ ✓ 🚀\"}",
        ] {
            let runs = tokenize(src, Lang::Json);
            let rebuilt: String = runs.iter().map(|(t, _)| t.as_str()).collect();
            assert_eq!(rebuilt, src, "lossy on {src:?}: {runs:?}");
        }
    }

    /// **The #4283 witness, one row per language the upgrade added.**
    ///
    /// Each row is a `read_file` of a real source line, reached the way a
    /// surface reaches it: extension → [`Lang`] → [`tokenize`]. On the
    /// hand-written lexer every one of these fails at the *first* step —
    /// `lang_from_ext` answered `None` for `go`, `java`, `c`, `sql` and `php`,
    /// so `BodyPaint::lang` was `None` and the body rendered as flat text on
    /// the deck, the export grid and the Observatory alike. That is the cost
    /// this issue named, and this is the test that says it is paid.
    #[test]
    fn every_newly_supported_language_lexes_where_the_old_lexer_saw_nothing() {
        for (ext, src, want) in [
            (
                "go",
                "func Sum(a int) error { return fmt.Errorf(\"x\", 1) }",
                &["func", "Sum", "int", "Errorf", "\"x\"", "1"][..],
            ),
            (
                "java",
                "public static void main(String[] args) { System.out.println(\"hi\"); }",
                &["public", "void", "main", "String", "println", "\"hi\""][..],
            ),
            (
                "c",
                "static int add(int a) { /* c */ return 0x1F; }",
                &["static", "int", "add", "/* c */", "return", "0x1F"][..],
            ),
            (
                "sql",
                "SELECT id FROM users WHERE age > 21",
                &["SELECT", "FROM", "WHERE"][..],
            ),
            (
                "php",
                "<?php function greet(string $n) { return \"hi\"; }",
                &["function", "greet", "string", "return", "\"hi\""][..],
            ),
        ] {
            let lang = lang_from_ext(ext)
                .unwrap_or_else(|| panic!(".{ext} maps to no language, so its body renders flat"));
            let runs = tokenize(src, lang);
            assert_eq!(rebuilt(&runs), src, ".{ext} lexed lossily: {runs:?}");
            for text in want {
                assert!(
                    runs.iter().any(|(t, tok)| t == text && tok.is_some()),
                    ".{ext} left {text:?} untinted — the grammar is not reaching this line: {runs:?}"
                );
            }
        }
    }

    /// The two token classes a keyword table cannot produce.
    ///
    /// This is what justifies growing [`Tok`] — and growing it is a three-site
    /// change (`tok_style`, `tok_color`, `tok_class`), so the payoff had better
    /// be visible. A hand-written scan sees `Duration`, `total` and `compute`
    /// as one thing: an identifier.
    #[test]
    fn the_grammar_names_types_and_functions_a_keyword_table_could_not() {
        let runs = tokenize("fn main() { let s = parse(x); }", Lang::Rust);
        assert_eq!(
            run_tok(&runs, "main"),
            Some(&Some(Tok::Function)),
            "the declared name: {runs:?}"
        );
        assert_eq!(
            run_tok(&runs, "parse"),
            Some(&Some(Tok::Function)),
            "the callee: {runs:?}"
        );
        // ...and the identifier beside them stays plain, which is the half that
        // makes the distinction worth a colour.
        assert_eq!(run_tok(&runs, "x"), None, "a bare identifier: {runs:?}");

        let runs = tokenize("var d time.Duration = readAll(f)", Lang::Go);
        assert_eq!(
            run_tok(&runs, "Duration"),
            Some(&Some(Tok::Type)),
            "a named type: {runs:?}"
        );
        assert_eq!(
            run_tok(&runs, "readAll"),
            Some(&Some(Tok::Function)),
            "a callee through a package selector: {runs:?}"
        );
    }

    /// A fragment lexes as a fragment.
    ///
    /// The stateless, one-line contract survives the move to a whole-file
    /// parser only because tree-sitter recovers: a line sliced out of a body —
    /// which is every line of a middle-elided tool result — still lexes its
    /// leaves correctly inside the `ERROR` node wrapping them. If this ever
    /// regresses, every transcript body loses its colour below the first line
    /// that is not a complete item.
    #[test]
    fn a_line_sliced_out_of_a_body_still_lexes_its_tokens() {
        for (src, lang, want) in [
            ("    let total = compute(items);", Lang::Rust, "let"),
            ("} else if (x) {", Lang::TsJs, "else"),
            ("        return helper(n)  # tail", Lang::Python, "# tail"),
            ("    } // close", Lang::Java, "// close"),
        ] {
            let runs = tokenize(src, lang);
            assert_eq!(rebuilt(&runs), src, "{lang:?} lexed lossily: {runs:?}");
            assert!(
                runs.iter().any(|(t, tok)| t == want && tok.is_some()),
                "{lang:?} left {want:?} untinted in a fragment: {runs:?}"
            );
        }
    }

    #[test]
    fn markdown_and_toml_map_from_extensions_and_fence_tags() {
        assert_eq!(lang_from_fence("json"), Some(Lang::Json));
        assert_eq!(lang_from_path("docs/wire/schema.json"), Some(Lang::Json));
        assert_eq!(
            lang_from_path("skills/review/SKILL.md"),
            Some(Lang::Markdown)
        );
        assert_eq!(lang_from_path("docs/guide.markdown"), Some(Lang::Markdown));
        assert_eq!(lang_from_path(".stella/mcp.toml"), Some(Lang::Toml));
        assert_eq!(lang_from_fence("toml"), Some(Lang::Toml));
        assert_eq!(lang_from_fence("rust ignore"), Some(Lang::Rust));
        assert_eq!(lang_from_fence(""), None);
        assert_eq!(lang_from_fence("mermaid"), None);
        // The languages #4283 added reach both doors, so a fenced block in a
        // SKILL.md lights up the same way a `read_file` of the same source
        // does.
        assert_eq!(lang_from_fence("go"), Some(Lang::Go));
        assert_eq!(lang_from_fence("java"), Some(Lang::Java));
        assert_eq!(lang_from_fence("sql"), Some(Lang::Sql));
        assert_eq!(lang_from_fence("php"), Some(Lang::Php));
        assert_eq!(lang_from_path("cmd/serve/main.go"), Some(Lang::Go));
        assert_eq!(lang_from_path("src/Main.java"), Some(Lang::Java));
        assert_eq!(lang_from_path("lib/parse.h"), Some(Lang::C));
        assert_eq!(lang_from_path("migrations/001.sql"), Some(Lang::Sql));
        // Still not covered, and saying so is the point: a language with no
        // resident grammar renders plain rather than wrong.
        assert_eq!(lang_from_fence("yaml"), None);
        assert_eq!(lang_from_fence("bash"), None);
    }

    #[test]
    fn toml_keys_values_and_comments_get_their_colors() {
        let runs = tokenize("port = 8080 # local", Lang::Toml);
        assert_eq!(
            run_tok(&runs, "port"),
            Some(&Some(Tok::Keyword)),
            "{runs:?}"
        );
        assert_eq!(run_tok(&runs, "8080"), Some(&Some(Tok::Number)), "{runs:?}");
        assert_eq!(
            run_tok(&runs, "# local"),
            Some(&Some(Tok::Comment)),
            "{runs:?}"
        );
        let runs = tokenize("name = \"stella\"", Lang::Toml);
        assert_eq!(
            run_tok(&runs, "\"stella\""),
            Some(&Some(Tok::Str)),
            "{runs:?}"
        );
        let runs = tokenize("enabled = true", Lang::Toml);
        assert_eq!(
            run_tok(&runs, "true"),
            Some(&Some(Tok::Keyword)),
            "{runs:?}"
        );
    }

    #[test]
    fn toml_table_headers_color_as_structure_but_array_lines_do_not() {
        let runs = tokenize("[server]", Lang::Toml);
        assert_eq!(
            run_tok(&runs, "[server]"),
            Some(&Some(Tok::Keyword)),
            "{runs:?}"
        );
        let runs = tokenize("[[bin]]", Lang::Toml);
        assert_eq!(
            run_tok(&runs, "[[bin]]"),
            Some(&Some(Tok::Keyword)),
            "{runs:?}"
        );
        // An array continuation line is a value, not a header.
        let runs = tokenize("  [1, 2],", Lang::Toml);
        assert!(
            runs.iter().all(|(_, tok)| *tok != Some(Tok::Keyword)),
            "no header run in an array line: {runs:?}"
        );
        assert_eq!(run_tok(&runs, "1"), Some(&Some(Tok::Number)), "{runs:?}");
    }

    #[test]
    fn markdown_structure_colors_and_prose_stays_calm() {
        let runs = tokenize("## Usage", Lang::Markdown);
        assert_eq!(
            run_tok(&runs, "## Usage"),
            Some(&Some(Tok::Keyword)),
            "{runs:?}"
        );
        let runs = tokenize("- item with `span` inside", Lang::Markdown);
        assert_eq!(run_tok(&runs, "- "), Some(&Some(Tok::Number)), "{runs:?}");
        assert_eq!(run_tok(&runs, "`span`"), Some(&Some(Tok::Str)), "{runs:?}");
        let runs = tokenize("> a quote", Lang::Markdown);
        assert_eq!(run_tok(&runs, "> "), Some(&Some(Tok::Comment)), "{runs:?}");
        let runs = tokenize("see [docs](https://example.com).", Lang::Markdown);
        assert_eq!(
            run_tok(&runs, "(https://example.com)"),
            Some(&Some(Tok::Number)),
            "{runs:?}"
        );
        // Plain prose — including keywords of other languages — stays plain.
        let runs = tokenize("fn let def return in prose", Lang::Markdown);
        assert!(
            runs.iter().all(|(_, tok)| tok.is_none()),
            "prose never lights up like code: {runs:?}"
        );
        // An unterminated backtick is prose, not a runaway code span.
        let runs = tokenize("a stray ` backtick", Lang::Markdown);
        assert!(
            runs.iter().all(|(_, tok)| tok.is_none()),
            "unterminated backtick stays plain: {runs:?}"
        );
    }

    #[test]
    fn highlighter_tracks_frontmatter_fences_and_their_interiors() {
        let doc = [
            "---",
            "name: reviewer",
            "---",
            "# Reviewer",
            "prose with fn and let staying plain",
            "```toml",
            "port = 8080",
            "```",
            "```sh",
            "echo hi",
            "```",
        ];
        let mut hl = Highlighter::new(Some(Lang::Markdown));
        let all: Vec<Runs> = doc.iter().map(|l| hl.runs(l)).collect();
        assert_eq!(run_tok(&all[0], "---"), Some(&Some(Tok::Comment)), "open");
        assert_eq!(
            run_tok(&all[1], "name"),
            Some(&Some(Tok::Keyword)),
            "frontmatter key: {:?}",
            all[1]
        );
        assert_eq!(run_tok(&all[2], "---"), Some(&Some(Tok::Comment)), "close");
        assert_eq!(
            run_tok(&all[3], "# Reviewer"),
            Some(&Some(Tok::Keyword)),
            "heading"
        );
        assert!(all[4].iter().all(|(_, tok)| tok.is_none()), "{:?}", all[4]);
        assert_eq!(run_tok(&all[5], "```toml"), Some(&Some(Tok::Comment)));
        assert_eq!(
            run_tok(&all[6], "port"),
            Some(&Some(Tok::Keyword)),
            "fence interior highlights in its own language: {:?}",
            all[6]
        );
        assert_eq!(run_tok(&all[7], "```"), Some(&Some(Tok::Comment)), "close");
        assert!(
            all[9].iter().all(|(_, tok)| tok.is_none()),
            "unknown fence language renders plain: {:?}",
            all[9]
        );
    }

    #[test]
    fn highlighter_without_frontmatter_treats_the_first_line_as_body() {
        let mut hl = Highlighter::new(Some(Lang::Markdown));
        let runs = hl.runs("# Straight to a heading");
        assert_eq!(
            run_tok(&runs, "# Straight to a heading"),
            Some(&Some(Tok::Keyword)),
            "{runs:?}"
        );
        // A later `---` is a rule, not a frontmatter open.
        let runs = hl.runs("---");
        assert_eq!(run_tok(&runs, "---"), Some(&Some(Tok::Comment)));
        let runs = hl.runs("still body prose");
        assert!(runs.iter().all(|(_, tok)| tok.is_none()), "{runs:?}");
    }

    #[test]
    fn highlighter_is_lossless_over_a_whole_document() {
        let doc = "---\nname: x\ntools: [\"Read\", \"Grep\"]\n---\n# T\n\n> q\n\n```rust\nfn main() {}\n```\nplain [a](b) `c` end";
        let mut hl = Highlighter::new(Some(Lang::Markdown));
        for line in doc.split('\n') {
            assert_eq!(rebuilt(&hl.runs(line)), line, "line mangled: {line:?}");
        }
    }

    #[test]
    fn highlighter_with_no_language_passes_lines_through_untagged() {
        let mut hl = Highlighter::new(None);
        let runs = hl.runs("anything at all");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, "anything at all");
        assert_eq!(runs[0].1, None);
    }
}
