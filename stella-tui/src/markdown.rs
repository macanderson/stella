//! Lightweight markdown-to-styled-lines renderer for the transcript.
//!
//! Agent responses are markdown-capable, so every `Text` transcript entry is
//! parsed for common markdown constructs before rendering. Block-level syntax
//! (headings, lists, fenced code, blockquotes, rules) is detected per-line;
//! inline syntax (**bold**, *italic*, `code`, \[links\](url)) is parsed within
//! each line. The output is a vector of styled [`Line`]s that the transcript
//! renderer and word-wrapper consume unchanged.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::syntax;
use crate::theme;

/// Render a markdown string into styled lines.
///
/// Each input line is classified into a block type; inline formatting is parsed
/// within non-code lines. Fenced code blocks (```...```) are rendered verbatim
/// in a distinct style.
pub fn render(text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    // `Some((fence_len, lang))` while inside a fenced block: `fence_len` is
    // the backtick run length that opened it and `lang` is the opening
    // fence's info string (```rust, ```toml, …). Tracking the length (rather
    // than just "are we in a fence") matches CommonMark: a fence only closes
    // on a run of backticks *at least* as long as the one that opened it, and
    // with no trailing info string — so a doc demonstrating markdown fences
    // (a ```` ```` outer fence wrapping a ``` ``` example) renders its inner
    // ``` as code content instead of prematurely closing the block.
    let mut code_block: Option<(usize, Option<syntax::Lang>)> = None;

    for raw in text.lines() {
        // ── Fenced code block toggle ────────────────────────────────────────
        let lead = raw.trim_start();
        if let Some(run) = backtick_run(lead) {
            match code_block {
                None => {
                    code_block = Some((run, syntax::lang_from_fence(&lead[run..])));
                    continue;
                }
                Some((open_len, _)) if run >= open_len && lead[run..].trim().is_empty() => {
                    code_block = None;
                    continue;
                }
                // A shorter (or info-string-carrying) backtick run inside the
                // block is content, not a close — fall through to render it
                // as a code line below.
                _ => {}
            }
        }

        if let Some((_, lang)) = code_block {
            out.push(code_block_line(raw, lang));
            continue;
        }

        // ── Horizontal rule ────────────────────────────────────────────────
        if is_hr(raw) {
            out.push(Line::from(Span::styled(
                "───────────────────────────────────────────",
                Style::new().fg(theme::RULE),
            )));
            continue;
        }

        // ── Headings (# .. ######) ─────────────────────────────────────────
        if let Some(rest) = strip_heading(raw) {
            let (level, content) = rest;
            out.push(heading_line(content, level));
            continue;
        }

        // ── Blockquote (> ...) ─────────────────────────────────────────────
        if let Some(rest) = raw.strip_prefix("> ") {
            let mut spans = vec![Span::styled("▎ ", Style::new().fg(theme::MUTED))];
            spans.extend(parse_inline_spans(rest));
            out.push(Line::from(spans));
            continue;
        }

        // ── Bullet list (- / * / +) ────────────────────────────────────────
        let indent = raw.len() - lead.len();
        if let Some(rest) = lead
            .strip_prefix("- ")
            .or_else(|| lead.strip_prefix("* "))
            .or_else(|| lead.strip_prefix("+ "))
        {
            let prefix = format!("{}• ", " ".repeat(indent));
            let mut spans = vec![Span::styled(prefix, Style::new().fg(theme::MUTED))];
            spans.extend(parse_inline_spans(rest));
            out.push(Line::from(spans));
            continue;
        }

        // ── Numbered list (1. / 42. ) ──────────────────────────────────────
        // The ordinal is content, not decoration: an agent answering "1. …
        // 2. … 3. …" is describing an *order*, and dropping the marker (as
        // this arm used to, emitting a bare two-space indent) rendered every
        // step as an identical unlabelled row. Keep the marker, styled like
        // the bullet glyph.
        if let Some((marker, rest)) = strip_numbered(lead) {
            let prefix = format!("{}{marker} ", " ".repeat(indent));
            let mut spans = vec![Span::styled(prefix, Style::new().fg(theme::MUTED))];
            spans.extend(parse_inline_spans(rest));
            out.push(Line::from(spans));
            continue;
        }

        // ── Blank line ─────────────────────────────────────────────────────
        if raw.trim().is_empty() {
            out.push(Line::raw(""));
            continue;
        }

        // ── Regular paragraph text ─────────────────────────────────────────
        out.push(Line::from(parse_inline_spans(raw)));
    }

    out
}

// ── Inline parsing ─────────────────────────────────────────────────────────

/// How many levels of nested emphasis are parsed before the remaining
/// delimiters render literally.
///
/// Bold and italic recurse into their own content, and the content is
/// re-collected into a fresh `Vec<char>` at every level — so on
/// model-controlled text (a `Text` transcript entry is never length-capped)
/// a crafted `**`/`*`/`_` alternation would recurse once per few characters:
/// O(depth) stack and O(depth × n) work, i.e. a stack overflow the panel
/// panic boundary cannot catch (it is not a panic). Real prose never nests
/// emphasis more than two or three deep, so a small cap costs nothing and
/// bounds both.
const MAX_EMPHASIS_DEPTH: usize = 8;

/// Parse inline markdown within a single line into styled spans.
///
/// Supports `**bold**`, `*italic*`, `_italic_`, `__bold__`, `` `code` ``,
/// `[text](url)`, and `~~strike~~`. Unmatched delimiters pass through as
/// literal text.
/// The style plain prose renders in: an explicit white, never the terminal's
/// default foreground.
///
/// `Span::raw` was the old default here, which leaves `fg` as `Color::Reset`.
/// That reads correctly only by coincidence — the deck paints its own black
/// ground onto the frame, so a reader whose terminal profile is light gets our
/// background under their dark default text. Prose is the transcript's primary
/// voice; it is entitled to say what colour it is.
fn body() -> Style {
    Style::new().fg(theme::INK)
}

fn parse_inline_spans(text: &str) -> Vec<Span<'static>> {
    parse_inline_spans_at(text, 0)
}

/// [`parse_inline_spans`] with the current nesting level. Past
/// [`MAX_EMPHASIS_DEPTH`] the text is emitted verbatim rather than descending
/// further.
fn parse_inline_spans_at(text: &str, depth: usize) -> Vec<Span<'static>> {
    if depth >= MAX_EMPHASIS_DEPTH {
        return vec![Span::styled(text.to_string(), body())];
    }
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    let flush = |buf: &mut String, spans: &mut Vec<Span<'static>>| {
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(buf), body()));
        }
    };

    while i < chars.len() {
        // **bold** or __bold__
        if (chars[i] == '*' || chars[i] == '_') && i + 1 < chars.len() && chars[i + 1] == chars[i] {
            let delim: String = std::iter::repeat_n(chars[i], 2).collect();
            let Some(end) = find_str(&chars, i + 2, &delim) else {
                buf.push(chars[i]);
                i += 1;
                continue;
            };
            flush(&mut buf, &mut spans);
            let content: String = chars[i + 2..end].iter().collect();
            for inner in parse_inline_spans_at(&content, depth + 1) {
                let new_style = inner.style.add_modifier(Modifier::BOLD);
                spans.push(Span::styled(inner.content.into_owned(), new_style));
            }
            i = end + 2;
            continue;
        }

        // *italic* or _italic_ (single delimiter, not part of ** or __)
        if (chars[i] == '*' || chars[i] == '_')
            && (i + 1 >= chars.len() || chars[i + 1] != chars[i])
        {
            let close = chars[i];
            let Some(end) = find_single_delim(&chars, i + 1, close) else {
                buf.push(chars[i]);
                i += 1;
                continue;
            };
            flush(&mut buf, &mut spans);
            let content: String = chars[i + 1..end].iter().collect();
            for inner in parse_inline_spans_at(&content, depth + 1) {
                let new_style = inner.style.add_modifier(Modifier::ITALIC);
                spans.push(Span::styled(inner.content.into_owned(), new_style));
            }
            i = end + 1;
            continue;
        }

        // `code`
        if chars[i] == '`' {
            let Some(end) = find_char(&chars, i + 1, '`') else {
                buf.push(chars[i]);
                i += 1;
                continue;
            };
            flush(&mut buf, &mut spans);
            let content: String = chars[i + 1..end].iter().collect();
            spans.push(Span::styled(content, code_style()));
            i = end + 1;
            continue;
        }

        // ~~strike~~
        if chars[i] == '~' && i + 1 < chars.len() && chars[i + 1] == '~' {
            let Some(end) = find_str(&chars, i + 2, "~~") else {
                buf.push(chars[i]);
                i += 1;
                continue;
            };
            flush(&mut buf, &mut spans);
            let content: String = chars[i + 2..end].iter().collect();
            spans.push(Span::styled(
                content,
                Style::new().add_modifier(Modifier::CROSSED_OUT),
            ));
            i = end + 2;
            continue;
        }

        // [text](url)
        if chars[i] == '['
            && let Some(close) = find_char(&chars, i + 1, ']')
            && close + 1 < chars.len()
            && chars[close + 1] == '('
            && let Some(paren) = find_char(&chars, close + 2, ')')
        {
            flush(&mut buf, &mut spans);
            let link_text: String = chars[i + 1..close].iter().collect();
            let url: String = chars[close + 2..paren].iter().collect();
            spans.push(Span::styled(
                if url.is_empty() {
                    link_text
                } else {
                    format!("{link_text} ({url})")
                },
                Style::new()
                    .fg(theme::RUN)
                    .add_modifier(Modifier::UNDERLINED),
            ));
            i = paren + 1;
            continue;
        }

        buf.push(chars[i]);
        i += 1;
    }

    flush(&mut buf, &mut spans);
    spans
}

// ── Block helpers ──────────────────────────────────────────────────────────

/// The length of a leading run of ≥3 backticks in an already-left-trimmed
/// line, or `None` if it doesn't open/close a fence.
fn backtick_run(lead: &str) -> Option<usize> {
    let run = lead.chars().take_while(|&c| c == '`').count();
    (run >= 3).then_some(run)
}

/// True if `line` is a horizontal rule (`---`, `***`, `___` with 3+ chars).
fn is_hr(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 3 {
        return false;
    }
    let first = match t.chars().next() {
        Some(c) => c,
        None => return false,
    };
    (first == '-' || first == '*' || first == '_') && t.chars().all(|c| c == first || c == ' ')
}

/// Strip a heading prefix (`# ` .. `###### `) and return `(level, content)`.
fn strip_heading(line: &str) -> Option<(usize, &str)> {
    let rest = line.trim_start_matches('#');
    let level = line.len() - rest.len();
    if level == 0 || level > 6 {
        return None;
    }
    // ATX headings require a space after the `#`s.
    let content = rest.strip_prefix(' ')?;
    if content.trim().is_empty() {
        return None;
    }
    Some((level, content))
}

/// Split a numbered list prefix (`1. `, `42) `) off `lead`, returning the
/// marker without its trailing space (`"1."`, `"42)"`) and the item text.
fn strip_numbered(lead: &str) -> Option<(&str, &str)> {
    let digits_end = lead.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits_end == 0 || digits_end >= lead.len() {
        return None;
    }
    // `digits_end` counts ASCII digits, so it is also a byte offset.
    let rest = lead.get(digits_end..)?;
    let text = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some((&lead[..digits_end + 1], text))
}

/// Build a heading line with level-appropriate styling.
///
/// The hierarchy is sky → sky → white, all bold:
/// * **H1** is a filled brand-sky pill — near-black [`theme::GROUND`] text on
///   an [`theme::ACCENT`] background, with a space of padding each side so
///   it reads as a solid title bar. This is the deliberate high-contrast
///   replacement for the old washed-out heading.
/// * **H2** is bold brand-sky text (no fill).
/// * **H3+** is bold primary-ink text.
fn heading_line(content: &str, level: usize) -> Line<'static> {
    if level == 1 {
        // One span so the sky fill is a single unbroken pill behind the text.
        let pill = Style::new()
            .bg(theme::ACCENT)
            .fg(theme::GROUND)
            .add_modifier(Modifier::BOLD);
        return Line::from(Span::styled(format!(" ◆ {content} "), pill));
    }
    let (prefix, style) = match level {
        2 => (
            "◈ ",
            Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ),
        _ => (
            "· ",
            Style::new().fg(theme::INK).add_modifier(Modifier::BOLD),
        ),
    };
    Line::from(vec![
        Span::styled(prefix.to_string(), style),
        Span::styled(content.to_string(), style),
    ])
}

/// Style for inline code spans and fenced code blocks.
fn code_style() -> Style {
    Style::new().fg(theme::WARN)
}

/// One line inside a fenced code block: indented two spaces, tokenized in the
/// fence's language when it named one we highlight (keywords/strings/numbers/
/// comments take their syntax colors; plain runs keep the code amber), or
/// rendered verbatim in the code style otherwise.
fn code_block_line(raw: &str, lang: Option<syntax::Lang>) -> Line<'static> {
    let mut spans = vec![Span::styled("  ", code_style())];
    match lang {
        Some(lang) => {
            for (text, tok) in syntax::tokenize(raw, lang) {
                spans.push(match tok {
                    Some(t) => Span::styled(text, syntax::tok_style(t)),
                    None => Span::styled(text, code_style()),
                });
            }
        }
        None => spans.push(Span::styled(raw.to_string(), code_style())),
    }
    Line::from(spans)
}

// ── Search helpers ─────────────────────────────────────────────────────────

/// Find the index of `needle` in `chars[start..]`, or `None`.
fn find_str(chars: &[char], start: usize, needle: &str) -> Option<usize> {
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.is_empty() || start >= chars.len() {
        return None;
    }
    let end_bound = chars.len().saturating_sub(needle_chars.len());
    for i in start..=end_bound {
        if chars[i..i + needle_chars.len()] == needle_chars[..] {
            return Some(i);
        }
    }
    None
}

/// Find the index of `target` in `chars[start..]`, or `None`.
fn find_char(chars: &[char], start: usize, target: char) -> Option<usize> {
    chars[start..]
        .iter()
        .position(|&c| c == target)
        .map(|p| start + p)
}

/// Find a single delimiter (e.g. `*`) while skipping doubled pairs (`**`).
/// This prevents `*italic **bold** text*` from matching the first `*` of `**`.
fn find_single_delim(chars: &[char], start: usize, target: char) -> Option<usize> {
    let mut i = start;
    while i < chars.len() {
        if chars[i] == target {
            if i + 1 < chars.len() && chars[i + 1] == target {
                i += 2;
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_spans_text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn plain_text_passes_through() {
        let lines = render("hello world");
        assert_eq!(lines.len(), 1);
        assert_eq!(collect_spans_text(&lines[0]), "hello world");
    }

    #[test]
    fn bold_is_bolded() {
        let lines = render("**important**");
        let span = &lines[0].spans[0];
        assert!(
            span.style.add_modifier.contains(Modifier::BOLD),
            "bold modifier set"
        );
    }

    #[test]
    fn italic_is_italicized() {
        let lines = render("*emphasis*");
        let span = &lines[0].spans[0];
        assert!(
            span.style.add_modifier.contains(Modifier::ITALIC),
            "italic modifier set"
        );
    }

    #[test]
    fn code_span_has_yellow_fg() {
        let lines = render("inline `code` here");
        // Three spans: "inline ", code, " here"
        let code_span = &lines[0].spans[1];
        assert_eq!(code_span.content, "code");
        assert_eq!(code_span.style.fg, Some(theme::WARN));
    }

    #[test]
    fn headings_get_bold() {
        let lines = render("# Title\n## Subtitle\n### Section");
        assert_eq!(lines.len(), 3);
        for line in &lines {
            let span = &line.spans[0];
            assert!(
                span.style.add_modifier.contains(Modifier::BOLD),
                "heading is bold"
            );
        }
        // H1 is a padded pill; H2/H3 keep their glyph prefixes.
        assert_eq!(collect_spans_text(&lines[0]), " \u{25c6} Title ");
        assert_eq!(collect_spans_text(&lines[1]), "\u{25c8} Subtitle");
        assert_eq!(collect_spans_text(&lines[2]), "\u{b7} Section");
    }

    #[test]
    fn h1_is_a_high_contrast_sky_pill() {
        // The exact fix the user asked for: the H1 must be a bold, filled,
        // high-contrast bar — near-black ink on brand sky — never washed-out
        // or a light-text-on-pale-background combination.
        let lines = render("# Rust Async Patterns");
        let span = &lines[0].spans[0];
        assert_eq!(span.style.bg, Some(theme::ACCENT), "sky fill");
        assert_eq!(span.style.fg, Some(theme::GROUND), "near-black text");
        assert!(span.style.add_modifier.contains(Modifier::BOLD), "bold");
    }

    #[test]
    fn no_baby_blue_cyan_in_rendered_markdown() {
        // Nothing markdown emits should carry these old baby-blue cyan values.
        const OLD_CYANS: [ratatui::style::Color; 2] = [
            ratatui::style::Color::Rgb(0x60, 0xBF, 0xD6),
            ratatui::style::Color::Rgb(126, 197, 214),
        ];
        let lines = render(
            "# Heading\n\nBody with a [link](https://example.com) and `code`.\n\n```\nlet n = 42;\n```",
        );
        for line in &lines {
            for span in &line.spans {
                assert!(
                    !OLD_CYANS.contains(&span.style.fg.unwrap_or(ratatui::style::Color::Reset)),
                    "a span still uses a baby-blue fg"
                );
                assert!(
                    !OLD_CYANS.contains(&span.style.bg.unwrap_or(ratatui::style::Color::Reset)),
                    "a span still uses a baby-blue bg"
                );
            }
        }
    }

    #[test]
    fn bullet_list_items_get_bullet_prefix() {
        let lines = render("- first\n- second");
        assert_eq!(lines.len(), 2);
        assert!(
            collect_spans_text(&lines[0]).contains('•'),
            "bullet prefix rendered"
        );
    }

    #[test]
    fn numbered_list_items_keep_their_ordinal() {
        // The ordinal is the whole point of an ordered list: two steps that
        // render as identical unlabelled rows have lost the agent's meaning.
        let lines = render("1. first\n2. second\n10) tenth");
        assert_eq!(lines.len(), 3);
        assert_eq!(collect_spans_text(&lines[0]), "1. first");
        assert_eq!(collect_spans_text(&lines[1]), "2. second");
        assert_eq!(collect_spans_text(&lines[2]), "10) tenth");
    }

    #[test]
    fn nested_fence_of_fewer_backticks_stays_inside_the_outer_block() {
        // A doc demonstrating markdown fences wraps its example in a longer
        // outer fence (CommonMark convention). The inner ``` must render as
        // code content, not prematurely close the outer block.
        let lines = render("````\nExample:\n```\ncode\n```\nmore\n````\nafter");
        // One line per source line inside the fence (7 total: the two ``` +
        // the three intervening lines are all fence content), plus the
        // trailing "after" paragraph line outside it.
        assert_eq!(lines.len(), 6, "{lines:?}");
        assert!(collect_spans_text(&lines[0]).contains("Example:"));
        assert!(collect_spans_text(&lines[1]).contains("```"));
        assert!(collect_spans_text(&lines[2]).contains("code"));
        assert!(collect_spans_text(&lines[3]).contains("```"));
        assert!(collect_spans_text(&lines[4]).contains("more"));
        assert_eq!(collect_spans_text(&lines[5]), "after");
    }

    #[test]
    fn closing_fence_with_trailing_text_is_not_a_close() {
        // Per CommonMark, a closing fence line must contain only backticks
        // (+ optional whitespace) — a ``` followed by anything else is
        // fence content, not the close.
        let lines = render("```\n``` still code\nreal close\n```\nafter");
        assert!(collect_spans_text(&lines[0]).contains("still code"));
        assert!(collect_spans_text(&lines[1]).contains("real close"));
        assert_eq!(collect_spans_text(&lines[2]), "after");
    }

    #[test]
    fn fenced_code_block_renders_indented() {
        let lines = render("```\nfn main() {}\n```");
        assert_eq!(lines.len(), 1);
        let text = collect_spans_text(&lines[0]);
        assert!(
            text.contains("fn main"),
            "code block content visible: {text}"
        );
    }

    #[test]
    fn link_shows_text_and_url() {
        let lines = render("[docs](https://example.com)");
        let text = collect_spans_text(&lines[0]);
        assert!(
            text.contains("docs") && text.contains("example.com"),
            "link text and url visible: {text}"
        );
    }

    #[test]
    fn blockquote_gets_bar_prefix() {
        let lines = render("> quoted text");
        let text = collect_spans_text(&lines[0]);
        assert!(text.contains('▎'), "blockquote bar rendered: {text}");
        assert!(text.contains("quoted text"));
    }

    #[test]
    fn horizontal_rule_renders_as_line() {
        let lines = render("---");
        assert_eq!(lines.len(), 1);
        let text = collect_spans_text(&lines[0]);
        assert!(text.contains('─'), "rule rendered as dashes: {text}");
    }

    #[test]
    fn unmatched_delimiters_are_literal() {
        let lines = render("this *is not closed");
        assert_eq!(lines.len(), 1);
        let text = collect_spans_text(&lines[0]);
        assert_eq!(text, "this *is not closed");
    }

    #[test]
    fn mixed_inline_formatting() {
        let lines = render("This is **bold** and `code` and *italic*");
        assert_eq!(lines.len(), 1);
        // Should have multiple spans with different styles
        assert!(lines[0].spans.len() > 1);
    }

    #[test]
    fn empty_string_produces_no_lines() {
        let lines = render("");
        assert!(lines.is_empty());
    }

    #[test]
    fn nested_bold_in_italic() {
        let lines = render("*outer **bold** outer*");
        assert_eq!(lines.len(), 1);
        // The "bold" span should have both ITALIC and BOLD
        let bold_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "bold")
            .expect("bold span exists");
        assert!(
            bold_span
                .style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::ITALIC),
            "nested formatting: bold inside italic has both modifiers"
        );
    }

    #[test]
    fn tagged_fences_highlight_their_language() {
        let lines = render("```rust\nfn main() {}\n```");
        assert_eq!(lines.len(), 1);
        let kw = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "fn")
            .expect("keyword is its own span");
        assert_eq!(kw.style.fg, Some(theme::SYNTAX_KEYWORD), "keyword colored");
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style.fg == Some(theme::WARN)),
            "plain runs keep the amber code style: {:?}",
            lines[0].spans
        );
    }

    #[test]
    fn untagged_fences_keep_the_uniform_code_style() {
        let lines = render("```\nfn main() {}\n```");
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|s| s.style.fg == Some(theme::WARN)),
            "no tag, no tokenizing: {:?}",
            lines[0].spans
        );
    }

    #[test]
    fn toml_fences_highlight_keys_and_values() {
        let lines = render("```toml\n[server]\nport = 8080\n```");
        assert_eq!(lines.len(), 2);
        let header = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "[server]")
            .expect("table header is its own span");
        assert_eq!(header.style.fg, Some(theme::SYNTAX_KEYWORD));
        let num = lines[1]
            .spans
            .iter()
            .find(|s| s.content == "8080")
            .expect("value is its own span");
        assert_eq!(num.style.fg, Some(theme::SYNTAX_NUMBER));
    }
}
