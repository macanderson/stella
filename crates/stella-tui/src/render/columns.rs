//! Widths here are counted in **terminal columns**.
//!
//! A terminal draws text in columns. `str::chars().count()` counts code
//! points. The two agree for ASCII. They do not agree for CJK text or for most
//! emoji, which draw two columns wide.
//!
//! So a budget read off a [`Rect`] and then spent in `char`s fits every ASCII
//! test. It fails on the first real name, path or prompt. It can be off by
//! half. That is enough to shove the next cell off the row.
//!
//! [`super::row`] already counts columns for styled spans. This is the same
//! job for plain strings. Three copies of the cut grew here before it existed.
//! Two of them counted chars.
//!
//! [`Rect`]: ratatui::layout::Rect

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Columns `text` occupies on a terminal.
pub(crate) fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// The start of `text`, in at most `cols` columns. Ends in `…` when it was
/// cut.
///
/// The `…` takes one column. So the kept text gets `cols - 1`. A cut can land
/// inside a wide glyph. Then the glyph is dropped and the result is one column
/// short. Short is safe. Long would shove the next cell aside.
///
/// A budget of zero draws nothing. Even a lone `…` is one column too many.
pub(crate) fn head(text: &str, cols: usize) -> String {
    if width(text) <= cols {
        return text.to_string();
    }
    if cols == 0 {
        return String::new();
    }
    format!("{}…", take_left(text, cols - 1))
}

/// The end of `text`, in at most `cols` columns. Opens with `…` when the
/// start was cut.
///
/// This is what a live edit buffer needs. The caret sits at the end, and that
/// is the part the typist is watching.
pub(crate) fn tail(text: &str, cols: usize) -> String {
    if width(text) <= cols {
        return text.to_string();
    }
    if cols == 0 {
        return String::new();
    }
    format!("…{}", take_right(text, cols - 1))
}

/// `text` padded with spaces out to `cols` columns. Text that already fills
/// them is left alone.
///
/// Rust's own `{:<width$}` pads to a **`char`** count. A fixed column laid out
/// that way still shifts under wide text, even after a correct cut.
pub(crate) fn pad(text: &str, cols: usize) -> String {
    let used = width(text);
    format!("{text}{}", " ".repeat(cols.saturating_sub(used)))
}

/// The longest prefix of `text` that fits in `cols` display columns.
pub(crate) fn take_left(text: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > cols {
            break;
        }
        used += w;
        out.push(ch);
    }
    out
}

/// The longest suffix of `text` that fits in `cols` display columns.
pub(crate) fn take_right(text: &str, cols: usize) -> String {
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0usize;
    for ch in text.chars().rev() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > cols {
            break;
        }
        used += w;
        kept.push(ch);
    }
    kept.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One CJK glyph is one char and two columns. That gap is the whole
    /// point of this file.
    #[test]
    fn a_wide_glyph_is_two_columns_and_one_char() {
        assert_eq!(width("abc"), 3);
        assert_eq!("符号".chars().count(), 2);
        assert_eq!(width("符号"), 4);
    }

    #[test]
    fn head_spends_its_budget_in_columns() {
        // Eight columns of CJK cut to five. Two glyphs fit, plus the `…`.
        // A char count would keep four glyphs and draw nine columns.
        assert_eq!(head("符号符号", 5), "符号…");
        assert_eq!(width(&head("符号符号", 5)), 5);
        assert_eq!(head("符号", 4), "符号");
        assert_eq!(head("abcdef", 4), "abc…");
        assert_eq!(head("abc", 9), "abc");
    }

    /// A cut inside a wide glyph lands under budget, never over it.
    #[test]
    fn head_under_runs_rather_than_over_runs_an_odd_budget() {
        let cut = head("符号符号", 4);
        assert_eq!(cut, "符…");
        assert_eq!(width(&cut), 3);
    }

    #[test]
    fn tail_keeps_the_end_and_spends_columns() {
        assert_eq!(tail("符号符号", 5), "…符号");
        assert_eq!(width(&tail("符号符号", 5)), 5);
        assert_eq!(tail("abcdef", 4), "…def");
        assert_eq!(tail("abc", 9), "abc");
    }

    /// A `…` is one column. A budget of zero cannot pay for it.
    #[test]
    fn a_zero_budget_draws_nothing() {
        assert_eq!(head("符号", 0), "");
        assert_eq!(tail("符号", 0), "");
        assert_eq!(head("", 0), "");
    }

    #[test]
    fn pad_fills_to_columns_not_to_chars() {
        assert_eq!(width(&pad("符号", 6)), 6);
        assert_eq!(pad("符号", 6), "符号  ");
        assert_eq!(pad("abc", 5), "abc  ");
        assert_eq!(pad("abcdef", 3), "abcdef");
    }

    #[test]
    fn take_left_and_take_right_never_split_a_glyph() {
        assert_eq!(take_left("符号符", 3), "符");
        assert_eq!(take_right("符号符", 3), "符");
        assert_eq!(take_left("符号符", 4), "符号");
        assert_eq!(take_right("符号符", 4), "号符");
    }
}
