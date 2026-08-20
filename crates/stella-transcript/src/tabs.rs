//! Tab expansion — the one place a tab's rendered width is decided.
//!
//! A tab is the only character whose width depends on where it sits, which is
//! precisely what a fixed-column renderer cannot have. Both of this workspace's
//! transcript renderers were wrong about it, in the same direction and for the
//! same reason: `unicode-width` reports a tab as zero columns, so every width
//! computed from it under-counts.
//!
//! The two failures looked different. In the deck, ratatui's
//! `Span::styled_graphemes` filters control characters out of the grapheme
//! stream, so the tab was *deleted* and `read_file`'s line-number gutter
//! collapsed onto the source (#4020). In the grid, the tab survives into a
//! `Cell` and a terminal renders it as a real tab stop — so nothing is lost, but
//! `grid::line_width` says a row is narrower than it draws, and every column
//! measured from that walks.
//!
//! One cause, one fix, one constant: expand before anything measures.

use std::borrow::Cow;

/// The column a tab advances to: the next multiple of this.
///
/// Four rather than the terminal-conventional eight because a transcript pane
/// is a fraction of a frame, and a tab-indented file three levels deep would
/// spend half of a narrow one on whitespace. Four still preserves the
/// multiple-of-a-stop arithmetic every tab-using file assumes; it only makes
/// each step cheaper.
///
/// Shared rather than declared per renderer, so the same file cannot indent
/// differently in the deck and in an exported transcript.
pub const TAB_STOP: usize = 4;

/// Expand every tab in `text` to the spaces that reach the next tab stop,
/// counting from `start_col`.
///
/// `start_col` is the true screen column the text begins at — gutters, rails
/// and indents included — so the expansion is the one a terminal would have
/// performed. Borrowed back unchanged when there is no tab, which is the
/// overwhelmingly common case and worth not allocating for.
///
/// A file that uses tabs to *align* rather than to indent renders slightly off
/// when its author's stop was not [`TAB_STOP`]. That is the same
/// "degrades to slightly-off, never wrong" contract [`crate::syntax`] already
/// accepts for a line lexed out of context.
#[must_use]
pub fn expand(text: &str, start_col: usize) -> Cow<'_, str> {
    if !text.contains('\t') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + TAB_STOP);
    let mut col = start_col;
    for ch in text.chars() {
        if ch == '\t' {
            let pad = TAB_STOP - (col % TAB_STOP);
            out.extend(std::iter::repeat_n(' ', pad));
            col += pad;
        } else {
            out.push(ch);
            col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_free_line_is_borrowed_unchanged() {
        assert!(matches!(
            expand("plain text", 0),
            Cow::Borrowed("plain text")
        ));
    }

    /// The stop is absolute, so the same text expands differently depending on
    /// where it starts — which is the whole reason `start_col` is a parameter
    /// and not an assumption.
    #[test]
    fn a_tab_advances_to_the_next_stop_from_where_it_starts() {
        assert_eq!(expand("\tx", 0), "    x");
        assert_eq!(expand("\tx", 1), "   x");
        assert_eq!(expand("\tx", 3), " x");
        assert_eq!(expand("\tx", 4), "    x");
    }

    /// Expansion never loses a character, and never leaves one behind.
    #[test]
    fn expansion_preserves_everything_but_the_tabs() {
        let text = "a\tb\t\tc";
        let expanded = expand(text, 0);
        assert!(!expanded.contains('\t'), "a tab survived: {expanded:?}");
        assert_eq!(
            expanded.replace(' ', ""),
            text.replace(['\t', ' '], ""),
            "expansion changed the non-whitespace text"
        );
    }

    /// Every expanded run lands the following text on a stop boundary.
    #[test]
    fn expansion_lands_on_a_stop() {
        for start in 0..8usize {
            let expanded = expand("\t", start);
            assert_eq!(
                (start + expanded.len()) % TAB_STOP,
                0,
                "a tab at column {start} did not reach a stop"
            );
        }
    }
}
