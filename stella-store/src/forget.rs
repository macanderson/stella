//! Reversible suppression of context that steers the agent — "forget".
//!
//! Every surface that feeds the model (memories, rules, skills, agents,
//! commands) can acquire an entry the user wants gone. Until now the only
//! removal path for a memory was citing it untruthful twice to trip
//! [`crate::QUARANTINE_NEGATIVES_THRESHOLD`], which is an odd thing to ask of
//! someone looking straight at the offending text.
//!
//! ## Why a tombstone and not a `DELETE`
//!
//! Deleting the row does not hold, because the reflection loop re-learns.
//! Observed in this project's own store: two memories recorded two days
//! apart, both naming a test file that had already been deleted —
//!
//! ```text
//! id 51  "In stella-cli witness tests (slash_models_witness.rs), prefer
//!         updating assertions to match the live renderer output … rather
//!         than changing core CLI behavior."
//! id 53  "In stella-cli witness tests (slash_models_witness.rs), prefer
//!         updating assertions to match the live renderer output … rather
//!         than changing the renderer."
//! ```
//!
//! Node inserts do not dedupe on `content_hash`, and `auto_create_skills`
//! mines the whole reflection log into skills that re-enter context through
//! the skills half of the recall block. So a delete leaves two doors open:
//! the miner writes the lesson back, or promotes it to a skill.
//!
//! A tombstone closes both. It records what was forgotten and is consulted
//! at three points — recall, the reflection recorder, and the skill miner —
//! so a forgotten lesson stays forgotten without being unrecoverable.
//!
//! ## Why lexical similarity, not equality
//!
//! The two memories above are not byte-identical, so a `content_hash` match
//! would not have caught the second one. Measured over this project's real
//! memory corpus, token-set Jaccard separates the cases with a wide margin:
//!
//! ```text
//! 51 vs 53   0.641   the same lesson, re-learned
//! 51 vs 55   0.058   |
//! 51 vs 56   0.020   |  every unrelated pair
//! 53 vs 57   0.095   |
//! ```
//!
//! Roughly a sevenfold gap between signal and noise, which is why
//! [`SIMILARITY_THRESHOLD`] sits at 0.5: far above the observed noise floor
//! and far below the observed re-learning score. Cheap, deterministic, and
//! needs no embedder on the record path — the alternative (embedding every
//! candidate lesson to compare against tombstones) buys nothing here and
//! puts a model call in the way of finishing a turn.

use std::collections::{BTreeSet, HashSet};

/// Token-set overlap above which a candidate counts as a restatement of
/// something already forgotten. See the module docs for the measurements
/// behind this number; it is a constant rather than a literal so the
/// threshold can be tuned against a larger corpus without hunting call
/// sites.
pub const SIMILARITY_THRESHOLD: f64 = 0.5;

/// A context surface a tombstone can apply to. The agent is steered by more
/// than memories, and "forget this" should mean the same thing whichever
/// surface the user is looking at, so the tombstone is keyed by surface plus
/// that surface's own stable id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContextSurface {
    /// A mined reflection in `context.db`, keyed by its `nod_…` public id.
    /// Reaches the model through the volatile recall block.
    Memory,
    /// A hand-written note at `.stella/memories/<name>.md`, keyed by
    /// filename stem. Distinct from [`Self::Memory`]: these are authored,
    /// not mined, and are pasted into the *system prompt* rather than
    /// recalled — today with no filter of any kind in front of them.
    WorkspaceMemory,
    /// A project rule at `.stella/rules/<slug>.md`, keyed by rule id. Steers
    /// twice — the rules section of the system prompt, and the tool-boundary
    /// guards — which must never disagree, so one filter covers both.
    Rule,
    /// An installed skill, by name. Reaches the model three ways: the recall
    /// block, explicit `/skill-name` invocation, and `skill_search` results.
    Skill,
    /// A subagent definition at `.stella/agents/<name>.md`, by name.
    Agent,
    /// A slash command at `.stella/commands/<name>.md`, by name.
    Command,
    /// A domain in `.stella/domains.toml`, by name. Steers indirectly: it is
    /// both text in `project_overview` output and the selection signal that
    /// decides which skills and memories are recalled at all.
    Domain,
}

impl ContextSurface {
    /// The wire/storage spelling. Stable — it is a primary-key component.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::WorkspaceMemory => "workspace-memory",
            Self::Rule => "rule",
            Self::Skill => "skill",
            Self::Agent => "agent",
            Self::Command => "command",
            Self::Domain => "domain",
        }
    }

    /// Parse the storage spelling back, for rows read out of SQLite and for
    /// the CLI's surface argument.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "memory" => Some(Self::Memory),
            "workspace-memory" => Some(Self::WorkspaceMemory),
            "rule" => Some(Self::Rule),
            "skill" => Some(Self::Skill),
            "agent" => Some(Self::Agent),
            "command" => Some(Self::Command),
            "domain" => Some(Self::Domain),
            _ => None,
        }
    }

    /// Every surface, for enumeration in the CLI and the deck's browser.
    pub fn all() -> [Self; 7] {
        [
            Self::Memory,
            Self::WorkspaceMemory,
            Self::Rule,
            Self::Skill,
            Self::Agent,
            Self::Command,
            Self::Domain,
        ]
    }

    /// Whether a tombstone on this surface should also suppress *restatements*
    /// of the forgotten text, not just the exact id.
    ///
    /// True only for the surfaces a background loop can regenerate on its own:
    /// mined reflections, and the skills `auto_create_skills` mines out of the
    /// reflection log. The rest are authored by a human — re-creating a rule
    /// by hand is a deliberate act, and silently swallowing it because it
    /// resembles something forgotten months ago would be its own bug.
    pub fn suppresses_restatements(&self) -> bool {
        matches!(self, Self::Memory | Self::Skill)
    }

    /// Every surface [`Self::suppresses_restatements`] answers `true` for.
    ///
    /// The enumeration lives next to the predicate so a tombstone sweep cannot
    /// drift from it: callers used to name `Memory` and `Skill` inline, which
    /// meant a newly regenerable surface would have been silently skipped by
    /// the sweep while the predicate said it should not be.
    pub fn restatement_suppressing() -> Vec<Self> {
        Self::all()
            .into_iter()
            .filter(|s| s.suppresses_restatements())
            .collect()
    }
}

/// Comparison tokens: lowercased alphanumeric runs, keeping the characters
/// that make identifiers and paths one token (`slash_models_witness.rs`
/// must not shatter into `slash`/`models`/`witness`/`rs`, or two lessons
/// about different files would look alike).
fn tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '/')))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Jaccard similarity of two already-tokenized texts. Split out so
/// [`is_suppressed`] can tokenize the candidate ONCE and reuse it across every
/// tombstone, instead of re-splitting and re-lowercasing it per comparison —
/// the miner asks this question for every candidate lesson against the whole
/// tombstone set, on the path that finishes a turn.
fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    intersection / union
}

/// Jaccard similarity of two texts' token sets, in `0.0..=1.0`. Two empty
/// texts are 0.0, not 1.0 — an empty lesson is not a restatement of
/// anything, and returning 1.0 would let one empty tombstone suppress every
/// future empty-ish candidate.
pub fn similarity(a: &str, b: &str) -> f64 {
    jaccard(&tokens(a), &tokens(b))
}

/// Is `candidate` close enough to `forgotten` to be the same lesson wearing
/// different words?
pub fn is_restatement(candidate: &str, forgotten: &str) -> bool {
    similarity(candidate, forgotten) >= SIMILARITY_THRESHOLD
}

/// Does `candidate` restate anything in `forgotten`? The question the
/// reflection recorder and the skill miner both ask before persisting.
pub fn is_suppressed<'a>(candidate: &str, forgotten: impl IntoIterator<Item = &'a str>) -> bool {
    let candidate = tokens(candidate);
    forgotten
        .into_iter()
        .any(|f| jaccard(&candidate, &tokens(f)) >= SIMILARITY_THRESHOLD)
}

/// One surface's suppression state, read once and reusable — the filter every
/// surface applies, with that surface's own policy already resolved.
///
/// Suppression has always been two predicates: an exact id match against the
/// tombstoned ids, and a restatement match against the tombstoned *texts*. They
/// were applied at different places by different callers, each spelling out the
/// half it happened to need, and one surface — the workspace memory files baked
/// into the system prompt — applied neither, so forgetting one did not stop it
/// shipping (#712 deliverable 6).
///
/// Bundling them here is what makes "the same filter" a true statement rather
/// than an aspiration. The restatement half is governed by
/// [`ContextSurface::suppresses_restatements`], so an authored surface keeps
/// its id-only policy by construction: re-writing a rule by hand is a
/// deliberate act, and swallowing it because it resembles something forgotten
/// months ago would be its own bug.
#[derive(Debug, Clone)]
pub struct SurfaceSuppression {
    ids: HashSet<String>,
    texts: Vec<String>,
    match_restatements: bool,
}

impl SurfaceSuppression {
    /// Build from a surface's tombstoned ids and texts.
    #[must_use]
    pub fn new(surface: ContextSurface, ids: HashSet<String>, texts: Vec<String>) -> Self {
        Self {
            ids,
            texts,
            match_restatements: surface.suppresses_restatements(),
        }
    }

    /// A filter that suppresses nothing — the honest shape for "this surface
    /// has no tombstones", distinct from "the state could not be read", which
    /// callers must fail closed on instead of substituting this.
    #[must_use]
    pub fn none() -> Self {
        Self {
            ids: HashSet::new(),
            texts: Vec::new(),
            match_restatements: false,
        }
    }

    /// Whether anything is suppressed at all — lets a caller skip the work
    /// entirely on the overwhelmingly common empty case.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty() && self.texts.is_empty()
    }

    /// Is this item suppressed, by id or (where the surface allows it) by
    /// restatement?
    #[must_use]
    pub fn suppresses(&self, id: &str, text: &str) -> bool {
        if self.ids.contains(id) {
            return true;
        }
        self.match_restatements && is_suppressed(text, self.texts.iter().map(String::as_str))
    }
}

impl crate::Store {
    /// This surface's suppression state, both halves read together.
    ///
    /// The pair has to come from one call: reading ids and texts separately
    /// leaves a window where a concurrent forget lands between them, and a
    /// caller that reads only the half it remembers is how the workspace-memory
    /// surface ended up with no filter at all (#712 deliverable 6).
    ///
    /// A read failure is an error, never an empty set. Callers must fail
    /// closed — surfacing everything because the suppression state was
    /// unreadable is the one outcome that is definitely wrong.
    pub fn suppression_for(
        &self,
        surface: crate::ContextSurface,
    ) -> crate::Result<SurfaceSuppression> {
        Ok(SurfaceSuppression::new(
            surface,
            self.forgotten_ids(surface)?,
            self.forgotten_texts(surface)?,
        ))
    }
}

#[cfg(test)]
mod tests;
