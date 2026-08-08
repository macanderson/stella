//! Query-conditional evidence: the admission gate behind "a frame is recalled
//! if and only if something ties it to *this* query".
//!
//! The fusion in [`super`] already states the principle — vector, graph, and
//! domain are grounded signals; recency is a tiebreaker, not evidence — but
//! nothing enforced it: every embedded node is in the vector list, so on a
//! store with ≥`max_frames` live nodes the budget filled every slot no matter
//! how badly the survivors scored. A workspace whose store held five stale
//! notes surfaced the same five on every prompt, forever (#2289). The measured
//! incident is quoted at [`super::DEFAULT_RECENCY_WEIGHT`]: damping recency
//! removed one injection mechanism, this module removes the other — admission
//! without evidence.
//!
//! What counts as evidence is deliberately narrow, and each channel is
//! query-conditional (it can only fire because of what *this* query said):
//!
//! - an **anchor** — the goal named the file verbatim;
//! - **anchor adjacency** — a 1-hop graph neighbor of an anchor;
//! - a **distinctive lexical match** — the node contains a query term that is
//!   rare in this workspace's corpus (see [`term_is_distinctive`]);
//! - **domain overlap** — the query was scoped and the node shares a domain;
//! - a **semantic similarity floor** — only when the active embedder declares
//!   [`SimilarityPosture::Semantic`](crate::embed::SimilarityPosture). The
//!   default [`HashEmbedder`](crate::embed::HashEmbedder) declares `Surface`:
//!   its trigram cosine orders candidates but certifies nothing, which is not
//!   caution but measurement — contaminant frames scored 0.38–0.50 against a
//!   prompt whose genuinely relevant frames scored 0.45–0.63, so no floor
//!   separates them (see [`super::DEFAULT_RECENCY_WEIGHT`]).
//!
//! Like the rest of `retrieval`'s split-out modules, everything here is a pure
//! function over already-loaded data. The parent owns the I/O: it streams the
//! corpus past [`TermMatcher::observe`] once (the same scan shape the lexical
//! fallback always paid) and hands the finished [`TermEvidence`] back down.

use std::collections::{HashMap, HashSet};

/// Whether a query term constitutes evidence when a node contains it.
///
/// A term found in most of the corpus says nothing about *this* node — glue
/// words ("the", "with") match everything and would make the gate vacuous.
/// The line is the sign change of the Robertson–Spärck Jones IDF used by BM25,
/// `ln((N − df + 0.5) / (df + 0.5))`: positive exactly when the term appears
/// in fewer than half the documents. Integer form, with the corpus floored at
/// 2 so a one-node store can still be matched (`df = 1, N = 1` is that node's
/// own distinctive content, not glue).
///
/// Corpus-relative on purpose: a stop-word list would be a language
/// commitment this plane has avoided everywhere else (the hash embedder is
/// character-level for the same reason), and it would go stale against code
/// identifiers. The known cost: in a corpus where *every* node shares the
/// query's topical term, that term saturates (`df = N`) and stops being
/// evidence — the gate then abstains unless another channel (anchor, domain,
/// semantic floor) fires. That window is transient — df falls below N/2 as
/// soon as memories on other topics accumulate — and it errs toward silence,
/// which is the deliberate direction: the failure this module exists to stop
/// is confidently surfacing noise.
pub(crate) fn term_is_distinctive(df: usize, corpus: usize) -> bool {
    2 * df <= corpus.max(2)
}

/// One streaming pass's accumulator: which query terms each live node
/// contains, and each term's document frequency.
///
/// Built over the **deduplicated** term list so a goal that repeats a word
/// neither double-scans nor double-counts df. The original list's
/// multiplicities are kept so [`TermEvidence::fallback_scores`] can reproduce
/// the historical lexical-fallback score (hits over the *original* term
/// count) byte-for-byte.
pub(crate) struct TermMatcher {
    /// Deduplicated terms, in first-appearance order.
    unique: Vec<String>,
    /// How many times each unique term appeared in the original list.
    multiplicity: Vec<usize>,
    /// Length of the original (undeduplicated) term list — the fallback
    /// score's historical denominator.
    original_len: usize,
    /// Per unique term: how many live nodes matched it.
    df: Vec<usize>,
    /// node id → indices into `unique` that its text contained.
    matched: HashMap<i64, Vec<usize>>,
    /// Live nodes observed, matching or not — the df denominator.
    corpus: usize,
}

impl TermMatcher {
    pub(crate) fn new(terms: &[String]) -> Self {
        let mut unique: Vec<String> = Vec::new();
        let mut multiplicity: Vec<usize> = Vec::new();
        for t in terms {
            match unique.iter().position(|u| u == t) {
                Some(i) => multiplicity[i] += 1,
                None => {
                    unique.push(t.clone());
                    multiplicity.push(1);
                }
            }
        }
        let df = vec![0; unique.len()];
        Self {
            unique,
            multiplicity,
            original_len: terms.len(),
            df,
            matched: HashMap::new(),
            corpus: 0,
        }
    }

    /// Fold one live node's text into the pass. `haystack` must already be
    /// lowercased by the caller (terms are lowercased at extraction).
    pub(crate) fn observe(&mut self, id: i64, haystack: &str) {
        self.corpus += 1;
        let mut hits: Vec<usize> = Vec::new();
        for (slot, term) in self.unique.iter().enumerate() {
            if haystack.contains(term.as_str()) {
                self.df[slot] += 1;
                hits.push(slot);
            }
        }
        if !hits.is_empty() {
            self.matched.insert(id, hits);
        }
    }

    pub(crate) fn finish(self) -> TermEvidence {
        TermEvidence {
            multiplicity: self.multiplicity,
            original_len: self.original_len,
            df: self.df,
            matched: self.matched,
            corpus: self.corpus,
        }
    }
}

/// The finished pass: everything admission and the lexical fallback need,
/// with the corpus's bodies long gone.
pub(crate) struct TermEvidence {
    multiplicity: Vec<usize>,
    original_len: usize,
    df: Vec<usize>,
    matched: HashMap<i64, Vec<usize>>,
    corpus: usize,
}

impl TermEvidence {
    /// The nodes whose match includes at least one distinctive term — the
    /// lexical evidence channel.
    pub(crate) fn distinctive_matchers(&self) -> HashSet<i64> {
        self.matched
            .iter()
            .filter(|(_, slots)| {
                slots
                    .iter()
                    .any(|&s| term_is_distinctive(self.df[s], self.corpus))
            })
            .map(|(&id, _)| id)
            .collect()
    }

    /// The historical lexical-fallback scores: for every matching node, the
    /// fraction of the *original* term list its text contained. A term the
    /// goal repeated counts once per repetition, exactly as the substring
    /// scan always scored it.
    pub(crate) fn fallback_scores(&self) -> Vec<(i64, f32)> {
        if self.original_len == 0 {
            return Vec::new();
        }
        self.matched
            .iter()
            .map(|(&id, slots)| {
                let hits: usize = slots.iter().map(|&s| self.multiplicity[s]).sum();
                (id, hits as f32 / self.original_len as f32)
            })
            .collect()
    }
}

/// The assembled admission set: a candidate is admissible iff it is in here.
///
/// `None` means the gate is off (`require_evidence: false` — the documented
/// escape hatch back to pre-#2289 behavior) and everything is admissible.
pub(crate) fn admissible_ids(
    anchor_ids: &[i64],
    anchor_neighbors: &HashSet<i64>,
    lexical: &HashSet<i64>,
    domain_ranked: &[i64],
    semantic_hits: &[i64],
) -> HashSet<i64> {
    let mut evidence: HashSet<i64> = HashSet::new();
    evidence.extend(anchor_ids.iter().copied());
    evidence.extend(anchor_neighbors.iter().copied());
    evidence.extend(lexical.iter().copied());
    evidence.extend(domain_ranked.iter().copied());
    evidence.extend(semantic_hits.iter().copied());
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn distinctiveness_is_the_bm25_idf_sign() {
        // df ≤ N/2 is distinctive; above it is glue. The exact zero crossing
        // of ln((N - df + 0.5)/(df + 0.5)).
        assert!(term_is_distinctive(1, 10));
        assert!(term_is_distinctive(5, 10));
        assert!(!term_is_distinctive(6, 10));
        assert!(!term_is_distinctive(10, 10));
    }

    #[test]
    fn a_one_node_store_can_match_its_own_content() {
        // N floored at 2, so df=1 of N=1 still counts as evidence.
        assert!(term_is_distinctive(1, 1));
        assert!(term_is_distinctive(1, 0));
    }

    #[test]
    fn glue_terms_admit_nothing_distinctive() {
        let mut m = TermMatcher::new(&terms(&["the", "keybinding"]));
        m.observe(1, "the deck keybinding for scrollback");
        m.observe(2, "the witness stage authors the failing test");
        m.observe(3, "the budget pack reports the drops");
        let e = m.finish();
        // "the" matched all three (df 3 of 3 → glue); "keybinding" matched
        // only node 1 (df 1 of 3 → distinctive).
        let d = e.distinctive_matchers();
        assert_eq!(d, HashSet::from([1]));
    }

    #[test]
    fn fallback_scores_keep_the_original_denominator_and_multiplicity() {
        // "test" repeated in the goal counts once per repetition, over the
        // undeduplicated length — byte-compatible with the substring scan
        // this pass replaced.
        let mut m = TermMatcher::new(&terms(&["test", "test", "deck"]));
        m.observe(7, "a test of the deck");
        m.observe(8, "nothing relevant here");
        let e = m.finish();
        let scores: HashMap<i64, f32> = e.fallback_scores().into_iter().collect();
        assert_eq!(scores.get(&7), Some(&1.0)); // (2 + 1) / 3
        assert_eq!(scores.get(&8), None); // no match, no row — as before
    }

    #[test]
    fn admissibility_is_the_union_of_the_channels() {
        let ids = admissible_ids(
            &[1],
            &HashSet::from([2]),
            &HashSet::from([3]),
            &[4],
            &[5],
        );
        assert_eq!(ids, HashSet::from([1, 2, 3, 4, 5]));
    }
}
