// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One lowercase non-ASCII letter → the ASCII letters that stand for it, for
//! [`super::slugify`] (#4913).
//!
//! ## Why transliterate rather than widen the alphabet
//!
//! A mining slug is a **directory name** — `.stella/skills/<slug>-<hash8>/` —
//! so widening [`super::slugify`] to accept Unicode letters would decide, as a
//! side effect, that `.stella/skills/` may hold non-ASCII paths. That is a
//! question about filesystems, not about mining: macOS's HFS+ stores `é`
//! decomposed while a Linux clone writes it composed, so the same logical name
//! is two different byte strings depending on where the workspace was cloned,
//! and a case-insensitive volume folds pairs an ext4 clone keeps apart.
//! Folding to ASCII first keeps every path in the alphabet the repository
//! already commits to, and the readable name is the point of the fix rather
//! than the original codepoints.
//!
//! ## What is here and what is not
//!
//! Three scripts, chosen by one rule: a letter is here when it has an
//! unambiguous ASCII stand-in that does not need a dictionary.
//!
//! - **Latin-1 Supplement and Latin Extended-A** — diacritic folding plus the
//!   handful of digraphs (`ß` → `ss`, `æ` → `ae`, `þ` → `th`).
//! - **Cyrillic** — BGN/PCGN romanization, the one an English reader is most
//!   likely to have seen (`щ` → `shch`, `х` → `kh`). The hard and soft signs
//!   romanize to nothing, which is why the table maps to a `&str` rather than
//!   a `char`.
//! - **Greek** — ISO 843 transcription, letter by letter. The digraph rules
//!   (`γγ` → `ng`, `ου` → `ou`) are not applied: they need context, and a
//!   slug is not a transcription.
//!
//! **CJK, Arabic, Hebrew and every other script are absent, and their text
//! still slugs to the bare artifact kind.** Not an oversight: Han characters
//! have no single-codepoint romanization at all — pinyin is a per-character
//! dictionary of thousands of entries, and which reading is right depends on
//! the word — so anything this table could return would be invented. #4968
//! tracks that half.
//!
//! Uppercase is absent for the same reason it is not needed:
//! [`super::slugify`] lowercases before it folds, and `char::to_lowercase`
//! already knows every one of these scripts (Greek's final sigma included).

/// The ASCII letters `ch` stands for, or `None` when this table has no answer
/// and the caller should treat the character as a separator.
///
/// `Some("")` is a real answer, distinct from `None`: Cyrillic `ъ` and `ь`
/// modify the letter beside them and contribute no letter of their own, so
/// `объект` is `obekt` rather than `ob-ekt`.
pub(super) fn ascii_fold(ch: char) -> Option<&'static str> {
    latin(ch).or_else(|| cyrillic(ch)).or_else(|| greek(ch))
}

/// Latin-1 Supplement (U+00E0–U+00FF) and Latin Extended-A (U+0100–U+017F),
/// lowercase.
fn latin(ch: char) -> Option<&'static str> {
    Some(match ch {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĳ' => "ij",
        'ĵ' => "j",
        'ķ' | 'ĸ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' | 'ŋ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ſ' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// Cyrillic (U+0430–U+044F plus `ё`), lowercase, BGN/PCGN.
fn cyrillic(ch: char) -> Option<&'static str> {
    Some(match ch {
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' => "g",
        'д' => "d",
        'е' | 'ё' | 'э' => "e",
        'ж' => "zh",
        'з' => "z",
        'и' => "i",
        'й' => "y",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'у' => "u",
        'ф' => "f",
        'х' => "kh",
        'ц' => "ts",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "shch",
        'ы' => "y",
        // The hard and soft signs are not letters of their own.
        'ъ' | 'ь' => "",
        'ю' => "yu",
        'я' => "ya",
        _ => return None,
    })
}

/// Greek (U+03B1–U+03C9 plus the accented vowels), lowercase, ISO 843.
fn greek(ch: char) -> Option<&'static str> {
    Some(match ch {
        'α' | 'ά' => "a",
        'β' => "v",
        'γ' => "g",
        'δ' => "d",
        'ε' | 'έ' => "e",
        'ζ' => "z",
        'η' | 'ή' | 'ι' | 'ί' | 'ϊ' | 'ΐ' => "i",
        'θ' => "th",
        'κ' => "k",
        'λ' => "l",
        'μ' => "m",
        'ν' => "n",
        'ξ' => "x",
        'ο' | 'ό' | 'ω' | 'ώ' => "o",
        'π' => "p",
        'ρ' => "r",
        // Final sigma: `char::to_lowercase` leaves it as its own codepoint.
        'σ' | 'ς' => "s",
        'τ' => "t",
        'υ' | 'ύ' | 'ϋ' | 'ΰ' => "y",
        'φ' => "f",
        'χ' => "ch",
        'ψ' => "ps",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every folding this table performs must land back inside the alphabet
    /// [`super::super::slugify`] is allowed to emit — that is the whole reason
    /// to fold rather than widen, and an entry that returned a non-ASCII
    /// character would put a non-ASCII byte into a directory name through the
    /// one door built to keep them out.
    #[test]
    fn every_folding_is_ascii_lowercase_alphanumeric() {
        for codepoint in 0u32..0x2FFF {
            let Some(ch) = char::from_u32(codepoint) else {
                continue;
            };
            let Some(folded) = ascii_fold(ch) else {
                continue;
            };
            assert!(
                folded.chars().all(|c| c.is_ascii_lowercase()),
                "{ch:?} (U+{codepoint:04X}) folds to {folded:?}, which slugify may not emit"
            );
        }
    }

    /// No ASCII character may be in the table. `slugify` handles those itself,
    /// and an entry here would be a second answer for a character that already
    /// has one.
    #[test]
    fn no_ascii_character_is_folded() {
        for codepoint in 0u32..128 {
            let ch = char::from_u32(codepoint).expect("ASCII is valid");
            assert_eq!(
                ascii_fold(ch),
                None,
                "{ch:?} is ASCII — slugify decides it, not this table"
            );
        }
    }

    /// The scripts this table leaves out, so the header paragraph saying so
    /// cannot become false without a test going red.
    #[test]
    fn a_script_with_no_single_codepoint_romanization_is_absent() {
        for ch in ['数', '据', 'あ', 'ア', 'ا', 'א', '한'] {
            assert_eq!(
                ascii_fold(ch),
                None,
                "{ch:?} has no single-codepoint ASCII stand-in — see #4968"
            );
        }
    }
}
