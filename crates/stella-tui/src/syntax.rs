//! The deck's half of syntax highlighting: token classes to ratatui styles.
//!
//! The lexers themselves live in [`stella_transcript::syntax`] and are shared
//! with the export and Observatory renderers. They were here once — the JSON
//! one moved down in #3644 so an exported transcript could colour a tool result
//! the way the deck does, and the other five followed in #4036 the moment a
//! `read_file` result had to render as *source* on every surface rather than as
//! flat grey (#4019).
//!
//! What could not follow is this file: turning a [`Tok`] into a `Style` needs
//! ratatui and [`crate::theme`], and `stella-transcript` is a near-leaf that
//! has neither and wants neither. So the split is by dependency, not by taste —
//! one lexer, three palettes, and no surface can disagree about what a token
//! *is*, only about what colour it wears. The grid and HTML renderers hold the
//! other two palettes (`grid::tok_color`, `html::tok_class`).
//!
//! Everything the deck used to call on this module is re-exported below, so
//! call sites read exactly as they did.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::theme;

pub use stella_transcript::syntax::{
    Highlighter, Lang, Runs, Tok, lang_from_ext, lang_from_fence, lang_from_path, tokenize,
};

/// The foreground style for a token class (comments also italicize).
pub fn tok_style(t: Tok) -> Style {
    match t {
        Tok::Keyword => Style::default().fg(theme::SYNTAX_KEYWORD),
        Tok::Str => Style::default().fg(theme::SYNTAX_STRING),
        Tok::Number => Style::default().fg(theme::SYNTAX_NUMBER),
        Tok::Comment => Style::default()
            .fg(theme::SYNTAX_COMMENT)
            .add_modifier(Modifier::ITALIC),
    }
}

/// The styling half of a [`Highlighter`], as an extension trait.
///
/// A trait rather than a free function because the call sites read as
/// `hl.spans(line, base)` and that is the right sentence — the highlighter is
/// carrying cross-line state and this is one more line fed to it. It cannot be
/// an inherent method any more: the type is `stella-transcript`'s now, and the
/// orphan rule puts the impl here, next to the theme it needs.
pub trait HighlightSpans {
    /// The styled spans for the next line — token colors from [`tok_style`],
    /// plain runs in `base`.
    fn spans(&mut self, line: &str, base: Style) -> Vec<Span<'static>>;
}

impl HighlightSpans for Highlighter {
    fn spans(&mut self, line: &str, base: Style) -> Vec<Span<'static>> {
        self.runs(line)
            .into_iter()
            .map(|(text, tok)| match tok {
                Some(t) => Span::styled(text, tok_style(t)),
                None => Span::styled(text, base),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one behaviour that stayed up here when the lexers moved down: a
    /// highlighter with no language still styles, in the caller's base tone.
    /// Its lexing sibling lives with the lexers.
    #[test]
    fn highlighter_with_no_language_passes_lines_through_in_base_style() {
        let mut hl = Highlighter::new(None);
        let base = Style::default().fg(theme::INK);
        let spans = hl.spans("anything at all", base);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "anything at all");
        assert_eq!(spans[0].style, base);
    }

    /// Every token class resolves to a distinct hue.
    ///
    /// Cheap, and it is the property the three palettes each have to satisfy
    /// independently — a shared lexer that painted two classes the same colour
    /// would have closed half the gap #3644 and #4036 exist to close.
    #[test]
    fn every_token_class_wears_its_own_colour() {
        let hues: Vec<_> = [Tok::Keyword, Tok::Str, Tok::Number, Tok::Comment]
            .into_iter()
            .map(|t| tok_style(t).fg)
            .collect();
        for (i, a) in hues.iter().enumerate() {
            for b in hues.iter().skip(i + 1) {
                assert_ne!(a, b, "two token classes share a colour: {hues:?}");
            }
        }
    }
}
