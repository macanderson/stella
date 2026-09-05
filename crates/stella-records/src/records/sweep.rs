//! The truth sweep: deciding, before anything is rendered, whether a record is
//! still true enough to steer.
//!
//! This is the reason the record surface has a truth axis at all. The worked case
//! is `docs/spec/adaptive-context/context-record-examples/05-imported-proposals.toml`: `CLAUDE.md` says
//! "we use Node 20", `.nvmrc` has said `22` for months. Publishing that claim with
//! a `must` force teaches the agent something false on *every turn*, forever,
//! with the full authority of a reviewed and committed policy file. Ingest probes
//! that claim once, at extraction. Nothing re-checked it — until this module.
//!
//! # Resolved before rendering, never expressed in the cached prefix
//!
//! `docs/spec/adaptive-context/context-record-examples/07-agent-projection.md` forbids a clock in the
//! byte-stable system prefix: a "last checked 3 days ago" note would change every
//! turn and break the prompt cache for the whole prefix behind it. So a stale
//! record does not get a warning *appended* — it gets **demoted to the volatile
//! channel** ([`Disposition::SelectStale`]), where per-turn text is already the
//! contract. The cached block only ever contains records that are currently
//! believed.
//!
//! # Refuted and expired are different failures
//!
//! A **refuted** record has been measured against the world and lost. Its default
//! is [`Disposition::Drop`] — the Node-20 case is precisely a refuted `must`, and
//! "keep citing it with a warning" is the harm, not the mitigation. An author who
//! genuinely wants a refuted claim to keep steering has to say so with
//! `on_expiry = "stale"`.
//!
//! An **expired** record has merely gone unchecked past its `ttl`. Nobody has
//! shown it to be wrong; its shelf life lapsed. Its default is
//! [`Disposition::SelectStale`] — demoted, stated as unverified, still available —
//! because dropping every record whose owner went on holiday would make the TTL a
//! foot-gun rather than a review prompt.
//!
//! Both defaults are overridable by `truth.on_expiry`, and both are stated on the
//! disposition so the CLI can explain what happened rather than silently changing
//! what the agent sees.

use super::super::ingest::gate::origin_is_untrusted;
use super::super::ingest::record::{Probe, Record, TruthBasis, Verdict};
use super::Trust;
use super::clock;

/// What `truth.on_expiry` asks for when a claim stops being believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryAction {
    /// Keep the record in play, demoted and stated as unverified.
    Stale,
    /// Take the record out of selection entirely.
    Drop,
    /// Take it out of selection and flag it for an owner — the claim mattering
    /// enough to block is a stronger statement than the claim being wrong.
    Block,
}

impl ExpiryAction {
    /// Parse the surface spelling. An unrecognized value is treated as
    /// [`ExpiryAction::Stale`], the least destructive reading of an instruction
    /// nobody here understands.
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value?.trim() {
            "stale" => Some(Self::Stale),
            "drop" => Some(Self::Drop),
            "block" => Some(Self::Block),
            _ => Some(Self::Stale),
        }
    }
}

/// Everything the sweep needs to judge one record.
#[derive(Debug, Clone)]
pub struct SweepInput<'a> {
    /// The record under review.
    pub record: &'a Record,
    /// The tier the record was published at. The loader stamps it from the
    /// directory the file was read out of. [`honored_probe`] needs it: whether
    /// a gated probe may run is a question about the publisher, and the record
    /// cannot answer that about itself.
    pub trust: Trust,
    /// The verdict a probe produced this sweep, when one ran. `None` means no
    /// probe ran — either nothing is due, or nothing about this record is
    /// probeable.
    pub verdict: Option<Verdict>,
    /// When this record's probe last ran, from the sweep ledger. `None` means
    /// never.
    pub last_checked: Option<&'a str>,
    /// Now, RFC-3339. Supplied by the caller: this crate has no clock.
    pub now: &'a str,
}

/// What the sweep decided about a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Believed. Renders in its declared channel.
    Select,
    /// Nothing can judge this claim. Renders, stated as unverified — never folded
    /// into "supported", because a checker that reports OK for claims it never
    /// checked launders unvalidated content with a validated stamp.
    SelectUnfalsifiable,
    /// No longer confidently true, but kept in play. **Demoted to the volatile
    /// channel** so the reason can be said out loud without a clock entering the
    /// cached prefix.
    SelectStale {
        /// Why it is stale, in words a person can act on.
        reason: String,
    },
    /// Out of selection.
    Drop {
        /// Why.
        reason: String,
    },
    /// Out of selection, and flagged for an owner.
    Block {
        /// Why.
        reason: String,
    },
}

impl Disposition {
    /// Whether this record reaches the model at all.
    pub fn is_selected(&self) -> bool {
        matches!(
            self,
            Self::Select | Self::SelectUnfalsifiable | Self::SelectStale { .. }
        )
    }

    /// Whether this record must render in the volatile channel regardless of its
    /// declared `force`. A stale record carries a per-turn reason, and per-turn
    /// text cannot enter the byte-stable prefix.
    pub fn forces_volatile(&self) -> bool {
        matches!(self, Self::SelectStale { .. })
    }

    /// The human-readable reason, when there is one.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Select => None,
            Self::SelectUnfalsifiable => Some("no probe can judge this claim"),
            Self::SelectStale { reason } | Self::Drop { reason } | Self::Block { reason } => {
                Some(reason)
            }
        }
    }
}

/// The probe this record is allowed to have run for it.
///
/// A gated probe (`command_succeeds`, `http_ok`) runs a command or reaches the
/// network. The claim it checks may have come from a document nobody read. An
/// `http_ok` aimed at a host someone else chose leaks data, with a policy
/// file's authority behind it. So three things must hold, and each one closes a
/// way to fake the other two:
///
/// - **The loader stamped [`Trust::User`].** Every other field here is written
///   inside the file being judged. A checkout can set a clean `origin` and a
///   named `verified_by` as easily as it sets anything else. That would let
///   anyone who can open a pull request arm a command. Where the file was read
///   from is the one fact it cannot write, and [`super::registry::load`] stamps
///   that from the directory. [`super::bridge`] asks the same question before
///   it lets a record approve its own blocking guard.
/// - **The origin is stamped, and is not `imported` or `inferred`.** A record
///   with no origin is refused too. It has said nothing about where it came
///   from, and saying nothing must not be the permissive answer.
///   `origin_is_untrusted` belongs to `super::super::ingest::gate`, so
///   extraction and the sweep read one rule rather than two copies.
/// - **`truth.basis` is a `decree` with a non-empty `verified_by`.** What is
///   being relied on is a person whose name is on the claim. A name nobody
///   wrote is not one.
///
/// The gate runs here as well as at extraction because a record can be edited
/// by hand afterwards. The file is all the loader sees.
pub fn honored_probe(record: &Record, trust: Trust) -> Option<&Probe> {
    let truth = record.truth.as_ref()?;
    let probe = truth.probe.as_ref()?;
    if !probe.kind.is_gated() {
        return Some(probe);
    }
    if trust != Trust::User {
        return None;
    }
    let trusted_origin = record
        .origin
        .is_some_and(|origin| !origin_is_untrusted(origin));
    let decreed_by_a_human = truth.basis == TruthBasis::Decree
        && truth
            .verified_by
            .as_deref()
            .is_some_and(|by| !by.is_empty());
    (trusted_origin && decreed_by_a_human).then_some(probe)
}

/// Whether this record's probe should run now.
///
/// Driven by `review_every` (recurring) or `ttl` (one shelf life), whichever is
/// declared — `review_every` wins when both are, because a recurring cadence is a
/// stronger statement of intent than a deadline. A `manual` probe is never due on a
/// timer: its cadence is a human's, and firing it automatically would produce a
/// verdict nobody actually checked.
pub fn probe_is_due(input: &SweepInput<'_>) -> bool {
    let Some(probe) = honored_probe(input.record, input.trust) else {
        return false;
    };
    if matches!(
        probe.kind,
        super::super::ingest::record::ProbeKind::Manual
            | super::super::ingest::record::ProbeKind::None
    ) {
        return false;
    }
    let truth = input.record.truth.as_ref();
    let cadence = truth
        .and_then(|truth| truth.review_every.as_deref())
        .or_else(|| truth.and_then(|truth| truth.ttl.as_deref()));
    let last = input
        .last_checked
        .or_else(|| truth.and_then(|truth| truth.verified_at.as_deref()));
    clock::is_due(last, cadence, input.now)
}

/// Judge one record. This is the function that stands between a record that has
/// stopped being true and the system prompt.
pub fn disposition(input: &SweepInput<'_>) -> Disposition {
    let truth = input.record.truth.as_ref();
    let on_expiry = ExpiryAction::parse(truth.and_then(|truth| truth.on_expiry.as_deref()));

    // A measured refutation is the strongest signal available and outranks
    // everything else: the world has been consulted and disagrees.
    if input.verdict == Some(Verdict::Refuted) {
        let detail = "its truth probe no longer holds";
        return match on_expiry.unwrap_or(ExpiryAction::Drop) {
            ExpiryAction::Stale => Disposition::SelectStale {
                reason: format!("{detail} (on_expiry = stale, so it still applies)"),
            },
            ExpiryAction::Drop => Disposition::Drop {
                reason: detail.to_string(),
            },
            ExpiryAction::Block => Disposition::Block {
                reason: format!("{detail}, and on_expiry = block"),
            },
        };
    }

    // Shelf life. Unchecked past its `ttl` is not the same as shown to be wrong.
    let expired = clock::is_expired(
        truth.and_then(|truth| truth.valid_from.as_deref()),
        truth.and_then(|truth| truth.ttl.as_deref()),
        input.now,
    );
    if expired {
        let ttl = truth
            .and_then(|truth| truth.ttl.as_deref())
            .unwrap_or("its ttl");
        let detail = format!("unverified past {ttl}");
        return match on_expiry.unwrap_or(ExpiryAction::Stale) {
            ExpiryAction::Stale => Disposition::SelectStale { reason: detail },
            ExpiryAction::Drop => Disposition::Drop { reason: detail },
            ExpiryAction::Block => Disposition::Block {
                reason: format!("{detail}, and on_expiry = block"),
            },
        };
    }

    match input.verdict {
        Some(Verdict::Unfalsifiable) => Disposition::SelectUnfalsifiable,
        // A due-but-unrun probe is an unmeasured claim, not a supported one. Saying
        // so is the difference between a sweep and a sweep that reports success for
        // work it skipped.
        None if probe_is_due(input) => Disposition::SelectStale {
            reason: "its re-verification is due and has not run".to_string(),
        },
        _ => Disposition::Select,
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::context_record::kind::Origin;
    use super::super::super::ingest::record::{ProbeKind, Truth};
    use super::super::tests::{record_named, with_truth};
    use super::*;

    const NOW: &str = "2026-07-20T00:00:00Z";

    fn input<'a>(record: &'a Record, verdict: Option<Verdict>) -> SweepInput<'a> {
        SweepInput {
            record,
            trust: Trust::User,
            verdict,
            last_checked: None,
            now: NOW,
        }
    }

    /// A record whose own fields all argue for a gated probe: a
    /// `command_succeeds`, decreed, with a named human, from a `user` origin.
    /// Only the tier the loader stamps tells the honored case from the refused
    /// one.
    fn gated_and_decreed(origin: Option<Origin>) -> Record {
        let mut truth = measured(Some(Probe {
            kind: ProbeKind::CommandSucceeds,
            path: None,
            pattern: None,
            expect: None,
            note: None,
        }));
        truth.basis = TruthBasis::Decree;
        truth.verified_by = Some("platform-owner".to_string());
        let mut record = with_truth(record_named("ctx.a.b.c"), truth);
        record.origin = origin;
        record
    }

    fn measured(probe: Option<Probe>) -> Truth {
        Truth {
            basis: TruthBasis::Measured,
            confidence: Some(90),
            verified_by: None,
            verified_at: Some("2026-07-19T00:00:00Z".to_string()),
            valid_from: Some("2026-01-01T00:00:00Z".to_string()),
            ttl: None,
            on_expiry: None,
            review_every: None,
            probe,
        }
    }

    fn path_probe() -> Probe {
        Probe {
            kind: ProbeKind::PathExists,
            path: Some(".nvmrc".to_string()),
            pattern: None,
            expect: None,
            note: None,
        }
    }

    // The 05 node-version case — the whole reason this module exists.

    #[test]
    fn a_refuted_record_leaves_selection_by_default() {
        let record = with_truth(
            record_named("ctx.acme.web.node-version"),
            measured(Some(path_probe())),
        );
        let decided = disposition(&input(&record, Some(Verdict::Refuted)));
        assert!(
            !decided.is_selected(),
            "a claim the world refuted must stop steering: {decided:?}"
        );
        assert!(decided.reason().unwrap().contains("no longer holds"));
    }

    #[test]
    fn a_refuted_record_keeps_steering_only_when_the_author_said_so() {
        let mut truth = measured(Some(path_probe()));
        truth.on_expiry = Some("stale".to_string());
        let record = with_truth(record_named("ctx.acme.web.node-version"), truth);
        let decided = disposition(&input(&record, Some(Verdict::Refuted)));
        assert!(decided.is_selected(), "on_expiry = stale opts back in");
        assert!(
            decided.forces_volatile(),
            "a stale record must leave the cached prefix — its reason changes per turn"
        );
    }

    #[test]
    fn on_expiry_block_takes_a_refuted_record_out_and_flags_it() {
        let mut truth = measured(Some(path_probe()));
        truth.on_expiry = Some("block".to_string());
        let record = with_truth(record_named("ctx.acme.api.contract"), truth);
        let decided = disposition(&input(&record, Some(Verdict::Refuted)));
        assert!(matches!(decided, Disposition::Block { .. }), "{decided:?}");
    }

    // Expiry: unchecked is not the same as wrong.

    #[test]
    fn an_expired_record_is_demoted_rather_than_dropped() {
        let mut truth = measured(Some(path_probe()));
        truth.ttl = Some("P30D".to_string());
        truth.valid_from = Some("2026-01-01T00:00:00Z".to_string());
        let record = with_truth(record_named("ctx.acme.web.pkg-manager"), truth);
        let decided = disposition(&input(&record, None));
        assert!(
            matches!(decided, Disposition::SelectStale { .. }),
            "{decided:?}"
        );
        assert!(decided.reason().unwrap().contains("P30D"));
    }

    #[test]
    fn a_fresh_supported_record_selects_cleanly() {
        let mut truth = measured(Some(path_probe()));
        truth.ttl = Some("P3650D".to_string());
        let record = with_truth(record_named("ctx.acme.web.pkg-manager"), truth);
        let decided = disposition(&input(&record, Some(Verdict::Supported)));
        assert_eq!(decided, Disposition::Select);
        assert!(!decided.forces_volatile());
    }

    #[test]
    fn unfalsifiable_is_selected_but_never_reported_as_supported() {
        let record = with_truth(record_named("ctx.acme.style.tone"), measured(None));
        let decided = disposition(&input(&record, Some(Verdict::Unfalsifiable)));
        assert_eq!(decided, Disposition::SelectUnfalsifiable);
        assert!(decided.is_selected());
        assert_eq!(decided.reason(), Some("no probe can judge this claim"));
    }

    #[test]
    fn a_due_probe_that_did_not_run_reads_as_unverified_not_supported() {
        let mut truth = measured(Some(path_probe()));
        truth.review_every = Some("P1D".to_string());
        truth.verified_at = Some("2026-01-01T00:00:00Z".to_string());
        let record = with_truth(record_named("ctx.acme.web.node-version"), truth);
        let decided = disposition(&input(&record, None));
        assert!(
            matches!(decided, Disposition::SelectStale { .. }),
            "{decided:?}"
        );
        assert!(decided.reason().unwrap().contains("due"));
    }

    // Due-ness

    #[test]
    fn review_every_drives_the_cadence_and_outranks_ttl() {
        let mut truth = measured(Some(path_probe()));
        truth.review_every = Some("P90D".to_string());
        truth.ttl = Some("P1D".to_string());
        truth.verified_at = Some("2026-07-19T00:00:00Z".to_string());
        let record = with_truth(record_named("ctx.a.b.c"), truth);
        assert!(
            !probe_is_due(&input(&record, None)),
            "review_every = P90D, checked yesterday: not due, even though ttl is P1D"
        );
    }

    #[test]
    fn a_manual_probe_is_never_due_on_a_timer() {
        let mut truth = measured(Some(Probe {
            kind: ProbeKind::Manual,
            path: None,
            pattern: None,
            expect: None,
            note: Some("ask the platform owner".to_string()),
        }));
        truth.review_every = Some("P1D".to_string());
        truth.verified_at = Some("2020-01-01T00:00:00Z".to_string());
        let record = with_truth(record_named("ctx.a.b.c"), truth);
        assert!(
            !probe_is_due(&input(&record, None)),
            "a human's cadence is not a timer — auto-firing it would fake a verdict"
        );
    }

    #[test]
    fn a_record_with_no_cadence_is_never_due() {
        let record = with_truth(record_named("ctx.a.b.c"), measured(Some(path_probe())));
        assert!(!probe_is_due(&input(&record, None)));
    }

    // The gated-probe boundary

    #[test]
    fn no_gated_probe_is_honored_at_project_trust_ever() {
        for origin in [
            None,
            Some(Origin::User),
            Some(Origin::System),
            Some(Origin::Observed),
            Some(Origin::Inferred),
            Some(Origin::Imported),
        ] {
            let record = gated_and_decreed(origin);
            assert!(
                honored_probe(&record, Trust::Project).is_none(),
                "a repository can write every field this record asserts, so none of \
                 them may unlock a command: origin {origin:?}"
            );
        }
    }

    #[test]
    fn an_unstamped_origin_does_not_unlock_a_gated_probe() {
        let record = gated_and_decreed(None);
        assert!(
            honored_probe(&record, Trust::User).is_none(),
            "a record that said nothing about where it came from is not the trusted case"
        );
    }

    #[test]
    fn a_gated_probe_is_refused_on_an_imported_record() {
        let mut record = with_truth(
            record_named("ctx.a.b.c"),
            measured(Some(Probe {
                kind: ProbeKind::HttpOk,
                path: None,
                pattern: None,
                expect: None,
                note: None,
            })),
        );
        record.origin = Some(Origin::Imported);
        assert!(
            honored_probe(&record, Trust::User).is_none(),
            "an imported document must never get us to make a network call"
        );
    }

    #[test]
    fn a_gated_probe_is_refused_on_an_inferred_record_even_when_decreed() {
        let record = gated_and_decreed(Some(Origin::Inferred));
        assert!(honored_probe(&record, Trust::User).is_none());
    }

    #[test]
    fn a_gated_probe_is_honored_on_a_human_decreed_user_record() {
        let record = gated_and_decreed(Some(Origin::User));
        assert!(honored_probe(&record, Trust::User).is_some());
    }

    #[test]
    fn a_decree_with_no_named_verifier_does_not_unlock_a_gated_probe() {
        let mut truth = measured(Some(Probe {
            kind: ProbeKind::HttpOk,
            path: None,
            pattern: None,
            expect: None,
            note: None,
        }));
        truth.basis = TruthBasis::Decree;
        truth.verified_by = Some(String::new());
        let mut record = with_truth(record_named("ctx.a.b.c"), truth);
        record.origin = Some(Origin::User);
        assert!(
            honored_probe(&record, Trust::User).is_none(),
            "\"a human said so\" needs the human's name to mean anything"
        );
    }

    #[test]
    fn an_ungated_probe_is_honored_on_an_imported_project_record() {
        let mut record = with_truth(record_named("ctx.a.b.c"), measured(Some(path_probe())));
        record.origin = Some(Origin::Imported);
        assert!(
            honored_probe(&record, Trust::Project).is_some(),
            "reading .nvmrc is how an imported claim gets refuted at all"
        );
    }

    #[test]
    fn an_unrecognized_on_expiry_reads_as_the_least_destructive_action() {
        assert_eq!(
            ExpiryAction::parse(Some("explode")),
            Some(ExpiryAction::Stale)
        );
        assert_eq!(ExpiryAction::parse(None), None);
    }
}
