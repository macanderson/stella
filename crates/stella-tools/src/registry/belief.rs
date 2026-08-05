//! What this session believes a file holds, and how much of it the agent was
//! actually shown.
//!
//! One entry of [`ToolRegistry::observed`](super::ToolRegistry) — the whole of
//! the no-clobber guarantee. This module owns what a belief is, the form it
//! persists in, and the rule for updating one
//! ([`ToolRegistry::remember_observed`](super::ToolRegistry::remember_observed)).
//! The map itself and the comparison a mutation is checked against
//! (`clobbered_paths`) stay on the registry, next to the touch that triggers
//! them.

/// How much of a file's content the agent actually saw when it formed the
/// belief.
///
/// The no-clobber guard needs three states, not two, and this is the third.
/// "No belief" and "a belief" alone force a choice between two failures:
/// letting a windowed read overwrite a whole-file belief (which launders a
/// concurrent edit into it), or letting no windowed read record anything
/// (which leaves the file unguarded). Recording *how* the belief was earned
/// keeps both closed — see
/// [`ToolRegistry::remember_observed`](super::ToolRegistry::remember_observed)
/// for the rule itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Coverage {
    /// Every byte was seen or written by this agent: a read that rendered the
    /// whole file unclipped and uncapped, or any create/update/delete, since
    /// the agent authored the content it just wrote.
    Whole,
    /// Only part of it was seen — a windowed `read_file`, a `read_symbol`
    /// span, or a read clipped by the per-line or payload cap.
    Partial,
}

/// What this session last observed a file to hold, and how much of that
/// content the agent was actually shown.
#[derive(Clone, Debug)]
pub(super) struct Belief {
    pub(super) sha256: String,
    pub(super) coverage: Coverage,
}

/// Marks a partial belief in the persisted staleness map.
///
/// A whole-file belief is written as a bare hex digest, exactly as before
/// coverage was tracked, so a snapshot of a session that only ever read whole
/// files is byte-identical to the one the previous version wrote — and an
/// older map restores as [`Coverage::Whole`], which is the truth for it: only
/// a whole-file read could establish belief in the version that wrote it.
const PARTIAL_BELIEF_PREFIX: &str = "partial:";

impl Belief {
    /// The persisted form: bare digest for a whole-file belief, prefixed for a
    /// partial one. See [`PARTIAL_BELIEF_PREFIX`] on why the whole-file case
    /// stays bare.
    pub(super) fn encode(&self) -> String {
        match self.coverage {
            Coverage::Whole => self.sha256.clone(),
            Coverage::Partial => format!("{PARTIAL_BELIEF_PREFIX}{}", self.sha256),
        }
    }

    /// Inverse of [`Self::encode`]. An unprefixed value is a whole-file belief
    /// — including every map written before partial beliefs were recorded at
    /// all, for which that is the correct reading.
    ///
    /// A map written by a NEWER version and restored here would read the
    /// prefix as part of the digest, which can only ever fail to match disk —
    /// so the guard over-refuses and asks for a re-read. That is the direction
    /// a no-clobber guard is allowed to be wrong in.
    pub(super) fn decode(encoded: &str) -> Self {
        match encoded.strip_prefix(PARTIAL_BELIEF_PREFIX) {
            Some(sha256) => Self {
                sha256: sha256.to_string(),
                coverage: Coverage::Partial,
            },
            None => Self {
                sha256: encoded.to_string(),
                coverage: Coverage::Whole,
            },
        }
    }
}

impl super::ToolRegistry {
    /// Record what this session now knows `path` to hold, hashed from disk,
    /// and how much of that content the agent was actually shown.
    ///
    /// Called for every successful touch, reads included: a read is how an
    /// agent acquires the belief a later write acts on, so it is exactly as
    /// load-bearing here as a write. A path that no longer exists drops its
    /// entry — after a delete there is nothing left to clobber, and the next
    /// agent to create the file is starting fresh rather than overwriting.
    ///
    /// # Why a partial read may establish but not overwrite
    ///
    /// This hashes what is on DISK, not what was rendered, so what a partial
    /// read may do to an existing belief is the whole question — and both
    /// extremes fail, in opposite directions:
    ///
    /// - **Always overwrite** launders a concurrent edit into belief. A reads
    ///   the file whole, B edits the tail, A peeks at the head and learns
    ///   nothing about B's edit — but the refreshed hash now says A has seen
    ///   B's content, so A's next write compares B's work against itself,
    ///   passes clean, and destroys it.
    /// - **Never record** leaves the file unguarded: `clobbered_paths` cannot
    ///   flag a path it has no entry for. Not the exotic case — `read_file`
    ///   windows at 2000 lines, so above that ceiling *every* read is partial
    ///   and none can ever be whole.
    ///
    /// So a partial read establishes a belief where there is none and
    /// refreshes one that was itself partial, but never overwrites a
    /// whole-file belief. That closes both holes without deadlocking the
    /// agent: a file holding a whole-file belief is small enough to be read
    /// whole again — the recovery the refusal asks for — while one too big for
    /// that only ever holds a partial belief, so the windowed reads that are
    /// all it can offer still refresh it.
    pub(super) fn remember_observed(&self, path: &str, coverage: Coverage) {
        let mut observed = self.observed.lock().unwrap_or_else(|p| p.into_inner());
        let Some(bytes) = crate::rootfd::read_confined_bytes(&self.root, path) else {
            observed.remove(path);
            return;
        };
        if coverage == Coverage::Partial
            && observed
                .get(path)
                .is_some_and(|held| held.coverage == Coverage::Whole)
        {
            return;
        }
        observed.insert(
            path.to_string(),
            Belief {
                sha256: crate::staleness::hex_sha256(&bytes),
                coverage,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whole-file belief must persist as a bare digest: the pre-coverage
    /// format, so a map written by the version before this one and a map
    /// written by this one are the same bytes for the same beliefs.
    #[test]
    fn a_whole_file_belief_persists_as_a_bare_digest() {
        let belief = Belief {
            sha256: "abc123".into(),
            coverage: Coverage::Whole,
        };
        assert_eq!(belief.encode(), "abc123");
    }

    /// And an old map — every value a bare digest — restores as whole-file,
    /// which is the truth for it.
    #[test]
    fn an_unprefixed_value_decodes_as_a_whole_file_belief() {
        let belief = Belief::decode("abc123");
        assert_eq!(belief.sha256, "abc123");
        assert_eq!(belief.coverage, Coverage::Whole);
    }

    #[test]
    fn a_partial_belief_round_trips_through_its_prefix() {
        let belief = Belief {
            sha256: "def456".into(),
            coverage: Coverage::Partial,
        };
        let decoded = Belief::decode(&belief.encode());
        assert_eq!(decoded.sha256, "def456");
        assert_eq!(
            decoded.coverage,
            Coverage::Partial,
            "a partial belief that decoded as whole-file would regain the \
             power to be laundered by the next windowed read"
        );
    }
}
