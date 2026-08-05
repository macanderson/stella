//! Turning a TOML context record into the thing the shipped engine enforces.
//!
//! `docs/spec/adaptive-context/context-pr.md` §11 maps the enforcement ladder onto a two-tier engine
//! that already exists: Tier 1 renders into the prompt, Tier 2 blocks at the tool
//! boundary through [`evaluate_guards`][super::super::rules::evaluate_guards]. TOML
//! records carry the same deny-shaped guard fields markdown rules do — and until
//! this module, nothing evaluated them. A `hard`-mode record was a comment.
//!
//! Rather than write a second enforcement path (two enforcement surfaces that can
//! disagree about what is armed is the failure mode this codebase already guards
//! against in `guards_to_deny`), a record becomes a [`Rule`] and rides the one
//! that ships.
//!
//! # Three preconditions, all required, none inferable
//!
//! A record blocks a tool call only when all three hold:
//!
//! 1. **It asked to.** `enforcement.mode = "hard"`. A `soft` or `none` record never
//!    blocks, no matter what guard fields it carries.
//! 2. **There is a real enforcer.** At least one concrete, evaluable guard value.
//!    A natural-language statement never silently becomes blocking behavior —
//!    "never touch production config" is a sentence, not a matcher, and a guard
//!    that matches nothing while advertising Tier 2 is worse than no guard: it
//!    reports enforcement that does not exist.
//! 3. **A human approved it.** Either the record is the user's own decree with a
//!    named verifier, or the decision ledger records an explicit approval.
//!
//! # Self-approval is a property of the tier, not of the fields
//!
//! `origin`, `truth.basis`, and `verified_by` are all written *inside the file being
//! judged*. In `~/.stella/rules` that is exactly right: the author, the approver, and
//! the person the guard will fire on are the same person, so writing the file **is**
//! the approval and demanding a second ceremony would make the solo path require a
//! ledger. In a checkout it proves nothing — a repository can assert `origin = "user"`
//! and `verified_by = "someone"` as easily as it can assert anything else, and the
//! person those three fields would be approving *on behalf of* has never seen them.
//!
//! So self-attestation is gated on [`Trust::User`]. A repository-published record
//! still reaches the tool boundary, but only through the decision ledger — an
//! explicit local act by the person the guard will actually fire on.
//!
//! And one absolute refusal: an `imported` or `inferred` record can never *start*
//! blocking. Content extracted from a document is a hypothesis about policy; the
//! model that split the prose may have inverted a negation. A hypothesis may
//! advise. It may not deny a tool call. Promoting one to blocking requires a human
//! re-authoring it under their own origin — which is a Context PR, reviewable like
//! any other.

use super::super::context_record::kind::Origin;
use super::super::ingest::record::{EnforcementMode, Record, TruthBasis};
use super::super::rules::{Rule, RuleGuard};
use super::{LoadedRecord, Trust};

/// Why a record that asked to block was not armed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockingRefusal {
    /// `mode = "hard"` with no concrete guard value to evaluate.
    NoEnforcer,
    /// The record's origin can never start blocking.
    UntrustedOrigin(Origin),
    /// No human has approved this record blocking tool calls.
    NotApproved,
    /// The record is published in the repository, so its own `verified_by` cannot
    /// stand in for an approval — only the local decision ledger can.
    ProjectNeedsApproval,
}

impl std::fmt::Display for BlockingRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoEnforcer => write!(
                f,
                "asks to block but carries no evaluable guard (guard_tool / \
                 guard_deny_path / guard_deny_command) — a statement is not a matcher, \
                 so it stays advisory"
            ),
            Self::UntrustedOrigin(origin) => write!(
                f,
                "asks to block but its origin is {} — an extracted or inferred claim \
                 can advise, never deny; re-author it under your own origin to enforce it",
                origin.as_str()
            ),
            Self::NotApproved => write!(
                f,
                "asks to block but no human has approved it blocking tool calls \
                 (needs an approval decision, or a decree with a named verifier)"
            ),
            Self::ProjectNeedsApproval => write!(
                f,
                "asks to block and is published in this repository, so its own \
                 verified_by cannot approve it — a repository record denies tool calls \
                 only after you approve it locally (`stella context review`)"
            ),
        }
    }
}

/// The outcome of asking whether a record may block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardDecision {
    /// The armed guard, when every precondition held.
    pub guard: Option<RuleGuard>,
    /// Why not, when the record asked to block and was refused. `None` when the
    /// record never asked.
    pub refusal: Option<BlockingRefusal>,
}

/// Whether this origin may ever start blocking. Extracted (`imported`) and mined
/// (`inferred`) content may not; `observed` may not either — it is a pattern
/// somebody noticed, not a policy somebody wrote.
fn may_block(origin: Option<Origin>) -> bool {
    match origin {
        // `system` is Stella's own built-in policy; `user` is the person at the
        // keyboard. Both are authored, not induced.
        Some(Origin::User | Origin::System) => true,
        Some(other) => !matches!(
            other,
            Origin::Imported | Origin::Inferred | Origin::Observed
        ),
        // An origin-less record has not said where it came from. Blocking is the
        // one decision that must not default to permissive.
        None => false,
    }
}

/// A record is self-approving when it is the user's own decree, verified by a named
/// human, **and it is the user's own record**. That is the hand-authored
/// `~/.stella/rules/*.toml` case: writing the file, naming yourself, and keeping it
/// on your own disk *is* the approval, and demanding a second ceremony for it would
/// make the local-first solo path require a ledger.
///
/// [`Trust::Project`] never qualifies, however impeccable the fields look — see the
/// module docs. A repository record's route to blocking is the decision ledger.
fn self_attested(record: &Record, trust: Trust) -> bool {
    if trust != Trust::User {
        return false;
    }
    let named_human = record.truth.as_ref().is_some_and(|truth| {
        truth.basis == TruthBasis::Decree
            && truth
                .verified_by
                .as_deref()
                .is_some_and(|by| !by.trim().is_empty())
    });
    matches!(record.origin, Some(Origin::User)) && named_human
}

/// Whether a guard's fields amount to something [`evaluate_guards`][ev] can act on.
///
/// [ev]: super::super::rules::evaluate_guards
fn concrete(guard: &RuleGuard) -> bool {
    guard.tool.is_some() || guard.deny_path_glob.is_some() || guard.deny_command_glob.is_some()
}

/// Read the record's guard fields, dropping blank ones.
///
/// A key present with an empty value is not a guard condition — the same rule the
/// markdown parser applies, for the same reason: `guard_tool = ""` made a rule
/// advertise Tier 2 that could never fire.
fn declared_guard(record: &Record) -> Option<RuleGuard> {
    let enforcement = record.enforcement.as_ref()?;
    let field = |value: &Option<String>| -> Option<String> {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let guard = RuleGuard {
        tool: field(&enforcement.guard_tool),
        deny_path_glob: field(&enforcement.guard_deny_path),
        deny_command_glob: field(&enforcement.guard_deny_command),
    };
    concrete(&guard).then_some(guard)
}

/// The guard this record *establishes*, independent of whether it is approved yet.
///
/// The precedence merge needs a stable answer to "does this record set up
/// enforcement" — stable meaning it cannot change when a ledger entry is added or a
/// probe runs. Approval decides whether a guard **fires**; this decides whether the
/// record is one a lower-trust record is allowed to displace.
pub fn declared_hard_guard(record: &Record) -> Option<RuleGuard> {
    record
        .enforcement
        .as_ref()
        .filter(|enforcement| enforcement.mode == EnforcementMode::Hard)?;
    declared_guard(record)
}

/// Decide whether `record` blocks at the tool boundary.
///
/// `trust` is stamped from the directory the record was read out of, never from the
/// record's own fields — it decides whether the record may approve itself.
/// `ledger_approved` is the caller's own fact: a recorded human approval for this
/// record's lineage *at this tier*. `stella-core` has no ledger, so the CLI supplies
/// it.
pub fn guard_for(record: &Record, trust: Trust, ledger_approved: bool) -> GuardDecision {
    let asks_to_block = record
        .enforcement
        .as_ref()
        .is_some_and(|enforcement| enforcement.mode == EnforcementMode::Hard);
    if !asks_to_block {
        return GuardDecision {
            guard: None,
            refusal: None,
        };
    }
    if !may_block(record.origin) {
        return GuardDecision {
            guard: None,
            refusal: Some(BlockingRefusal::UntrustedOrigin(
                record.origin.unwrap_or(Origin::Inferred),
            )),
        };
    }
    if !(ledger_approved || self_attested(record, trust)) {
        return GuardDecision {
            guard: None,
            // Name the *reason this record* was refused. A project record whose
            // fields look self-approving is refused for a different reason than a
            // user record missing a verifier, and telling it the generic story
            // would send its author to add a `verified_by` that cannot help.
            refusal: Some(if trust == Trust::User {
                BlockingRefusal::NotApproved
            } else {
                BlockingRefusal::ProjectNeedsApproval
            }),
        };
    }
    match declared_guard(record) {
        Some(guard) => GuardDecision {
            guard: Some(guard),
            refusal: None,
        },
        None => GuardDecision {
            guard: None,
            refusal: Some(BlockingRefusal::NoEnforcer),
        },
    }
}

/// Project a loaded record onto the engine's [`Rule`] so the shipped Tier-1 and
/// Tier-2 paths apply to it unchanged.
///
/// The rule's `id` is the record's **handle**, not its `record_id`: a blocked tool
/// call returns `rule "<id>" — <text>` to the model, and the handle is the token the
/// model can cite back. Using the opaque `rec_…_9f3c` id there would hand the model
/// an identifier it cannot reuse, reopening the attribution gap handles exist to
/// close.
pub fn rule_from_record(loaded: &LoadedRecord, ledger_approved: bool) -> (Rule, GuardDecision) {
    let decision = guard_for(&loaded.record, loaded.trust, ledger_approved);
    let rule = Rule {
        id: loaded.handle.clone(),
        description: loaded
            .record
            .provenance
            .as_ref()
            .and_then(|provenance| provenance.source_uri.clone())
            .unwrap_or_else(|| loaded.source.clone()),
        text: loaded.record.statement.clone(),
        guard: decision.guard.clone(),
        source: loaded.source.clone(),
        // The TOML surface IS the schema (ADR 0011). The markdown frontmatter
        // metadata parser survives only to load existing `.md` files and is
        // deliberately not extended to reach here.
        metadata: None,
        metadata_errors: Vec::new(),
    };
    (rule, decision)
}

#[cfg(test)]
mod tests {
    use super::super::super::ingest::record::{Enforcement, Probe, ProbeKind, Truth};
    use super::super::tests::{hard_guard, record_named, with_truth};
    use super::*;

    fn decreed_by(name: &str) -> Truth {
        Truth {
            basis: TruthBasis::Decree,
            confidence: Some(100),
            verified_by: Some(name.to_string()),
            verified_at: Some("2026-07-19T00:00:00Z".to_string()),
            valid_from: None,
            ttl: None,
            on_expiry: None,
            review_every: None,
            probe: Some(Probe {
                kind: ProbeKind::Manual,
                path: None,
                pattern: None,
                expect: None,
                note: None,
            }),
        }
    }

    fn user_decree_with_guard() -> Record {
        let mut record = with_truth(
            record_named("ctx.acme.web.no-force-push"),
            decreed_by("mac"),
        );
        record.origin = Some(Origin::User);
        record.enforcement = Some(hard_guard(Some("Bash"), None, Some("git push --force*")));
        record
    }

    #[test]
    fn a_user_decreed_record_with_a_real_enforcer_arms_a_guard() {
        let decision = guard_for(&user_decree_with_guard(), Trust::User, false);
        assert_eq!(
            decision.guard,
            Some(RuleGuard {
                tool: Some("Bash".to_string()),
                deny_path_glob: None,
                deny_command_glob: Some("git push --force*".to_string()),
            })
        );
        assert_eq!(decision.refusal, None);
    }

    // The absolute refusal — the acceptance test #890 asks for.

    #[test]
    fn no_imported_or_inferred_record_reaches_blocking_by_any_path() {
        for origin in [Origin::Imported, Origin::Inferred, Origin::Observed] {
            let mut record = user_decree_with_guard();
            record.origin = Some(origin);
            for ledger_approved in [false, true] {
                let decision = guard_for(&record, Trust::User, ledger_approved);
                assert_eq!(
                    decision.guard, None,
                    "{origin:?} armed a guard with ledger_approved={ledger_approved}"
                );
                assert_eq!(
                    decision.refusal,
                    Some(BlockingRefusal::UntrustedOrigin(origin)),
                    "the refusal must name the origin so the reason is actionable"
                );
            }
        }
    }

    #[test]
    fn an_origin_less_record_does_not_default_into_blocking() {
        let mut record = user_decree_with_guard();
        record.origin = None;
        assert_eq!(guard_for(&record, Trust::User, false).guard, None);
        assert!(guard_for(&record, Trust::User, false).refusal.is_some());
    }

    #[test]
    fn hard_mode_with_no_evaluable_guard_stays_advisory() {
        let mut record = user_decree_with_guard();
        record.enforcement = Some(Enforcement {
            mode: EnforcementMode::Hard,
            guard_tool: Some("  ".to_string()),
            guard_deny_command: None,
            guard_deny_path: Some(String::new()),
            severity: Some("error".to_string()),
            on_violation: Some("Never force-push to a shared branch.".to_string()),
            check: None,
            rubric: None,
        });
        let decision = guard_for(&record, Trust::User, true);
        assert_eq!(decision.guard, None, "blank guard fields are not a guard");
        assert_eq!(decision.refusal, Some(BlockingRefusal::NoEnforcer));
    }

    #[test]
    fn a_soft_or_advisory_record_never_blocks_even_with_guard_fields() {
        for mode in [EnforcementMode::Soft, EnforcementMode::None] {
            let mut record = user_decree_with_guard();
            if let Some(enforcement) = record.enforcement.as_mut() {
                enforcement.mode = mode;
            }
            let decision = guard_for(&record, Trust::User, true);
            assert_eq!(decision.guard, None, "{mode:?} must not block");
            assert_eq!(
                decision.refusal, None,
                "a record that never asked to block has nothing to refuse"
            );
        }
    }

    #[test]
    fn a_user_record_without_a_named_verifier_needs_a_ledger_approval() {
        let mut record = user_decree_with_guard();
        record.truth = Some(Truth {
            verified_by: None,
            ..decreed_by("ignored")
        });
        assert_eq!(
            guard_for(&record, Trust::User, false).refusal,
            Some(BlockingRefusal::NotApproved)
        );
        assert!(
            guard_for(&record, Trust::User, true).guard.is_some(),
            "an explicit approval is the other way in"
        );
    }

    #[test]
    fn a_measured_user_record_is_not_self_approving() {
        let mut record = user_decree_with_guard();
        if let Some(truth) = record.truth.as_mut() {
            truth.basis = TruthBasis::Measured;
        }
        assert_eq!(
            guard_for(&record, Trust::User, false).refusal,
            Some(BlockingRefusal::NotApproved),
            "\"the world agrees\" is not \"a person authorized blocking\""
        );
    }

    #[test]
    fn the_projected_rule_is_keyed_by_handle_so_a_block_is_citable() {
        let mut loaded = super::super::tests::loaded_from(user_decree_with_guard());
        loaded.trust = Trust::User;
        loaded.handle = "no-force-push".to_string();
        let (rule, decision) = rule_from_record(&loaded, false);
        assert_eq!(rule.id, "no-force-push");
        assert_eq!(rule.text, loaded.record.statement);
        assert!(decision.guard.is_some());
        assert!(
            rule.metadata.is_none(),
            "the legacy markdown metadata parser must not be extended to TOML records"
        );
    }
}
