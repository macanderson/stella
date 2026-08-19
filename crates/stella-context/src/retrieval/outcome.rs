//! What a recall returns: the shortlist candidate the budget rules on, why it
//! was selected, what had to be dropped, and the result the caller reads.
//!
//! Split out of `retrieval.rs` (#3705). The pipeline that produces these lives
//! in the module above; this is the vocabulary it reports in, and the half a
//! consumer reads — every type here is public API except [`Ranked`], which
//! never leaves the crate.

use contextgraph_types::{ContextFrame, ContextQueryResult};

use crate::candidates::NodeMeta;

/// A ranked candidate on its way to the budget: everything packing needs, and
/// no body.
///
/// Packing used to run on `ContextFrame`s, which meant every candidate on the
/// shortlist had its body cloned and its frame minted before the budget got a
/// say — and the budget then discarded most of them. `NodeMeta` already carries
/// the two things a packer reads (a token cost and something to name a drop
/// by), so the frames are built after the cut instead: "frame construction only
/// for packed survivors" (#712 deliverable 2).
#[derive(Debug, Clone)]
pub(crate) struct Ranked {
    /// The candidate's ranking metadata — id, label, hash, byte count.
    pub meta: NodeMeta,
    /// Its max-normalized fused (RRF) score, carried through packing so the
    /// frame built for a survivor declares the score the ranking gave it.
    /// MMR changes the order these arrive in, not this value.
    pub relevance: f32,
    /// Why this candidate is in front of the budget at all — Phase 2 (#713).
    pub selection_reason: SelectionReason,
}

impl Ranked {
    /// Whether ranking is forbidden from evicting this candidate. See
    /// [`SelectionReason::is_required`].
    pub fn is_required(&self) -> bool {
        self.selection_reason.is_required()
    }
}

/// Why a candidate was selected into the ranked shortlist.
///
/// Phase 2 (#713) deliverable 5. Before this, a frame arrived with a score and
/// no account of what produced it, so "why is this in my context?" was
/// answerable only by re-deriving the whole retrieval. The vocabulary is
/// deliberately small — it names the *mechanism*, which is stable, rather than
/// a rationale, which would drift into prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionReason {
    /// The goal named this file verbatim, so the graph expansion anchored on
    /// it. **Required**: the user pointed at it, and no ranking heuristic is a
    /// better verifier of relevance than that.
    Anchored,
    /// It won the hybrid ranking — vector similarity fused with recency and
    /// graph adjacency, then diversified. The ordinary path.
    Ranked,
    /// Vector coverage of the goal fell below the floor and labeled lexical
    /// search ran instead (`L-C6`). Carried so a consumer can tell grounding
    /// from a keyword match dressed up as grounding.
    LexicalFallback,
}

impl SelectionReason {
    /// Whether budget packing may drop this candidate **by rank**.
    ///
    /// A required item can still be dropped, but only by an explicit decision
    /// that is reported — [`DropReason::RequiredOverBudget`] when it exceeds
    /// the whole token budget alone, [`DropReason::TokenBudget`] when earlier
    /// required items already spent the budget it needed — never by
    /// falling off the bottom of a ranked list. That is the ADR 0006
    /// guarantee: "required items cannot be evicted by ranking; precedence is
    /// category-aware, and budget packing may drop only non-required items,
    /// always with a drop-report".
    #[must_use]
    pub fn is_required(self) -> bool {
        matches!(self, SelectionReason::Anchored)
    }

    /// The stable wire spelling, for receipts and drop reports.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SelectionReason::Anchored => "anchored",
            SelectionReason::Ranked => "ranked",
            SelectionReason::LexicalFallback => "lexical_fallback",
        }
    }
}

/// Which precedence band a candidate competes in once the budget binds.
///
/// This is deliberately **not** a score. Scores are query-dependent and already
/// fully expressed by the fused ranking; a tier says something the query cannot
/// know — that a whole *class* of frame is worth less than the others when, and
/// only when, something has to be dropped. Within a tier the ranking still
/// decides everything.
///
/// The vocabulary has exactly two entries, and the asymmetry is the point.
/// [`Normal`](RecallTier::Normal) is the default for every node the store has
/// ever written, so adding this changes nothing for code symbols, episodes, or
/// ordinary memories. [`Deferred`](RecallTier::Deferred) has to be asked for.
/// Nothing is *promoted* by a tier — a frame can only volunteer to yield first
/// — which is what keeps a writer from buying rank by relabeling its own
/// content.
///
/// Its one caller today is the reflection lifecycle: a lesson about how the
/// agent went about its work (`LessonKind::Process`) is written `Deferred`, so
/// that a five-frame budget spends its slots on facts about the codebase before
/// it spends them on commentary about the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord)]
pub enum RecallTier {
    /// Competes on rank alone. Every node written before this existed, and
    /// every node whose writer says nothing about tiering.
    #[default]
    Normal,
    /// Admitted only after every `Normal` candidate has taken what it needs.
    /// Still ranked, still citable, still recalled whenever the budget has
    /// room — it simply loses every tie against a frame that is not deferred.
    Deferred,
}

impl RecallTier {
    /// The stable wire spelling, for provenance and drop reports.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RecallTier::Normal => "normal",
            RecallTier::Deferred => "deferred",
        }
    }

    /// How the tier is stored on the `node` row. `Normal` is 0 so the column's
    /// `DEFAULT 0` and this enum's `Default` are the same value, and the v9
    /// backfill is a no-op.
    #[must_use]
    pub fn as_i64(self) -> i64 {
        match self {
            RecallTier::Normal => 0,
            RecallTier::Deferred => 1,
        }
    }

    /// Read a tier back from storage or from the wire.
    ///
    /// An unrecognized value reads as `Normal` rather than failing: a tier is a
    /// de-prioritization hint, and the safe direction for an unknown one is to
    /// leave the frame competing normally. A newer stella's tiers cannot reach
    /// here anyway — the schema migration rejects a store stamped by a newer
    /// binary.
    #[must_use]
    pub fn from_i64(value: i64) -> Self {
        match value {
            1 => RecallTier::Deferred,
            _ => RecallTier::Normal,
        }
    }
}

/// Why a candidate frame did not make it into the assembled context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Keeping it would have exceeded the query's `max_tokens`.
    TokenBudget,
    /// The query's `max_frames` count was already reached.
    FrameCount,
    /// A **required** item that could not be honored: on its own it exceeds
    /// the query's entire token budget, so no ordering of the pack could have
    /// admitted it — Phase 2 (#713).
    ///
    /// Distinct from [`Self::TokenBudget`] on purpose. That one says "the
    /// budget filled up before we got here", which is a statement about the
    /// other candidates and is fixed by asking for more or for fewer frames.
    /// This one says "you asked for this specifically and it does not fit at
    /// all", which is the explicit, reported decision ADR 0006 requires before
    /// a required item may be dropped. Collapsing the two would hide the only
    /// case where the caller's own instruction was overruled.
    RequiredOverBudget,
}

/// A frame that was retrieved and scored but did not fit the budget. Reported
/// so assembly is never a silent truncation (`L-C5`).
#[derive(Debug, Clone)]
pub struct DroppedFrame {
    /// The frame id it would have carried (the node's `nod_…` public id), so a
    /// caller can ask for it explicitly on a follow-up query.
    pub id: String,
    /// Its citation label, so the drop report reads as names rather than ids
    /// (`L-C4`).
    pub title: String,
    /// What it would have cost, so a caller can size the budget a re-query
    /// needs instead of guessing.
    pub token_cost: u32,
    /// Which limit dropped it.
    pub reason: DropReason,
}

/// The typed, inspectable result of a recall (typed
/// outputs, not stringly telemetry). Carries the packed frames, the dropped
/// report, the coverage score, and the honesty flag for lexical fallback.
#[derive(Debug, Clone)]
pub struct RecallResult {
    /// Budget-respecting, MMR-ordered frames ready to assemble into a prompt.
    pub frames: Vec<ContextFrame>,
    /// What was scored but dropped, and why (`L-C5`).
    pub dropped: Vec<DroppedFrame>,
    /// Mean top-k vector coverage of the goal, in `[0, 1]`.
    pub coverage: f32,
    /// True when coverage fell below threshold and lexical fallback ran
    /// (`L-C6`). Individual fallback frames are also marked in their provenance.
    pub used_lexical_fallback: bool,
    /// How many candidates the budget actually chose between — the packed
    /// survivors plus [`Self::dropped`].
    ///
    /// That is `frames.len() + dropped.len()` in every ordinary recall, but
    /// not by invariant: frame construction runs after packing and skips a
    /// row that vanished between the two reads (a frame's digest must
    /// describe bytes that exist), so `frames` can come up short of the
    /// packed count. The denominator deliberately counts what the budget
    /// *decided over*, not what survived serving.
    ///
    /// This is the denominator [`Self::dropped`] is a numerator of, and it is
    /// the field that makes the drop report mean something. It used to be the
    /// corpus: recency contributes every live node to the fusion, so a
    /// workspace with 500 memories reported ~495 drops and permanent truncation
    /// every single turn. That number was true and useless — it described how
    /// much the workspace had accumulated, not anything a caller could change
    /// by raising a budget. Now it describes the ranked shortlist the budget
    /// was offered, so "12 of 20 dropped" is an actionable statement about this
    /// query (#712 deliverable 3).
    pub considered: usize,
    /// How many fused candidates were cut by the candidate bound *before* the
    /// budget saw them — ranked below the shortlist, never offered.
    ///
    /// Reported separately rather than folded into [`Self::dropped`] because
    /// the two say different things: a budget drop is reversible by asking for
    /// more, while these were judged not worth scoring. Reported at all because
    /// `L-C5` bans silent truncation, and a bound that vanishes from the report
    /// is exactly that.
    pub candidates_cut: usize,
    /// How many candidates were refused for carrying **no query-conditional
    /// evidence** — nothing tied them to this query beyond existing and
    /// ranking somewhere (`require_evidence`, #2289).
    ///
    /// Reported apart from both [`Self::dropped`] (a budget drop is reversible
    /// by asking for more) and [`Self::candidates_cut`] (a rank cut is about
    /// shortlist size): this count says the gate judged them unrelated, which
    /// no budget raise changes. Zero whenever the gate is off. `L-C5`: a gate
    /// that silently vanished candidates would be exactly the truncation that
    /// principle bans.
    pub no_evidence_cut: usize,
    /// Whether the IVF index served the similarity scan instead of the exact
    /// one.
    ///
    /// Additive and explicit, following the [`Self::considered`] precedent, and
    /// for the same reason: the alternative is a caller *inferring* it from a
    /// settings file plus a build watermark plus a drift threshold, three facts
    /// that can each be true while the probe still declined. This is the only
    /// place a recall says which scan actually ran.
    ///
    /// `false` on a store with the setting on but no index, a stale index, or a
    /// lexical-fallback turn — in every one of those the exact scan ran and the
    /// result is what an unindexed store would have returned.
    pub used_ann_index: bool,
    /// How many centroid posting lists the probe read, and how many exist:
    /// `(probed, total)`. `(0, 0)` when the exact scan ran.
    ///
    /// The ratio is the honest summary of how approximate this recall was — 8
    /// of 100 says far more about what the ranking did *not* look at than a bare
    /// "approximate: true", and it is the number to raise `ann_probes` against
    /// if a frame that should have been recalled was not.
    pub ann_probes: (usize, usize),
    /// How many vectors the similarity pass cosine-scored — the probe's
    /// candidate count when [`Self::used_ann_index`], the whole live corpus
    /// under the fingerprint when it is not.
    ///
    /// This is the denominator behind the acceleration claim, so it is reported
    /// rather than asserted in prose: a caller comparing it to their memory
    /// count can see exactly what fraction of the corpus was considered.
    pub vectors_scored: usize,
}

impl RecallResult {
    /// Total token cost of the assembled frames — must never exceed the
    /// query's `max_tokens` (the invariant the packer guarantees).
    #[must_use]
    pub fn assembled_tokens(&self) -> u64 {
        self.frames.iter().map(|f| f.token_cost as u64).sum()
    }
}

/// The CGP wire shape of a recall — the drop report survives as
/// `truncated`/`dropped_estimate`, so adapting a recall to the provider seam
/// never silently discards it (`L-C5`).
impl From<RecallResult> for ContextQueryResult {
    fn from(result: RecallResult) -> Self {
        ContextQueryResult {
            truncated: !result.dropped.is_empty(),
            dropped_estimate: u32::try_from(result.dropped.len()).ok(),
            frames: result.frames,
        }
    }
}
