// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The palette's matcher: which commands a query reaches, and **which letters
//! of each one it reached** — SPEC 10, rendering `08-command-palette`.
//!
//! SPEC 10 named `nucleo`; the tier walk below is the recorded deviation, and
//! ADR 0019 carries the argument (the palette ranks by *what* matched and then
//! by session state, so a scored matcher would be run for the by-product — the
//! indices — of a decision it did not make). The guard is
//! `spec_10_names_the_matcher_the_palette_uses` in `crate::render::tests::slash`.
//!
//! Matching is case-insensitive on the ASCII fold. [`str::to_ascii_lowercase`]
//! maps `A-Z` to `a-z` and leaves every other byte alone, so the lowered
//! haystack has byte-identical layout to the original — which is what lets an
//! offset found in the fold be reported straight back as an offset into the
//! name the caller will render. Command names are ASCII slugs, so the cheap
//! fold is exact for every name the CLI registers; a non-ASCII name still
//! matches on its ASCII letters and never panics or mis-slices.

/// What a query matched, best first. The palette's browse and query lists are
/// both ordered by this before anything else — see
/// [`SlashMenu::filter_with`](super::SlashMenu::filter_with).
///
/// The ordering is the whole point: `/gr` should open on `/graph` (prefix)
/// rather than on `/regraph` (substring) or on some command whose
/// *description* mentions a graph, however good a fuzzy score those might
/// earn. Derived `Ord` follows declaration order, so the enum itself is the
/// ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// The name begins with the query: `/gr` → `/graph`.
    Prefix,
    /// The query appears whole inside the name: `/ph` → `/gra`ph.
    Substring,
    /// The query's letters appear in the name in order but apart: `ga` →
    /// `/g`r`a`ph. This tier is what #5048 added; before it, a query whose
    /// letters were scattered reached nothing at all.
    Subsequence,
    /// Nothing in the name matched, but the one-line description did. Last on
    /// purpose: it is the tier that answers "what was that command called",
    /// and it must never outrank a row the user can see themselves typing.
    Description,
}

impl Tier {
    /// Position in the tier order — the sort key the menu actually carries.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Tier::Prefix => 0,
            Tier::Substring => 1,
            Tier::Subsequence => 2,
            Tier::Description => 3,
        }
    }
}

/// One matched command name: how it matched, and which of its bytes to light.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameMatch {
    /// Which tier the name matched at.
    pub tier: Tier,
    /// Byte offsets **into the name as given** of every character the query
    /// lit, ascending and each on a `char` boundary. Empty for an empty query
    /// and for a [`Tier::Description`] match (nothing in the *name* was
    /// reached, so nothing in the name lights).
    pub lit: Vec<usize>,
}

impl NameMatch {
    /// Whether `at` — a byte offset into the name — starts a lit character.
    ///
    /// `lit` is ascending by construction, so this is a binary search rather
    /// than a scan: the renderer asks once per character of every visible row.
    #[must_use]
    pub fn lights(&self, at: usize) -> bool {
        self.lit.binary_search(&at).is_ok()
    }
}

/// Match `needle` against `name`, or `None` when the name does not contain it
/// at any tier.
///
/// `needle` must already be lowercased and stripped of its leading slash —
/// the caller holds one query for the whole vocabulary and lowers it once.
/// `name` is the command name **as it will be rendered**, leading slash
/// included, and every offset in the result indexes into it.
///
/// An empty needle matches every name at [`Tier::Prefix`] with nothing lit,
/// which is what makes a bare `/` the browse list rather than a filter.
///
/// The leading slash is never lit: it is punctuation the user did not type
/// (`slash_menu` strips it from the query before it gets here), and lighting
/// it would put a gold `/` in front of every row in the list.
#[must_use]
pub fn match_name(needle: &str, name: &str) -> Option<NameMatch> {
    // Everything below indexes `bare`, then shifts by `offset` to report
    // offsets into `name`. `strip_prefix` rather than `trim_start_matches`:
    // `//x` would lose both slashes to the latter and the offsets would land
    // one byte short of the characters they name.
    let offset = usize::from(name.starts_with('/'));
    let bare = &name[offset..];
    // Byte-identical layout to `bare` — see the module doc's note on the fold.
    let folded = bare.to_ascii_lowercase();

    if needle.is_empty() {
        return Some(NameMatch {
            tier: Tier::Prefix,
            lit: Vec::new(),
        });
    }
    if folded.starts_with(needle) {
        return Some(NameMatch {
            tier: Tier::Prefix,
            lit: run_starts(bare, 0, needle.len(), offset),
        });
    }
    if let Some(at) = folded.find(needle) {
        return Some(NameMatch {
            tier: Tier::Substring,
            lit: run_starts(bare, at, needle.len(), offset),
        });
    }
    subsequence(&folded, needle).map(|lit| NameMatch {
        tier: Tier::Subsequence,
        lit: lit.into_iter().map(|at| at + offset).collect(),
    })
}

/// The char-start offsets inside `haystack[at..at + len]`, shifted by
/// `offset`. A contiguous run still reports one offset **per character**
/// rather than a range, so the renderer treats a prefix hit and a scattered
/// hit with the same code path — there is only one way to be lit.
fn run_starts(haystack: &str, at: usize, len: usize, offset: usize) -> Vec<usize> {
    haystack[at..at + len]
        .char_indices()
        .map(|(i, _)| offset + at + i)
        .collect()
}

/// Byte offsets in `folded` of `needle`'s characters in order, or `None` when
/// they do not all appear.
///
/// Leftmost-greedy: each needle character takes the earliest position left to
/// it. That is not always the *prettiest* set of highlights — a
/// word-boundary-preferring walk would light `q` in `query` rather than in
/// `graph` for a query of `gq` — but it is the one rule that is obvious to
/// read, cannot backtrack, and gives the same answer every frame. The palette
/// ranks by tier, not by how well a subsequence scattered, so nothing
/// downstream depends on the choice being optimal.
fn subsequence(folded: &str, needle: &str) -> Option<Vec<usize>> {
    let mut lit = Vec::with_capacity(needle.chars().count());
    let mut wanted = needle.chars();
    let mut want = wanted.next()?;
    for (at, ch) in folded.char_indices() {
        if ch != want {
            continue;
        }
        lit.push(at);
        match wanted.next() {
            Some(next) => want = next,
            // Every character placed — the tail of the name is not lit.
            None => return Some(lit),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lit characters, rendered back as the substring they cover, so a
    /// failure reads as text rather than as a list of offsets.
    fn lit_text(name: &str, m: &NameMatch) -> String {
        name.char_indices()
            .filter(|(i, _)| m.lights(*i))
            .map(|(_, c)| c)
            .collect()
    }

    /// **The witness (#5048).** A query whose letters are scattered through
    /// the name matches, and lights exactly those letters — not a prefix, not
    /// the whole name.
    #[test]
    fn a_scattered_query_lights_the_letters_it_matched() {
        let m = match_name("ga", "/graph query").expect("`ga` reaches `/graph query`");
        assert_eq!(m.tier, Tier::Subsequence);
        assert_eq!(lit_text("/graph query", &m), "ga");
        // `g` at 1 and `a` at 3 — inside the name, past the slash.
        assert_eq!(m.lit, vec![1, 3]);
    }

    #[test]
    fn a_prefix_lights_the_head_and_outranks_a_substring() {
        let prefix = match_name("gra", "/graph").expect("prefix");
        assert_eq!(prefix.tier, Tier::Prefix);
        assert_eq!(prefix.lit, vec![1, 2, 3]);

        let substring = match_name("rap", "/graph").expect("substring");
        assert_eq!(substring.tier, Tier::Substring);
        assert_eq!(lit_text("/graph", &substring), "rap");
        assert!(prefix.tier < substring.tier, "a prefix ranks first");
    }

    /// A substring used to light nothing — only a *typed prefix* was ever
    /// lit — so `/ph` found `/graph` and then gave the reader no clue why.
    #[test]
    fn a_substring_lights_where_it_sits_not_the_head() {
        // `/graph` is `/`0 `g`1 `r`2 `a`3 `p`4 `h`5.
        let m = match_name("ph", "/graph").expect("substring");
        assert_eq!(m.lit, vec![4, 5]);
        assert!(!m.lights(1), "the head is not lit for a mid-name match");
    }

    #[test]
    fn the_leading_slash_never_lights() {
        for query in ["g", "graph", "gh"] {
            let m = match_name(query, "/graph").expect(query);
            assert!(!m.lights(0), "`{query}` lit the slash");
        }
    }

    #[test]
    fn an_empty_query_matches_everything_and_lights_nothing() {
        let m = match_name("", "/graph").expect("a bare slash is the browse list");
        assert_eq!(m.tier, Tier::Prefix);
        assert!(m.lit.is_empty());
    }

    #[test]
    fn letters_out_of_order_do_not_match() {
        assert_eq!(match_name("ag", "/graph"), None, "`a` precedes `g`");
        assert_eq!(match_name("graphs", "/graph"), None, "one letter short");
        assert_eq!(match_name("z", "/graph"), None);
    }

    #[test]
    fn matching_is_case_insensitive_on_the_ascii_fold() {
        let m = match_name("gq", "/Graph Query").expect("case-folded");
        assert_eq!(lit_text("/Graph Query", &m), "GQ");
    }

    /// A name with a multi-byte character neither panics nor reports an
    /// offset in the middle of one: every offset is a `char` boundary of the
    /// name as given.
    #[test]
    fn a_multibyte_name_reports_char_boundaries_only() {
        let name = "/日本語 graph";
        let m = match_name("gr", name).expect("the ASCII tail still matches");
        for at in &m.lit {
            assert!(name.is_char_boundary(*at), "{at} splits a character");
        }
        assert_eq!(lit_text(name, &m), "gr");
    }

    /// A leftmost-greedy walk, stated as a test so the choice is pinned
    /// rather than incidental.
    #[test]
    fn a_repeated_letter_takes_the_earliest_position_left_to_it() {
        let m = match_name("aa", "/banana").expect("two `a`s");
        assert_eq!(m.lit, vec![2, 4], "the first two, not the last two");
    }

    #[test]
    fn a_name_with_no_slash_is_matched_from_its_first_character() {
        let m = match_name("gr", "graph").expect("no leading slash");
        assert_eq!(m.lit, vec![0, 1]);
    }

    #[test]
    fn the_tier_order_is_the_rank_order() {
        let tiers = [
            Tier::Prefix,
            Tier::Substring,
            Tier::Subsequence,
            Tier::Description,
        ];
        for (i, tier) in tiers.iter().enumerate() {
            assert_eq!(tier.rank() as usize, i);
        }
        assert!(tiers.windows(2).all(|w| w[0] < w[1]), "declaration is rank");
    }
}
