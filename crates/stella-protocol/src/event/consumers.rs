//! The signal-consumer ledger — the "every emitted signal names its
//! consumer" invariant, declared instead of assumed.
//!
//! Every [`AgentEvent`](super::AgentEvent) variant names what consumes it.
//! A signal with no consumer is legal, but only as a **declared, issue-cited
//! gap** — never as a silence nobody noticed.
//!
//! This exists because "produced and not consumed" is a repo-wide shape whose
//! every instance so far was found by a bench run paying for it rather than by
//! a test going red: `flip.json` written with nothing reading it (#1536);
//! `verify_done` confirmations tallied but not feeding the halt, which cost
//! `solved_then_timeout` four times on one cert panel before #2661 wired it;
//! the flip transition itself still emitting nothing durable. Each was fixed
//! as whack-a-mole. The ledger makes the class structural (#2701).
//!
//! AGENTS.md holds the invariant's normative text and its number. The number
//! is deliberately **not** cited from this crate: it is a shared cell two PRs
//! can write at once, and a citation that silently resolves to the wrong
//! invariant is worse than none. Cite it by name.
//!
//! It is the direct analogue of [`content_free`] in `stella-store`, which
//! ended the same shape for telemetry egress: a reviewed table, plus tests
//! that enforce it from both sides, plus negative controls proving the harness
//! itself can fail. [`DRAIN_FORMATS`]'s `NotYetBuilt { issue }` arm is the
//! ancestor of [`ConsumerPosture::RecordedOnly`] and
//! [`ConsumerPosture::Unclassified`] here.
//!
//! [`content_free`]: https://github.com/macanderson/stella/blob/main/crates/stella-store/src/content_free.rs
//! [`DRAIN_FORMATS`]: https://github.com/macanderson/stella/blob/main/crates/stella-store/src/content_free.rs
//!
//! # What is machine-checked, and what is not
//!
//! Stating the limit plainly, because a guard believed to prove more than it
//! does is worse than no guard. **Checked:** that the ledger is total over
//! [`KNOWN_TYPE_TAGS`] in both directions, that no tag
//! appears twice, that every gap posture cites an issue, and that the
//! posture/`surfaces` combination is internally coherent. **Not checked:** that
//! a [`ConsumerPosture::Behavioral`] row's `site` string still points at live
//! code. `site` is prose for a human reviewer, and a moved consumer leaves it
//! stale in exactly the way a doc comment goes stale.
//!
//! That is the same epistemic position `DRAIN_FORMATS` holds, and it was enough
//! to end that class: the value is not in proving consumption mechanically, it
//! is in making a PR author answer "what reads this?" before the merge rather
//! than after the bench run.
//!
//! # Why this is a hand-maintained table and not macro-generated
//!
//! `agent_event_tags!` already generates `type_tag()` and `KNOWN_TYPE_TAGS`
//! from one list, and extending it with a third output would make totality
//! compile-enforced rather than test-enforced — strictly stronger. It is
//! deliberately not done, for two reasons worth recording so the next author
//! does not re-litigate it from scratch:
//!
//! - That macro is the wire decoder's source of truth: a tag missing from
//!   `KNOWN_TYPE_TAGS` silently demotes every event carrying it to
//!   [`AgentEvent::Unknown`](super::AgentEvent::Unknown). Widening its
//!   invocation to carry consumer metadata puts editorial churn on the one
//!   list whose stability protects against silent data loss.
//! - `event.rs` sits 35 lines under the 1500-line ratchet with no baseline
//!   entry, and the postures would add roughly forty. The fix would be
//!   splitting the tag macro out — a worthwhile change, but not one to smuggle
//!   into the PR that introduces the ledger.
//!
//! The totality test below fails loudly with the exact row to add, so the
//! practical difference is a red `cargo test` instead of a red `cargo build`.
//!
//! # Adding a variant
//!
//! Add the row here. [`audit_ledger`] is walked by
//! `the_ledger_is_total_over_known_type_tags`, and there is no posture that
//! means "not my problem" — [`ConsumerPosture::Unclassified`] is a debt under
//! a down-only ratchet ([`MAX_UNCLASSIFIED`]), not an escape hatch.

use super::KNOWN_TYPE_TAGS;

/// A surface that renders some signals and not others.
///
/// Deliberately narrow. Three plausible surfaces are **absent** because
/// membership in them is not a per-variant fact:
///
/// - The **TUI** matches `AgentEvent` exhaustively
///   (`stella-tui`'s `model::Model::apply`, `textline::event_line`,
///   `deck::trace_of`), so every variant reaches it by construction.
/// - **Replay** likewise tags every variant (`stella-pipeline`'s
///   `replay::event_signature`).
/// - **Serve** forwards the event stream opaquely rather than selecting
///   variants.
///
/// A field whose value is the same on every row records nothing. This enum
/// names only surfaces that *choose*, which is what makes a surface claim
/// falsifiable — and what gives the observatory's whitelist something to be
/// checked against (#2707).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Surface {
    /// The `stella observe` dashboard, whose journal query names an explicit
    /// subset of `event_type` values.
    Observatory,
    /// The headless engine server, for the arms where it does something
    /// variant-specific rather than forwarding.
    Serve,
}

/// Where an emitted signal's value is realized.
///
/// Postures are ordered by strength of claim. `surfaces` on
/// [`SignalConsumers`] is **orthogonal** to this: a `Behavioral` signal is
/// usually rendered somewhere too, and saying so is not a weaker claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumerPosture {
    /// Something **branches** on this signal — a halt, a verdict, a
    /// projection that feeds a later decision. `site` names the consuming
    /// code so a reviewer can go read it.
    ///
    /// Rendering it is not enough to qualify; the test is whether removing
    /// the consumer would change what the engine *does*, not what a human
    /// sees.
    Behavioral {
        /// The consuming code, as `crate/path.rs::symbol` — prose for a
        /// human, not a checked reference. See the module doc's limits.
        site: &'static str,
    },

    /// Rendering is the realized value, deliberately. Requires a non-empty
    /// `surfaces` list: a signal claimed as surfaced that reaches no
    /// selecting surface is [`ConsumerPosture::RecordedOnly`] wearing a
    /// friendlier name.
    Surfaced,

    /// Persisted and replay-tagged, and nothing else. A **declared** gap: the
    /// `DRAIN_FORMATS::NotYetBuilt` analog.
    ///
    /// Legal, and sometimes correct — some signals genuinely exist for the
    /// record. But it must cite the issue where "wire a consumer or annotate
    /// this as deliberate" is being decided, so the gap is reviewable rather
    /// than merely old.
    RecordedOnly {
        /// The tracking issue, as `#1234`. Empty fails the audit.
        issue: &'static str,
    },

    /// Nobody has audited this signal's consumers yet.
    ///
    /// The honest posture for a row that exists only to keep the ledger
    /// total. It is **not** a synonym for [`ConsumerPosture::RecordedOnly`] —
    /// claiming "nothing consumes this" without looking would put a false
    /// statement in the ledger, and a ledger that lies is worse than no
    /// ledger. Under a down-only ratchet ([`MAX_UNCLASSIFIED`]).
    Unclassified {
        /// The tracking issue, as `#1234`. Empty fails the audit.
        issue: &'static str,
    },
}

/// One signal's declared consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalConsumers {
    /// Must be a member of [`KNOWN_TYPE_TAGS`].
    pub type_tag: &'static str,
    /// What realizes this signal's value.
    pub posture: ConsumerPosture,
    /// Selecting surfaces that render it. Orthogonal to `posture`; see
    /// [`Surface`] for why the exhaustive renderers are not listed.
    pub surfaces: &'static [Surface],
}

/// The ledger: one row per [`AgentEvent`](super::AgentEvent) variant.
///
/// Kept in `agent_event_tags!` order so the two lists diff against each other
/// by eye. The totality test enforces the set; the ordering is a courtesy.
pub const SIGNAL_CONSUMERS: &[SignalConsumers] = &[
    SignalConsumers {
        type_tag: "stage",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[Surface::Observatory],
    },
    // Exemplar — `Surfaced`. Assistant prose has no behavioral consumer by
    // design: the engine does not branch on what the model said in English.
    SignalConsumers {
        type_tag: "text",
        posture: ConsumerPosture::Surfaced,
        surfaces: &[Surface::Observatory],
    },
    SignalConsumers {
        type_tag: "text_delta",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "reasoning",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[Surface::Observatory],
    },
    SignalConsumers {
        type_tag: "tool_start",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[Surface::Observatory],
    },
    // Exemplar — `Behavioral`. Projected into the store's `tool_calls` table
    // in the same transaction that records the event.
    //
    // Worth stating what is *not* the consumer here, because the intuitive
    // answer is wrong and the mistake is instructive: loop detection does not
    // read this signal. It reads the transcript
    // (`stella-core/src/driver/loop_evidence.rs::recent_call_records` over
    // `CompletionMessage`s), which is a different plane that happens to carry
    // the same facts. Citing it here would have described a dependency that
    // does not exist — and severing the event would not have been caught.
    SignalConsumers {
        type_tag: "tool_result",
        posture: ConsumerPosture::Behavioral {
            site: "stella-store/src/tool_calls.rs::project_event",
        },
        surfaces: &[Surface::Observatory],
    },
    SignalConsumers {
        type_tag: "speculation_discarded",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[Surface::Observatory],
    },
    SignalConsumers {
        type_tag: "retry",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "steered",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "turn_parked",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[Surface::Observatory],
    },
    SignalConsumers {
        type_tag: "turn_woken",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[Surface::Observatory],
    },
    SignalConsumers {
        type_tag: "loop_detected",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "budget_denied",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "retries_exhausted",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "policy_decision",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "compaction",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "budget_tick",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "step_usage",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "usage_incomplete",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "goal_verdict",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "provider_fallback",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "file_change",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "context_recall",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "context_write",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "block_registered",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "step_manifest",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "proof",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "verdict",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "scope_review",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "hunk_review",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "ask_user",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "media_progress",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "media_complete",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "commit",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "pr",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "task_update",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "sub_agent",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    SignalConsumers {
        type_tag: "error",
        posture: ConsumerPosture::Unclassified { issue: "#2703" },
        surfaces: &[],
    },
    // Exemplar — `Behavioral`. The session journal classifies this as a
    // transition, which is what makes it flush durably instead of buffering:
    // the signal decides persistence timing, not just content.
    SignalConsumers {
        type_tag: "complete",
        posture: ConsumerPosture::Behavioral {
            site: "stella-store/src/journal.rs::JournalRecord::is_transition",
        },
        surfaces: &[],
    },
];

/// How many rows may still be [`ConsumerPosture::Unclassified`].
///
/// A **down-only** ratchet, legitimate for the one reason a ratchet ever is
/// here: the audit predates the guard, so this records a debt that already
/// existed rather than granting new permission. The same argument
/// `scripts/typed-errors-baseline.txt` makes for its own count.
///
/// The test asserts equality, not `<=`. Lowering it is the point and must
/// show up as a diff; raising it means a new variant was filed away unread,
/// which is the exact move the ledger exists to prevent. #2703 drives it to
/// zero, after which a new variant cannot reach `Unclassified` at all.
pub const MAX_UNCLASSIFIED: usize = 36;

/// What the audit found wrong with one ledger row.
///
/// Returned rather than panicked, so the harness is itself testable — see
/// `the_audit_catches_a_malformed_row`. A check that cannot be shown to fail
/// is a claim, not a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerViolation {
    /// A row names a tag that is not a real `AgentEvent` wire tag, so it
    /// documents a signal that does not exist.
    UnknownTag { type_tag: String },
    /// A wire tag has no row: a signal nobody declared a consumer for.
    MissingRow { type_tag: String },
    /// Two rows claim the same tag, so one of them is dead documentation.
    DuplicateRow { type_tag: String },
    /// A gap posture cites no issue, making it an undeclared gap wearing a
    /// declared gap's clothes.
    UncitedGap {
        type_tag: String,
        posture: &'static str,
    },
    /// An issue citation is not in `#1234` form.
    MalformedIssue { type_tag: String, issue: String },
    /// A `Behavioral` row names no consuming site, so the claim cannot be
    /// checked by the human it exists for.
    UnnamedSite { type_tag: String },
    /// A `Surfaced` row reaches no selecting surface — it is `RecordedOnly`
    /// under a kinder name, which is exactly the euphemism this ledger is
    /// meant to strip out.
    SurfacedNowhere { type_tag: String },
    /// A `RecordedOnly` row lists a surface, contradicting itself: something
    /// does render it, so the posture is at least `Surfaced`.
    RecordedYetSurfaced { type_tag: String },
}

impl std::fmt::Display for LedgerViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTag { type_tag } => write!(
                f,
                "`{type_tag}` is in SIGNAL_CONSUMERS but is not a known AgentEvent \
                 wire tag — either the tag was renamed and this row was left behind, \
                 or it is a typo that documents a signal nobody emits"
            ),
            Self::MissingRow { type_tag } => write!(
                f,
                "`{type_tag}` is an AgentEvent wire tag with no row in \
                 SIGNAL_CONSUMERS — every emitted signal names its consumer \
                 (every emitted signal names its consumer). Add a row saying what reads it; if the honest \
                 answer is \"nobody has looked\", that is \
                 ConsumerPosture::Unclassified with a tracking issue, not an omission"
            ),
            Self::DuplicateRow { type_tag } => write!(
                f,
                "`{type_tag}` has more than one row in SIGNAL_CONSUMERS — one of \
                 them is documentation nothing keeps true"
            ),
            Self::UncitedGap { type_tag, posture } => write!(
                f,
                "`{type_tag}` is {posture} but cites no issue — a gap is only \
                 declared if someone can go read the decision. File the issue and \
                 name it here"
            ),
            Self::MalformedIssue { type_tag, issue } => write!(
                f,
                "`{type_tag}` cites `{issue}`, which is not a `#1234` issue \
                 reference — the citation has to resolve to something"
            ),
            Self::UnnamedSite { type_tag } => write!(
                f,
                "`{type_tag}` claims a behavioral consumer but names no site — \
                 the claim exists to be checked, so it has to say where to look"
            ),
            Self::SurfacedNowhere { type_tag } => write!(
                f,
                "`{type_tag}` is Surfaced but lists no selecting surface. Note \
                 that Surface deliberately omits the TUI and replay, which render \
                 every variant: if those are the only consumers, the honest \
                 posture is RecordedOnly with a tracking issue"
            ),
            Self::RecordedYetSurfaced { type_tag } => write!(
                f,
                "`{type_tag}` is RecordedOnly yet lists a surface — something does \
                 render it, so the posture is at least Surfaced"
            ),
        }
    }
}

/// Whether `issue` is a `#1234` reference.
fn is_issue_reference(issue: &str) -> bool {
    issue
        .strip_prefix('#')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Audit a ledger against a set of wire tags, reporting every problem found.
///
/// Takes both as parameters rather than reading the consts directly so the
/// negative-control tests can feed it deliberately broken input — the same
/// reason `content_free::audit_encoder` takes its encoder by reference.
#[must_use]
pub fn audit_ledger(ledger: &[SignalConsumers], known_tags: &[&str]) -> Vec<LedgerViolation> {
    let mut violations = Vec::new();
    let mut seen: Vec<&str> = Vec::with_capacity(ledger.len());

    for row in ledger {
        if seen.contains(&row.type_tag) {
            violations.push(LedgerViolation::DuplicateRow {
                type_tag: row.type_tag.to_string(),
            });
        }
        seen.push(row.type_tag);

        if !known_tags.contains(&row.type_tag) {
            violations.push(LedgerViolation::UnknownTag {
                type_tag: row.type_tag.to_string(),
            });
        }

        match &row.posture {
            ConsumerPosture::Behavioral { site } if site.trim().is_empty() => {
                violations.push(LedgerViolation::UnnamedSite {
                    type_tag: row.type_tag.to_string(),
                });
            }
            ConsumerPosture::Surfaced if row.surfaces.is_empty() => {
                violations.push(LedgerViolation::SurfacedNowhere {
                    type_tag: row.type_tag.to_string(),
                });
            }
            ConsumerPosture::RecordedOnly { issue } => {
                if !row.surfaces.is_empty() {
                    violations.push(LedgerViolation::RecordedYetSurfaced {
                        type_tag: row.type_tag.to_string(),
                    });
                }
                violations.extend(audit_citation(row.type_tag, issue, "RecordedOnly"));
            }
            ConsumerPosture::Unclassified { issue } => {
                violations.extend(audit_citation(row.type_tag, issue, "Unclassified"));
            }
            ConsumerPosture::Behavioral { .. } | ConsumerPosture::Surfaced => {}
        }
    }

    for tag in known_tags {
        if !seen.contains(tag) {
            violations.push(LedgerViolation::MissingRow {
                type_tag: (*tag).to_string(),
            });
        }
    }

    violations
}

/// The shared citation check for the two gap postures.
fn audit_citation(
    type_tag: &'static str,
    issue: &str,
    posture: &'static str,
) -> Option<LedgerViolation> {
    if issue.trim().is_empty() {
        Some(LedgerViolation::UncitedGap {
            type_tag: type_tag.to_string(),
            posture,
        })
    } else if !is_issue_reference(issue) {
        Some(LedgerViolation::MalformedIssue {
            type_tag: type_tag.to_string(),
            issue: issue.to_string(),
        })
    } else {
        None
    }
}

/// Every tag the ledger declares as reaching `surface`.
///
/// The read side of the ledger, for a surface that wants to check its own
/// selection against what it claims to show (#2707).
#[must_use]
pub fn tags_surfaced_by(surface: Surface) -> Vec<&'static str> {
    SIGNAL_CONSUMERS
        .iter()
        .filter(|row| row.surfaces.contains(&surface))
        .map(|row| row.type_tag)
        .collect()
}

/// How many rows are still awaiting an audit. Watched by [`MAX_UNCLASSIFIED`].
#[must_use]
pub fn unclassified_count() -> usize {
    SIGNAL_CONSUMERS
        .iter()
        .filter(|row| matches!(row.posture, ConsumerPosture::Unclassified { .. }))
        .count()
}

/// The ledger's own audit, against the real tag list.
#[must_use]
pub fn audit() -> Vec<LedgerViolation> {
    audit_ledger(SIGNAL_CONSUMERS, KNOWN_TYPE_TAGS)
}

#[cfg(test)]
mod tests;
