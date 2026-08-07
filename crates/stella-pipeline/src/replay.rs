//! Golden-trajectory replay harness. Two jobs,
//! both pure: **validate** that an `AgentEvent` stream obeys the protocol's
//! structural invariants, and **structurally diff** two streams (kinds + order,
//! ignoring volatile fields like durations and exact costs) so a Rust-stack
//! trajectory can be asserted equivalent to a reference one.
//!
//! # Golden trajectories, and what is still missing
//!
//! [`golden`] adds the fixture format those comparisons are made against: a
//! recorded stream plus a manifest naming what produced it, gated on load so a
//! malformed or truncated recording fails loudly instead of becoming a weaker
//! yardstick. `tests/fixtures/golden/` holds recordings made from this
//! workspace's own pipeline — a **drift baseline**, which detects a stage that
//! stopped being emitted or a tool that changed name, but is not independent
//! evidence, because both sides are the same code.
//!
//! A **reference trajectory** — recorded from an independent engine — remains
//! outstanding, and the blocker is not access: it is that the reference engine
//! emits a different wire format (untyped stage labels, no tool `call_id`, a
//! `result` terminator instead of `complete`). Recording it requires an adapter
//! onto this protocol first. `docs/spec/replay-golden-trajectories.md` specifies
//! that adapter contract, and `tests/reference_conformance.rs` pins it
//! executably. The synthetic fixtures directly under `tests/fixtures/` remain
//! what they always were: exercises for the invariants and the differ.
//!
//! # Torn tails (L-T1)
//!
//! A crashed writer must never poison a reader: [`parse_jsonl`] tolerates a
//! single unparseable *final* line (a torn tail) by dropping it, while a
//! malformed *interior* line is a real error. Envelope evolution is
//! additive-only, so parsing is forward-tolerant by construction: serde
//! ignores unknown fields on the structs that opt in, and an unknown event
//! *variant* is preserved as [`stella_protocol::AgentEvent::Unknown`] rather
//! than failing the line — so a stream from a newer stella replays here
//! intact. `tests/fixtures/from_a_newer_stella.jsonl` pins that.
//!
//! What remains fatal is real damage: a line that is not valid JSON, or one
//! whose `"type"` this build *does* know but whose body does not fit that
//! variant. Both mean the record is wrong, not merely newer.

pub mod golden;
pub mod ground_truth;
pub mod independence;
pub mod reference_adapter;

use ground_truth::PendingPass;
use independence::GraderCohorts;
use stella_protocol::{AgentEvent, OracleObservation, ProofTree, StageKind, VerdictEvidence};

/// Verifier-calibration tallies folded from recorded event streams (#871).
///
/// The event store already persists every verdict (with its ladder snapshot,
/// #865) and every PR/CI observation — so calibration is a *reading* of what
/// exists, not a new write path. A `Verdict` with `deterministic: false,
/// passed: true` is a model verifier's PASS; the next **terminal** CI verdict in
/// the same stream (`Pr { ci: Some(Passing | Failing) }`) reconciles every
/// unreconciled pass before it. Deterministic passes are tallied identically,
/// because a false-positive *rate* means nothing without the cohort it should
/// be compared against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CalibrationReport {
    /// Model-verifier PASS verdicts observed.
    pub verifier_passes: u32,
    /// …of which a later terminal CI verdict reconciled.
    pub verifier_reconciled: u32,
    /// …reconciled as CI-FAILING: the verifier approved work CI rejected.
    pub verifier_false_positives: u32,
    /// Deterministic (ladder) passes observed — the comparison cohort.
    pub deterministic_passes: u32,
    pub deterministic_reconciled: u32,
    pub deterministic_false_positives: u32,
    /// Verdicts carrying a ladder snapshot — the denominator for the
    /// verifier-alone rate below (#1295). Verdicts recorded before snapshots
    /// existed (#865) are excluded rather than assumed either way.
    pub snapshotted_verdicts: u32,
    /// …of which NOTHING mechanical corroborated the result: no flip, and no
    /// touched-test run observed green.
    ///
    /// This is `LadderInputs::verifier_pass_stands_alone` read back off the
    /// record, and it is the number #1295 turns on: the gated "send it back
    /// for evidence" behaviour is worth a turn only where this is the
    /// suspicious minority. Counted over every snapshotted verdict, not only
    /// verifier passes, because the question is how often the *condition* holds
    /// — which is what decides whether gating on it taxes every turn.
    pub uncorroborated_verdicts: u32,
    /// Model-verifier PASSes where the condition held — the turns
    /// `verifier_evidence_demand` would actually have sent back,
    /// and the ones the pipeline relabels UNVERIFIED while it is off.
    pub verifier_passes_standing_alone: u32,
    /// …of the reconciled verifier passes, how many were settled by a **revert**
    /// rather than by CI (#1293).
    ///
    /// Counted apart because it is a different and stronger claim. CI failing
    /// can mean a flake, a neighbouring change, or an infrastructure day; a
    /// human reverting a commit is a person deciding, later and with more
    /// information, that the work should not have landed. A verifier whose false
    /// positives are mostly reverts is failing differently from one whose
    /// false positives are mostly red CI, and a single number would hide it.
    pub verifier_reverted: u32,
    /// The same, for the deterministic cohort.
    pub deterministic_reverted: u32,
    /// The verifier cohort above, partitioned by who graded it (#1865):
    /// self-graded / independent / unknown, with unknown never assumed into
    /// either measured cohort. The three partitions sum to the unpartitioned
    /// verifier tallies.
    pub by_grader: GraderCohorts,
}

impl CalibrationReport {
    /// Combine per-session reports into a workspace total.
    pub fn merge(mut self, other: Self) -> Self {
        self.verifier_passes += other.verifier_passes;
        self.verifier_reconciled += other.verifier_reconciled;
        self.verifier_false_positives += other.verifier_false_positives;
        self.deterministic_passes += other.deterministic_passes;
        self.deterministic_reconciled += other.deterministic_reconciled;
        self.deterministic_false_positives += other.deterministic_false_positives;
        self.snapshotted_verdicts += other.snapshotted_verdicts;
        self.uncorroborated_verdicts += other.uncorroborated_verdicts;
        self.verifier_passes_standing_alone += other.verifier_passes_standing_alone;
        self.verifier_reverted += other.verifier_reverted;
        self.deterministic_reverted += other.deterministic_reverted;
        self.by_grader = self.by_grader.merge(other.by_grader);
        self
    }

    /// How often a verdict had no deterministic corroboration behind it, over
    /// the verdicts that recorded enough to say (#1295). `None` when nothing
    /// was snapshotted — an unmeasured rate is reported as unmeasured, never
    /// as zero.
    ///
    /// **The number that decides
    /// `PipelineConfig::verifier_evidence_demand`.** A minority
    /// rate is the situation the send-back was designed for; a majority rate
    /// is the Terminal-Bench measurement that switched it off, and leaving it
    /// off is then the answer with the reason recorded beside it.
    #[must_use]
    pub fn uncorroborated_rate(&self) -> Option<f64> {
        (self.snapshotted_verdicts > 0)
            .then(|| f64::from(self.uncorroborated_verdicts) / f64::from(self.snapshotted_verdicts))
    }

    /// The measured false-positive rate over RECONCILED verifier passes, or
    /// `None` when nothing was reconciled — an unmeasured rate is reported
    /// as unmeasured, never as zero.
    pub fn verifier_false_positive_rate(&self) -> Option<f64> {
        (self.verifier_reconciled > 0)
            .then(|| f64::from(self.verifier_false_positives) / f64::from(self.verifier_reconciled))
    }

    /// Same rate for the deterministic cohort.
    pub fn deterministic_false_positive_rate(&self) -> Option<f64> {
        (self.deterministic_reconciled > 0).then(|| {
            f64::from(self.deterministic_false_positives) / f64::from(self.deterministic_reconciled)
        })
    }
}

/// Render a [`CalibrationReport`] for a human — the `stella calibration`
/// body. States unmeasured rates as unmeasured and names the cohort sizes,
/// so a rate is never read without its denominator.
pub fn render_calibration(report: &CalibrationReport) -> String {
    use independence::cohort_line as cohort;
    // #1295: the rate that decides whether asking for evidence is worth a
    // turn. Rendered beside the calibration cohorts because it is read for
    // the same reason — to replace an argument about the verifier with a number
    // — and stated with its denominator, like every other rate here.
    let verifier_alone = match report.uncorroborated_rate() {
        Some(rate) => format!(
            "  verifier-alone rate: {:.0}% of {} snapshotted verdict(s) had no flip and no green \
             test ({} of them were model-verifier PASSes)\n  \
             → a MINORITY is the condition `verifier_evidence_demand` was built for; \
             a majority reproduces the measurement that switched it off (#1295)",
            100.0 * rate,
            report.snapshotted_verdicts,
            report.verifier_passes_standing_alone,
        ),
        None => "  verifier-alone rate: unmeasured (no verdict carried a ladder snapshot yet)"
            .to_string(),
    };
    // #1293: reverts are counted apart from CI, because they are a different
    // and stronger statement — a human deciding later that the work should
    // not have landed. Stated only when there are any: a zero here would read
    // as "no work was reverted", when the ordinary case is that the caller
    // gathered no revert evidence at all.
    let reverts = match (report.verifier_reverted, report.deterministic_reverted) {
        (0, 0) => String::new(),
        (verifier, deterministic) => format!(
            "\n  of those, settled by a REVERT (a human said it was wrong, not CI): \
             {verifier} verifier, {deterministic} deterministic"
        ),
    };
    // #1865: the model-verifier line above, partitioned by who graded it.
    // Rendered only once there is a verifier pass to partition — the section
    // answers a question about recorded verifier verdicts, and with none
    // recorded there is no cohort to compare.
    let grader = if report.verifier_passes > 0 {
        format!(
            "\n{}",
            independence::render_grader_cohorts(&report.by_grader)
        )
    } else {
        String::new()
    };
    format!(
        "verifier calibration (#871) — passes reconciled against later CI verdicts and reverts\n\
         {}\n{}{reverts}{grader}\n\
         note: a pass is reconciled by a terminal CI verdict or by a revert of\n\
         a commit it covers, from any session or from the git history (#1293).\n\
         A pass no evidence reaches stays UNRECONCILED and out of every\n\
         denominator — absence of a revert is never a confirmation.\n\
         {verifier_alone}",
        cohort(
            "  model verifier  ",
            report.verifier_passes,
            report.verifier_reconciled,
            report.verifier_false_positives
        ),
        cohort(
            "  deterministic",
            report.deterministic_passes,
            report.deterministic_reconciled,
            report.deterministic_false_positives
        ),
    )
}

/// Fold one event stream into a [`CalibrationReport`] (#871), discarding the
/// passes it could not settle.
///
/// Reconciliation here is stream-local and forward-only: a terminal CI verdict
/// covers the passes that PRECEDED it (the PR carries the session's adopted
/// work), and passes after the last terminal CI observation stay
/// unreconciled. `Pending`/`Running` reconcile nothing — absence of a
/// verdict is not a verdict.
///
/// Use [`calibration_pending`] to keep those trailing passes and settle them
/// against evidence that arrives after the session ends (#1293); this
/// signature is the pre-#1293 one, kept for callers that only want the
/// in-stream reading.
pub fn calibration(events: &[AgentEvent]) -> CalibrationReport {
    calibration_pending("", events).0
}

/// [`calibration`], plus the pass verdicts this stream could not settle —
/// each carrying the commits and PRs the session recorded after it (#1293).
///
/// The returned [`PendingPass`] values are what makes a *late* verdict usable:
/// a CI run that finishes after the session, a revert that lands next week, a
/// terminal verdict recorded in some other session's stream. Feed them to
/// [`ground_truth::reconcile`] with whatever evidence exists.
///
/// `session` is carried through onto each pending pass purely so a caller can
/// report which records a late verdict settled; the fold does not read it.
///
/// The attribution rule for artifacts mirrors the CI one already in place: a
/// commit or PR recorded at index *i* covers every unsettled pass before it.
/// That is deliberately generous, and it is the only rule available — a
/// session's events do not carry which verdict a given commit implements.
/// The consequence is stated rather than hidden: a session that produced two
/// passes and one commit attributes that commit's fate to both, so a revert
/// there counts two false positives. Sessions that adopt one change are the
/// ordinary case, and over-attributing a revert errs toward reporting the
/// verifier as *worse* than it is — the safe direction for an instrument whose
/// measured failure is leniency.
pub fn calibration_pending(
    session: &str,
    events: &[AgentEvent],
) -> (CalibrationReport, Vec<PendingPass>) {
    use stella_protocol::CiStatus;
    let mut report = CalibrationReport::default();
    // Unsettled pass verdicts, in order, with the artifacts recorded since
    // each one. `verifier` distinguishes the two cohorts.
    let mut pending: Vec<PendingPass> = Vec::new();
    for event in events {
        match event {
            // Every verdict — pass or fail — contributes to the verifier-alone
            // denominator (#1295): the question is how often the pipeline
            // reaches a verdict with nothing mechanical behind it, and a
            // failing turn is as much a part of that population as a passing
            // one. The pass cohorts below are counted separately, on the same
            // pass.
            AgentEvent::Verdict { passed, evidence } => {
                if let Some(snapshot) = evidence.ladder.as_deref() {
                    report.snapshotted_verdicts += 1;
                    if stands_alone(snapshot) {
                        report.uncorroborated_verdicts += 1;
                        if *passed && !evidence.deterministic {
                            report.verifier_passes_standing_alone += 1;
                        }
                    }
                }
                if !*passed {
                    continue;
                }
                // #1865: which model graded a verifier pass rides the ladder
                // snapshot (#1795); absent — pre-#1795, worker-unresolvable,
                // or no snapshot at all — stays UNKNOWN, never assumed.
                let grader_independent = evidence
                    .ladder
                    .as_deref()
                    .and_then(|snapshot| snapshot.verifier_independent);
                if evidence.deterministic {
                    report.deterministic_passes += 1;
                } else {
                    report.verifier_passes += 1;
                    report.by_grader.tally_mut(grader_independent).passes += 1;
                }
                pending.push(PendingPass {
                    session: session.to_string(),
                    verifier: !evidence.deterministic,
                    grader_independent,
                    commits: Vec::new(),
                    prs: Vec::new(),
                });
            }
            // Every commit and PR the session records after a pass is an
            // artifact that pass covers — the handle a late verdict is keyed
            // on. Attached to the unsettled passes only: one already
            // reconciled in-stream has had its answer.
            AgentEvent::Commit { sha, .. } => {
                for pass in &mut pending {
                    pass.commits.push(sha.clone());
                }
            }
            AgentEvent::Pr { url, ci, .. } => {
                for pass in &mut pending {
                    pass.prs.push(url.clone());
                }
                let failing = match ci {
                    Some(CiStatus::Passing) => false,
                    Some(CiStatus::Failing) => true,
                    // Absence of a verdict is not a verdict — the PR is still
                    // recorded above, so a terminal result arriving later
                    // (this session or another) can still settle these.
                    Some(CiStatus::Pending | CiStatus::Running) | None => continue,
                };
                for pass in pending.drain(..) {
                    if pass.verifier {
                        report.verifier_reconciled += 1;
                        report.verifier_false_positives += u32::from(failing);
                        let tally = report.by_grader.tally_mut(pass.grader_independent);
                        tally.reconciled += 1;
                        tally.false_positives += u32::from(failing);
                    } else {
                        report.deterministic_reconciled += 1;
                        report.deterministic_false_positives += u32::from(failing);
                    }
                }
            }
            _ => {}
        }
    }
    (report, pending)
}

/// Whether a recorded verdict had no deterministic corroboration behind it
/// (#1295) — the snapshot-side reading of
/// `crate::verify::LadderInputs::verifier_pass_stands_alone`.
///
/// Deliberately the same two conjuncts and no more. A readable diff or a
/// recorded file touch proves the tree **changed**; neither says the change
/// is **correct**, and only the second claim is what a pass makes — so
/// counting them here would report a rate the pipeline does not gate on.
fn stands_alone(snapshot: &stella_protocol::LadderSnapshot) -> bool {
    !snapshot.flip_achieved && snapshot.touched_tests_passed != Some(true)
}

/// Render an oracle trace compactly — `baseline:fail → candidate:pass` —
/// the canonical rendering shared by the verifier prompt (#864) and verdict
/// provenance (#865). A trailing candidate entry after a flip is the
/// pre-submit confirmation run.
pub fn render_oracle_trace(trace: &[OracleObservation]) -> String {
    trace
        .iter()
        .map(|obs| {
            let tree = match obs.tree {
                ProofTree::Baseline => "baseline",
                ProofTree::Candidate => "candidate",
            };
            let result = if obs.passed { "pass" } else { "fail" };
            format!("{tree}:{result}")
        })
        .collect::<Vec<_>>()
        .join(" → ")
}

/// Answer "why did this verdict happen?" from its recorded ladder snapshot
/// (#865) — no re-derivation, no probes. The snapshot was frozen at decision
/// time; this only renders it. `None` for evidence recorded before
/// snapshots existed, which a caller reports as "provenance not recorded",
/// never as a reconstructed guess.
pub fn verdict_provenance(evidence: &VerdictEvidence) -> Option<String> {
    let snapshot = evidence.ladder.as_deref()?;
    let flip = if snapshot.flip_achieved {
        "achieved".to_string()
    } else if snapshot.unstable_flip {
        "unstable (confirmation re-run did not pass)".to_string()
    } else {
        "none".to_string()
    };
    // The rung leads, because it is the literal answer to the question this
    // function asks; everything after it is the evidence the rung was chosen
    // from. Absent on verdicts recorded before the rung joined the snapshot
    // (#1043), and not guessed at then — the surrounding flags cannot separate
    // a deterministic pass from a waived review.
    let mut out = match snapshot.rung {
        Some(rung) => format!("rung={}; flip={flip}", rung.as_str()),
        None => format!("flip={flip}"),
    };
    if let Some(cmd) = &snapshot.tracked_command {
        out.push_str(&format!(" tracking `{cmd}`"));
    }
    if !snapshot.oracle_trace.is_empty() {
        out.push_str(&format!(
            " [{}]",
            render_oracle_trace(&snapshot.oracle_trace)
        ));
    }
    out.push_str(&format!(
        "; touched_tests={}",
        match (snapshot.touched_tests_passed, &snapshot.test_infra) {
            (Some(true), _) => "green".to_string(),
            (Some(false), _) => "red".to_string(),
            (None, Some(label)) => format!("unobserved ({label})"),
            (None, None) => "not run".to_string(),
        }
    ));
    out.push_str(&format!(
        "; diff={}/{} lines ({}); file_changes={}; mutating_actions={}",
        snapshot.diff_lines,
        snapshot.diff_budget,
        if snapshot.diff_available {
            "readable"
        } else {
            "unreadable tree"
        },
        snapshot.file_change_events,
        snapshot.mutating_actions,
    ));
    if snapshot.new_diag_errors > 0 || snapshot.new_diag_warnings > 0 {
        out.push_str(&format!(
            "; new_diagnostics={}e/{}w",
            snapshot.new_diag_errors, snapshot.new_diag_warnings
        ));
    }
    if snapshot.witness_intact == Some(true) {
        out.push_str("; witness=intact");
    }
    // #1291: rendered whenever the snapshot carries it, `unmeasured`
    // included. Unlike the verifier prompt — which has nothing to reason from in
    // that case — a provenance reader is asking "what was checked?", and "the
    // overlap was not measured" is a direct answer to it.
    if let Some(coverage) = &snapshot.diff_coverage {
        out.push_str(&format!("; diff_coverage={coverage}"));
    }
    // #1795: a provenance reader asking "who graded this?" gets the stored
    // fact — `self-graded` is the finding this field exists to surface, and
    // `independent` is stated too so its absence stays distinguishable from
    // "recorded before the fact existed".
    if let Some(independent) = snapshot.verifier_independent {
        out.push_str(if independent {
            "; grader=independent"
        } else {
            "; grader=self-graded (worker's own model)"
        });
    }
    Some(out)
}

/// A structural invariant an event stream violated, with a human-readable
/// reason. Returned as a list so a single validation pass reports every
/// problem, not just the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamViolation {
    /// Index of the offending event in the stream (or the index the problem
    /// is attributed to — e.g. an unmatched `ToolStart`).
    pub index: usize,
    pub reason: String,
}

/// Validate a stream against the protocol's structural invariants
/// :
///
/// 1. **Legal stage ordering** — consecutive `Stage` events move forward in
///    the canonical order or take a known revise back-edge (Verify/Verdict →
///    Execute); no other backward jump is legal.
/// 2. **Tool pairing** — every `ToolStart` has a later matching `ToolResult`
///    (same `call_id`), and no `ToolResult` appears without a prior
///    `ToolStart`.
/// 3. **Terminal `Complete`** — at most one `Complete`, and if present it is
///    the last event.
/// 4. **Monotonic budget** — `BudgetTick.spent_usd` never decreases.
///
/// Returns every violation found (empty vec = the stream is well-formed).
pub fn validate_stream(events: &[AgentEvent]) -> Vec<StreamViolation> {
    let mut violations = Vec::new();
    validate_stage_ordering(events, &mut violations);
    validate_tool_pairing(events, &mut violations);
    validate_terminal(events, &mut violations);
    validate_budget_monotonic(events, &mut violations);
    violations
}

/// Canonical rank of a stage in the one-turn data flow.
/// Forward motion is any non-decreasing rank; the only legal backward
/// motion is the revise/best-of-N loop back to Execute.
fn stage_rank(stage: StageKind) -> u8 {
    match stage {
        StageKind::Triage => 0,
        StageKind::ContextRecall => 1,
        // Research is demand-driven pre-plan evidence (#1778): triage names
        // the questions, so it can only follow triage, and its findings feed
        // the planner, so it must precede Plan.
        StageKind::Research => 2,
        StageKind::Plan => 3,
        StageKind::ScopeReview => 4,
        StageKind::Execute => 5,
        // Witness authoring is demand-driven: it runs AFTER execution, once
        // the warrant has read the executed diff and found something to prove
        // (L-E11 front half). The revise back-edges land on Execute below it —
        // re-execution never re-authors.
        StageKind::Witness => 6,
        StageKind::Verify => 7,
        StageKind::Verdict => 8,
        // Reflect is post-verdict self-reflection, before context write-back.
        StageKind::Reflect => 9,
        StageKind::ContextWrite => 10,
        StageKind::Complete => 11,
    }
}

/// Whether a transition between two consecutive `Stage` events is legal: a
/// forward (or same-rank) move, or the revise back-edge from Verify/Verdict to
/// Execute (the revision loop and best-of-N re-execute the work).
pub fn stage_transition_legal(from: StageKind, to: StageKind) -> bool {
    if stage_rank(to) >= stage_rank(from) {
        return true;
    }
    matches!(
        (from, to),
        (StageKind::Verify, StageKind::Execute) | (StageKind::Verdict, StageKind::Execute)
    )
}

fn validate_stage_ordering(events: &[AgentEvent], out: &mut Vec<StreamViolation>) {
    let mut last_stage: Option<StageKind> = None;
    for (i, event) in events.iter().enumerate() {
        if let AgentEvent::Stage { name } = event {
            if let Some(prev) = last_stage
                && !stage_transition_legal(prev, *name)
            {
                out.push(StreamViolation {
                    index: i,
                    reason: format!("illegal stage transition {prev:?} -> {name:?}"),
                });
            }
            last_stage = Some(*name);
        }
    }
}

fn validate_tool_pairing(events: &[AgentEvent], out: &mut Vec<StreamViolation>) {
    // Open ToolStarts keyed by call_id → the index they started at.
    let mut open: Vec<(String, usize)> = Vec::new();
    for (i, event) in events.iter().enumerate() {
        match event {
            AgentEvent::ToolStart { call } => open.push((call.call_id.clone(), i)),
            // `AskUser` is the `ask_user` tool's question; its answer returns
            // as an ordinary `ToolResult` keyed by this `id`, so it opens a
            // pending call exactly like a `ToolStart`.
            AgentEvent::AskUser { id, .. } => open.push((id.clone(), i)),
            AgentEvent::ToolResult { call_id, .. } => {
                if let Some(pos) = open.iter().position(|(id, _)| id == call_id) {
                    open.remove(pos);
                } else {
                    out.push(StreamViolation {
                        index: i,
                        reason: format!("tool_result for `{call_id}` with no preceding tool_start"),
                    });
                }
            }
            _ => {}
        }
    }
    for (call_id, start_index) in open {
        out.push(StreamViolation {
            index: start_index,
            reason: format!("tool_start for `{call_id}` never matched by a tool_result"),
        });
    }
}

fn validate_terminal(events: &[AgentEvent], out: &mut Vec<StreamViolation>) {
    let complete_indices: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, AgentEvent::Complete { .. }))
        .map(|(i, _)| i)
        .collect();
    if complete_indices.len() > 1 {
        for &i in &complete_indices[1..] {
            out.push(StreamViolation {
                index: i,
                reason: "more than one Complete event; a stream terminates once".to_string(),
            });
        }
    }
    if let Some(&first) = complete_indices.first()
        && first != events.len() - 1
    {
        out.push(StreamViolation {
            index: first,
            reason: "Complete is not the last event; nothing may follow it".to_string(),
        });
    }
}

fn validate_budget_monotonic(events: &[AgentEvent], out: &mut Vec<StreamViolation>) {
    let mut last_spent: Option<f64> = None;
    for (i, event) in events.iter().enumerate() {
        if let AgentEvent::BudgetTick { spent_usd, .. } = event {
            if let Some(prev) = last_spent
                && *spent_usd + f64::EPSILON < prev
            {
                out.push(StreamViolation {
                    index: i,
                    reason: format!(
                        "budget spent went backwards: {spent_usd:.6} < previous {prev:.6}"
                    ),
                });
            }
            last_spent = Some(*spent_usd);
        }
    }
}

/// A stable identifier for one event, capturing its kind and the fields that
/// matter for *structural* equivalence, while dropping volatile fields
/// (durations, exact costs/spend, free-text deltas). Two streams that agree on
/// their sequence of signatures are structurally equivalent even if they took
/// different wall-clock time or cost slightly different amounts.
///
/// Deliberately keeps identity-bearing fields (a tool's `name`, a stage's
/// `name`, a verdict's `passed`) and drops magnitude-bearing ones — the
/// distinction the golden-replay comparison rests on.
pub fn event_signature(event: &AgentEvent) -> String {
    match event {
        AgentEvent::Stage { name } => format!("stage:{name:?}"),
        // Text/Reasoning deltas are volatile content — only their presence
        // and kind are structural.
        AgentEvent::Text { .. } => "text".to_string(),
        // Streaming previews have no structural identity at all — even their
        // COUNT varies run to run with chunk boundaries — so
        // [`structural_diff`] excludes them before comparing; the signature
        // exists only to keep this function total.
        AgentEvent::TextDelta { .. } => "text_delta".to_string(),
        AgentEvent::Reasoning { .. } => "reasoning".to_string(),
        // A discarded speculation is timing-dependent — which stream attempt
        // failed (and so which read-only pool is dropped) varies run to run —
        // so, like `TextDelta`, [`structural_diff`] excludes it before
        // comparing; the signature exists only to keep this function total.
        AgentEvent::SpeculationDiscarded { .. } => "speculation_discarded".to_string(),
        // Structural to the last field that carries a decision: whether a test
        // was warranted, whether one was produced, and which way the oracle
        // went on which tree. Paths, reasons and fingerprints are dropped —
        // they name the artifact, not the shape of the proof. Two runs that
        // prove the same work the same way agree here even when the author
        // picked a different filename.
        AgentEvent::Proof { step } => {
            use stella_protocol::ProofStep;
            match step {
                ProofStep::Assurance { witness, verifier } => {
                    format!("proof:assurance:{witness}:{verifier}")
                }
                ProofStep::Warrant { required, .. } => format!("proof:warrant:{required}"),
                ProofStep::WitnessAuthored { .. } => "proof:witness_authored".to_string(),
                ProofStep::WitnessUnavailable { .. } => "proof:witness_unavailable".to_string(),
                ProofStep::VerificationUnavailable { .. } => {
                    "proof:verification_unavailable".to_string()
                }
                ProofStep::Oracle { passed, tree, .. } => {
                    format!("proof:oracle:{tree:?}:{passed}")
                }
                // The reason is prose about the outage, not the shape of the
                // proof; which candidate degraded is structural.
                ProofStep::VerdictDegraded { candidate, .. } => {
                    format!("proof:verdict_degraded:{candidate}")
                }
            }
        }
        AgentEvent::ToolStart { call } => format!("tool_start:{}", call.name),
        // A tool_result's structural identity is that it answered a call and
        // whether it errored — not its duration or output body.
        AgentEvent::ToolResult { output, .. } => {
            format!("tool_result:error={}", output.is_error())
        }
        AgentEvent::Retry { .. } => "retry".to_string(),
        AgentEvent::Compaction { .. } => "compaction".to_string(),
        // Steering text is user-authored free text; only its occurrence is
        // structural (same posture as budget ticks).
        AgentEvent::Steered { .. } => "steered".to_string(),
        // Whether a turn parks at all depends on external state and timing —
        // like `SpeculationDiscarded`, [`structural_diff`] excludes the pair
        // before comparing; the signatures exist only to keep this function
        // total.
        AgentEvent::TurnParked { .. } => "turn_parked".to_string(),
        AgentEvent::TurnWoken { .. } => "turn_woken".to_string(),
        // Budget ticks vary in magnitude every run; only their occurrence is
        // structural.
        AgentEvent::BudgetTick { mode, .. } => format!("budget_tick:{mode:?}"),
        AgentEvent::ProviderFallback { from, to, .. } => {
            format!("provider_fallback:{from}->{to}")
        }
        AgentEvent::FileChange { kind, .. } => format!("file_change:{kind:?}"),
        AgentEvent::ContextRecall { .. } => "context_recall".to_string(),
        AgentEvent::ContextWrite { .. } => "context_write".to_string(),
        AgentEvent::MediaProgress { kind, .. } => format!("media_progress:{kind:?}"),
        AgentEvent::MediaComplete { .. } => "media_complete".to_string(),
        AgentEvent::Verdict { passed, evidence } => {
            format!(
                "verdict:passed={},deterministic={}",
                passed, evidence.deterministic
            )
        }
        AgentEvent::ScopeReview { .. } => "scope_review".to_string(),
        // The diff text and the review id are volatile; how many hunks were put
        // up for approval is the structural part.
        AgentEvent::HunkReview { proposal } => {
            format!("hunk_review:hunks={}", proposal.hunks.len())
        }
        // The question text is volatile; the number of structured options is
        // the structural part (the free-text option is always implied).
        AgentEvent::AskUser { options, .. } => format!("ask_user:options={}", options.len()),
        AgentEvent::Commit { .. } => "commit".to_string(),
        AgentEvent::Pr { status, .. } => format!("pr:{status:?}"),
        // Step usage is pure magnitude (tokens/cost/duration) — only its
        // occurrence is structural, like a budget tick.
        AgentEvent::StepUsage { .. } => "step_usage".to_string(),
        AgentEvent::UsageIncomplete { reason, .. } => {
            format!("usage_incomplete:{reason:?}")
        }
        // An event from a newer stella. Its tag is the only part this build
        // can honestly read, and two different unknown tags are genuinely
        // different events — so keep the tag and nothing else. Excluded from
        // `structural_diff` below regardless (see the keep-set), so this
        // signature exists mainly to keep the function total and to make an
        // unknown event legible in a diff that is printed rather than
        // compared.
        AgentEvent::Unknown { event_type, .. } => format!("unknown:{event_type}"),
        // A goal verdict's structural identity is whether the goal was met
        // (mirrors `verdict`); the reasoning text and cost are volatile.
        AgentEvent::GoalVerdict { met, .. } => format!("goal_verdict:met={met}"),
        AgentEvent::Error { retryable, .. } => format!("error:retryable={retryable}"),
        AgentEvent::Complete { .. } => "complete".to_string(),
        // Task subjects/descriptions are volatile content; the board's shape
        // (how many tasks, how many resolved) is the structural part.
        AgentEvent::TaskUpdate { tasks } => {
            let done = tasks.iter().filter(|t| !t.status.is_open()).count();
            format!("task_update:tasks={},resolved={done}", tasks.len())
        }
        // Context receipts are additive observability (spec §4/§5), excluded
        // from the structural comparison below just like TextDelta — a golden
        // stream recorded before receipts existed has none, so they must not
        // shift the aligned positions. The signatures exist only to keep this
        // function total; they capture occurrence + shape, never volatile ids.
        AgentEvent::BlockRegistered { kind, .. } => format!("block_registered:{kind:?}"),
        AgentEvent::StepManifest { blocks, .. } => {
            format!("step_manifest:blocks={}", blocks.len())
        }
        // Typed decision events (receipts spec §6.3/§6.4) are the parseable
        // twins of prose Errors/steers already in the stream; the decision
        // itself is structural, its evidence/reason text volatile.
        AgentEvent::LoopDetected { kind, aborted, .. } => {
            format!("loop_detected:{kind}:aborted={aborted}")
        }
        AgentEvent::BudgetDenied { scope, mode, .. } => {
            format!("budget_denied:{scope:?}:{mode:?}")
        }
        AgentEvent::RetriesExhausted { attempts, .. } => {
            format!("retries_exhausted:attempts={attempts}")
        }
        AgentEvent::PolicyDecision { kind, subject, .. } => {
            format!("policy_decision:{kind:?}:{subject}")
        }
        // A sub-agent's structural identity is which child ran, under which
        // permissions, and how it ended. Its cost, step count, absorbed
        // message count and summary text are all magnitude/content — the
        // same posture as `step_usage` and `goal_verdict`. `truncated` and
        // `write_access` stay: both are decisions, not measurements.
        AgentEvent::SubAgent { phase } => {
            use stella_protocol::SubAgentPhase;
            match phase {
                SubAgentPhase::Started {
                    agent_id,
                    write_access,
                    depth,
                    ..
                } => format!("sub_agent:started:{agent_id}:write={write_access}:depth={depth}"),
                SubAgentPhase::Finished {
                    agent_id,
                    status,
                    truncated,
                    ..
                } => {
                    format!("sub_agent:finished:{agent_id}:{status:?}:truncated={truncated}")
                }
            }
        }
    }
}

/// One positional difference between two structurally-compared streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDiff {
    /// Position in the sequence where the streams differ.
    pub index: usize,
    /// The left stream's signature at this position, `None` if the left ran
    /// out (the right is longer).
    pub left: Option<String>,
    /// The right stream's signature at this position, `None` if the right ran
    /// out (the left is longer).
    pub right: Option<String>,
}

/// Structurally diff two event streams by comparing their [`event_signature`]
/// sequences positionally (kinds + order, volatile fields ignored). Returns a
/// diff entry at every position where the signatures differ, plus one entry
/// per trailing event when the streams differ in length. An empty result
/// means the two streams are structurally equivalent.
///
/// Positional (not longest-common-subsequence) by design: golden replay
/// compares a run against a reference produced by the *same staged flow*, so
/// an aligned position-by-position comparison is the intended semantics —
/// a spurious insertion should surface as a divergence, not be quietly
/// realigned away.
pub fn structural_diff(left: &[AgentEvent], right: &[AgentEvent]) -> Vec<StreamDiff> {
    // `TextDelta` previews are excluded before the positional walk: the same
    // answer streams in different chunkings run to run, so even their count
    // is volatile — the authoritative `Text` event that follows them is the
    // structural record. Diff indices therefore address the delta-free
    // sequence.
    // Context receipts (BlockRegistered/StepManifest) join TextDelta in the
    // exclusion set: they are additive observability a pre-receipt golden
    // stream does not carry, so keeping them would shift every later position.
    // `SpeculationDiscarded` (#415) joins them: it is a run-to-run scheduling
    // artifact absent from pre-speculation goldens, so it too must not shift
    // aligned positions.
    // `Unknown` is the general case of that same rule. An event this build
    // cannot decode came from a different stella, so it is present in exactly
    // one of the two streams by construction — keeping it would shift every
    // later position and report drift that says nothing about behaviour. The
    // comparison is therefore over the vocabulary both sides share, which is
    // the only vocabulary either side can reason about. Unknown events are
    // still counted and still validated; they are only excluded from
    // *positional* structural comparison.
    // `SubAgent` (#922) joins them for the same reason as context receipts:
    // a golden recorded before the sub-agent primitive existed carries no
    // lifecycle bracket, and the goal loop's verifier — previously an inline
    // engine turn — now emits one. Keeping it would shift every later
    // position and report drift about the observability plane rather than
    // about behaviour. What the child DID is still compared: its forwarded
    // `tool_start`/`tool_result`/`step_usage` signatures stay in the walk.
    // `TurnParked`/`TurnWoken` (#1857) join the exclusions on both grounds at
    // once: whether a run parks — and how many polls it spends — depends on
    // external state and wall-clock timing (`SpeculationDiscarded`'s reason),
    // and the pair is additive observability absent from goldens recorded
    // before parked waits existed (the context receipts' reason).
    let keep = |e: &&AgentEvent| {
        !matches!(
            e,
            AgentEvent::TextDelta { .. }
                | AgentEvent::BlockRegistered { .. }
                | AgentEvent::StepManifest { .. }
                | AgentEvent::SpeculationDiscarded { .. }
                | AgentEvent::TurnParked { .. }
                | AgentEvent::TurnWoken { .. }
                | AgentEvent::SubAgent { .. }
                | AgentEvent::Unknown { .. }
        )
    };
    let left: Vec<&AgentEvent> = left.iter().filter(keep).collect();
    let right: Vec<&AgentEvent> = right.iter().filter(keep).collect();
    let mut diffs = Vec::new();
    let max_len = left.len().max(right.len());
    for i in 0..max_len {
        let l = left.get(i).copied().map(event_signature);
        let r = right.get(i).copied().map(event_signature);
        if l != r {
            diffs.push(StreamDiff {
                index: i,
                left: l,
                right: r,
            });
        }
    }
    diffs
}

/// Whether two streams are structurally equivalent (no diffs).
pub fn streams_equivalent(left: &[AgentEvent], right: &[AgentEvent]) -> bool {
    structural_diff(left, right).is_empty()
}

/// An error parsing an event-stream JSONL document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JsonlError {
    /// A non-final line failed to parse as an `AgentEvent`. Interior
    /// corruption is fatal — only a torn *tail* is tolerated (L-T1).
    #[error("malformed event on line {line} (1-indexed): {message}")]
    MalformedLine { line: usize, message: String },
}

/// Parse an event-stream JSONL document (one `AgentEvent` per line) into a
/// vector of events, tolerating a single torn final line (L-T1): if the *last*
/// non-empty line fails to parse — the signature of a writer that crashed
/// mid-line — it is dropped rather than failing the whole parse. A malformed
/// *interior* line is a [`JsonlError::MalformedLine`].
///
/// A line carrying an event type this build does not recognize is **not**
/// malformed: it parses into [`stella_protocol::AgentEvent::Unknown`] with its
/// payload preserved. Only genuinely broken JSON, or a recognized `"type"`
/// with a body that does not fit it, is an error.
pub fn parse_jsonl(input: &str) -> Result<Vec<AgentEvent>, JsonlError> {
    // Collect (1-indexed line number, content) for every non-blank line.
    let lines: Vec<(usize, &str)> = input
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.is_empty())
        .collect();

    let mut events = Vec::with_capacity(lines.len());
    let last_index = lines.len().saturating_sub(1);
    for (pos, (line_no, content)) in lines.iter().enumerate() {
        match serde_json::from_str::<AgentEvent>(content) {
            Ok(event) => events.push(event),
            Err(err) => {
                if pos == last_index {
                    // Torn tail: a crashed writer left a partial final line.
                    // Drop it and return what parsed cleanly (L-T1).
                    break;
                }
                return Err(JsonlError::MalformedLine {
                    line: *line_no,
                    message: err.to_string(),
                });
            }
        }
    }
    Ok(events)
}

/// Run the structural conformance check over a recorded JSONL stream — the
/// one-call composition of [`parse_jsonl`] and [`validate_stream`].
///
/// This is the runnable half of the wire contract, and it checks the half a
/// JSON Schema cannot. `docs/wire/agentevent.schema.json` describes what one
/// *event* may look like; the invariants that span several events — legal
/// stage ordering, `tool_start`/`tool_result` pairing, a single terminal
/// `Complete`, monotonic budget — are not expressible in JSON Schema at all. A
/// recording in which every line validates against the schema can still be an
/// illegal stream, and this is what says so.
///
/// `Ok(vec![])` means the recording conforms. `Ok(violations)` means it parsed
/// but breaks the protocol. `Err` means it is not a readable recording at all
/// (a malformed *interior* line — a torn tail is tolerated, L-T1).
///
/// # Errors
///
/// [`JsonlError::MalformedLine`] when an interior line is not a readable
/// event.
pub fn conform_jsonl(input: &str) -> Result<Vec<StreamViolation>, JsonlError> {
    Ok(validate_stream(&parse_jsonl(input)?))
}

/// Serialize a stream to JSONL (one event per line) — the inverse of
/// [`parse_jsonl`], used to write fixtures and to emit `--output-format
/// stream-json`. Never fails: every `AgentEvent` is serde-serializable by
/// construction.
pub fn to_jsonl(events: &[AgentEvent]) -> String {
    let mut out = String::new();
    for event in events {
        // `AgentEvent` always serializes (no non-string map keys, no
        // non-finite floats introduced by this crate); `expect` documents that
        // invariant rather than hiding a real fallible path.
        let line = serde_json::to_string(event).expect("AgentEvent is always serializable");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::event::BudgetMode;
    use stella_protocol::{ToolCall, ToolOutput, VerdictEvidence};

    fn stage(name: StageKind) -> AgentEvent {
        AgentEvent::Stage { name }
    }
    fn tool_start(id: &str, name: &str) -> AgentEvent {
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: id.into(),
                name: name.into(),
                input: serde_json::json!({}),
            },
        }
    }
    fn tool_result(id: &str, err: bool) -> AgentEvent {
        AgentEvent::ToolResult {
            call_id: id.into(),
            output: if err {
                ToolOutput::Error {
                    message: "boom".into(),
                }
            } else {
                ToolOutput::Ok {
                    content: "ok".into(),
                }
            },
            duration_ms: 12,
            speculated: false,
        }
    }
    fn budget(spent: f64) -> AgentEvent {
        AgentEvent::BudgetTick {
            spent_usd: spent,
            limit_usd: None,
            mode: BudgetMode::Observed,
            session_spent_usd: None,
            session_limit_usd: None,
        }
    }
    fn complete() -> AgentEvent {
        AgentEvent::Complete {
            model: "glm-5.2".into(),
            cost_usd: 0.01,
        }
    }
    fn verifier(passed: bool, deterministic: bool) -> AgentEvent {
        AgentEvent::Verdict {
            passed,
            evidence: VerdictEvidence {
                summary: "x".into(),
                deterministic,
                evidence_refs: vec![],
                ladder: None,
            },
        }
    }

    // stage ordering

    #[test]
    fn canonical_forward_ordering_is_legal() {
        let events = [
            stage(StageKind::Triage),
            stage(StageKind::ContextRecall),
            stage(StageKind::Plan),
            stage(StageKind::Execute),
            // The authored-witness position: after execution (the warrant
            // reads the diff before buying an author turn), before verify.
            stage(StageKind::Witness),
            stage(StageKind::Verify),
            stage(StageKind::Complete),
            complete(),
        ];
        assert!(validate_stream(&events).is_empty());
    }

    #[test]
    fn the_revise_back_edge_is_legal() {
        assert!(stage_transition_legal(
            StageKind::Verify,
            StageKind::Execute
        ));
        assert!(stage_transition_legal(
            StageKind::Verdict,
            StageKind::Execute
        ));
        // Witness authoring is demand-driven and FOLLOWS execution (the
        // warrant reads the executed diff first), so Execute → Witness is the
        // forward move and the revise back-edges jump over Witness back to
        // Execute — re-execution never re-authors.
        assert!(stage_transition_legal(
            StageKind::Execute,
            StageKind::Witness
        ));
        assert!(stage_transition_legal(
            StageKind::Witness,
            StageKind::Verify
        ));
        assert!(!stage_transition_legal(
            StageKind::Witness,
            StageKind::Execute
        ));
        assert!(!stage_transition_legal(
            StageKind::Verify,
            StageKind::Witness
        ));
        // But you cannot jump backward to planning.
        assert!(!stage_transition_legal(StageKind::Execute, StageKind::Plan));
    }

    #[test]
    fn an_illegal_backward_stage_jump_is_flagged() {
        let events = [stage(StageKind::Execute), stage(StageKind::Triage)];
        let v = validate_stream(&events);
        assert_eq!(v.len(), 1);
        assert!(v[0].reason.contains("illegal stage transition"));
    }

    // tool pairing

    #[test]
    fn matched_tool_calls_pass() {
        let events = [tool_start("c1", "read_file"), tool_result("c1", false)];
        assert!(validate_stream(&events).is_empty());
    }

    #[test]
    fn an_unmatched_tool_start_is_flagged() {
        let events = [tool_start("c1", "read_file")];
        let v = validate_stream(&events);
        assert_eq!(v.len(), 1);
        assert!(v[0].reason.contains("never matched"));
    }

    #[test]
    fn a_dangling_tool_result_is_flagged() {
        let events = [tool_result("c9", false)];
        let v = validate_stream(&events);
        assert_eq!(v.len(), 1);
        assert!(v[0].reason.contains("no preceding tool_start"));
    }

    // terminal

    #[test]
    fn two_completes_are_flagged() {
        let events = [complete(), complete()];
        let v = validate_stream(&events);
        // one for "more than one Complete", one for "not the last"
        assert!(
            v.iter()
                .any(|x| x.reason.contains("more than one Complete"))
        );
    }

    #[test]
    fn complete_not_last_is_flagged() {
        let events = [complete(), stage(StageKind::Execute)];
        let v = validate_stream(&events);
        assert!(v.iter().any(|x| x.reason.contains("not the last event")));
    }

    // budget monotonic

    #[test]
    fn monotonic_budget_passes_and_regression_is_flagged() {
        assert!(validate_stream(&[budget(0.1), budget(0.2), budget(0.2)]).is_empty());
        let v = validate_stream(&[budget(0.5), budget(0.2)]);
        assert_eq!(v.len(), 1);
        assert!(v[0].reason.contains("backwards"));
    }

    // structural diff

    #[test]
    fn identical_kind_streams_are_equivalent_despite_volatile_fields() {
        let a = [
            tool_start("c1", "read_file"),
            tool_result("c1", false),
            budget(0.1),
        ];
        // Same kinds/names, different call_ids, durations, spend — still
        // structurally equivalent.
        let b = [
            tool_start("c2", "read_file"),
            tool_result("c2", false),
            budget(0.9),
        ];
        assert!(streams_equivalent(&a, &b));
        assert!(structural_diff(&a, &b).is_empty());
    }

    #[test]
    fn a_different_tool_name_diverges() {
        let a = [tool_start("c1", "read_file")];
        let b = [tool_start("c1", "write_file")];
        let diff = structural_diff(&a, &b);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].index, 0);
        assert_eq!(diff[0].left.as_deref(), Some("tool_start:read_file"));
        assert_eq!(diff[0].right.as_deref(), Some("tool_start:write_file"));
    }

    #[test]
    fn a_verdict_flip_diverges() {
        assert!(!streams_equivalent(
            &[verifier(true, true)],
            &[verifier(false, true)]
        ));
        assert!(!streams_equivalent(
            &[verifier(true, true)],
            &[verifier(true, false)]
        ));
    }

    #[test]
    fn length_mismatch_reports_trailing_events() {
        let a = [stage(StageKind::Execute)];
        let b = [stage(StageKind::Execute), complete()];
        let diff = structural_diff(&a, &b);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].index, 1);
        assert_eq!(diff[0].left, None);
        assert_eq!(diff[0].right.as_deref(), Some("complete"));
    }

    // JSONL round-trip + torn tail

    #[test]
    fn jsonl_round_trips() {
        let events = vec![
            stage(StageKind::Triage),
            tool_start("c1", "read_file"),
            tool_result("c1", false),
            complete(),
        ];
        let jsonl = to_jsonl(&events);
        let parsed = parse_jsonl(&jsonl).unwrap();
        assert_eq!(parsed.len(), 4);
        assert!(streams_equivalent(&events, &parsed));
    }

    #[test]
    fn parse_jsonl_tolerates_a_torn_final_line() {
        let mut jsonl = to_jsonl(&[stage(StageKind::Execute), complete()]);
        // Simulate a crashed writer: append a partial final line.
        jsonl.push_str("{\"type\":\"tool_start\",\"call\":{\"call_id\":\"c1\",\"na");
        let parsed = parse_jsonl(&jsonl).unwrap();
        assert_eq!(parsed.len(), 2, "torn tail dropped, clean prefix kept");
    }

    #[test]
    fn parse_jsonl_rejects_a_malformed_interior_line() {
        let mut jsonl = String::new();
        jsonl.push_str(&serde_json::to_string(&stage(StageKind::Execute)).unwrap());
        jsonl.push('\n');
        jsonl.push_str("{ not valid json }\n");
        jsonl.push_str(&serde_json::to_string(&complete()).unwrap());
        jsonl.push('\n');
        match parse_jsonl(&jsonl) {
            Err(JsonlError::MalformedLine { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected an interior MalformedLine error, got {other:?}"),
        }
    }

    #[test]
    fn parse_jsonl_ignores_blank_lines() {
        let jsonl = format!(
            "\n{}\n\n{}\n",
            serde_json::to_string(&stage(StageKind::Execute)).unwrap(),
            serde_json::to_string(&complete()).unwrap()
        );
        let parsed = parse_jsonl(&jsonl).unwrap();
        assert_eq!(parsed.len(), 2);
    }
}

#[cfg(test)]
mod calibration_tests {
    use super::*;
    use stella_protocol::{CiStatus, PrStatus, VerdictEvidence};

    fn verdict(passed: bool, deterministic: bool) -> AgentEvent {
        AgentEvent::Verdict {
            passed,
            evidence: VerdictEvidence {
                summary: String::new(),
                deterministic,
                evidence_refs: vec![],
                ladder: None,
            },
        }
    }

    fn ci(status: CiStatus) -> AgentEvent {
        AgentEvent::Pr {
            url: "https://example.test/pr/1".into(),
            status: PrStatus::Open,
            number: Some(1),
            ci: Some(status),
        }
    }

    /// The #871 acceptance: a VerifierPass that later fails CI is recorded as a
    /// false positive; the deterministic cohort reconciles beside it.
    #[test]
    fn a_verifier_pass_that_fails_ci_is_a_false_positive() {
        let events = vec![
            verdict(true, false), // verifier PASS
            verdict(true, true),  // deterministic pass
            ci(CiStatus::Failing),
        ];
        let report = calibration(&events);
        assert_eq!(report.verifier_passes, 1);
        assert_eq!(report.verifier_reconciled, 1);
        assert_eq!(report.verifier_false_positives, 1);
        assert_eq!(report.verifier_false_positive_rate(), Some(1.0));
        assert_eq!(report.deterministic_false_positives, 1);
    }

    /// Pending/Running reconcile nothing, and passes after the last terminal
    /// CI verdict stay unreconciled — an unmeasured rate reads None.
    #[test]
    fn non_terminal_ci_and_trailing_passes_stay_unreconciled() {
        let events = vec![
            verdict(true, false),
            ci(CiStatus::Running),
            ci(CiStatus::Passing),
            verdict(true, false), // after the last terminal verdict
        ];
        let report = calibration(&events);
        assert_eq!(report.verifier_passes, 2);
        assert_eq!(report.verifier_reconciled, 1);
        assert_eq!(report.verifier_false_positives, 0);
        assert_eq!(report.verifier_false_positive_rate(), Some(0.0));

        let unmeasured = calibration(&[verdict(true, false)]);
        assert_eq!(unmeasured.verifier_false_positive_rate(), None);
    }

    /// Failed verdicts and revise-loop reds never enter the tallies — the
    /// question is the false-POSITIVE rate of passes.
    #[test]
    fn failed_verdicts_are_not_tallied() {
        let events = vec![
            verdict(false, true),
            verdict(false, false),
            ci(CiStatus::Passing),
        ];
        let report = calibration(&events);
        assert_eq!(report.verifier_passes, 0);
        assert_eq!(report.deterministic_passes, 0);
    }

    #[test]
    fn merge_adds_componentwise() {
        let a = calibration(&[verdict(true, false), ci(CiStatus::Failing)]);
        let b = calibration(&[verdict(true, false), ci(CiStatus::Passing)]);
        let merged = a.merge(b);
        assert_eq!(merged.verifier_passes, 2);
        assert_eq!(merged.verifier_reconciled, 2);
        assert_eq!(merged.verifier_false_positives, 1);
    }

    /// A verdict carrying its ladder snapshot, so the verifier-alone fold has
    /// something to read. `corroborated` decides whether the recorded turn
    /// had a flip behind it.
    fn snapshotted(passed: bool, deterministic: bool, corroborated: bool) -> AgentEvent {
        let snapshot = stella_protocol::LadderSnapshot {
            rung: None,
            tracked_command: None,
            oracle_trace: vec![],
            flip_achieved: corroborated,
            unstable_flip: false,
            flip_refused_different_failure: false,
            touched_tests_passed: corroborated.then_some(true),
            test_infra: None,
            diff_lines: 4,
            diff_budget: 400,
            diff_available: true,
            file_change_events: 1,
            mutating_actions: 2,
            new_diag_errors: 0,
            new_diag_warnings: 0,
            witness_intact: None,
            witness_mutation: None,
            diff_coverage: None,
            verifier_independent: None,
        };
        AgentEvent::Verdict {
            passed,
            evidence: VerdictEvidence {
                summary: String::new(),
                deterministic,
                evidence_refs: vec![],
                ladder: Some(Box::new(snapshot)),
            },
        }
    }

    /// #1295: the rate that decides whether asking for evidence is worth a
    /// turn is measured off what is already recorded — every snapshotted
    /// verdict counts toward the denominator, and a model-verifier PASS with
    /// nothing behind it is separately identified as a turn the gated
    /// behaviour would have sent back.
    #[test]
    fn the_verifier_alone_rate_is_measured_from_recorded_snapshots() {
        let events = vec![
            snapshotted(true, false, false),  // verifier PASS, nothing behind it
            snapshotted(true, true, true),    // deterministic pass, flip achieved
            snapshotted(false, false, false), // a FAILING verdict, also uncorroborated
            snapshotted(true, false, true),   // verifier PASS with a flip behind it
        ];
        let report = calibration(&events);
        assert_eq!(report.snapshotted_verdicts, 4);
        assert_eq!(
            report.uncorroborated_verdicts, 2,
            "the denominator is every verdict, not only the passing ones — the question is how \
             often the CONDITION holds"
        );
        assert_eq!(report.uncorroborated_rate(), Some(0.5));
        assert_eq!(
            report.verifier_passes_standing_alone, 1,
            "only the verifier PASS with no flip and no green test would be sent back"
        );
        // The pass cohorts are untouched by the new fold.
        assert_eq!(report.verifier_passes, 2);
        assert_eq!(report.deterministic_passes, 1);
    }

    /// A rate with no denominator is reported as unmeasured, never as 0% —
    /// the same discipline the false-positive rates already keep. Verdicts
    /// recorded before ladder snapshots existed (#865) contribute nothing
    /// rather than being assumed corroborated.
    #[test]
    fn a_stream_without_snapshots_leaves_the_verifier_alone_rate_unmeasured() {
        let report = calibration(&[verdict(true, false), verdict(false, true)]);
        assert_eq!(report.snapshotted_verdicts, 0);
        assert_eq!(report.uncorroborated_rate(), None);
        assert!(
            render_calibration(&report).contains("verifier-alone rate: unmeasured"),
            "an unmeasured rate must say so: {}",
            render_calibration(&report)
        );
    }
}

#[cfg(test)]
mod late_reconciliation_tests;
