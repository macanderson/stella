// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The depth ladder and the context budget behind the `search` tool — the
//! decision half, with no I/O in it (invariant 2).
//!
//! # What this module decides
//!
//! `search` ranks files by meaning and then spends a fixed context allowance
//! saying more about the ones that ranked highest. Two questions have to be
//! answered before any file is read, and both are pure arithmetic over owned
//! data:
//!
//! - **How much do we want to know about one hit?** That is [`Depth`], a
//!   1..=10 dial the *maintainer* sets and the model never sees. 1 is the
//!   path; 10 is every bloody detail. Each level strictly adds
//!   ([`facets_at`]), which is what makes a sweep between two settings
//!   interpretable: any behaviour change between depth *n* and depth *n+1*
//!   came from the facets *n+1* added and from nothing else.
//! - **How much can we actually afford?** That is [`allocate`], which spends
//!   a character budget across ranked hits and reports what it could not
//!   afford. Depth is intent; the budget is the hard stop.
//!
//! # Why the budget decides, and not an inferred intent
//!
//! The tempting design is to read the query ("what calls X?") and enrich
//! accordingly. It is the wrong one here: `search` deliberately has no mode
//! parameter, so a wrong intent inference is **unrecoverable** — the model has
//! no flag with which to correct it and must fall back to the tools `search`
//! exists to replace. A budget cannot be wrong in that way. It degrades: the
//! top hit keeps its detail, the tail loses depth first, and the answer says
//! so.
//!
//! # Why the tail decays geometrically — and why leftover budget undoes it
//!
//! [`allocate`] halves the requested depth at each rank. Rank 0 is the answer
//! often enough to earn the full allowance; by rank 3 the hit is a pointer the
//! model may never follow, and paying body-excerpt prices for it is how a
//! one-call search becomes as expensive as the six calls it replaced.
//!
//! That argument is about *scarcity* — where money should go when there is
//! not enough of it. It says nothing about money left on the table, and for a
//! long time the allocator left most of it there: a three-hit answer at the
//! old default depth spent under a tenth of the budget, and the agent then
//! paid a `read_file` for the body the budget had already bought. So after
//! the decayed pass, [`allocate`] spends what remains **promoting** hits back
//! toward the requested depth, best-ranked first. The decay decides who eats
//! first; the budget decides when dinner is over; nothing is thrown away
//! while anyone is still hungry.

/// How much to say about a single search hit: 1 = just the path,
/// 10 = every detail the code graph and the file can produce.
///
/// Owned by configuration, never by the model. A knob the model must learn to
/// use well will not be used well — that is the measured lesson of
/// `gather_context`, which offered exactly this power through parameters and
/// was called zero times in 408 trials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Depth(u8);

impl Depth {
    /// Just the path.
    pub const MIN: Depth = Depth(1);
    /// Every facet the ladder knows about.
    pub const MAX: Depth = Depth(10);
    /// Everything, and let the budget govern.
    ///
    /// This was 4 — "cheap enough that a ten-hit answer is still a pointer" —
    /// which quietly made the *dial* the binding constraint instead of the
    /// budget: signature, doc, callers and body could never render at any
    /// budget, a three-hit answer spent under a tenth of its allowance, and
    /// the agent then paid a `read_file` for the body the tool's own default
    /// budget was explicitly sized to carry (`stella-tools`'
    /// `DEFAULT_BUDGET_CHARS`). The budget is the honest governor — it
    /// degrades gracefully and reports what it dropped, where a depth cap is
    /// silent — so the default intent is "everything affordable".
    pub const DEFAULT: Depth = Depth::MAX;

    /// Clamp `level` into 1..=10. Total by construction: a depth read from
    /// configuration is runtime data, and there is no level at which
    /// refusing to search is the better answer.
    #[must_use]
    pub const fn new(level: u8) -> Depth {
        if level < Depth::MIN.0 {
            Depth::MIN
        } else if level > Depth::MAX.0 {
            Depth::MAX
        } else {
            Depth(level)
        }
    }

    /// The clamped level, 1..=10.
    #[must_use]
    pub const fn level(self) -> u8 {
        self.0
    }

    /// One rung down, saturating at [`Depth::MIN`].
    #[must_use]
    pub const fn shallower(self) -> Depth {
        Depth::new(self.0.saturating_sub(1))
    }

    /// One rung up, saturating at [`Depth::MAX`].
    #[must_use]
    pub const fn deeper(self) -> Depth {
        Depth::new(self.0.saturating_add(1))
    }
}

impl Default for Depth {
    fn default() -> Self {
        Depth::DEFAULT
    }
}

/// One kind of detail the renderer can add to a hit.
///
/// The order is the ladder, and the ladder is ordered by **rendered cost**,
/// cheapest first. That ordering is the reason the budget can degrade a hit
/// gracefully: dropping a rung always removes the most expensive thing still
/// present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Facet {
    /// The file path. Always present — a hit with no path is not a hit.
    Path,
    /// The names of the symbols the file defines.
    SymbolNames,
    /// Each symbol's kind and definition line.
    SymbolKinds,
    /// What the file imports.
    Imports,
    /// What imports the file.
    Importers,
    /// The declaration line of the file's leading symbols.
    Signature,
    /// The doc comment immediately above those declarations.
    DocComment,
    /// What calls those symbols.
    Callers,
    /// What those symbols call.
    Callees,
    /// The source body of the leading symbol.
    Body,
}

impl Facet {
    /// Every facet, in ladder order.
    pub const ALL: [Facet; 10] = [
        Facet::Path,
        Facet::SymbolNames,
        Facet::SymbolKinds,
        Facet::Imports,
        Facet::Importers,
        Facet::Signature,
        Facet::DocComment,
        Facet::Callers,
        Facet::Callees,
        Facet::Body,
    ];

    /// The lowest depth at which this facet is rendered.
    #[must_use]
    pub const fn min_depth(self) -> Depth {
        match self {
            Facet::Path => Depth(1),
            Facet::SymbolNames => Depth(2),
            Facet::SymbolKinds => Depth(3),
            Facet::Imports => Depth(4),
            Facet::Importers => Depth(5),
            Facet::Signature => Depth(6),
            Facet::DocComment => Depth(7),
            Facet::Callers => Depth(8),
            Facet::Callees => Depth(9),
            Facet::Body => Depth(10),
        }
    }

    /// What this facet is estimated to cost in rendered characters.
    ///
    /// Estimates, deliberately: the budget's job is to stop an answer running
    /// away, and an estimate that is uniformly a little wrong still orders
    /// the ladder correctly. Measuring the true cost would require rendering
    /// first, which is the I/O this module is not allowed to do — and would
    /// make the allocation depend on the workspace, so two runs at the same
    /// depth would no longer be comparable.
    #[must_use]
    pub const fn estimated_cost(self) -> usize {
        match self {
            Facet::Path => 60,
            Facet::SymbolNames => 140,
            Facet::SymbolKinds => 160,
            Facet::Imports => 200,
            Facet::Importers => 200,
            Facet::Signature => 140,
            Facet::DocComment => 320,
            Facet::Callers => 400,
            Facet::Callees => 400,
            Facet::Body => 1_600,
        }
    }

    /// Whether this facet is rendered at `depth`.
    #[must_use]
    pub const fn rendered_at(self, depth: Depth) -> bool {
        self.min_depth().0 <= depth.0
    }
}

/// The facets rendered at `depth`, in ladder order.
///
/// Monotone **by construction**: membership is `facet.min_depth() <= depth`,
/// so `facets_at(n)` is a prefix of `facets_at(n + 1)` and no level can ever
/// take a facet away from the level below it. That is a structural property,
/// not a convention someone has to remember — which matters because a
/// non-monotone rung would make a depth sweep uninterpretable.
#[must_use]
pub fn facets_at(depth: Depth) -> Vec<Facet> {
    Facet::ALL
        .into_iter()
        .filter(|facet| facet.rendered_at(depth))
        .collect()
}

/// The estimated rendered cost of one hit at `depth`, in characters.
#[must_use]
pub fn hit_cost(depth: Depth) -> usize {
    Facet::ALL
        .iter()
        .filter(|facet| facet.rendered_at(depth))
        .map(|facet| facet.estimated_cost())
        .sum()
}

/// The depth rank `rank` asks for when the top hit is granted `top`.
///
/// Halves per rank: rank 0 gets `top`, rank 1 half of it, and so on to the
/// floor. See the module docs for why the tail decays rather than sharing
/// equally.
#[must_use]
pub fn requested_depth(top: Depth, rank: usize) -> Depth {
    let shift = u32::try_from(rank).unwrap_or(u32::MAX);
    Depth::new(top.level().checked_shr(shift).unwrap_or(0))
}

/// A budget spent across ranked hits.
///
/// Total by construction and never a `Result`: every input has an answer,
/// including a budget too small for a single path, which allocates nothing
/// and omits everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    /// The depth granted to each hit, in rank order. Shorter than the hit
    /// count exactly when the budget ran out.
    pub granted: Vec<Depth>,
    /// Hits the budget could not afford at any depth. **Must be stated in
    /// the rendered answer**: an agent that cannot tell a complete answer
    /// from a truncated one will stop looking, which is the failure mode this
    /// field exists to prevent.
    pub omitted: usize,
    /// Estimated characters the granted depths will spend.
    pub spent: usize,
}

impl Allocation {
    /// Whether anything was dropped for want of budget.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.omitted > 0
    }
}

/// Spend `budget` characters across `hits` ranked results, granting every
/// hit at most `top` depth.
///
/// Two passes, both greedy in rank order — the only order that respects the
/// ranking:
///
/// 1. **Decay.** Each hit asks for [`requested_depth`] and is reduced to
///    fit, and reduced again, down to [`Depth::MIN`]; only a budget that
///    cannot afford a bare path stops the walk, and everything from there on
///    is omitted rather than silently rendered thin.
/// 2. **Promotion.** Whatever the decayed pass did not spend deepens the
///    granted hits back toward `top`, best-ranked first, one rung at a time.
///    The decay is a priority order under scarcity, not a verdict that the
///    tail deserves less when there is budget to spare — leaving most of the
///    allowance unspent while the caller pays a follow-up read for the body
///    it would have bought is the failure this pass removes.
///
/// Promotion cannot break the decay's shape: a later hit climbs through the
/// same rung prices an earlier hit already faced, with strictly less budget
/// left, so the granted depths stay non-increasing by rank (a property test
/// pins this).
#[must_use]
pub fn allocate(hits: usize, top: Depth, budget: usize) -> Allocation {
    let mut granted = Vec::with_capacity(hits);
    let mut spent = 0usize;
    let mut omitted = 0usize;

    'ranks: for rank in 0..hits {
        let mut depth = requested_depth(top, rank);
        loop {
            if spent + hit_cost(depth) <= budget {
                spent += hit_cost(depth);
                granted.push(depth);
                break;
            }
            if depth == Depth::MIN {
                omitted = hits - granted.len();
                break 'ranks;
            }
            depth = depth.shallower();
        }
    }

    for depth in &mut granted {
        while *depth < top {
            let step = hit_cost(depth.deeper()) - hit_cost(*depth);
            if spent + step > budget {
                // Move on rather than stop: a later, shallower hit's next
                // rung is cheaper and may still fit.
                break;
            }
            spent += step;
            *depth = depth.deeper();
        }
    }

    Allocation {
        granted,
        omitted,
        spent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn depth_clamps_into_the_ladder() {
        assert_eq!(Depth::new(0), Depth::MIN);
        assert_eq!(Depth::new(200), Depth::MAX);
        assert_eq!(Depth::new(7).level(), 7);
    }

    #[test]
    fn the_shallowest_depth_is_the_path_alone() {
        assert_eq!(facets_at(Depth::MIN), vec![Facet::Path]);
        assert_eq!(facets_at(Depth::MAX).len(), Facet::ALL.len());
    }

    #[test]
    fn a_budget_too_small_for_one_path_omits_everything() {
        let allocation = allocate(5, Depth::MAX, 0);
        assert!(allocation.granted.is_empty());
        assert_eq!(allocation.omitted, 5);
        assert!(allocation.truncated());
    }

    /// Under scarcity the decay is visible: the top hit keeps its depth and
    /// the tail thins. The budget here affords the decayed pass exactly
    /// (1620 + 560 + 200 + 3x60) plus one spare rung for rank 1.
    #[test]
    fn the_top_hit_can_be_deep_while_the_tail_is_shallow() {
        let budget = hit_cost(Depth::new(8))
            + hit_cost(Depth::new(4))
            + hit_cost(Depth::new(2))
            + 3 * hit_cost(Depth::MIN)
            + (hit_cost(Depth::new(5)) - hit_cost(Depth::new(4)));
        let allocation = allocate(6, Depth::new(8), budget);
        assert_eq!(allocation.granted[0], Depth::new(8));
        assert_eq!(
            allocation.granted[1],
            Depth::new(5),
            "the spare rung promotes rank 1"
        );
        assert_eq!(allocation.granted[2], Depth::new(2));
        assert_eq!(allocation.granted[3], Depth::MIN);
        assert!(!allocation.truncated());
    }

    /// **The promotion witness.** A budget that affords every hit at full
    /// depth grants every hit full depth — before the promotion pass, this
    /// allocation left the tail at [1, 5, 2, 1, 1, 1] with most of the
    /// budget unspent, and the caller paid follow-up reads for bodies the
    /// budget could already have carried.
    #[test]
    fn leftover_budget_deepens_the_tail_instead_of_going_unspent() {
        let allocation = allocate(6, Depth::MAX, 6 * hit_cost(Depth::MAX));
        assert_eq!(allocation.granted, vec![Depth::MAX; 6]);
        assert_eq!(allocation.spent, 6 * hit_cost(Depth::MAX));
        assert!(!allocation.truncated());
    }

    /// A budget one rung short of everything: the shortfall lands on the
    /// worst-ranked hit, never the best.
    #[test]
    fn promotion_spends_on_the_best_ranked_hits_first() {
        let step = hit_cost(Depth::MAX) - hit_cost(Depth::new(9));
        let allocation = allocate(3, Depth::MAX, 3 * hit_cost(Depth::MAX) - step);
        assert_eq!(
            allocation.granted,
            vec![Depth::MAX, Depth::MAX, Depth::new(9)]
        );
    }

    proptest! {
        /// The property the whole ladder rests on: level n + 1 strictly adds.
        #[test]
        fn each_depth_is_a_superset_of_the_one_below(level in 1u8..10) {
            let lower = facets_at(Depth::new(level));
            let higher = facets_at(Depth::new(level + 1));
            prop_assert!(higher.len() > lower.len(), "level {level} added nothing");
            for facet in &lower {
                prop_assert!(higher.contains(facet), "level {level} lost {facet:?}");
            }
        }

        #[test]
        fn cost_never_falls_as_depth_rises(level in 1u8..10) {
            prop_assert!(hit_cost(Depth::new(level + 1)) > hit_cost(Depth::new(level)));
        }

        #[test]
        fn allocation_never_overspends(
            hits in 0usize..24,
            top in 1u8..=10,
            budget in 0usize..40_000,
        ) {
            let allocation = allocate(hits, Depth::new(top), budget);
            prop_assert!(allocation.spent <= budget);
            prop_assert_eq!(allocation.granted.len() + allocation.omitted, hits);
            let recomputed: usize = allocation.granted.iter().copied().map(hit_cost).sum();
            prop_assert_eq!(recomputed, allocation.spent);
        }

        /// `top` is a ceiling on every hit, and the ranking's shape survives
        /// promotion: depths never increase with rank, so a worse-ranked hit
        /// is never told more about than a better-ranked one.
        #[test]
        fn grants_respect_the_ceiling_and_never_rise_with_rank(
            hits in 0usize..24,
            top in 1u8..=10,
            budget in 0usize..40_000,
        ) {
            let allocation = allocate(hits, Depth::new(top), budget);
            for (rank, granted) in allocation.granted.iter().enumerate() {
                prop_assert!(*granted <= Depth::new(top));
                if rank > 0 {
                    prop_assert!(*granted <= allocation.granted[rank - 1]);
                }
            }
        }

        /// Invariant 7 reaches here: the same configuration must produce the
        /// same plan, or two runs of the same query are not comparable.
        #[test]
        fn allocation_is_deterministic(
            hits in 0usize..24,
            top in 1u8..=10,
            budget in 0usize..40_000,
        ) {
            prop_assert_eq!(
                allocate(hits, Depth::new(top), budget),
                allocate(hits, Depth::new(top), budget)
            );
        }
    }
}
