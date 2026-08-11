//! The steer→abort escalation ladder loop detection drives: warn once per
//! *loop* within a bounded budget, then abort — and say honestly which loop
//! the abort is about.
//!
//! `crate::loop_detect` decides *whether* the turn is looping, and
//! `super::loop_evidence` assembles what it reasons over. This module owns
//! what happens next: a detection either injects a steering warning through
//! the same seam as user steering — recording the [`LoopIdentity`] it warned
//! about — or is the turn's clean abort. Which one is [`LoopSteerBudget`]'s
//! call.
//!
//! The recorded identity is what keeps the abort message honest —
//! "persisted after a steering warning" is a claim about the *warned* loop,
//! and the nightly loop-bench caught the old unconditional phrasing blaming a
//! fresh `edit_file` loop on a `write_file` warning the model had actually
//! obeyed (#1524). Identity is the loop's tools *and* its arguments, because
//! the first pass recorded tool names alone and every `bash` loop in a turn
//! therefore read as one loop — the same false claim, surviving underneath
//! its own fix for the tool that loops most.
//!
//! The same identity now decides the *policy*, not just the phrasing: a turn
//! used to get one warning ever, so a model that obeyed the steer and fell
//! into a genuinely different loop was killed for a loop it was never told
//! about (#1743). That is a resolve-rate loss and it is unfair to the
//! compliant model — but unbounded steers are not the alternative, because a
//! model alternating between two loops would then burn every step of
//! `EngineConfig::max_steps`, which is the exact outcome loop detection
//! exists to prevent. [`MAX_LOOP_STEERS`] is the bound.
//!
//! Split out of `driver.rs` the same way `super::settlement` was: the parent
//! file sits at its file-size ceiling. The module is `pub(crate)` so
//! `crate::step::TurnState` can hold the budget it checkpoints; the ladder's
//! policy belongs beside the ladder, not in the state container.

use stella_protocol::{AgentEvent, CompletionMessage};

use crate::event_sender::EventSender;
use crate::loop_detect::{LoopIdentity, LoopVerdict, detect_loop};
use crate::ports::ToolExecutor;
use crate::step::AbortKind;

use super::loop_evidence::{ResultIdentities, recent_call_records};
use super::{EngineConfig, LOOP_STEER_PREFIX, TurnOutcome};

/// How many stuck-loop steering warnings one turn may spend.
///
/// Two, because two is what the witnessed failure costs and no more: the run
/// that motivated #1743 was steered about one `grep`, changed command exactly
/// as instructed, formed one different `grep` loop, and lost the whole turn
/// (10m25s, $1.6182, verdict `none`) for a loop it had never been warned
/// about. One extra warning covers that shape; the worst case a turn can pay
/// for it is one additional step.
///
/// Not unbounded, and not larger, for the same reason the abort exists at
/// all: `crate::loop_detect`'s contract is that it fires orders of magnitude
/// before `EngineConfig::max_steps`, and a model that alternates between two
/// loops would earn a fresh warning every time it switched.
///
/// The cap and [`LoopSteerBudget`]'s single memory slot are one decision, not
/// two. Comparing only against the MOST RECENT warned loop is sufficient at
/// two — once the budget is spent every detection aborts, whichever loop it
/// is — and stops being sufficient the moment the cap rises, because an
/// A→B→A ping-pong would then be three "different" loops in a row. Raising
/// this constant means remembering every warned identity, not just editing
/// the number.
pub(crate) const MAX_LOOP_STEERS: u32 = 2;

/// What a loop detection is worth: another warning, or the turn's end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoopEscalation {
    /// Warn the model and let the turn continue.
    Steer,
    /// End the turn. Carries the loop the most recent warning was about, so
    /// [`abort_reason`] can never be reached without it — the alternative is
    /// an `Option` the caller unwraps on a case that cannot happen, which is
    /// exactly the panic invariant #5 forbids.
    Abort { warned: LoopIdentity },
}

/// A turn's stuck-loop steering budget: the loop the most recent warning
/// named, and how many warnings the turn has spent of [`MAX_LOOP_STEERS`].
///
/// Pure decision state over owned data (invariant #2) — no I/O, no clock, no
/// transcript. [`Self::escalate`] is the whole ladder in one total function,
/// deliberately taking `&mut self` and answering in the same call: a
/// `decide` + `spend` pair is a budget a caller can forget to charge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LoopSteerBudget {
    /// The loop the most recent steering warning was about, or `None` while
    /// the turn has warned about nothing.
    warned: Option<LoopIdentity>,
    /// Warnings spent. Invariant, established by construction and every
    /// mutation below: `spent == 0` exactly when `warned` is `None`.
    spent: u32,
}

impl LoopSteerBudget {
    /// The budget of a turn resumed from a [`crate::step::Checkpoint`].
    ///
    /// The count itself is not on the wire — `Checkpoint` records the warned
    /// loop and a `loop_steered: bool`, and adding a field to that struct is
    /// a cross-crate change (#2809). Reconciled the only safe direction: any
    /// recorded warning restores one spent steer, so a resume can never
    /// *refund* the warning a turn already had. It can re-open the second
    /// one, which costs a resumed turn at most one extra warning and one
    /// extra step — the same bounded-allowance-starts-over trade
    /// `TurnState::length_continuations` and `OverflowRecovery` already make
    /// across a checkpoint, and strictly less permissive than either since
    /// the warned loop itself survives and still aborts on sight.
    pub(crate) fn resumed(warned: Option<LoopIdentity>) -> Self {
        let spent = u32::from(warned.is_some());
        Self { warned, spent }
    }

    /// The loop the most recent warning was about — what a checkpoint
    /// records, and `None` while no warning has been spent.
    pub(crate) fn warned(&self) -> Option<&LoopIdentity> {
        self.warned.as_ref()
    }

    /// Charge `detected` against the budget and answer what happens to the
    /// turn.
    ///
    /// A warning is granted only when the turn can *prove* this is a loop it
    /// has not already been told about:
    ///
    /// - nothing warned yet → warn (the turn's first detection);
    /// - budget spent → abort, whichever loop it is;
    /// - [`LoopIdentity::same_loop_as`] `Some(false)` → warn: the model broke
    ///   the loop it was told about and formed a different one, so it has
    ///   never been told about this one (#1743);
    /// - `Some(true)` → abort: told, and looping again on the same loop;
    /// - `None` → abort. The comparison is unanswerable (a turn resumed from
    ///   a checkpoint written before identities carried arguments), and
    ///   "cannot tell" is not proof of a different loop. This keeps
    ///   `Checkpoint::loop_steered`'s promise — a resumed turn never earns a
    ///   second warning for the loop it was already told about — for the one
    ///   input where honouring it and verifying it are the same act.
    pub(crate) fn escalate(&mut self, detected: &LoopIdentity) -> LoopEscalation {
        let Some(warned) = &self.warned else {
            self.warned = Some(detected.clone());
            self.spent = 1;
            return LoopEscalation::Steer;
        };
        if self.spent >= MAX_LOOP_STEERS || warned.same_loop_as(detected) != Some(false) {
            return LoopEscalation::Abort {
                warned: warned.clone(),
            };
        }
        self.warned = Some(detected.clone());
        self.spent += 1;
        LoopEscalation::Steer
    }
}

/// Loop detection, before spending a model call on a step that's already
/// stuck. Progress-aware: each call is paired with the output it produced
/// ([`recent_call_records`]), so only repeats and cycles with byte-identical
/// outputs count — legitimate polling (identical input, changing output)
/// never trips it (`crate::loop_detect`). `result_identities` is the turn's
/// pre-compaction snapshot ([`super::loop_evidence::snapshot_result_identities`])
/// — compaction runs immediately before this check and rewrites older
/// results in place, so without it a repeat streak reads as
/// `[stub, stub, real]` and never counts.
///
/// Steer first, abort second: a granted warning goes in through the same seam
/// as user steering — a user-role message the model answers this very step,
/// plus its `Steered` transcript event — and the turn continues. An abort is
/// the turn's clean end (`Some`): the model was told and is looping again, or
/// the turn's [`MAX_LOOP_STEERS`] budget is spent, so more steps would be
/// pure waste. Which of the two a detection earns is [`LoopSteerBudget`]'s
/// call, and whether the abort may claim the warned loop *persisted* is
/// [`abort_reason`]'s — when the loop that aborted is a different one, the
/// honest answer is that the steer worked (#1524).
pub(super) fn check_loop_detection(
    config: &EngineConfig,
    tools: &dyn ToolExecutor,
    messages: &mut Vec<CompletionMessage>,
    result_identities: &ResultIdentities,
    loop_steer: &mut LoopSteerBudget,
    total_cost_usd: f64,
    events: &EventSender,
) -> Option<TurnOutcome> {
    let turn_instance = config.turn_instance;
    let records = recent_call_records(messages, result_identities);
    let verdict = detect_loop(&records, config.loop_detection);
    // The typed twin of the prose steer/abort (receipts spec §6.3): a
    // receipt parses this instead of string-matching "stuck-loop
    // detected:" prefixes. Emitted for both outcomes — `aborted` says
    // whether this detection steered or killed the turn. Matching the
    // verdict is also the healthy-turn early return, so there is exactly
    // one place that decides "is this a loop".
    let (kind, pattern, repeats) = match &verdict {
        LoopVerdict::NoLoop => return None,
        LoopVerdict::ExactRepeat { tool, count, .. } => {
            ("exact_repeat", vec![tool.clone()], *count)
        }
        LoopVerdict::ShortCycle { pattern, repeats } => (
            "short_cycle",
            pattern.iter().map(|c| c.name.clone()).collect(),
            *repeats,
        ),
        LoopVerdict::Stagnant { tool, count } => ("stagnation", vec![tool.clone()], *count),
        // A distinct `kind` rather than folding into `exact_repeat`: a
        // receipt reading these should be able to tell "the same call three
        // times in a row" from "three times with other work between them",
        // because they are different pathologies and the second is the one
        // no contiguous scan can see (#1851).
        LoopVerdict::InterleavedRepeat { tool, count, .. } => {
            ("interleaved_repeat", vec![tool.clone()], *count)
        }
    };
    let evidence = verdict
        .evidence()
        .unwrap_or_else(|| "loop detected".to_string());
    // `identity` is `Some` for every verdict but `NoLoop`, which returned
    // above. Propagated rather than unwrapped: nothing here is worth a panic,
    // and `None` here means the same thing it means above — not a loop.
    let identity = verdict.identity()?;
    // Charged before the event is sent, because `aborted` is a claim about
    // what this detection DID and the budget is the only thing that knows.
    let escalation = loop_steer.escalate(&identity);
    let _ = events.send(AgentEvent::LoopDetected {
        turn_instance,
        kind: kind.to_string(),
        pattern,
        repeats,
        evidence: evidence.clone(),
        aborted: matches!(escalation, LoopEscalation::Abort { .. }),
    });
    let LoopEscalation::Abort { warned } = escalation else {
        // Duration priors (#1472) are not wired yet: `None` states the
        // blocking wait without a number, and the composer cites one the
        // moment a caller can supply it.
        let text = steer_text(&evidence, polling_tool(&verdict, tools).as_deref(), None);
        let _ = events.send(AgentEvent::Steered { text: text.clone() });
        messages.push(CompletionMessage::user(text));
        return None;
    };
    let reason = abort_reason(&warned, &identity, &evidence);
    let _ = events.send(AgentEvent::Error {
        message: reason.clone(),
        retryable: false,
    });
    Some(TurnOutcome::Aborted {
        reason,
        kind: AbortKind::DeliberateStop,
        cost_usd: total_cost_usd,
    })
}

/// The polling shape (#1473): ONE read-only tool re-issued against state
/// that has not changed. The detector only trips on byte-identical outputs,
/// so by verdict time "nothing changed" is established fact — what remains
/// is whether the repeated call was a status check (read-only, where "vary
/// the arguments / try another tool" is precisely wrong) or an action worth
/// varying. Cycles keep the generic steer: several tools are involved and
/// no single wait replaces them. Pure over the verdict plus the declared
/// schemas (invariant #2) — `schemas()` returns owned declarations, no I/O.
fn polling_tool(verdict: &LoopVerdict, tools: &dyn ToolExecutor) -> Option<String> {
    let tool = match verdict {
        LoopVerdict::ExactRepeat { tool, .. }
        | LoopVerdict::Stagnant { tool, .. }
        | LoopVerdict::InterleavedRepeat { tool, .. } => tool,
        LoopVerdict::NoLoop | LoopVerdict::ShortCycle { .. } => return None,
    };
    tools
        .schemas()
        .iter()
        .any(|schema| schema.name == *tool && schema.read_only)
        .then(|| tool.clone())
}

/// The first-detection steer. Generic by default; for a polling loop the
/// prescription is the ONE move that helps — a single blocking wait —
/// because the generic "vary the arguments, try a different tool" makes a
/// polling model strictly worse (#1473). The session that motivated this
/// obeyed the generic steer into a 600s `gh run watch` that timed out as a
/// tool error and voided the prompt cache, then blind `sleep` calls with no
/// signal at all. The wait cites a duration prior when one is supplied
/// (#1472's data, once a caller carries it) and is stated without a number
/// otherwise. Volatile mid-turn context either way — invariant #7 safe by
/// construction.
fn steer_text(evidence: &str, polling_tool: Option<&str>, prior_secs: Option<u64>) -> String {
    let Some(tool) = polling_tool else {
        return format!(
            "{LOOP_STEER_PREFIX}] you appear to be looping: {evidence}. Repeating the \
             same call cannot produce new information — change strategy: vary the \
             arguments, try a different tool, or report what is blocking you. If you \
             keep looping, the turn will be aborted."
        );
    };
    let sizing = match prior_secs {
        Some(secs) => format!(
            "this operation typically takes ~{secs}s here, so make ONE blocking wait \
             of at least that long (a single call whose timeout exceeds {secs}s)"
        ),
        None => "make ONE blocking wait sized to how long this operation usually \
                 takes — a single call with a generous timeout, not repeated checks"
            .to_string(),
    };
    format!(
        "{LOOP_STEER_PREFIX}] you are polling: {evidence} — `{tool}` is read-only and \
         its result has not changed between your calls. Do NOT vary the arguments or \
         switch tools: the state you are waiting on genuinely has not changed, and \
         checking it differently cannot change it. Instead, {sizing}; then check once. \
         If nothing lets you wait, report what you are blocked on and stop. If you \
         keep polling, the turn will be aborted."
    )
}

/// The abort's reason line, phrased by what the steering warning was about.
///
/// Reached only once [`LoopSteerBudget`] has decided the turn ends here, so
/// `warned` is the loop the most recent warning named. What varies is the
/// claim: "persisted" is only true when the re-detected loop *is* the warned
/// one. A different loop — which at this point means the budget ran out, not
/// that the loop went unwarned — means the steer
/// worked on the loop it named and a fresh one formed, and saying so matters
/// precisely because the two read the same in a verdict table — the false
/// "persisted" hid a working steer for months (#1524).
///
/// Which loop is which is [`LoopIdentity`]'s call, not this function's, and
/// that is the whole of the #1524 follow-up: comparing tool *names* made
/// every `bash` loop in a turn the same loop, so the claim stayed false for
/// the tool that loops most. A `None` answer is a turn resumed from a
/// checkpoint written before identities carried arguments — it cannot
/// support either claim, so it makes none.
fn abort_reason(warned: &LoopIdentity, detected: &LoopIdentity, evidence: &str) -> String {
    match warned.same_loop_as(detected) {
        None => format!("stuck-loop detected (after an earlier steering warning): {evidence}"),
        Some(true) => {
            format!("stuck-loop detected (persisted after a steering warning): {evidence}")
        }
        Some(false) => format!(
            "stuck-loop detected (a new loop after a steering warning about {}, which the \
             model did break): {evidence}",
            warned.describe()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LoopEscalation, LoopIdentity, LoopSteerBudget, LoopVerdict, MAX_LOOP_STEERS, abort_reason,
        polling_tool, steer_text,
    };
    use crate::ports::ToolExecutor;
    use proptest::prelude::*;
    use stella_protocol::tool::{ToolOutput, ToolSchema};

    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// A registry with one status-shaped (read-only) tool and one mutating
    /// tool — the classification input, nothing more.
    struct StatusAndBash;

    #[async_trait::async_trait]
    impl ToolExecutor for StatusAndBash {
        fn schemas(&self) -> Vec<ToolSchema> {
            let schema = |name: &str, read_only: bool| ToolSchema {
                name: name.into(),
                description: String::new(),
                input_schema: serde_json::json!({}),
                read_only,
                speculation_safe: read_only,
            };
            vec![schema("ci_status", true), schema("bash", false)]
        }
        async fn execute(&self, _name: &str, _input: &serde_json::Value) -> ToolOutput {
            unreachable!("steer classification never executes a tool")
        }
    }

    fn exact_repeat(tool: &str) -> LoopVerdict {
        LoopVerdict::ExactRepeat {
            tool: tool.to_string(),
            input: serde_json::json!({"wait": true}),
            count: 3,
        }
    }

    /// #1473's witness: a 3-repeat of a read-only status tool with
    /// byte-identical output steers to ONE blocking wait — where the
    /// generic text prescribed "vary the arguments", the exact move that
    /// made the motivating session strictly worse (a 600s watch timeout
    /// that voided the prompt cache, then blind sleeps).
    #[test]
    fn a_polling_loop_is_steered_to_one_blocking_wait_not_variation() {
        let verdict = exact_repeat("ci_status");
        let tool = polling_tool(&verdict, &StatusAndBash);
        assert_eq!(tool.as_deref(), Some("ci_status"));

        let text = steer_text("ci_status repeated 3×", tool.as_deref(), None);
        assert!(text.contains("ONE blocking wait"), "{text}");
        assert!(
            !text.contains("vary the arguments, try a different tool"),
            "the generic prescription is precisely wrong for a poll: {text}"
        );
        assert!(
            text.contains("has not changed"),
            "the steer must say WHY re-checking is futile: {text}"
        );
    }

    /// A supplied duration prior is cited with a concrete number (#1472's
    /// data, once a caller carries it).
    #[test]
    fn a_duration_prior_sizes_the_wait_concretely() {
        let text = steer_text("ci_status repeated 3×", Some("ci_status"), Some(720));
        assert!(text.contains("~720s"), "{text}");
    }

    /// A mutating tool keeps the generic steer: repeating an ACTION is not
    /// polling, and "vary the arguments" is real advice there.
    #[test]
    fn a_mutating_repeat_keeps_the_generic_steer() {
        let verdict = exact_repeat("bash");
        assert_eq!(polling_tool(&verdict, &StatusAndBash), None);
        let text = steer_text("bash repeated 3×", None, None);
        assert!(text.contains("vary the arguments"), "{text}");
    }

    /// A cycle involves several tools; no single wait replaces it.
    #[test]
    fn a_cycle_keeps_the_generic_steer_even_over_read_only_tools() {
        let verdict = LoopVerdict::ShortCycle {
            pattern: Vec::new(),
            repeats: 3,
        };
        assert_eq!(polling_tool(&verdict, &StatusAndBash), None);
    }

    /// An exact-repeat loop: one tool, one set of arguments.
    fn repeat(tool: &str, input: &str) -> LoopIdentity {
        LoopIdentity {
            tools: tools(&[tool]),
            inputs: Some(vec![input.to_string()]),
        }
    }

    /// A `bash` loop on `command`, the shape of nearly every real one.
    fn bash(command: &str) -> LoopIdentity {
        repeat("bash", &format!("{{\"command\":\"{command}\"}}"))
    }

    #[test]
    fn re_detecting_the_warned_pattern_claims_persistence() {
        let reason = abort_reason(&bash("cargo test"), &bash("cargo test"), "bash repeated 4×");
        assert_eq!(
            reason, "stuck-loop detected (persisted after a steering warning): bash repeated 4×",
            "the byte-exact legacy phrasing must survive for the one case it was true of"
        );
    }

    /// #1524 witness: on `prove-plus-comm` the model was warned about a
    /// `write_file` loop, stopped repeating `write_file`, and was then
    /// aborted over a fresh `edit_file` loop by a message claiming the
    /// warned loop had "persisted". The abort stays; the false claim goes.
    #[test]
    fn a_different_pattern_does_not_claim_the_warned_loop_persisted() {
        let reason = abort_reason(
            &repeat("write_file", r#"{"path":"a.rs"}"#),
            &repeat("edit_file", r#"{"path":"a.rs"}"#),
            "edit_file repeated 3×",
        );
        assert!(
            !reason.contains("persisted"),
            "a fresh loop must not be blamed on the warned one: {reason}"
        );
        assert!(
            reason.contains("write_file"),
            "the abort should name what the warning was actually about: {reason}"
        );
        assert!(
            reason.contains("edit_file repeated 3×"),
            "the detector's evidence for the loop that DID abort stays: {reason}"
        );
    }

    #[test]
    fn a_cycle_pattern_only_persists_if_the_whole_cycle_matches() {
        let cycle = LoopIdentity {
            tools: tools(&["read_file", "bash"]),
            inputs: Some(vec![
                r#"{"path":"a.rs"}"#.to_string(),
                r#"{"command":"cargo test"}"#.to_string(),
            ]),
        };
        let reason = abort_reason(&cycle, &bash("cargo test"), "bash repeated 3×");
        assert!(!reason.contains("persisted"), "{reason}");
    }

    #[test]
    fn an_unknown_warned_pattern_makes_no_claim_either_way() {
        let unknown = LoopIdentity {
            tools: Vec::new(),
            inputs: None,
        };
        let reason = abort_reason(&unknown, &bash("cargo test"), "bash repeated 3×");
        assert!(
            !reason.contains("persisted") && !reason.contains("new loop"),
            "a checkpoint-resumed turn with no recorded pattern cannot support \
             either claim: {reason}"
        );
        assert!(reason.contains("steering warning"), "{reason}");
    }

    /// A turn resumed from a checkpoint that recorded the warned *tools* but
    /// not the arguments (#1524's format) knows the tool and not the loop.
    /// Tool-name equality is exactly the inference this change removed, so
    /// it must not sneak back in through the resume path.
    #[test]
    fn a_resumed_turn_that_recorded_only_tools_still_makes_no_claim() {
        let partial = LoopIdentity {
            tools: tools(&["bash"]),
            inputs: None,
        };
        let reason = abort_reason(&partial, &bash("cargo test"), "bash repeated 3×");
        assert!(
            !reason.contains("persisted") && !reason.contains("new loop"),
            "knowing only the tool is not knowing the loop: {reason}"
        );
    }

    /// The witness, taken verbatim from the run that exposed this
    /// (`ses-1785988025124`, worker `moonshotai/kimi-k3`): warned about one
    /// `grep`, the model changed the command as instructed, looped on a
    /// different `grep` over a different file — and the abort told the user
    /// the warned loop had "persisted". Both loops are `bash`, so tool-name
    /// identity called them one loop and #1524's fix could not fire.
    #[test]
    fn two_different_bash_loops_are_not_one_loop() {
        let warned = bash(r#"grep -n \"fn open\\b\" -B 8 crates/stella-graph/src/store.rs"#);
        let detected = bash(r#"grep -n \"fn index_all\" -B 4 crates/stella-graph/src/graph.rs"#);
        assert_eq!(
            warned.same_loop_as(&detected),
            Some(false),
            "same tool, different arguments — the detector keyed on the \
             arguments, so identity must too"
        );

        let reason = abort_reason(
            &warned,
            &detected,
            "the same `bash` call … repeated 3 times",
        );
        assert!(
            !reason.contains("persisted"),
            "the model obeyed the steer and broke the warned loop; the abort \
             must not say otherwise: {reason}"
        );
        assert!(
            reason.contains("fn open"),
            "and it should name the loop the warning was actually about, or \
             a reader cannot tell the two `bash` loops apart: {reason}"
        );
    }

    /// The other half: arguments are what identity is keyed on, so the same
    /// `bash` command re-detected still reports persistence. Without this,
    /// "compare the arguments too" could be satisfied by never claiming
    /// persistence at all.
    #[test]
    fn the_same_bash_command_twice_still_persists() {
        let loop_ = bash("cargo test --workspace");
        assert_eq!(loop_.same_loop_as(&loop_), Some(true));
        assert!(
            abort_reason(&loop_, &loop_, "bash repeated 3×").contains("persisted"),
            "the byte-exact legacy phrasing must survive for the case it is true of"
        );
    }

    /// Stagnation's loop spans *differing* arguments by definition, so its
    /// identity is the tool alone and two stagnation verdicts on one tool
    /// are the same loop. Recorded as an empty input list, which must not be
    /// confused with the `None` of an unrecorded one.
    #[test]
    fn stagnation_identity_is_the_tool_alone() {
        let stagnant = |tool: &str| LoopIdentity {
            tools: tools(&[tool]),
            inputs: Some(Vec::new()),
        };
        assert_eq!(stagnant("bash").same_loop_as(&stagnant("bash")), Some(true));
        assert_eq!(
            stagnant("bash").same_loop_as(&stagnant("read_file")),
            Some(false)
        );
        assert_eq!(
            stagnant("bash").same_loop_as(&bash("cargo test")),
            Some(false),
            "an empty input list is a real identity, not an absent one"
        );
    }

    // ── The steering budget (#1743) ──────────────────────────────────────

    /// Whether this escalation is the turn's end, without naming the loop.
    fn aborted(escalation: &LoopEscalation) -> bool {
        matches!(escalation, LoopEscalation::Abort { .. })
    }

    #[test]
    fn the_first_detection_of_a_turn_is_always_a_steer() {
        let mut budget = LoopSteerBudget::default();
        let first = bash("cargo test");
        assert!(!aborted(&budget.escalate(&first)));
        assert_eq!(
            budget.warned(),
            Some(&first),
            "and it records what it warned"
        );
    }

    /// #1743's unit witness. The model was told about loop A, obeyed, and
    /// formed loop B — so it has never been told about B, and killing the
    /// turn here is both a resolve-rate loss and unfair to a compliant
    /// model.
    #[test]
    fn a_provably_different_second_loop_earns_its_own_steer() {
        let mut budget = LoopSteerBudget::default();
        let first = bash(r#"grep -n \"fn open\" crates/stella-graph/src/store.rs"#);
        let second = bash(r#"grep -n \"fn index_all\" crates/stella-graph/src/graph.rs"#);
        assert!(!aborted(&budget.escalate(&first)));
        assert!(
            !aborted(&budget.escalate(&second)),
            "same tool, different arguments — a loop the model was never warned about"
        );
        assert_eq!(
            budget.warned(),
            Some(&second),
            "the warning the abort will be phrased against is the most recent one"
        );
    }

    /// The half that keeps the ladder a ladder: obeying does not reset it.
    /// Re-detecting the loop the model WAS told about ends the turn on the
    /// first re-detection, budget remaining or not.
    #[test]
    fn the_warned_loop_re_detected_aborts_on_sight() {
        let mut budget = LoopSteerBudget::default();
        let warned = bash("cargo test");
        assert!(!aborted(&budget.escalate(&warned)));
        assert_eq!(
            budget.escalate(&warned),
            LoopEscalation::Abort {
                warned: warned.clone()
            },
            "one steer spent of two, and it still dies: the model was told"
        );
    }

    /// The cap. A model that keeps forming genuinely new loops would earn a
    /// warning every time without one, which is `max_steps` of waste — the
    /// outcome loop detection exists to prevent.
    #[test]
    fn a_third_distinct_loop_is_not_steered() {
        let mut budget = LoopSteerBudget::default();
        let (a, b, c) = (bash("one"), bash("two"), bash("three"));
        assert!(!aborted(&budget.escalate(&a)));
        assert!(!aborted(&budget.escalate(&b)));
        assert_eq!(
            budget.escalate(&c),
            LoopEscalation::Abort { warned: b },
            "the budget is spent, and the abort is phrased against the loop \
             the last warning actually named"
        );
        assert_eq!(
            MAX_LOOP_STEERS, 2,
            "the ladder above assumes the cap it documents"
        );
    }

    /// A rotated cycle is the same cycle one call later, not a new loop —
    /// otherwise a period-3 grind would earn a fresh warning every step.
    #[test]
    fn a_rotated_cycle_buys_no_second_steer() {
        let cycle = |shift: usize| {
            let names = ["read_file", "edit_file", "bash"];
            let args = [r#"{"path":"a.rs"}"#, r#"{"old":"x"}"#, r#"{"cmd":"test"}"#];
            LoopIdentity {
                tools: (0..3).map(|i| names[(i + shift) % 3].to_string()).collect(),
                inputs: Some((0..3).map(|i| args[(i + shift) % 3].to_string()).collect()),
            }
        };
        let mut budget = LoopSteerBudget::default();
        assert!(!aborted(&budget.escalate(&cycle(0))));
        assert!(
            aborted(&budget.escalate(&cycle(1))),
            "the same read → edit → test grind, reported from a window that \
             moved on by one call"
        );
    }

    /// "I cannot tell" is not proof of a different loop. A turn resumed from
    /// a checkpoint written before identities carried arguments knows it was
    /// warned and not about what, so it spends the budget rather than
    /// risking a second warning for the loop it was already told about.
    #[test]
    fn an_unidentifiable_loop_never_earns_the_second_steer() {
        let unrecorded = LoopIdentity {
            tools: tools(&["bash"]),
            inputs: None,
        };
        let mut budget = LoopSteerBudget::resumed(Some(unrecorded.clone()));
        assert_eq!(
            budget.escalate(&bash("cargo test")),
            LoopEscalation::Abort { warned: unrecorded },
        );
    }

    /// The checkpoint reconciliation, both directions: a recorded warning
    /// restores as one spent steer (never zero — a resume cannot refund the
    /// warning), and no recorded warning restores a fresh budget.
    ///
    /// This pins the LOSSY half deliberately: the count is not on the wire,
    /// so a turn resumed after spending both warnings re-opens the second
    /// one. #2809 carries the durable field, and closing it means rewriting
    /// this test rather than loosening it.
    #[test]
    fn a_resumed_turn_restores_exactly_one_spent_steer() {
        let warned = bash("cargo test");
        let mut resumed = LoopSteerBudget::resumed(Some(warned.clone()));
        assert!(
            aborted(&resumed.escalate(&warned)),
            "the warning is not refunded by the round trip"
        );
        let mut fresh = LoopSteerBudget::resumed(Some(warned));
        assert!(
            !aborted(&fresh.escalate(&bash("cargo build"))),
            "and one steer remains for a provably different loop"
        );
        assert_eq!(
            LoopSteerBudget::resumed(None),
            LoopSteerBudget::default(),
            "a turn that never steered resumes with its whole budget"
        );
    }

    /// A loop identity over a deliberately tiny alphabet, so an arbitrary
    /// sequence collides with itself often enough to exercise the
    /// same-loop rung rather than wandering through distinct loops.
    fn arb_identity() -> impl Strategy<Value = LoopIdentity> {
        proptest::collection::vec(
            (
                proptest::sample::select(vec!["bash", "read_file", "edit_file"]),
                proptest::sample::select(vec!["a", "b", "c"]),
            ),
            0..4usize,
        )
        .prop_flat_map(|pairs| {
            let tools: Vec<String> = pairs.iter().map(|(t, _)| (*t).to_string()).collect();
            let inputs: Vec<String> = pairs.iter().map(|(_, i)| (*i).to_string()).collect();
            // `None` inputs are the unrecorded resume shape, which must be
            // covered by the same properties as a fully recorded loop.
            prop_oneof![
                Just(LoopIdentity {
                    tools: tools.clone(),
                    inputs: Some(inputs)
                }),
                Just(LoopIdentity {
                    tools,
                    inputs: None
                }),
            ]
        })
    }

    proptest! {
        /// The bound that makes the whole change safe: whatever the model
        /// does, the turn cannot buy more than [`MAX_LOOP_STEERS`] warnings.
        /// Driven past its own aborts on purpose — the engine stops at the
        /// first one, so this is the strictly harder claim.
        #[test]
        fn no_detection_sequence_outspends_the_cap(
            detections in proptest::collection::vec(arb_identity(), 0..24),
        ) {
            let mut budget = LoopSteerBudget::default();
            let steers = detections
                .iter()
                .filter(|identity| !aborted(&budget.escalate(identity)))
                .count();
            prop_assert!(
                steers <= MAX_LOOP_STEERS as usize,
                "{steers} warnings spent of a {MAX_LOOP_STEERS} cap"
            );
        }

        /// A warning is never issued twice for one loop: whatever a
        /// detection just earned, re-detecting the SAME loop immediately
        /// afterwards ends the turn. This is what stops the per-loop budget
        /// becoming a licence to loop.
        #[test]
        fn re_detecting_what_was_just_warned_always_aborts(
            detections in proptest::collection::vec(arb_identity(), 1..12),
        ) {
            let mut budget = LoopSteerBudget::default();
            for identity in &detections {
                if aborted(&budget.escalate(identity)) {
                    continue;
                }
                let mut echo = budget.clone();
                prop_assert!(
                    aborted(&echo.escalate(identity)),
                    "a loop just warned about earned a second warning: {identity:?}"
                );
            }
        }

        /// Every escalation leaves a loop recorded, so the abort message can
        /// always name what the last warning was about — the `Option` that
        /// `LoopEscalation::Abort` exists to remove from the caller.
        #[test]
        fn a_charged_budget_always_knows_what_it_warned_about(
            detections in proptest::collection::vec(arb_identity(), 1..12),
        ) {
            let mut budget = LoopSteerBudget::default();
            for identity in &detections {
                if let LoopEscalation::Abort { warned } = budget.escalate(identity) {
                    prop_assert_eq!(Some(&warned), budget.warned());
                }
                prop_assert!(budget.warned().is_some());
            }
        }
    }
}
