//! What Stella will draw.
//!
//! One rule, read wherever a plugin's text reaches a screen. Two kinds of
//! `char` are turned away, for two reasons.
//!
//! A control `char` is an escape. `\x1b` opens every `CSI`, `OSC` and `SGR`
//! run. `U+009B` opens one on its own. `\n`, `\r` and `\t` set a row's shape.
//! A screen obeys them all. Stella writes every escape byte a screen sees.
//!
//! A bidi `char` is a lie. `U+202E` and its kin flip the text after them. A
//! run can then read in an order its bytes do not have. That is the Trojan
//! Source shape (`CVE-2021-42574`). None of it leaves the leased box, since
//! the host clips each blit. It still counts. The box is chrome a person
//! chose to trust.
//!
//! The rest of `Cf` stays. `U+200D` builds each family and job emoji. `U+200C`
//! is plain text in Persian and in `Indic` scripts. To bar them is to bar a
//! script. `U+200B` takes a cell here and none on a screen. That is the gap a
//! wide glyph already opens; [`crate::PanelText::cells`] says so.

use crate::error::ManifestError;

/// Every bidi `char`, in code point order.
///
/// Written out, not looked up. The set is closed and has not moved since
/// `Unicode 6.3` added the isolates. This crate takes a new dep only on a
/// good case, and a whole crate for the list below is not one.
const BIDI_CONTROLS: &[char] = &[
    '\u{061c}', // ARABIC LETTER MARK
    '\u{200e}', // LEFT-TO-RIGHT MARK
    '\u{200f}', // RIGHT-TO-LEFT MARK
    '\u{202a}', // LEFT-TO-RIGHT EMBEDDING
    '\u{202b}', // RIGHT-TO-LEFT EMBEDDING
    '\u{202c}', // POP DIRECTIONAL FORMATTING
    '\u{202d}', // LEFT-TO-RIGHT OVERRIDE
    '\u{202e}', // RIGHT-TO-LEFT OVERRIDE
    '\u{2066}', // LEFT-TO-RIGHT ISOLATE
    '\u{2067}', // RIGHT-TO-LEFT ISOLATE
    '\u{2068}', // FIRST STRONG ISOLATE
    '\u{2069}', // POP DIRECTIONAL ISOLATE
];

/// The first control `char` in `text`, as `(index in chars, character)`.
///
/// Read in two places. [`crate::PanelText::new`] holds it for a panel's own
/// text, and [`validate_plugin_name`] holds it for the name Stella prints. A
/// second copy would drift. Both ask one thing: can this string reach a screen
/// with no escape in it?
pub(crate) fn first_control_character(text: &str) -> Option<(usize, char)> {
    text.chars()
        .enumerate()
        .find(|(_, found)| found.is_control())
}

/// The first bidi `char` in `text`, as `(index in chars, character)`.
///
/// The index counts `char`s, as [`first_control_character`] does. A refusal
/// then names a spot a reader can count to.
pub(crate) fn first_bidi_control(text: &str) -> Option<(usize, char)> {
    text.chars()
        .enumerate()
        .find(|(_, found)| BIDI_CONTROLS.contains(found))
}

/// Whether a plugin's name is one Stella can print in its own chrome.
///
/// The name is the one string Stella puts in chrome it owns: the
/// `◳ panel · <plugin>` label, the install prompt, a popup head, the rules
/// panel. [`crate::PanelText`] keeps a panel's own text free of escapes. That
/// is worth nothing if the label around it is not. A plugin named `"\x1b[2J"`
/// wipes the screen from inside Stella's own border.
///
/// Asked once, at load, not at each reader. Each reader is a place to forget.
/// Here, not in `manifest.rs`, so it sits by the rule it shares with
/// [`crate::PanelText::new`].
///
/// It refuses both hazards [`crate::PanelText::new`] refuses: a control
/// `char` first, since that is the worse of the two, and then a bidi one — a
/// name carrying `U+202E` or its kin can render in an order its bytes do not
/// have, the Trojan Source shape (`CVE-2021-42574`), and this string sits in
/// Stella's own chrome rather than a leased rectangle the host clips.
///
/// # Errors
///
/// [`ManifestError::NameNotDrawable`] for a control `char`,
/// [`ManifestError::NameCarriesBidiControl`] for a bidi one. Each names the
/// `char` and where it sits.
pub(crate) fn validate_plugin_name(name: &str) -> Result<(), ManifestError> {
    if let Some((index, found)) = first_control_character(name) {
        return Err(ManifestError::NameNotDrawable {
            index,
            code: found as u32,
        });
    }
    if let Some((index, found)) = first_bidi_control(name) {
        return Err(ManifestError::NameCarriesBidiControl {
            index,
            code: found as u32,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bidi_control_is_found_and_the_joiners_are_not() {
        for hazard in BIDI_CONTROLS {
            assert_eq!(
                first_bidi_control(&format!("gates {hazard} green")),
                Some((6, *hazard)),
                "U+{:04X} is listed and must be found",
                *hazard as u32
            );
        }
        // The chars the rule keeps. To bar one is to bar a script.
        for kept in ['\u{200b}', '\u{200c}', '\u{200d}', '✦', '👩'] {
            assert_eq!(first_bidi_control(&kept.to_string()), None, "{kept:?}");
        }
    }

    #[test]
    fn the_list_is_sorted_and_carries_no_duplicate() {
        let mut sorted = BIDI_CONTROLS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted, BIDI_CONTROLS,
            "the list reads in code-point order, once each"
        );
    }

    #[test]
    fn a_bidi_control_is_not_a_control_character() {
        // The two rules ask different things. `char::is_control` is `Cc`, and
        // each `char` above is `Cf`.
        for hazard in BIDI_CONTROLS {
            assert!(!hazard.is_control(), "U+{:04X}", *hazard as u32);
        }
    }

    #[test]
    fn a_name_carrying_an_escape_is_refused_by_position() {
        // `matches!` rather than `assert_eq!`, because `ManifestError` carries
        // no `PartialEq`.
        assert!(matches!(
            validate_plugin_name("✦\u{1b}[2J"),
            Err(ManifestError::NameNotDrawable { index: 1, code: 27 })
        ));
        assert!(validate_plugin_name("vera").is_ok());
    }

    /// `validate_plugin_name` asks [`first_bidi_control`] as well as
    /// [`first_control_character`], refusing every one of the twelve
    /// [`BIDI_CONTROLS`] and none of the joiners a script legitimately needs.
    /// Fails on a build where that second ask is missing: a name carrying
    /// `U+202E` loads and renders reordered under Stella's own border.
    #[test]
    fn a_name_that_reorders_itself_with_a_bidi_control_is_refused_by_position() {
        for hazard in BIDI_CONTROLS {
            assert!(
                matches!(
                    validate_plugin_name(&format!("gates {hazard}neerg")),
                    Err(ManifestError::NameCarriesBidiControl {
                        index: 6,
                        code
                    }) if code == *hazard as u32
                ),
                "U+{:04X} loaded, or was refused by the wrong rule",
                *hazard as u32
            );
        }
        assert!(matches!(
            validate_plugin_name("✦\u{202e}"),
            Err(ManifestError::NameCarriesBidiControl {
                index: 1,
                code: 0x202e
            })
        ));
        // The joiners stay allowed — the control-character rule above is what
        // bars an escape, not this one.
        for kept in ['\u{200c}', '\u{200d}'] {
            assert!(
                validate_plugin_name(&format!("gates{kept}status")).is_ok(),
                "{kept:?} is a joiner and should load"
            );
        }
    }
}
