// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where a palette query met a command name — the positions SPEC 10's gold
//! letters are painted at.
//!
//! The filter used to answer one question, "does this row survive?", so the
//! renderer had nothing to light but the typed prefix it could re-derive
//! itself. A match here carries the char offsets it consumed, which is what
//! lets `ga` light the `g` and the `a` of `/graph query`.
//!
//! SPEC 10 names `nucleo` and this is not it. A command name is a short ASCII
//! slug and the palette's order is decided by [`Kind`] and by what the
//! session is doing (`super::palette::relevant_now`), so the scoring model a
//! fuzzy-finder crate exists for would be computed and then discarded. What
//! remains is the subsequence walk below.

/// How a query met a name, strongest reading first — the palette's first sort
/// key, ahead of session relevance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The name begins with the query: `/pl` → `/plan`.
    Prefix,
    /// The query appears whole, somewhere later: `/iff` → `/diff`.
    Substring,
    /// The query's letters appear in order with gaps between them: `ga` →
    /// the `g` and the `a` of `/graph query`.
    Scattered,
}

/// The rank a description-substring match sorts at: below every name match,
/// including a scattered one. A name is what the user is typing at; a word
/// that happens to sit in an explanation is the weakest reason to offer a row.
pub const DESCRIPTION_RANK: u8 = 3;

impl Kind {
    /// The sort key, ascending. Kept below [`DESCRIPTION_RANK`] by
    /// construction — the tests hold that.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Kind::Prefix => 0,
            Kind::Substring => 1,
            Kind::Scattered => 2,
        }
    }
}

/// One name match: how it matched, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameMatch {
    pub kind: Kind,
    /// Char offsets into the haystack the needle consumed, ascending. Empty
    /// for an empty needle, which matches everything and lights nothing.
    pub indices: Vec<usize>,
}

/// Match `needle` against `haystack`, both already folded to the same case by
/// the caller. `None` when the needle's letters are not all present in order.
///
/// A contiguous run wins before the scattered walk is tried, so a query that
/// is a substring keeps the rank and the highlight it always had — `at` in
/// `/a cat` lights the `at` of `cat` rather than the first `a` and the last
/// `t`.
#[must_use]
pub fn match_name(haystack: &str, needle: &str) -> Option<NameMatch> {
    if needle.is_empty() {
        return Some(NameMatch {
            kind: Kind::Prefix,
            indices: Vec::new(),
        });
    }
    let hay: Vec<char> = haystack.chars().collect();
    let ndl: Vec<char> = needle.chars().collect();
    if let Some(at) = hay.windows(ndl.len()).position(|w| w == ndl.as_slice()) {
        let kind = if at == 0 {
            Kind::Prefix
        } else {
            Kind::Substring
        };
        return Some(NameMatch {
            kind,
            indices: (at..at + ndl.len()).collect(),
        });
    }
    // Greedy from the left: the earliest letter that can serve is the one
    // taken. For a slug this is also the tightest run available, and it is
    // what a reader expects — the highlight walks the name in reading order.
    let mut indices = Vec::with_capacity(ndl.len());
    let mut chars = hay.iter().enumerate();
    for want in &ndl {
        let at = chars.by_ref().find(|(_, c)| *c == want).map(|(i, _)| i)?;
        indices.push(at);
    }
    Some(NameMatch {
        kind: Kind::Scattered,
        indices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_needle_matches_everything_and_lights_nothing() {
        let m = match_name("plan", "").expect("everything matches");
        assert_eq!(m.kind, Kind::Prefix);
        assert!(m.indices.is_empty());
    }

    #[test]
    fn a_prefix_and_a_substring_are_told_apart_and_both_are_contiguous() {
        let prefix = match_name("plan", "pl").expect("prefix");
        assert_eq!(prefix.kind, Kind::Prefix);
        assert_eq!(prefix.indices, vec![0, 1]);

        let inner = match_name("diff", "iff").expect("substring");
        assert_eq!(inner.kind, Kind::Substring);
        assert_eq!(inner.indices, vec![1, 2, 3]);
    }

    /// **The witness (#5048).** Scattered letters match and report where they
    /// landed — `ga` is neither a prefix nor a substring of `graph query`,
    /// and it is what the palette must light.
    #[test]
    fn scattered_letters_match_and_name_their_positions() {
        let m = match_name("graph query", "ga").expect("ga is in graph");
        assert_eq!(m.kind, Kind::Scattered);
        assert_eq!(m.indices, vec![0, 2], "the g and the a of graph");
    }

    #[test]
    fn a_contiguous_run_beats_the_scattered_walk() {
        let m = match_name("a cat", "at").expect("at is in cat");
        assert_eq!(m.kind, Kind::Substring);
        assert_eq!(m.indices, vec![3, 4], "the at of cat, not a…t");
    }

    #[test]
    fn letters_out_of_order_or_absent_do_not_match() {
        assert_eq!(match_name("graph", "ag"), None, "order is required");
        assert_eq!(match_name("graph", "gz"), None);
        assert_eq!(match_name("gr", "graph"), None, "needle longer than name");
    }

    #[test]
    fn every_name_rank_sorts_above_a_description_match() {
        for kind in [Kind::Prefix, Kind::Substring, Kind::Scattered] {
            assert!(kind.rank() < DESCRIPTION_RANK, "{kind:?}");
        }
    }
}
