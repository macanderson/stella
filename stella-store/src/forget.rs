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

use std::collections::BTreeSet;

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

/// Jaccard similarity of two texts' token sets, in `0.0..=1.0`. Two empty
/// texts are 0.0, not 1.0 — an empty lesson is not a restatement of
/// anything, and returning 1.0 would let one empty tombstone suppress every
/// future empty-ish candidate.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (tokens(a), tokens(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(&b).count() as f64;
    let union = a.union(&b).count() as f64;
    intersection / union
}

/// Is `candidate` close enough to `forgotten` to be the same lesson wearing
/// different words?
pub fn is_restatement(candidate: &str, forgotten: &str) -> bool {
    similarity(candidate, forgotten) >= SIMILARITY_THRESHOLD
}

/// Does `candidate` restate anything in `forgotten`? The question the
/// reflection recorder and the skill miner both ask before persisting.
pub fn is_suppressed<'a>(candidate: &str, forgotten: impl IntoIterator<Item = &'a str>) -> bool {
    forgotten.into_iter().any(|f| is_restatement(candidate, f))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two real memories from this project's store, verbatim. They are
    /// the reason this module exists: recorded two days apart, the same
    /// lesson about a file that no longer existed.
    const RELEARNED_A: &str = "In stella-cli witness tests (slash_models_witness.rs), prefer \
         updating assertions to match the live renderer output (abbreviated column headers like \
         'in'/'out'/'cached-i', actual seeded numeric formatting, and observed strings like \
         'thinks') rather than changing core CLI behavior.";
    const RELEARNED_B: &str = "In stella-cli witness tests (slash_models_witness.rs), prefer \
         updating assertions to match the live renderer output (abbreviated column headers like \
         'in'/'out'/'cached-i', actual seed values, etc.) rather than changing the renderer.";
    const UNRELATED: &str = "For cargo test filters like --exact, always place them after the -- \
         separator (cargo test -- --exact) to pass args to the test binary instead of Cargo.";

    #[test]
    fn a_relearned_lesson_is_recognized_as_a_restatement() {
        assert!(
            is_restatement(RELEARNED_B, RELEARNED_A),
            "similarity was {}",
            similarity(RELEARNED_B, RELEARNED_A)
        );
    }

    #[test]
    fn unrelated_lessons_are_not_restatements() {
        assert!(!is_restatement(UNRELATED, RELEARNED_A));
        assert!(!is_restatement(RELEARNED_A, UNRELATED));
    }

    /// The threshold is only defensible if signal and noise are far apart —
    /// pin the margin so a future tweak that collapses it fails loudly.
    #[test]
    fn signal_and_noise_stay_far_apart() {
        let signal = similarity(RELEARNED_A, RELEARNED_B);
        let noise = similarity(RELEARNED_A, UNRELATED);
        assert!(signal > 0.5, "re-learned pair scored {signal}");
        assert!(noise < 0.2, "unrelated pair scored {noise}");
        assert!(
            signal > noise * 3.0,
            "margin collapsed: signal {signal}, noise {noise}"
        );
    }

    #[test]
    fn identifiers_and_paths_stay_whole() {
        // Two lessons about *different* files must not look alike just
        // because they share sentence scaffolding.
        let about_a = "In witness tests (alpha_witness.rs), prefer updating assertions.";
        let about_b = "In witness tests (beta_witness.rs), prefer updating assertions.";
        assert!(tokens(about_a).contains("alpha_witness.rs"));
        // The filename stayed whole, so its parts are not tokens of their own.
        assert!(!tokens(about_a).contains("alpha"));
        assert!(
            similarity(about_a, about_b) < 1.0,
            "the differing filename must register"
        );
    }

    #[test]
    fn empty_text_matches_nothing() {
        assert_eq!(similarity("", ""), 0.0);
        assert_eq!(similarity("", RELEARNED_A), 0.0);
        assert!(!is_restatement("", ""));
    }

    #[test]
    fn suppression_scans_every_tombstone() {
        let forgotten = [UNRELATED, RELEARNED_A];
        assert!(is_suppressed(RELEARNED_B, forgotten));
        assert!(!is_suppressed(
            "An entirely new observation about graph queries.",
            forgotten
        ));
    }

    #[test]
    fn surface_spellings_round_trip() {
        for surface in ContextSurface::all() {
            assert_eq!(ContextSurface::parse(surface.as_str()), Some(surface));
        }
        assert_eq!(ContextSurface::parse("nonsense"), None);
    }
}
