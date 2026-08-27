//! The stylesheet's narrow-screen contract.
//!
//! The gutters here are fixed widths on purpose — rule 1 of the sheet's own
//! header is that rotating a chevron may not move a character to its right —
//! and that is a rule about columns being declared, not about their being the
//! same at every size. At 390px the desktop numbers left about twenty
//! characters for the object column, and the command bar clipped instead of
//! wrapping, so the half of a command naming the file it touched was simply
//! not on screen. Whether the result reads well is not decidable from Rust;
//! that the rules exist, and that the ones a phone depends on are not sitting
//! inside a wide-screen block, is.

use crate::html::STYLE;

/// The sheet carries a narrow-screen layer, and it shrinks the gutters rather
/// than releasing them.
#[test]
fn the_stylesheet_has_a_narrow_screen_layer() {
    for needle in [
        "@media (max-width: 720px)",                       // the phone layer
        "@media (max-width: 560px)", // …and the one that drops the role gutter
        ".grid { grid-template-columns: 40px 24px 1fr; }", // still declared, just smaller
    ] {
        assert!(
            STYLE.contains(needle),
            "the narrow-screen layer is missing {needle}"
        );
    }
    // The role gutter stops being a column under 560px: that is the one change
    // that gives a phone the whole line for text, and it must set a single
    // column rather than a narrower fixed one.
    let narrow = STYLE
        .split("@media (max-width: 560px)")
        .nth(1)
        .expect("the 560px block exists");
    assert!(
        narrow.contains(".role, .note { grid-template-columns: 1fr;"),
        "under 560px the role gutter stacks above its block"
    );
}

/// A command longer than the well wraps at every width.
///
/// `.cmdbar` is a flex row whose command is an anonymous item, so its minimum
/// width is its longest run with no space in it. One absolute path was enough
/// to push it past a well that clips rather than scrolls, which hid the tail
/// of a long command on a desktop and most of any command on a phone — so the
/// rule belongs to the base sheet, not to the narrow layer.
#[test]
fn a_long_command_wraps_instead_of_being_clipped() {
    let bar = STYLE
        .split(".cmdbar {")
        .nth(1)
        .expect("the command bar has a rule")
        .split('}')
        .next()
        .expect("the rule closes");
    for needle in ["flex-wrap: wrap", "overflow-wrap: anywhere", "min-width: 0"] {
        assert!(bar.contains(needle), "the command bar is missing {needle}");
    }
    let wide = STYLE
        .split("@media (max-width: 720px)")
        .next()
        .expect("there is a sheet before the narrow layer");
    assert!(
        wide.contains("overflow-wrap: anywhere"),
        "the wrap has to hold at desktop width too, where a long command was \
         clipped just as silently"
    );
}
