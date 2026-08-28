//! The regulated governance tier's promotion ledger (#994, ADR 0007).
//!
//! Enforcement transitions — advisory → blocking and back — are the grants
//! that let a record deny tool calls, and in the regulated tier (`docs/
//! context-pr.md` §5.3/§9) they must be **accountable** (approver identity +
//! stated reason), **immutable** (append-only, replayable), and **auditable**
//! (a policy version an auditor can cite for any commit). The private
//! decision ledger cannot carry this: it lives per-machine under
//! `.stella/private/`, while a team's promotion history must travel with the
//! repository and be reviewed through the same pull requests as the rules it
//! governs.
//!
//! So promotions live in a **project-visible, hash-chained JSONL ledger**
//! (`.stella/rules/promotions.jsonl`): each event carries the SHA-256 of the
//! previous event's exact line. Git history already makes rewrites visible;
//! the chain makes them *self-evident* — a validator needs only the file
//! itself to prove no line was edited, dropped, or reordered. "Immutable"
//! here is tamper-EVIDENT, not tamper-proof: nothing stops a hostile rewrite
//! of file plus history.
//!
//! # What the chain cannot see, and what does
//!
//! [`parse_and_verify`] checks each line against the one before it and the
//! sequence from 1, so **deleting the last N lines satisfies both**. What is
//! left is a shorter ledger that verifies perfectly. That is not a corner
//! case: truncating a demotion event resurrects a revoked blocking grant,
//! truncating a grant disarms a record — and `stella context validate`'s
//! regulated check keys on `is_enforced()`, itself downstream of the grant, so
//! the ungoverned list comes back empty and validate exits **green** on the
//! truncated ledger. Deleting the file entirely reads as a ledger with no
//! events at all (#5327).
//!
//! Detecting it needs one fact the file cannot carry: what the head was last
//! time. [`ChainHead`] is that fact and [`continuity_violation`] is the check.
//! The module doc used to claim a rewrite "cannot survive `parse_and_verify`
//! against a previously seen head digest" — true of the function, and empty in
//! practice, because nothing recorded a head digest to compare against.
//!
//! The pin is a **local** artifact and belongs under `.stella/private/`: it is
//! one machine's memory of what it last saw, not reviewed policy, and
//! committing it would make one developer's clone the authority on another's.
//! Its cost is that a fresh clone has nothing to compare against until its
//! first read — the anchor detects a truncation that happens *after* it has
//! seen the ledger once, which is the case that matters, and cannot speak to
//! what arrived before it.
//!
//! Per ADR 0007 the enforcement vocabulary is exactly two-valued
//! (`advisory` | `blocking`); the four review-ladder labels are UI over it
//! and never appear on the wire or in this ledger.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The governance mode a repository's context records operate under
/// (`docs/spec/adaptive-context/context-pr.md` §5). Stored in `.stella/rules/governance.toml` so
/// the mode itself is repository-visible and reviewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceMode {
    /// One person; keeping a record is approving it.
    #[default]
    Solo,
    /// Shared repository: publication through Git, owner routing.
    Team,
    /// Team plus accountable approval, immutable promotion history, policy
    /// versioning, and (optionally) proposer/approver separation.
    Regulated,
}

impl GovernanceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GovernanceMode::Solo => "solo",
            GovernanceMode::Team => "team",
            GovernanceMode::Regulated => "regulated",
        }
    }
}

/// The repository's governance settings (`.stella/rules/governance.toml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Governance {
    /// Which tier's promotion workflow applies.
    #[serde(default)]
    pub mode: GovernanceMode,
    /// Proposer/approver separation (§9): when `true`, the identity that
    /// authored a record may not approve its own enforcement grant. Only
    /// meaningful in [`GovernanceMode::Regulated`].
    #[serde(default)]
    pub separation: bool,
}

/// What kind of transition a ledger event records (#2728).
///
/// The ledger began as enforcement grants only, so a legacy line carries no
/// `action` key and reads as [`LedgerAction::Grant`] — and a grant serializes
/// without the key, keeping every pre-#2728 line byte-identical under
/// re-serialization and every old reader's `to == "blocking"` fold correct.
/// The lifecycle actions record what the spec (§4) calls retirement: a
/// record's valid time closing, by drop or by replacement. For those, `from`/
/// `to` carry the **status** transition (`active` → `archived`), not an
/// enforcement level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerAction {
    /// An enforcement transition (`advisory` ↔ `blocking`) — the original
    /// and default vocabulary.
    #[default]
    Grant,
    /// The record's source no longer asserts it; its revision was archived.
    Retired,
    /// The record was replaced by a new revision carrying a
    /// `supersedes_record_id` link back to it.
    Superseded,
}

impl LedgerAction {
    /// `true` for the default, so grants serialize without the key.
    fn is_grant(&self) -> bool {
        *self == LedgerAction::Grant
    }
}

/// One immutable transition. Append-only: current enforcement grants are
/// always a fold over the whole ledger, and every event names who decided
/// what, why, and under which policy version. Since #2728 the ledger also
/// carries record lifecycle events — retirement and supersession — because
/// the spec (§4) requires every retirement to append an accountable event,
/// and this is the only repository-visible, tamper-evident ledger there is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionEvent {
    /// 1-based position in the ledger; also the **policy version** after this
    /// event — an auditor cites "policy version N" and means the ledger
    /// prefix of length N.
    pub seq: u64,
    /// SHA-256 (hex) of the previous event's exact JSONL line;
    /// [`GENESIS`] for the first event.
    pub prev: String,
    /// When, RFC-3339.
    pub at: String,
    /// The record whose enforcement changed.
    pub lineage_id: String,
    /// Enforcement before, `advisory` or `blocking` (ADR 0007's two-value
    /// vocabulary).
    pub from: String,
    /// Enforcement after.
    pub to: String,
    /// Who granted it. In regulated mode this is a real identity, not a
    /// local username.
    pub approver: String,
    /// The record's author identity at promotion time, when it could be
    /// established — what proposer/approver separation checks against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposer: Option<String>,
    /// Why — always required for a promotion; a grant with no reason is
    /// evidence nobody can audit.
    pub reason: String,
    /// The governance mode in force when the event was recorded.
    pub mode: String,
    /// What kind of transition this is. Absent on legacy lines (= a grant),
    /// and omitted when serializing a grant so those lines stay byte-stable.
    #[serde(default, skip_serializing_if = "LedgerAction::is_grant")]
    pub action: LedgerAction,
}

/// The `prev` value of a ledger's first event.
pub const GENESIS: &str = "genesis";

/// SHA-256 (hex) over one ledger line's exact bytes — the chain link.
pub fn line_digest(line: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(line.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Why a ledger failed verification. The message names the line so an auditor
/// can look at exactly the break, not re-derive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainViolation {
    /// 1-based line number of the offending event.
    pub line: usize,
    pub reason: String,
}

/// What a reader last saw at the end of a ledger — the anchor a truncation
/// cannot forge.
///
/// Both fields are needed and neither is sufficient. The count alone would be
/// satisfied by a ledger rewritten to the same length; the digest alone has no
/// position to be checked at, since a truncated ledger's own last line is a
/// perfectly good line that hashes to itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainHead {
    /// How many events the ledger held.
    pub seq: u64,
    /// SHA-256 of the last line's exact bytes, by [`line_digest`].
    pub digest: String,
}

/// The head of `text`, or `None` for a ledger with no events.
///
/// Computed off the raw lines rather than off re-serialized events: the chain
/// is over exact bytes, and a round trip through `serde` is not guaranteed to
/// reproduce them.
#[must_use]
pub fn head_of(text: &str) -> Option<ChainHead> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let last = lines.last()?;
    Some(ChainHead {
        seq: lines.len() as u64,
        digest: line_digest(last),
    })
}

/// Has this ledger lost or rewritten history it is known to have held?
///
/// The question [`parse_and_verify`] structurally cannot ask. It walks each
/// line against the one before it and the sequence from 1, so a ledger with its
/// last N lines removed passes every check it makes — it is a shorter ledger,
/// internally consistent, and there is nothing inside the file that says
/// otherwise. `pinned` is the outside fact: what a reader on this machine saw
/// the last time it looked.
///
/// Growth is fine and is the normal case. What is refused is a ledger that is
/// now *shorter* than the anchor, or one whose line at the anchor's position no
/// longer hashes to what it did — the two shapes of "history was rewritten".
#[must_use]
pub fn continuity_violation(text: &str, pinned: &ChainHead) -> Option<ChainViolation> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let anchored = lines.get(pinned.seq as usize - 1);
    match anchored {
        None => Some(ChainViolation {
            line: lines.len() + 1,
            reason: format!(
                "the ledger has {} event(s) but this machine last read {} — {} event(s) were \
                 removed from the end. The hash chain cannot see this: a truncated ledger \
                 verifies perfectly, and a removed demotion resurrects the grant it revoked",
                lines.len(),
                pinned.seq,
                pinned.seq as usize - lines.len()
            ),
        }),
        Some(line) if line_digest(line) != pinned.digest => Some(ChainViolation {
            line: pinned.seq as usize,
            reason: format!(
                "event {} is not the one this machine last read — the line's digest changed, \
                 so history was rewritten rather than appended to",
                pinned.seq
            ),
        }),
        Some(_) => None,
    }
}

/// Parse a promotions ledger and verify its hash chain: every line must
/// parse, `seq` must be contiguous from 1, and each event's `prev` must equal
/// the SHA-256 of the previous line's exact bytes. An edited, dropped, or
/// reordered line breaks the chain at the first divergence.
///
/// **This cannot see a truncated tail.** Removing the last N lines leaves a
/// ledger that satisfies every rule here; [`continuity_violation`] is what
/// catches that, and it needs an anchor from outside the file (#5327).
pub fn parse_and_verify(text: &str) -> Result<Vec<PromotionEvent>, ChainViolation> {
    let mut events = Vec::new();
    let mut prev_line: Option<&str> = None;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: PromotionEvent = serde_json::from_str(line).map_err(|e| ChainViolation {
            line: index + 1,
            reason: format!("not a promotion event: {e}"),
        })?;
        let expected_prev = match prev_line {
            None => GENESIS.to_string(),
            Some(previous) => line_digest(previous),
        };
        if event.prev != expected_prev {
            return Err(ChainViolation {
                line: index + 1,
                reason: format!(
                    "hash chain broken: event {} declares prev {} but the preceding line \
                     digests to {} — a line was edited, dropped, or reordered",
                    event.seq, event.prev, expected_prev
                ),
            });
        }
        let expected_seq = events.len() as u64 + 1;
        if event.seq != expected_seq {
            return Err(ChainViolation {
                line: index + 1,
                reason: format!(
                    "sequence broken: expected seq {expected_seq}, found {}",
                    event.seq
                ),
            });
        }
        events.push(event);
        prev_line = Some(line);
    }
    Ok(events)
}

/// Serialize `event` as the next ledger line, stamping `seq` and `prev` from
/// the verified tail of `existing`. Returns the exact line to append.
pub fn next_line(existing: &str, mut event: PromotionEvent) -> Result<String, ChainViolation> {
    let events = parse_and_verify(existing)?;
    event.seq = events.len() as u64 + 1;
    event.prev = existing
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .map(line_digest)
        .unwrap_or_else(|| GENESIS.to_string());
    serde_json::to_string(&event).map_err(|e| ChainViolation {
        line: events.len() + 1,
        reason: format!("cannot serialize promotion event: {e}"),
    })
}

/// The policy version a verified ledger establishes: the seq of its last
/// event, `0` for an empty ledger.
pub fn policy_version(events: &[PromotionEvent]) -> u64 {
    events.last().map_or(0, |event| event.seq)
}

/// The lineages whose LATEST recorded transition grants `blocking` — the
/// fold enforcement arming consults. Later events supersede earlier ones for
/// the same lineage; a demotion back to `advisory` revokes the grant.
pub fn blocking_grants(events: &[PromotionEvent]) -> BTreeMap<String, PromotionEvent> {
    let mut latest: BTreeMap<String, PromotionEvent> = BTreeMap::new();
    for event in events {
        latest.insert(event.lineage_id.clone(), event.clone());
    }
    // Deliberate consequence of the latest-event fold: a lifecycle event
    // (retired/superseded) on a lineage clears its blocking grant, because a
    // record whose valid time has closed must not keep the authority to deny
    // tool calls. Old readers agree by accident of vocabulary — a lifecycle
    // event's `to` is a status, never `blocking`.
    latest.retain(|_, event| event.action == LedgerAction::Grant && event.to == "blocking");
    latest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(lineage: &str, to: &str, approver: &str) -> PromotionEvent {
        PromotionEvent {
            seq: 0,
            prev: String::new(),
            at: "2026-08-01T00:00:00Z".into(),
            lineage_id: lineage.into(),
            from: if to == "blocking" {
                "advisory"
            } else {
                "blocking"
            }
            .into(),
            to: to.into(),
            approver: approver.into(),
            proposer: Some("author@example.test".into()),
            reason: "measured advisory precision over 30 days".into(),
            mode: "regulated".into(),
            action: LedgerAction::Grant,
        }
    }

    fn ledger(events: &[PromotionEvent]) -> String {
        let mut text = String::new();
        for event in events {
            let line = next_line(&text, event.clone()).unwrap();
            text.push_str(&line);
            text.push('\n');
        }
        text
    }

    /// The #994 acceptance: a promotion to blocking is replayable from the
    /// ledger with its approver and reason intact.
    #[test]
    fn a_promotion_replays_with_approver_and_reason() {
        let text = ledger(&[event("^no-force-push", "blocking", "lead@example.test")]);
        let events = parse_and_verify(&text).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].approver, "lead@example.test");
        assert_eq!(events[0].reason, "measured advisory precision over 30 days");
        assert_eq!(policy_version(&events), 1);
        assert!(blocking_grants(&events).contains_key("^no-force-push"));
    }

    /// An edited line breaks the chain at exactly that line — the "immutable"
    /// in immutable promotion history.
    #[test]
    fn an_edited_line_breaks_the_chain() {
        let text = ledger(&[
            event("^a", "blocking", "lead@example.test"),
            event("^b", "blocking", "lead@example.test"),
        ]);
        let tampered = text.replace("measured advisory precision", "because I said so");
        let violation = parse_and_verify(&tampered).unwrap_err();
        assert_eq!(violation.line, 2, "the break surfaces at the successor");
        assert!(violation.reason.contains("hash chain broken"));
    }

    #[test]
    fn a_dropped_line_breaks_the_chain() {
        let text = ledger(&[
            event("^a", "blocking", "lead@example.test"),
            event("^b", "blocking", "lead@example.test"),
        ]);
        let without_first = text.lines().skip(1).collect::<Vec<_>>().join("\n");
        assert!(parse_and_verify(&without_first).is_err());
    }

    /// A demotion revokes the grant: the fold keeps only lineages whose
    /// LATEST transition is to blocking.
    #[test]
    fn a_demotion_revokes_the_blocking_grant() {
        let text = ledger(&[
            event("^a", "blocking", "lead@example.test"),
            event("^a", "advisory", "lead@example.test"),
        ]);
        let events = parse_and_verify(&text).unwrap();
        assert!(blocking_grants(&events).is_empty());
        assert_eq!(policy_version(&events), 2, "revocation is history too");
    }

    #[test]
    fn an_empty_ledger_is_valid_at_policy_version_zero() {
        let events = parse_and_verify("").unwrap();
        assert!(events.is_empty());
        assert_eq!(policy_version(&events), 0);
    }

    fn retirement(lineage: &str) -> PromotionEvent {
        PromotionEvent {
            seq: 0,
            prev: String::new(),
            at: "2026-08-10T00:00:00Z".into(),
            lineage_id: lineage.into(),
            from: "active".into(),
            to: "archived".into(),
            approver: "lead@example.test".into(),
            proposer: None,
            reason: "the source no longer asserts it".into(),
            mode: "regulated".into(),
            action: LedgerAction::Retired,
        }
    }

    /// Backward compatibility both ways (#2728): a pre-#2728 line with no
    /// `action` key parses as a grant, and a grant serializes without the
    /// key — so extending the vocabulary changed no existing line's bytes
    /// and therefore no existing chain digest.
    #[test]
    fn legacy_lines_parse_as_grants_and_grants_serialize_without_the_key() {
        let legacy = r#"{"seq":1,"prev":"genesis","at":"2026-08-01T00:00:00Z","lineage_id":"^a","from":"advisory","to":"blocking","approver":"lead@example.test","reason":"r","mode":"regulated"}"#;
        let event: PromotionEvent = serde_json::from_str(legacy).unwrap();
        assert_eq!(event.action, LedgerAction::Grant);
        let reserialized = serde_json::to_string(&event).unwrap();
        assert!(
            !reserialized.contains("action"),
            "a grant must not grow an `action` key: {reserialized}"
        );
    }

    /// Lifecycle events chain exactly like grants — one ledger, one chain.
    #[test]
    fn a_retirement_chains_and_replays_with_its_reason() {
        let text = ledger(&[
            event("^a", "blocking", "lead@example.test"),
            retirement("^a"),
        ]);
        let events = parse_and_verify(&text).unwrap();
        assert_eq!(events[1].action, LedgerAction::Retired);
        assert_eq!(events[1].reason, "the source no longer asserts it");
        assert_eq!(policy_version(&events), 2);
    }

    /// A retired record must not keep the authority to deny tool calls: the
    /// lifecycle event clears the lineage's blocking grant in the fold.
    #[test]
    fn a_retirement_revokes_the_blocking_grant() {
        let text = ledger(&[
            event("^a", "blocking", "lead@example.test"),
            retirement("^a"),
        ]);
        let events = parse_and_verify(&text).unwrap();
        assert!(blocking_grants(&events).is_empty());
    }

    /// **Witness (#5327).** A truncated tail passes the hash chain, and the
    /// anchor is what catches it.
    ///
    /// The first assertion is the defect: `parse_and_verify` walks each line
    /// against the one before it and the sequence from 1, so removing the last
    /// line leaves a ledger that satisfies both. The module doc claimed a
    /// rewrite "cannot survive `parse_and_verify` against a previously seen
    /// head digest" — true of the function, and empty in practice, because
    /// nothing recorded one.
    #[test]
    fn a_truncated_tail_verifies_and_is_caught_only_by_the_anchor() {
        let full = ledger(&[
            event("^a", "blocking", "lead@example.test"),
            retirement("^a"),
        ]);
        let seen = head_of(&full).expect("a ledger with events has a head");

        // Drop the retirement — the event whose removal resurrects the grant.
        let truncated: String = full.lines().take(1).map(|l| format!("{l}\n")).collect();

        let events = parse_and_verify(&truncated)
            .expect("the chain sees nothing wrong with a shorter ledger");
        assert_eq!(
            blocking_grants(&events).len(),
            1,
            "and the revoked grant is live again"
        );

        let violation = continuity_violation(&truncated, &seen)
            .expect("the anchor is what notices the ledger shrank");
        assert!(
            violation.reason.contains("removed from the end"),
            "and says what happened: {}",
            violation.reason
        );
    }

    /// Appending is not a violation, which is what keeps the check usable.
    ///
    /// Without this the anchor could be satisfied by refusing every ledger that
    /// ever changed, and the ledger is append-only by design — growth is the
    /// normal case and must be silent.
    #[test]
    fn a_ledger_that_only_grew_is_not_a_violation() {
        let first = ledger(&[event("^a", "blocking", "lead@example.test")]);
        let seen = head_of(&first).expect("head");
        let grown = ledger(&[
            event("^a", "blocking", "lead@example.test"),
            retirement("^a"),
        ]);

        assert_eq!(continuity_violation(&grown, &seen), None);
        assert_eq!(
            continuity_violation(&first, &seen),
            None,
            "and so is no change"
        );
    }

    /// A ledger rewritten to the same length is caught too.
    ///
    /// The count alone would miss this, which is why the anchor carries a
    /// digest as well: a hostile rewrite that preserves the event count is the
    /// obvious way around a length check.
    #[test]
    fn a_rewritten_event_at_the_same_position_is_a_violation() {
        let original = ledger(&[event("^a", "blocking", "lead@example.test")]);
        let seen = head_of(&original).expect("head");
        let rewritten = ledger(&[event("^a", "blocking", "someone-else@example.test")]);

        let violation = continuity_violation(&rewritten, &seen).expect("caught");
        assert!(
            violation.reason.contains("rewritten rather than appended"),
            "{}",
            violation.reason
        );
    }

    /// Deleting the whole file reads as an empty ledger, and the anchor is the
    /// only thing that can tell that from a repository that never promoted
    /// anything.
    #[test]
    fn an_emptied_ledger_is_a_violation_against_a_head_that_saw_events() {
        let original = ledger(&[event("^a", "blocking", "lead@example.test")]);
        let seen = head_of(&original).expect("head");

        assert!(continuity_violation("", &seen).is_some());
        assert_eq!(head_of(""), None, "and an empty ledger anchors nothing");
    }
}
