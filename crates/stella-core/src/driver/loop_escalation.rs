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
//! # The stall rung
//!
//! One more thing lives here: a turn that has asked to `sleep` away its
//! allowance (#2022). It is the same claim — this turn is stuck — reached by
//! the one route loop detection structurally cannot take, because sleeps
//! separated by any other call read as a progressing window and idling costs
//! $0 against a spend-based budget. It rides the same seam
//! ([`STALL_STEER_PREFIX`] extends `LOOP_STEER_PREFIX`) and deliberately
//! carries no abort: see [`steer_stalled_turn`].
//!
//! It runs on the arm where the detector found nothing **and** on the arm
//! where a detection bought a steering warning, because a turn can be looping
//! and sleeping at once and the two steers prescribe different things. It does
//! not run on the abort arm, where the turn is already over. Its threshold is
//! `crate::loop_detect::LoopDetectionConfig::stall_steer_threshold_secs` and
//! its ceiling is [`MAX_STALL_STEERS`] (#3623).
//!
//! Split out of `driver.rs` the same way `super::settlement` was: the parent
//! file sits at its file-size ceiling. The module is `pub(crate)` so
//! `crate::step::TurnState` can hold the budget it checkpoints; the ladder's
//! policy belongs beside the ladder, not in the state container.

use stella_protocol::{AgentEvent, CompletionMessage, MessageRole, SteerCause};

use crate::event_sender::EventSender;
use crate::loop_detect::{CallRecord, LoopIdentity, LoopVerdict, detect_loop};
use crate::ports::ToolExecutor;
use crate::step::AbortKind;

use super::loop_evidence::{ResultIdentities, recent_call_records, turn_start_index};
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

/// How many stalled-turn warnings one turn may spend.
///
/// The stall rung's warn-once is read off the transcript, and that is
/// deliberate — see [`steer_stalled_turn`] — but it is warn-once only while
/// the marker is *visible*. Compaction rewrites and drops old messages, so a
/// very long, very sleepy turn earns a fresh warning every time the marker
/// goes, and each one costs a transcript message plus the model call that
/// answers it. Nothing bounded that (#3623).
///
/// Two, matching [`MAX_LOOP_STEERS`], because the argument is the same one
/// arrived at from the other side. There it is "the model was told and is
/// looping anyway"; here it is "the model was told, cannot see it any more,
/// and is still sleeping" — a second telling is worth buying, a third is a
/// turn that has heard the point twice and is not going to act on it. Unlike
/// the loop budget there is no abort behind this one, so the cap is the only
/// thing that ends the sequence, which is why it cannot be left open.
///
/// Held in [`LoopSteerBudget`], which is per-turn state and is **not**
/// checkpointed for this rung: a resumed turn restores the transcript, so the
/// marker scan still suppresses a warning the model can see, and only the
/// compaction-dropped case starts its count again. A wire field would buy
/// exactness across a resume for a bound whose whole purpose is to stop a
/// sequence, and #2022's rung was built without one on purpose.
pub(crate) const MAX_STALL_STEERS: u32 = 2;

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
/// named, how many warnings the turn has spent of [`MAX_LOOP_STEERS`], and the
/// separate tally the stall rung spends of [`MAX_STALL_STEERS`].
///
/// Two counters and not one, because the two rungs are bought with different
/// evidence and end differently: a loop steer is granted per *distinct loop*
/// and escalates to an abort, while a stall steer is granted per *lost
/// marker* and never aborts. One shared count would let a looping turn spend
/// the sleeping turn's warnings and the other way round.
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
    /// Stalled-turn warnings spent, of [`MAX_STALL_STEERS`]. Deliberately not
    /// carried by [`Self::resumed`]: see that constant's doc comment.
    stall_spent: u32,
}

impl LoopSteerBudget {
    /// The budget of a turn resumed from a [`crate::step::Checkpoint`].
    ///
    /// `recorded` is `Checkpoint::loop_steers_spent`, which is on the wire as
    /// of #2809. It is reconciled against the legacy `loop_steered: bool`
    /// rather than trusted outright, because a checkpoint written before that
    /// field decodes it as `0` (`#[serde(default)]`) while still recording the
    /// warning through `warned` — and a resume must never *refund* a warning
    /// the turn already paid for. So any recorded warning restores at least
    /// one spent steer, and a checkpoint that carries the real count restores
    /// exactly it.
    ///
    /// `warned == None` restores a whole budget whatever `recorded` says: the
    /// struct's invariant is that `spent == 0` exactly when nothing is warned,
    /// and `to_checkpoint` cannot produce that pair. Honouring a count with no
    /// identity beside it would charge a turn for a warning no abort could
    /// ever name.
    pub(crate) fn resumed(warned: Option<LoopIdentity>, recorded: u32) -> Self {
        let spent = if warned.is_some() { recorded.max(1) } else { 0 };
        Self {
            warned,
            spent,
            stall_spent: 0,
        }
    }

    /// The loop the most recent warning was about — what a checkpoint
    /// records, and `None` while no warning has been spent.
    pub(crate) fn warned(&self) -> Option<&LoopIdentity> {
        self.warned.as_ref()
    }

    /// Warnings spent so far — the other half of what a checkpoint records
    /// (#2809), and the value [`Self::resumed`] reads back.
    pub(crate) fn spent(&self) -> u32 {
        self.spent
    }

    /// Warnings still buyable after everything charged so far. `0` means the
    /// next detection of any loop ends the turn, which is what
    /// [`steer_text`] must be able to say out loud (#2810).
    ///
    /// Saturating rather than wrapping: a checkpoint is runtime data, so a
    /// `spent` above the cap is representable on the wire even though
    /// [`Self::escalate`] can never produce one.
    pub(crate) fn remaining(&self) -> u32 {
        MAX_LOOP_STEERS.saturating_sub(self.spent)
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

    /// Charge a stalled-turn warning against [`MAX_STALL_STEERS`], answering
    /// whether the turn could afford it. `&mut self` and one call for the same
    /// reason [`Self::escalate`] is shaped that way.
    pub(crate) fn spend_stall_steer(&mut self) -> bool {
        if self.stall_spent >= MAX_STALL_STEERS {
            return false;
        }
        self.stall_spent += 1;
        true
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
    // Read from the same window the verdict is, and before the transcript is
    // touched, so both rungs answer about the same turn.
    let stall_secs = turn_stall_seconds(&records);
    let stall_threshold = config.loop_detection.stall_steer_threshold_secs;
    // The typed twin of the prose steer/abort (receipts spec §6.3): a
    // receipt parses this instead of string-matching "stuck-loop
    // detected:" prefixes. Emitted for both outcomes — `aborted` says
    // whether this detection steered or killed the turn. Matching the
    // verdict is also the healthy-turn early return, so there is exactly
    // one place that decides "is this a loop".
    let (kind, pattern, repeats) = match &verdict {
        // No loop is not the same as no trouble: the stall rung is exactly
        // the turn the detector above cannot see (#2022).
        LoopVerdict::NoLoop => {
            steer_stalled_turn(messages, stall_secs, stall_threshold, loop_steer, events);
            return None;
        }
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
        // `repeats` carries the wrap count rather than the call count: a
        // receipt reading this row wants the evidence, and for a sweep the
        // evidence is how many times it went back to the start (#4042).
        LoopVerdict::MonotonicSweep { tool, wraps, .. } => {
            ("monotonic_sweep", vec![tool.clone()], *wraps)
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
        // moment a caller can supply it. The remaining count is read AFTER
        // the charge above, so it is what this warning leaves behind — zero
        // means this is the last one and the text says so (#2810).
        let text = steer_text(
            &evidence,
            polling_tool(&verdict, tools).as_deref(),
            None,
            loop_steer.remaining(),
        );
        let _ = events.send(AgentEvent::Steered {
            text: text.clone(),
            cause: SteerCause::Loop,
        });
        messages.push(CompletionMessage::user(text));
        // The two rungs COMPOSE on this arm, and that is the decision #3623
        // asked for. A turn can be looping and sleeping at once, and the two
        // steers prescribe different things — "vary the arguments" says
        // nothing about a blind `sleep`, and a model that obeys the loop steer
        // is still sleeping away the clock. Withholding the stall steer here
        // is not deferring it either: the next detection may abort, and after
        // that the turn never hears it at all.
        //
        // They compose only where the turn continues. Below this point the
        // turn is over, and a warning nobody will answer is a message written
        // into a dead transcript.
        steer_stalled_turn(messages, stall_secs, stall_threshold, loop_steer, events);
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
///
/// A monotonic sweep is excluded for the reason cycles are, arrived at from
/// the other side: nothing about it is a status check, its arguments change on
/// every call by construction, and "make one blocking wait" is advice for a
/// turn that is waiting. [`LoopVerdict::evidence`] already carries the
/// prescription that shape needs.
fn polling_tool(verdict: &LoopVerdict, tools: &dyn ToolExecutor) -> Option<String> {
    let tool = match verdict {
        LoopVerdict::ExactRepeat { tool, .. }
        | LoopVerdict::Stagnant { tool, .. }
        | LoopVerdict::InterleavedRepeat { tool, .. } => tool,
        LoopVerdict::NoLoop
        | LoopVerdict::ShortCycle { .. }
        | LoopVerdict::MonotonicSweep { .. } => return None,
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
///
/// `remaining` is the warnings still buyable AFTER this one
/// ([`LoopSteerBudget::remaining`], read once the detection has been charged),
/// and it decides the closing sentence. Since #1743 a turn holds
/// [`MAX_LOOP_STEERS`] warnings, and one closing clause meant two different
/// things depending on which warning carried it: on a non-final warning only
/// re-detecting *this* loop aborts, while on the last one the next detection
/// of ANY loop ends the turn. The model was told the same thing either way and
/// so could not tell that its last warning was its last (#2810). #1473 is the
/// precedent that this exact wording moves model behaviour, which is why the
/// two clauses are separate strings with a witness each rather than one
/// string with a hedge in it.
fn steer_text(
    evidence: &str,
    polling_tool: Option<&str>,
    prior_secs: Option<u64>,
    remaining: u32,
) -> String {
    let consequence = if remaining == 0 {
        "This is your LAST warning: the next loop detected — this one or any \
         other — ends the turn."
    } else {
        "If you keep looping, the turn will be aborted."
    };
    let Some(tool) = polling_tool else {
        return format!(
            "{LOOP_STEER_PREFIX}] you appear to be looping: {evidence}. Repeating the \
             same call cannot produce new information — change strategy: vary the \
             arguments, try a different tool, or report what is blocking you. \
             {consequence}"
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
         If nothing lets you wait, report what you are blocked on and stop. \
         {consequence}"
    )
}

/// The opening of the stalled-turn steer, for the warn-once scan below.
///
/// A qualifier on `LOOP_STEER_PREFIX`, not a marker of its own. `crate::engine_markers`
/// is a closed table: a genuinely new marker has to join it, be classified by
/// `crate::receipts::user_block_kind`, and be taught verbatim by both static
/// system prompts — real cost, and for nothing here, because a stalled turn is
/// the same claim the loop steer makes ("this turn is stuck") arrived at by a
/// different route. Riding the existing prefix means every consumer that
/// already handles a loop steer — the receipt classifier, the injection-defense
/// contract, and `super::loop_evidence::turn_start_index`, which must not read
/// an engine injection as a user turn boundary — handles this one unchanged.
/// `stall_steer_prefix_extends_the_loop_steer_marker` is what keeps that true.
pub(crate) const STALL_STEER_PREFIX: &str = "[stuck-loop warning] this turn is stalling";

/// The seconds of pure `sleep` this turn has asked for, summed over every
/// call in the window.
///
/// Reads the calls' own text, so it is *requested* seconds rather than
/// measured ones — deliberately, and in both directions: a static text-shape
/// check keeps the same transcript classifying the same way every step (a
/// timing here would make this rung nondeterministic), and the request is the
/// thing the model chose and can be steered about. It follows that a call
/// that never spent what it asked for — killed by the shell's own timeout,
/// refused by policy, erroring before it slept — still contributes its full
/// request, so this is an upper bound on wall clock and not a measurement of
/// it. That is why [`stall_steer_text`] says "asked for" and claims nothing
/// about what the clock was actually charged.
///
/// **The threshold does not discount such a call, and that is decided rather
/// than inherited (#3624).** Skipping errored calls reads the same transcript
/// text and stays as deterministic, but it goes blind on the worst real
/// shape: three `sleep 300`s that `bash` killed at its own 120s timeout burn
/// 360s of wall clock and would count zero. Clamping to the call's own
/// timeout would teach `stella-core` a tool's parameter name and its default,
/// which is exactly what matching on a `command` string avoids. What is left
/// can only steer *early*, on a rung that carries no abort and warns once per
/// turn. Any call carrying a
/// `command` string counts, so the shell tool and the shell-shaped custom
/// tools are covered without `stella-core` learning a tool's name; the
/// classifier is strict enough (`stella_core::shell_text::bare_sleep_seconds`
/// answers `None` the moment a segment does real work) that a non-shell tool
/// would have to carry a `command` field whose entire value is a sleep before
/// it could contribute.
///
/// Saturating, never wrapping: the seconds come from model-authored text, and
/// `sleep 99999999999999999999` must not be an arithmetic overflow panic
/// (invariant 5).
///
/// The `contains` is a fast path, not a second classifier: this window is
/// rebuilt on every step and a `bash` command can carry a whole heredoc, so
/// tokenizing every command every step would be quadratic across a long turn
/// (the same cost `super::loop_evidence` borrows its records to avoid). A
/// command with no `sleep` anywhere in its bytes cannot be a bare sleep, so
/// the scan is exact — it only skips work the tokenizer would have thrown
/// away.
fn turn_stall_seconds(records: &[CallRecord<'_>]) -> u64 {
    records
        .iter()
        .filter_map(|record| record.call.input.get("command")?.as_str())
        .filter(|command| command.contains("sleep"))
        .filter_map(crate::shell_text::bare_sleep_seconds)
        .fold(0u64, |total, secs| total.saturating_add(secs))
}

/// Steer a turn that is sleeping away its allowance — once *per turn*, and at
/// most [`MAX_STALL_STEERS`] times however often the marker is lost.
///
/// Warn-once is read off the transcript rather than held in
/// [`LoopSteerBudget`] or `crate::step::TurnState`: a steer the model can see
/// is the only state this rung needs, and no checkpoint field, wire change or
/// resume-reconciliation rule has to be invented to carry it. Compaction can
/// drop the marker, after which a still-sleeping turn earns a second warning —
/// which is the right answer, since the model can no longer see the first, and
/// is why the transcript scan is not the whole rule. [`MAX_STALL_STEERS`] is
/// the ceiling on how many times that can happen, because nothing else here
/// ends the sequence: this rung carries no abort (#3623).
///
/// `threshold_secs` is `crate::loop_detect::LoopDetectionConfig::stall_steer_threshold_secs`,
/// and `0` disables the rung outright — the same convention every threshold
/// beside it honours.
///
/// **The scan is bounded by [`turn_start_index`], and that is load-bearing.**
/// A transcript is session-scoped — `run_turn` is handed the same `Vec` every
/// turn, with each new user prompt appended and nothing trimmed — while the
/// seconds this gates come from `super::loop_evidence::recent_call_records`,
/// which *is* turn-bounded. Scanning the whole vector therefore made the rung
/// fire at most once per **session**: turn 1 sleeps 130s and is steered, and
/// turn 5 can sleep 1,089s in silence because turn 1's marker is still sitting
/// upstream. That is precisely the shape #2022 is about, and it is why the
/// same boundary the evidence uses is the one the marker scan has to use. The
/// boundary rule already skips a `LOOP_STEER_PREFIX` message when it looks for
/// the turn's opening prompt, so this turn's own steer stays inside the window
/// it has to be visible in.
///
/// No abort ladder here, on purpose. A loop detection proves the turn cannot
/// make progress; a large `sleep` total proves only that the turn chose a
/// costly way to wait, and killing a turn for that would spend resolve rate on
/// a shape that is sometimes merely wasteful. Steer, and let the wall-clock
/// deadline (`crate::budget::BudgetGuard::check_deadline`) be the thing that
/// ends a turn.
fn steer_stalled_turn(
    messages: &mut Vec<CompletionMessage>,
    stall_secs: u64,
    threshold_secs: u64,
    budget: &mut LoopSteerBudget,
    events: &EventSender,
) {
    if threshold_secs == 0 || stall_secs < threshold_secs {
        return;
    }
    let turn_start = turn_start_index(messages);
    if messages[turn_start..]
        .iter()
        .any(|m| m.role == MessageRole::User && m.content.starts_with(STALL_STEER_PREFIX))
    {
        return;
    }
    // Charged last, so a turn under the threshold or already carrying a
    // visible marker spends nothing: the budget bounds warnings the model was
    // actually given.
    if !budget.spend_stall_steer() {
        return;
    }
    let text = stall_steer_text(stall_secs);
    let _ = events.send(AgentEvent::Steered {
        text: text.clone(),
        cause: SteerCause::Stall,
    });
    messages.push(CompletionMessage::user(text));
}

/// What the stalled-turn steer says.
///
/// It prescribes a bounded polling loop the shell can run unaided, and names
/// **no** tool the catalog does not carry. That constraint is the scar from
/// #3555: the sibling `bash` advisory spent months telling the model to use
/// `read_output`/`wait_for`, deleted in #3244, and an instruction that cannot
/// be followed teaches the model to discount the next one too. The parked wait
/// (`crate::waiting`) is the mechanism this shape is a worse version of, but it
/// is deposited by a *tool*, not requested by the model, and no built-in
/// deposits one today — so naming it here would repeat that defect exactly.
///
/// It also does not say "vary the arguments": #1473 established that the
/// generic loop steer made a waiting model strictly worse, and blind `sleep`
/// calls were the behaviour it produced.
fn stall_steer_text(stall_secs: u64) -> String {
    format!(
        "{LOOP_STEER_PREFIX}] this turn is stalling: you have asked for {stall_secs}s of pure \
         `sleep` in this turn — commands whose entire body is a sleep, doing no other work. A \
         blind sleep charges its whole interval to the turn whether or not the thing you are \
         waiting for finished in its first second, and wall clock — not spend — is what this \
         task is measured against. If you are waiting on something, poll for the condition \
         instead of sleeping through it: a bounded retry loop that checks and exits as soon \
         as the check passes (for example `for i in $(seq 30); do <check> && break; sleep 1; \
         done`) costs a fraction of a blind wait. If you are not waiting on anything, stop \
         sleeping and do the work — or report what you are blocked on and stop."
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
mod tests;
