//! Ingest provenance lineages — remembering where records came from, and
//! deciding when their source has drifted (#2683, first slice).
//!
//! Every `stella ingest <file>` leaves records behind whose accuracy depends on
//! a file that keeps changing after the run. Nothing here assumes anything
//! about *which* file: an agent-instruction file, a runbook, and a scratch note
//! someone wrote purely as an ingest vehicle are all just *ingested source
//! files*. This module owns the pure half of tracking them:
//!
//! - a [`Lineage`] is the durable memory of one ingested source file — the
//!   content hash it had, when it was read, and which proposal candidates it
//!   produced;
//! - a [`LineageLedger`] holds one lineage per source path, replaced (never
//!   merged) on re-ingest, because a re-ingest re-reads the whole file;
//! - [`Lineage::drift`] is the staleness decision: given the hash the source
//!   has *now*, does this lineage warrant an alert?
//!
//! # Dismissal is per-file, permanent, and about alerts only
//!
//! Whether drift matters is entirely the user's call, per file. A user may
//! author a file *as scaffolding* — ingest it, delete it, keep the records —
//! and for that file a deletion alert is pure noise. So each lineage carries an
//! [`AlertState`], and a dismissed lineage produces no drift ever again, in any
//! session, until the user re-arms it. Dismissal says nothing about the records
//! themselves: they stay live and recallable. It silences the messenger for one
//! file, and only that one — the same honor-it-completely, generalize-it-never
//! contract as memory tombstones.
//!
//! A fresh ingest of the same path re-arms alerts by default: re-ingesting is
//! the user re-declaring the file a live source, which contradicts the earlier
//! dismissal. The caller's `keep_dismissed` escape hatch preserves the old
//! state for the user who dismissed on purpose and re-ingests anyway.
//!
//! # Purity
//!
//! Like the rest of `stella-core` this module performs no I/O. Hashing a real
//! file, running `git`, persisting the ledger, and pushing notifications all
//! live in the CLI adapter; everything here is a deterministic function of its
//! inputs, so the dismissal contract is testable without a tree.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Whether staleness alerts for one lineage are live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    /// Drift in the source file produces an alert.
    Active,
    /// The user said "stop telling me about this file" — no alert, ever,
    /// until explicitly restored or the file is re-ingested.
    Dismissed,
}

/// The provenance of one ingested source file: what was read, when, and what
/// it produced. One lineage per source path; a re-ingest replaces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lineage {
    /// The content hash the source had when it was ingested — `git:<oid>` when
    /// git computed it (blob hash, clean filters applied), `sha256:<hex>`
    /// otherwise. Compared as an opaque string: two hashes under different
    /// schemes read as drift, which errs toward telling the user.
    pub source_hash: String,
    /// The repository's short HEAD at ingest time, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// When the ingest ran, RFC-3339.
    pub ingested_at: String,
    /// The ingest run that produced this lineage (`ing_<hex>`).
    pub ingest_run_id: String,
    /// The proposal candidate ids the run minted from this file, so the trail
    /// from "this file changed" to "these are the records it fed" is auditable.
    #[serde(default)]
    pub candidate_ids: Vec<String>,
    /// Whether drift in this source still alerts.
    pub alerts: AlertState,
}

/// What a staleness check found for one lineage. `None` from
/// [`Lineage::drift`] means "nothing worth telling the user".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drift {
    /// The source's content hash no longer matches the ingested one.
    Changed {
        /// The hash at ingest.
        from: String,
        /// The hash now.
        to: String,
    },
    /// The source no longer exists (or can no longer be read at all).
    Missing {
        /// The hash at ingest.
        from: String,
    },
}

impl Lineage {
    /// The staleness decision: given the hash the source has now (`None` when
    /// the file is gone), does this lineage warrant an alert?
    ///
    /// A dismissed lineage never does — that is the whole dismissal contract,
    /// and it must hold for deletion as much as for change: the file authored
    /// as scaffolding and deleted on purpose is the motivating case.
    #[must_use]
    pub fn drift(&self, current: Option<&str>) -> Option<Drift> {
        if self.alerts == AlertState::Dismissed {
            return None;
        }
        match current {
            Some(now) if now == self.source_hash => None,
            Some(now) => Some(Drift::Changed {
                from: self.source_hash.clone(),
                to: now.to_string(),
            }),
            None => Some(Drift::Missing {
                from: self.source_hash.clone(),
            }),
        }
    }
}

/// The per-workspace ledger: one [`Lineage`] per ingested source path.
///
/// Keys are the same workspace-relative, forward-slashed paths ingest displays,
/// so the path a user sees in an alert is exactly the string that dismisses it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageLedger {
    /// Lineages by source path.
    #[serde(default)]
    pub sources: BTreeMap<String, Lineage>,
}

/// Asking to dismiss or restore alerts for a path that was never ingested.
/// Carries the known paths so the caller can say what *would* work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSource {
    /// The path that matched no lineage.
    pub source_path: String,
    /// Every path the ledger does know, sorted.
    pub known: Vec<String>,
}

impl LineageLedger {
    /// Record a fresh ingest of `source_path`, replacing any previous lineage.
    ///
    /// Alerts re-arm by default even over a dismissed lineage: re-ingesting is
    /// the user re-declaring this file a live source, and honoring the stale
    /// dismissal would silently swallow the very drift they just showed
    /// interest in. `keep_dismissed` preserves the old state instead, for the
    /// caller who was told to.
    pub fn record_ingest(&mut self, source_path: &str, mut lineage: Lineage, keep_dismissed: bool) {
        if keep_dismissed
            && let Some(previous) = self.sources.get(source_path)
            && previous.alerts == AlertState::Dismissed
        {
            lineage.alerts = AlertState::Dismissed;
        }
        self.sources.insert(source_path.to_string(), lineage);
    }

    /// Set alerts for one lineage. Scoped to exactly that path — every other
    /// lineage keeps its own state.
    pub fn set_alerts(&mut self, source_path: &str, state: AlertState) -> Result<(), UnknownSource> {
        match self.sources.get_mut(source_path) {
            Some(lineage) => {
                lineage.alerts = state;
                Ok(())
            }
            None => Err(UnknownSource {
                source_path: source_path.to_string(),
                known: self.sources.keys().cloned().collect(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lineage(hash: &str, alerts: AlertState) -> Lineage {
        Lineage {
            source_hash: hash.to_string(),
            commit: Some("abc1234".to_string()),
            ingested_at: "2026-08-10T00:00:00Z".to_string(),
            ingest_run_id: "ing_1".to_string(),
            candidate_ids: vec!["pkg-manager-a1b2c3d4".to_string()],
            alerts,
        }
    }

    #[test]
    fn an_unchanged_source_produces_no_drift() {
        let l = lineage("git:aaaa", AlertState::Active);
        assert_eq!(l.drift(Some("git:aaaa")), None);
    }

    #[test]
    fn a_changed_source_names_both_hashes() {
        let l = lineage("git:aaaa", AlertState::Active);
        assert_eq!(
            l.drift(Some("git:bbbb")),
            Some(Drift::Changed {
                from: "git:aaaa".to_string(),
                to: "git:bbbb".to_string(),
            })
        );
    }

    #[test]
    fn a_missing_source_reports_deletion() {
        let l = lineage("git:aaaa", AlertState::Active);
        assert_eq!(
            l.drift(None),
            Some(Drift::Missing {
                from: "git:aaaa".to_string(),
            })
        );
    }

    /// The dismissal contract: no alert for change *or* deletion, ever. The
    /// deletion half is the one that matters most — a file authored purely as
    /// an ingest vehicle is deleted on purpose, and nagging about it forever
    /// is the failure dismissal exists to end.
    #[test]
    fn a_dismissed_lineage_never_drifts() {
        let l = lineage("git:aaaa", AlertState::Dismissed);
        assert_eq!(l.drift(Some("git:bbbb")), None);
        assert_eq!(l.drift(None), None);
    }

    /// Dismissal is scoped to its own path: the other lineage keeps alerting.
    #[test]
    fn dismissal_is_scoped_to_one_source() {
        let mut ledger = LineageLedger::default();
        ledger.record_ingest("a.md", lineage("git:aaaa", AlertState::Active), false);
        ledger.record_ingest("b.md", lineage("git:cccc", AlertState::Active), false);
        ledger
            .set_alerts("a.md", AlertState::Dismissed)
            .expect("a.md is known");

        assert_eq!(ledger.sources["a.md"].drift(None), None);
        assert!(ledger.sources["b.md"].drift(Some("git:dddd")).is_some());
    }

    /// Re-ingesting a dismissed source re-arms its alerts: the action
    /// contradicts the dismissal.
    #[test]
    fn re_ingest_re_arms_a_dismissed_lineage() {
        let mut ledger = LineageLedger::default();
        ledger.record_ingest("a.md", lineage("git:aaaa", AlertState::Dismissed), false);
        ledger.record_ingest("a.md", lineage("git:bbbb", AlertState::Active), false);
        assert_eq!(ledger.sources["a.md"].alerts, AlertState::Active);
    }

    /// The escape hatch: `keep_dismissed` carries the dismissal across a
    /// re-ingest — but only a dismissal. It cannot dismiss an active lineage.
    #[test]
    fn keep_dismissed_preserves_only_an_existing_dismissal() {
        let mut ledger = LineageLedger::default();
        ledger.record_ingest("a.md", lineage("git:aaaa", AlertState::Dismissed), false);
        ledger.record_ingest("a.md", lineage("git:bbbb", AlertState::Active), true);
        assert_eq!(ledger.sources["a.md"].alerts, AlertState::Dismissed);

        ledger.record_ingest("b.md", lineage("git:cccc", AlertState::Active), false);
        ledger.record_ingest("b.md", lineage("git:dddd", AlertState::Active), true);
        assert_eq!(ledger.sources["b.md"].alerts, AlertState::Active);
    }

    /// Restore is the reverse act: an explicitly re-armed lineage alerts again.
    #[test]
    fn restore_re_arms_alerts() {
        let mut ledger = LineageLedger::default();
        ledger.record_ingest("a.md", lineage("git:aaaa", AlertState::Dismissed), false);
        ledger
            .set_alerts("a.md", AlertState::Active)
            .expect("a.md is known");
        assert!(ledger.sources["a.md"].drift(None).is_some());
    }

    /// Dismissing a path that was never ingested is an error that names what
    /// would work, not a silent no-op that leaves the user believing they are
    /// covered.
    #[test]
    fn dismissing_an_unknown_source_lists_the_known_ones() {
        let mut ledger = LineageLedger::default();
        ledger.record_ingest("a.md", lineage("git:aaaa", AlertState::Active), false);
        let err = ledger
            .set_alerts("missing.md", AlertState::Dismissed)
            .expect_err("missing.md was never ingested");
        assert_eq!(err.source_path, "missing.md");
        assert_eq!(err.known, vec!["a.md".to_string()]);
    }

    /// The ledger round-trips through serde byte-for-byte semantics — it is
    /// persisted as JSON by the CLI, and a field silently dropped in either
    /// direction would forget a dismissal, which the contract forbids.
    #[test]
    fn the_ledger_round_trips_through_json() {
        let mut ledger = LineageLedger::default();
        ledger.record_ingest("a.md", lineage("git:aaaa", AlertState::Dismissed), false);
        ledger.record_ingest("docs/b.md", lineage("sha256:ff", AlertState::Active), false);
        let json = serde_json::to_string(&ledger).expect("serializes");
        let back: LineageLedger = serde_json::from_str(&json).expect("parses");
        assert_eq!(back, ledger);
    }

    /// A ledger written by a build that predates optional fields still parses:
    /// `commit` and `candidate_ids` default rather than failing the file, so a
    /// forward-compatible reader never throws away every recorded dismissal
    /// over one absent key.
    #[test]
    fn a_minimal_lineage_parses_with_defaults() {
        let json = r#"{
            "sources": {
                "a.md": {
                    "source_hash": "git:aaaa",
                    "ingested_at": "2026-08-10T00:00:00Z",
                    "ingest_run_id": "ing_1",
                    "alerts": "dismissed"
                }
            }
        }"#;
        let ledger: LineageLedger = serde_json::from_str(json).expect("parses");
        let lineage = &ledger.sources["a.md"];
        assert_eq!(lineage.commit, None);
        assert!(lineage.candidate_ids.is_empty());
        assert_eq!(lineage.alerts, AlertState::Dismissed);
    }
}
