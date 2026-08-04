//! Prompt-keyword highlighting for the composer.
//!
//! Certain words in a prompt are not prose — they *control what happens on
//! submission*. A leading slash command picks a dispatch path before the
//! driver ever sees the text; a future reserved word (say, `workflows`) may
//! trigger an engine wherever it appears. Those words earn a distinctive,
//! uniform treatment in the input itself, live as the user types, so the
//! prompt telegraphs what it will *do* before ⏎ commits it.
//!
//! The architecture is two pure layers, both over the composer's **display
//! text** (chips + buffer — the exact string [`crate::composer::layout`]
//! wraps):
//!
//! 1. **Matching** — [`prompt_keywords`] runs every matcher and returns
//!    non-overlapping [`KeywordSpan`]s in byte order. Matchers today: the
//!    leading slash command (mark + name as two spans, so each can carry its
//!    own colour) and reserved words from a vocabulary
//!    ([`RESERVED_WORDS`] — empty until a feature claims one) matched
//!    *anywhere* in the text. A new keyword family is one new matcher here;
//!    no renderer changes.
//! 2. **Segmentation** — [`segments`] intersects one wrapped row (or any
//!    fragment of the display text) with those spans, yielding
//!    `(Option<KeywordKind>, &str)` runs. Byte offsets make this exact
//!    across soft wraps: a keyword split over two rows highlights on both,
//!    because each row slices the same span table at its own offset.
//!
//! Styling is deliberately last and thin: [`style`] maps a kind to its
//! [`crate::theme`] treatment, so every surface that draws a composer picks
//! up the same look by construction.

use ratatui::style::{Modifier, Style};

use crate::theme;

/// What a highlighted run of prompt text *is* — the vocabulary of prompt
/// keywords. Styling maps from this, never from the matched text itself, so
/// every keyword of a kind renders identically wherever it appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeywordKind {
    /// The `/` of a leading slash command — the dispatch marker itself.
    SlashMark,
    /// The command slug right after the mark (`files` in `/files`).
    SlashName,
    /// A reserved word from the keyword vocabulary, anywhere in the prompt.
    Reserved,
}

/// One matched keyword: a byte range over the composer's display text.
/// Ranges from [`prompt_keywords`] are sorted and never overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeywordSpan {
    /// Byte offset (inclusive) into the display text.
    pub start: usize,
    /// Byte offset (exclusive) into the display text.
    pub end: usize,
    pub kind: KeywordKind,
}

/// The reserved-word vocabulary shipped today: none. When a feature claims a
/// word — a `workflows` engine wanting `workflows` lit up the moment it is
/// typed, anywhere in the prompt — it is added here (or a caller passes its
/// own slice) and the whole pipeline lights it with zero renderer changes.
pub const RESERVED_WORDS: &[&str] = &[];

/// Match every prompt keyword in `display`: the leading slash command first
/// (it always wins overlaps — `/diff` is a command even if `diff` were also
/// a reserved word), then `reserved` words anywhere. Returns byte-ordered,
/// non-overlapping spans.
pub fn prompt_keywords(display: &str, reserved: &[&str]) -> Vec<KeywordSpan> {
    let mut spans = Vec::new();
    leading_slash_command(display, &mut spans);
    for word in reserved {
        reserved_matches(display, word, &mut spans);
    }
    spans.sort_by_key(|s| s.start);
    spans
}

/// The leading slash command, as the submission paths actually parse it: the
/// first non-whitespace character must be `/` (mid-text slashes are paths,
/// not commands), and the slug is the rest of that whitespace-delimited head
/// token — the same head `split_once(char::is_whitespace)` gives the
/// dispatcher. Every slash command gets the treatment, known or not: an
/// unknown `/comand` still *dispatches* as a command (the driver answers it
/// with a hint), so the highlight stays honest about what ⏎ will do.
fn leading_slash_command(display: &str, spans: &mut Vec<KeywordSpan>) {
    let head_at = display.len() - display.trim_start().len();
    let head = &display[head_at..];
    if !head.starts_with('/') {
        return;
    }
    spans.push(KeywordSpan {
        start: head_at,
        end: head_at + 1,
        kind: KeywordKind::SlashMark,
    });
    let name_len = head[1..]
        .find(char::is_whitespace)
        .unwrap_or(head.len() - 1);
    if name_len > 0 {
        spans.push(KeywordSpan {
            start: head_at + 1,
            end: head_at + 1 + name_len,
            kind: KeywordKind::SlashName,
        });
    }
}

/// A character that can sit inside a keyword — the boundary test for
/// whole-word matching. `-` and `_` count (a future `ci-status` keyword must
/// not light inside `my-ci-status-page`).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Whole-word, ASCII-case-insensitive occurrences of `word` anywhere in
/// `display`, skipping any that overlap a span already claimed (the slash
/// command, or an earlier-listed reserved word).
fn reserved_matches(display: &str, word: &str, spans: &mut Vec<KeywordSpan>) {
    if word.is_empty() {
        return;
    }
    let len = word.len();
    let mut at = 0;
    while at + len <= display.len() {
        if !display.is_char_boundary(at)
            || !display.is_char_boundary(at + len)
            || !display[at..at + len].eq_ignore_ascii_case(word)
        {
            at += 1;
            continue;
        }
        let free_standing = display[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c))
            && display[at + len..]
                .chars()
                .next()
                .is_none_or(|c| !is_word_char(c));
        let unclaimed = spans.iter().all(|s| s.end <= at || s.start >= at + len);
        if free_standing && unclaimed {
            spans.push(KeywordSpan {
                start: at,
                end: at + len,
                kind: KeywordKind::Reserved,
            });
            at += len;
        } else {
            at += 1;
        }
    }
}

/// Slice `text` — a fragment of the display text beginning at byte `start`
/// within it — into runs of uniform keyword kind (`None` = plain prose).
/// Runs cover `text` exactly, in order; a keyword that begins before or ends
/// after the fragment still colours the part that falls inside it, which is
/// what keeps a soft-wrapped keyword lit on both of its rows.
pub fn segments<'a>(
    text: &'a str,
    start: usize,
    keywords: &[KeywordSpan],
) -> Vec<(Option<KeywordKind>, &'a str)> {
    let end = start + text.len();
    let mut runs = Vec::new();
    let mut at = start;
    for span in keywords {
        if span.end <= at {
            continue;
        }
        if span.start >= end {
            break;
        }
        let s = span.start.max(at);
        let e = span.end.min(end);
        if s > at {
            runs.push((None, &text[at - start..s - start]));
        }
        runs.push((Some(span.kind), &text[s - start..e - start]));
        at = e;
    }
    if at < end {
        runs.push((None, &text[at - start..]));
    }
    runs
}

/// The one place a keyword kind becomes a look. Gold for the dispatch mark
/// (the brand accent — it *is* chrome, like the `>>> ` prefix), the deck's
/// violet for the command slug, both bold so a command reads as a command at
/// a glance. Reserved words are amber italic — the same hue
/// [`theme::SYNTAX_KEYWORD`] gives keywords in code, which is exactly what
/// they are.
pub fn style(kind: KeywordKind) -> Style {
    match kind {
        KeywordKind::SlashMark => Style::default()
            .fg(theme::GOLD)
            .add_modifier(Modifier::BOLD),
        KeywordKind::SlashName => Style::default()
            .fg(theme::VIOLET)
            .add_modifier(Modifier::BOLD),
        KeywordKind::Reserved => Style::default()
            .fg(theme::AMBER)
            .add_modifier(Modifier::ITALIC),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans_of(display: &str, reserved: &[&str]) -> Vec<(usize, usize, KeywordKind)> {
        prompt_keywords(display, reserved)
            .into_iter()
            .map(|s| (s.start, s.end, s.kind))
            .collect()
    }

    #[test]
    fn a_leading_slash_command_is_a_mark_plus_a_name() {
        assert_eq!(
            spans_of("/files", &[]),
            vec![
                (0, 1, KeywordKind::SlashMark),
                (1, 6, KeywordKind::SlashName),
            ]
        );
    }

    #[test]
    fn leading_whitespace_does_not_hide_the_command() {
        // Submission trims before dispatching, so the highlight must agree.
        assert_eq!(
            spans_of("  /diff", &[]),
            vec![
                (2, 3, KeywordKind::SlashMark),
                (3, 7, KeywordKind::SlashName),
            ]
        );
    }

    #[test]
    fn only_the_head_token_is_the_command() {
        // `/model zai/glm-5.2` — the argument is prose, including its slash.
        assert_eq!(
            spans_of("/model zai/glm-5.2", &[]),
            vec![
                (0, 1, KeywordKind::SlashMark),
                (1, 6, KeywordKind::SlashName),
            ]
        );
    }

    #[test]
    fn a_mid_text_slash_is_a_path_not_a_command() {
        assert!(spans_of("open src/main.rs", &[]).is_empty());
    }

    #[test]
    fn a_bare_slash_lights_the_mark_alone() {
        // The moment `/` is typed the treatment begins — the slug span
        // appears with the first slug character.
        assert_eq!(spans_of("/", &[]), vec![(0, 1, KeywordKind::SlashMark)]);
    }

    #[test]
    fn a_reserved_word_lights_anywhere_case_insensitively() {
        // The future the architecture is for: a `workflows` engine reserves
        // its word, and it lights wherever it is typed.
        assert_eq!(
            spans_of("run my Workflows now", &["workflows"]),
            vec![(7, 16, KeywordKind::Reserved)]
        );
    }

    #[test]
    fn a_reserved_word_inside_another_word_stays_dark() {
        assert!(spans_of("reworkflows", &["workflows"]).is_empty());
        assert!(spans_of("workflowsx", &["workflows"]).is_empty());
        // `-` is a word character: this is one identifier, not the keyword.
        assert!(spans_of("my-workflows", &["workflows"]).is_empty());
        // …but plain punctuation is a boundary.
        assert_eq!(
            spans_of("workflows!", &["workflows"]),
            vec![(0, 9, KeywordKind::Reserved)]
        );
    }

    #[test]
    fn the_slash_command_wins_an_overlap_with_a_reserved_word() {
        // `/diff` is a command even if `diff` were also reserved — the
        // command spans claim their bytes first.
        assert_eq!(
            spans_of("/diff", &["diff"]),
            vec![
                (0, 1, KeywordKind::SlashMark),
                (1, 5, KeywordKind::SlashName),
            ]
        );
        // Outside the command head the reserved word still lights.
        assert_eq!(
            spans_of("/show the diff", &["diff"]),
            vec![
                (0, 1, KeywordKind::SlashMark),
                (1, 5, KeywordKind::SlashName),
                (10, 14, KeywordKind::Reserved),
            ]
        );
    }

    #[test]
    fn segments_cover_a_row_exactly_and_in_order() {
        let keywords = prompt_keywords("/files now", &[]);
        assert_eq!(
            segments("/files now", 0, &keywords),
            vec![
                (Some(KeywordKind::SlashMark), "/"),
                (Some(KeywordKind::SlashName), "files"),
                (None, " now"),
            ]
        );
    }

    #[test]
    fn a_keyword_split_by_a_soft_wrap_lights_on_both_rows() {
        // Display "/inspect" wrapped at width 4 → rows "/ins" (offset 0) and
        // "pect" (offset 4). The slug span [1,8) must colour its part of
        // each row.
        let keywords = prompt_keywords("/inspect", &[]);
        assert_eq!(
            segments("/ins", 0, &keywords),
            vec![
                (Some(KeywordKind::SlashMark), "/"),
                (Some(KeywordKind::SlashName), "ins"),
            ]
        );
        assert_eq!(
            segments("pect", 4, &keywords),
            vec![(Some(KeywordKind::SlashName), "pect")]
        );
    }

    #[test]
    fn an_empty_fragment_yields_no_segments() {
        let keywords = prompt_keywords("/files", &[]);
        assert_eq!(segments("", 6, &keywords), vec![]);
    }

    #[test]
    fn a_wide_char_prompt_keeps_offsets_on_char_boundaries() {
        // Multi-byte text around a reserved word: offsets are bytes, and the
        // matcher must never split a char.
        assert_eq!(
            spans_of("日本語 workflows 日本語", &["workflows"]),
            vec![(10, 19, KeywordKind::Reserved)]
        );
    }

    #[test]
    fn every_kind_has_a_distinct_uniform_style() {
        // The treatment is part of the contract: gold mark, violet slug —
        // distinct from each other and from body text.
        assert_ne!(style(KeywordKind::SlashMark), style(KeywordKind::SlashName));
        assert_eq!(
            style(KeywordKind::SlashMark).fg,
            Some(crate::theme::GOLD),
            "the dispatch mark is gold"
        );
        assert_eq!(
            style(KeywordKind::SlashName).fg,
            Some(crate::theme::VIOLET),
            "the slug rides the deck's violet"
        );
    }
}
