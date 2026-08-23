//! The operator's decision system, declared rather than inferred.
//!
//! `doc:backlog-self-driving` §6.5. Self-driving stands in for a person, and a
//! person deciding what to do next is not running a universal algorithm — they
//! are applying a system they developed, which differs from the next person's.
//! One maintainer's standing instruction is *"when two designs both work, ship
//! the one still correct in five years"*; another's is *"ship the smallest diff
//! that closes the ticket"*. **Both are legitimate, and a loop that hardcodes
//! either is a loop that can only replace one of them.**
//!
//! So the tie-breakers are configuration, and this module is their vocabulary.
//!
//! # What belongs here, and what deliberately does not
//!
//! This repository already carries **four steering planes that do not know
//! about each other** (#3243), and the fastest way to make that five is to add
//! a free-text "how should I decide things" field to a manifest. So the line is
//! drawn at machine-readability:
//!
//! - **Enumerated tie-breakers live here.** Every field is a closed enum or a
//!   number, consumed by the pure machines ([`step()`](crate::step()),
//!   [`deliver_next`](crate::deliver_next)). No model reads this type. That is
//!   what makes an
//!   operator's declared preference *testable* rather than hopeful — you can
//!   assert that `ForeignBreakage::FileAndWait` does not adopt, and the
//!   assertion cannot be talked out of.
//! - **Prose doctrine does not.** "Prefer the architecturally sound option",
//!   "match the neighbourhood", "no new dependencies casually" — that is
//!   instruction for the judgement half, it already has a home in the context
//!   record plane (`.stella/rules/*.toml`, `doc:context-pr`),
//!   and duplicating it here would create the fifth plane. A
//!   `[self_driving.doctrine]` block that wanted to say something prose-shaped
//!   is a context record, and this module's answer to that request is a
//!   pointer, not a field.
//!
//! The test for whether something belongs here is the same one invariant 5
//! applies to error types: **does a caller branch on it?** A pure machine
//! branching on a closed enum belongs; a sentence a model reads does not.

use serde::{Deserialize, Serialize};

/// What to do when the base branch is broken and this loop did not break it.
///
/// The interesting case, and the one an operator will genuinely disagree about.
/// A red `main` blocks *everyone*, so there is a real argument for fixing it
/// whoever broke it — and an equally real argument that a loop wandering into
/// unrelated code is how an autonomous system becomes a liability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForeignBreakage {
    /// File it and wait for whoever owns it.
    ///
    /// The conservative reading: the loop makes the breakage *visible*, which
    /// is the part nobody disputes, and leaves the fix to a human.
    FileAndWait,
    /// File it, then fix it at the top of the queue.
    ///
    /// A red base blocks this loop as surely as it blocks its author, and a
    /// loop that waits for someone else is a loop whose throughput is somebody
    /// else's response time. Filing still happens first and separately — the
    /// ticket is what makes the work attributable even if the fix fails.
    FileAndAdopt,
    /// Neither file nor fix.
    ///
    /// Here for completeness and for the operator who wants a silent loop, and
    /// deliberately **not** the default: an autonomous system that notices
    /// breakage and says nothing is the worst of the three.
    Ignore,
}

/// Whether the loop may act on a subject somebody else appears to be on.
///
/// The distinction matters because contention evidence is *heuristic* — a
/// remote branch whose name resembles the work is a strong hint and not a
/// proof — and how much weight to give a hint is exactly the kind of judgement
/// that differs between operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentionPolicy {
    /// Any sign of another actor on this subject defers.
    ///
    /// The default. A duplicated fix costs two workers' time and produces a
    /// merge conflict; a deferral costs a poll interval **only while the
    /// evidence can go away on its own**, which is what makes the default
    /// safe and is a claim about the evidence rather than about deferring.
    ///
    /// It is false for evidence the loop produced itself and nothing else
    /// clears: a crashed run's leftover worktree carries the issue key
    /// forever, and because a deferral writes no `spent` entry it is re-read
    /// on every pass, so the issue costs not a poll interval but the rest of
    /// the run (#4300). The claim-time probe therefore excludes the loop's own
    /// worktrees root before this policy ever sees it, leaving the recovery in
    /// `work::start` reachable — the fix belongs at the gathering site because
    /// weighing evidence is this enum's job and deciding what counts as
    /// evidence is not.
    ///
    /// That exclusion cannot be selective, because at claim time a **live
    /// peer** self-driving process against the same clone leaves an
    /// indistinguishable worktree under the same root. What keeps a live peer
    /// deferring is therefore not the worktree at all but
    /// [`Contention::ledger_claims`]: the loop holds a fenced lease on its
    /// issue for as long as a turn is in flight, so the peer that is *still
    /// running* is still evidence, and the run that died stops being evidence
    /// when its lease lapses. Expiry is the difference the path could not
    /// express, and it is the reason both halves of #4300 hold at once.
    Defer,
    /// Only a held ledger claim defers; branches and worktrees are advisory.
    ///
    /// For a solo operator whose remote is full of their own stale branches,
    /// where deferring on branch names would deadlock the loop against itself.
    ///
    /// It is the strictest policy that still separates a live peer from the
    /// loop's own wreckage, because a lease is the one signal here that
    /// carries a *time* — which is what makes it the remedy #4300's note
    /// points at rather than a way of turning contention off.
    ClaimsOnly,
    /// Proceed regardless. Recorded, never silent.
    Proceed,
}

/// The operator's declared decision system.
///
/// Declared in `[self_driving.doctrine]` in `stella.toml`, source-tracked with
/// the rest of the workspace's configuration: **the person who starts the loop
/// is the person who decides how it decides**, and that is only true if what it
/// will do is legible before it does it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Doctrine {
    /// What to do about breakage the loop did not cause.
    pub foreign_breakage: ForeignBreakage,
    /// How much weight another actor's apparent work carries.
    pub contention: ContentionPolicy,
    /// Whether an escalated PR parks the loop or is abandoned so it can carry
    /// on. Parking is the default: a PR a human must look at is usually a sign
    /// the loop should stop making more of them.
    pub abandon_escalated: bool,
}

impl Default for Doctrine {
    /// Fix a broken base; defer to anyone already fixing it; park on an
    /// escalation.
    ///
    /// # Why adopting is the default and waiting is the opt-in
    ///
    /// A red base blocks this loop exactly as hard as it blocks its author, so
    /// a loop that files and waits has handed its own throughput to somebody
    /// else's response time — and in the meantime every pull request it opens
    /// fails for a reason no diff of its caused. That is not caution, it is a
    /// loop that stops working the moment the repository needs it most.
    ///
    /// This default was [`ForeignBreakage::FileAndWait`] on the reasoning that
    /// the aggressive choice should be opted into. The reasoning was wrong
    /// about which choice is aggressive: **filing a ticket about a fire and
    /// walking away is the surprising behaviour**, not putting it out. An
    /// operator who genuinely wants a loop that only reports still has
    /// `FileAndWait`, one line of `stella.toml` away.
    ///
    /// The other two axes stay conservative, and for a reason that does not
    /// apply to the first: deferring on contention and parking on an
    /// escalation both avoid *duplicating or overriding another actor*, which
    /// is a different question from whether to act at all.
    fn default() -> Self {
        Self {
            foreign_breakage: ForeignBreakage::FileAndAdopt,
            contention: ContentionPolicy::Defer,
            abandon_escalated: false,
        }
    }
}

/// Evidence that somebody else may already be on this subject.
///
/// Gathered by the caller — the loop reads the forge and the local machine —
/// and reduced to a decision here. Every field is a **hint**, and naming them
/// separately is what lets [`ContentionPolicy`] weigh them differently.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contention {
    /// Remote branches whose names indicate work on this subject.
    #[serde(default)]
    pub remote_branches: Vec<String>,
    /// Open pull requests that appear to address it.
    #[serde(default)]
    pub open_prs: Vec<String>,
    /// Worktrees on **this machine** that appear to hold a fix in flight.
    ///
    /// The one nobody remembers to check, and the one most likely to be the
    /// loop's own other session: two self-driving processes against one clone
    /// will each see the other's worktree here.
    #[serde(default)]
    pub local_worktrees: Vec<String>,
    /// Ledger claims held by another worker.
    ///
    /// The only *authoritative* signal — a lease with an owner and an expiry,
    /// rather than a name that resembles the work.
    #[serde(default)]
    pub ledger_claims: Vec<String>,
}

impl Contention {
    /// Whether anything at all suggests another actor.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.remote_branches.is_empty()
            && self.open_prs.is_empty()
            && self.local_worktrees.is_empty()
            && self.ledger_claims.is_empty()
    }
}

/// Whether the loop may proceed on a contended subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "verdict")]
pub enum ContentionVerdict {
    /// Nothing suggests another actor, or policy says to proceed anyway.
    Proceed,
    /// Somebody else appears to be on it. Carries what was seen, because
    /// "deferred" with no evidence is not reviewable.
    Defer {
        /// The signals that caused the deferral, for the ledger.
        evidence: Vec<String>,
    },
}

/// Decide whether to act on a subject given what else appears to be in flight.
///
/// Pure and total. The evidence is heuristic and the policy says how much that
/// matters — which is the split that keeps a judgement call out of the
/// mechanism.
#[must_use]
pub fn contention_verdict(policy: ContentionPolicy, seen: &Contention) -> ContentionVerdict {
    let evidence: Vec<String> = match policy {
        ContentionPolicy::Proceed => return ContentionVerdict::Proceed,
        // Only the authoritative signal counts.
        ContentionPolicy::ClaimsOnly => seen
            .ledger_claims
            .iter()
            .map(|c| format!("ledger claim: {c}"))
            .collect(),
        ContentionPolicy::Defer => seen
            .ledger_claims
            .iter()
            .map(|c| format!("ledger claim: {c}"))
            .chain(seen.open_prs.iter().map(|p| format!("open pr: {p}")))
            .chain(
                seen.remote_branches
                    .iter()
                    .map(|b| format!("remote branch: {b}")),
            )
            .chain(
                seen.local_worktrees
                    .iter()
                    .map(|w| format!("local worktree: {w}")),
            )
            .collect(),
    };

    if evidence.is_empty() {
        ContentionVerdict::Proceed
    } else {
        ContentionVerdict::Defer { evidence }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default fixes a broken base rather than reporting it and stopping.
    ///
    /// This asserted `FileAndWait` until it was put in front of an operator,
    /// whose response was that fixing a repository somebody broke is the
    /// entire point. The reversal is deliberate and the reasoning is on
    /// [`Doctrine::default`]: filing a ticket about a fire and walking away is
    /// the surprising behaviour, not putting it out.
    ///
    /// The other two axes stay conservative, and this pins that too — they
    /// answer a different question (*is another actor already on it*), and
    /// moving them would be a separate argument nobody has made.
    #[test]
    fn the_default_fixes_a_broken_base_rather_than_only_reporting_it() {
        let d = Doctrine::default();
        assert_eq!(d.foreign_breakage, ForeignBreakage::FileAndAdopt);
        assert_eq!(d.contention, ContentionPolicy::Defer);
        assert!(!d.abandon_escalated);
    }

    /// ...and the spec says the same thing, on the one axis where saying the
    /// other thing changes what the loop does to a red base.
    ///
    /// `doc:backlog-self-driving` §6.5 marked `file_and_wait` *(default)* for as
    /// long as this crate shipped [`ForeignBreakage::FileAndAdopt`], so a reader
    /// of the spec expected a loop that reports a base somebody else broke and
    /// stops. The default is the operator-visible contract, and it is stated in
    /// two places; this is the assertion that keeps them one answer.
    ///
    /// The predicate reads only the bullets naming a `foreign_breakage`
    /// spelling and asks which carries the `*(default)*` marker. That marker
    /// sits immediately after the code span, so it stays on the bullet's first
    /// line however the prose after it is reflowed.
    #[test]
    fn doctrine_default_matches_the_spec() {
        let spec = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("docs/spec/backlog-self-driving.md");
        let text = std::fs::read_to_string(&spec)
            .unwrap_or_else(|e| panic!("read {}: {e}", spec.display()));

        let marked: Vec<&str> = ["file_and_wait", "file_and_adopt", "ignore"]
            .into_iter()
            .filter(|spelling| {
                let bullet = format!("- `{spelling}`");
                text.lines().any(|line| {
                    line.trim_start().starts_with(&bullet) && line.contains("*(default)*")
                })
            })
            .collect();

        let shipped = serde_json::to_value(Doctrine::default().foreign_breakage)
            .expect("serialize the default");
        let shipped = shipped.as_str().expect("a snake_case string");

        assert_eq!(
            marked,
            vec![shipped],
            "exactly one `foreign_breakage` bullet in §6.5 is marked *(default)*, \
             and it is the one `Doctrine::default()` ships"
        );
    }

    /// The witness for collision avoidance: a worktree on this machine defers
    /// the loop, and it is the signal nobody remembers to check — two
    /// self-driving sessions against one clone see each other only here.
    #[test]
    fn a_local_worktree_alone_is_enough_to_defer() {
        let seen = Contention {
            local_worktrees: vec!["fix-3912-design-refs".into()],
            ..Contention::default()
        };

        assert_eq!(
            contention_verdict(ContentionPolicy::Defer, &seen),
            ContentionVerdict::Defer {
                evidence: vec!["local worktree: fix-3912-design-refs".into()],
            },
            "a fix already in flight on this machine must not be duplicated"
        );
    }

    /// ...and the same evidence proceeds under `ClaimsOnly`, which is the whole
    /// reason the policy exists: a solo operator's remote is full of their own
    /// stale branches, and deferring on those deadlocks the loop against itself.
    #[test]
    fn claims_only_ignores_branches_and_worktrees_but_not_a_lease() {
        let hints = Contention {
            remote_branches: vec!["old-thing".into()],
            local_worktrees: vec!["stale".into()],
            open_prs: vec!["1".into()],
            ..Contention::default()
        };
        assert_eq!(
            contention_verdict(ContentionPolicy::ClaimsOnly, &hints),
            ContentionVerdict::Proceed
        );

        let leased = Contention {
            ledger_claims: vec!["3599@worker-2".into()],
            ..hints
        };
        assert!(matches!(
            contention_verdict(ContentionPolicy::ClaimsOnly, &leased),
            ContentionVerdict::Defer { .. }
        ));
    }

    /// A deferral with no evidence is not reviewable, so the evidence rides in
    /// the verdict rather than being logged and lost.
    #[test]
    fn a_deferral_names_every_signal_that_caused_it() {
        let seen = Contention {
            remote_branches: vec!["b".into()],
            open_prs: vec!["12".into()],
            local_worktrees: vec!["w".into()],
            ledger_claims: vec!["c".into()],
        };

        let ContentionVerdict::Defer { evidence } =
            contention_verdict(ContentionPolicy::Defer, &seen)
        else {
            panic!("expected a deferral");
        };

        assert_eq!(evidence.len(), 4, "{evidence:?}");
        // The authoritative signal is named first.
        assert!(evidence[0].starts_with("ledger claim:"));
    }

    #[test]
    fn no_evidence_proceeds_under_every_policy() {
        for policy in [
            ContentionPolicy::Defer,
            ContentionPolicy::ClaimsOnly,
            ContentionPolicy::Proceed,
        ] {
            assert_eq!(
                contention_verdict(policy, &Contention::default()),
                ContentionVerdict::Proceed,
                "{policy:?}"
            );
        }
    }

    /// `Proceed` overrides even an authoritative claim — recorded, never
    /// silent, which is why the verdict is a value rather than a bool.
    #[test]
    fn proceed_overrides_even_a_held_lease() {
        let leased = Contention {
            ledger_claims: vec!["3599@worker-2".into()],
            ..Contention::default()
        };
        assert_eq!(
            contention_verdict(ContentionPolicy::Proceed, &leased),
            ContentionVerdict::Proceed
        );
    }

    #[test]
    fn a_doctrine_round_trips_through_json() {
        let d = Doctrine {
            foreign_breakage: ForeignBreakage::FileAndAdopt,
            contention: ContentionPolicy::ClaimsOnly,
            abandon_escalated: true,
        };
        let json = serde_json::to_string(&d).expect("serialize");
        let back: Doctrine = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(d, back);
    }

    /// An operator who writes a partial manifest gets the conservative default
    /// for everything they did not mention, rather than a parse error.
    #[test]
    fn a_partial_manifest_fills_in_conservative_defaults() {
        let d: Doctrine =
            serde_json::from_str(r#"{"foreign_breakage":"file_and_adopt"}"#).expect("deserialize");
        assert_eq!(d.foreign_breakage, ForeignBreakage::FileAndAdopt);
        assert_eq!(d.contention, ContentionPolicy::Defer);
    }
}
