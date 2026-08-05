//! What this session believes a file holds, and how much of it the agent was
//! actually shown.
//!
//! One entry of [`ToolRegistry::observed`](super::ToolRegistry) — the whole of
//! the no-clobber guarantee. This module owns what a belief is, the form it
//! persists in, the rule for updating one
//! ([`ToolRegistry::remember_observed`](super::ToolRegistry::remember_observed))
//! and the rule for acting on one
//! ([`ToolRegistry::refused_mutation`](super::ToolRegistry::refused_mutation)).
//! The map itself and the digest comparison (`clobbered_paths`) stay on the
//! registry, next to the touch that triggers them.

use super::{FileOp, PendingTouch};

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

/// Why a pending mutation is refused before it is allowed to run.
///
/// Both arms make the same claim at different strengths — "this call would
/// destroy content the agent cannot account for" — and both are recoverable by
/// reading, so each carries the message that names its own way out.
pub(super) enum Refusal {
    /// The bytes moved: something outside this session edited these paths
    /// after the agent last looked at them.
    Drifted(Vec<String>),
    /// Nothing drifted, but the agent only ever saw part of this file and is
    /// about to replace the whole of it. What would be lost is not another
    /// agent's *edit* — it is every region the agent never read.
    Unseen(String),
}

impl Refusal {
    /// The model-facing refusal. Both arms name the file and the next action,
    /// because this is returned as a tool error the agent is expected to
    /// recover from in one step rather than as a failed turn.
    pub(super) fn message(&self) -> String {
        match self {
            Self::Drifted(paths) => format!(
                "refusing to write: {} changed since you last read {}. Another \
                 agent (or a person) edited it after you looked, and this call \
                 would overwrite their work with a plan formed against content \
                 that no longer exists.\n\nRe-read {} and redo the change \
                 against what is there now. Your edit is not lost — nothing was \
                 written.",
                paths.join(", "),
                if paths.len() == 1 { "it" } else { "them" },
                if paths.len() == 1 {
                    "the file"
                } else {
                    "those files"
                },
            ),
            Self::Unseen(path) => format!(
                "refusing to write: you have only seen part of {path}. Every \
                 read of it this session was windowed, clipped, or a single \
                 span, so replacing the whole file would overwrite regions that \
                 were never on your screen — including any work another agent \
                 (or a person) left in them.\n\nRe-read {path} in full and \
                 rewrite it against what is actually there, or use edit_file to \
                 change only the region you did see. Your edit is not lost — \
                 nothing was written.",
            ),
        }
    }
}

impl super::ToolRegistry {
    /// Whether this call must be refused before it runs, and why.
    ///
    /// Drift is checked first: when a file both drifted and is only partly
    /// seen, "someone else changed this" is the more specific fact and the one
    /// the agent has to act on.
    pub(super) fn refused_mutation(&self, tool: &str, pending: &[PendingTouch]) -> Option<Refusal> {
        let drifted = self.clobbered_paths(pending);
        if !drifted.is_empty() {
            return Some(Refusal::Drifted(drifted));
        }
        self.unseen_whole_file_rewrite(tool, pending)
            .map(Refusal::Unseen)
    }

    /// The path this call would replace in full while holding only a partial
    /// belief about it, if any.
    ///
    /// # Why this is not "refuse every mutation on a partial belief"
    ///
    /// What makes a partial belief dangerous is *whole-file replacement*, not
    /// mutation as such. `write_file` substitutes the entire contents, so every
    /// line the agent never read is destroyed whether or not it was the target
    /// — and unlike drift, no digest comparison can catch it, because the bytes
    /// the agent never saw are exactly the bytes it also never hashed a
    /// disagreement about.
    ///
    /// `edit_file` and `apply_edits` replace a matched region and leave the
    /// rest of the file byte-identical, so an unseen remainder is not at risk
    /// and refusing them would buy nothing. It would also cost everything:
    /// `read_file` windows at 2000 lines, so on any file above that ceiling
    /// EVERY belief is partial and always will be. Refusing edits there would
    /// make the largest files in a tree permanently unwritable — the deadlock
    /// this guard is specifically built to avoid, since being told to look
    /// again must cost a step, never the whole file.
    ///
    /// So the split is by what the tool destroys, not by how the belief was
    /// earned: whole-file replacement is refused, targeted edits proceed, and
    /// the agent's way back to a full rewrite is one honest whole-file read.
    fn unseen_whole_file_rewrite(&self, tool: &str, pending: &[PendingTouch]) -> Option<String> {
        // `web_download` lands a file exactly as `write_file` does — same
        // classification, same total substitution of the contents — so it is
        // the same hazard and takes the same rule.
        if !matches!(tool, "write_file" | "web_download") {
            return None;
        }
        let observed = self.observed.lock().unwrap_or_else(|p| p.into_inner());
        pending
            .iter()
            // A `Create` replaces nothing: there is no prior content to be
            // wrong about, and a path with no file has no belief either.
            .filter(|p| matches!(p.op, FileOp::Update))
            .find(|p| {
                observed
                    .get(&p.path)
                    .is_some_and(|held| held.coverage == Coverage::Partial)
            })
            .map(|p| p.path.clone())
    }

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
