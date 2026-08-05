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
//! of file plus history, but no such rewrite can survive
//! [`parse_and_verify`] against a previously seen head digest.
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

/// One immutable enforcement transition. Append-only: current enforcement
/// grants are always a fold over the whole ledger, and every event names who
/// granted what, why, and under which policy version.
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

/// Parse a promotions ledger and verify its hash chain: every line must
/// parse, `seq` must be contiguous from 1, and each event's `prev` must equal
/// the SHA-256 of the previous line's exact bytes. An edited, dropped, or
/// reordered line breaks the chain at the first divergence.
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
    latest.retain(|_, event| event.to == "blocking");
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
}
