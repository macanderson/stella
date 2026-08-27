//! The **turn in flight** and the boundaries that fence it — the half of
//! [`super::SessionModel`] that answers "what is happening now", as opposed
//! to the scrollback that records what happened.
//!
//! Two types, and they are the two ends of the same thing. [`Hud`] is live
//! state, read by the statline while the turn runs and overwritten by the
//! next one. [`TurnOpening`] is the settled stamp that state leaves on the
//! transcript when a turn opens, so the boundary can still say what the turn
//! was long after the HUD has moved on.
//!
//! Split out of `model.rs` when it crossed the 1500-line guard, on the
//! sibling-submodule remedy AGENTS.md names rather than a raised baseline —
//! the same cut `file_state` and `recall` already make, and along the same
//! seam: a `TranscriptEntry`'s supporting types live beside it, not in it.

use stella_protocol::{AgentEvent, BudgetMode, ModelCallRole, StageKind, StageName};

use super::TranscriptEntry;

// Doc-link target only: named in `TurnOpening::queued_steer`'s docs, gated by
// the fold in `super`. `cfg(doc)` keeps the intra-doc link resolving without
// an import a normal build would flag as unused.
#[cfg(doc)]
use stella_protocol::SteerCause;

/// Whether one model call is the one **answering the turn**, and so may supply
/// the model name on SPEC 6.1's opening rule.
///
/// The one named place the fold asks this, deliberately. The role vocabulary is
/// in flux — see the roleless-core work (#3903) — and a predicate spelled out at
/// each call site is a predicate that gets updated at some of them.
///
/// Two conditions, and each excludes a different wrong answer:
///
/// * **[`ModelCallRole::Worker`]** is the ordinary execution role
///   (`stella-core`'s `driver::capabilities`: "every call has a role, and
///   `Worker` is the ordinary"). Every other role is auxiliary to the turn
///   rather than the turn — a `Summarization` call is the overflow summarizer,
///   a `Verdict` call is a verifier — and naming one of those on the rule would
///   say the turn was answered by a model that never answered it.
/// * **`call_seq == 0`** is the engine's own call for the step. Auxiliary calls
///   riding the same `(turn_instance, step)` take 1, 2, … from a per-execution
///   counter, so the zero is what separates the worker from whatever shared its
///   step.
///
/// [`ModelCallRole::Unknown`] is deliberately **not** accepted, even though it
/// is the `serde` default for an absent role and so covers every session
/// recorded before call-role attribution existed. Naming a model from a call
/// this build cannot identify is the same move #4124 refused when it declined
/// to substitute the configured default: the rule would assert a routing
/// decision nothing recorded. A replayed legacy session therefore keeps the
/// blank it has today, which is the honest outcome rather than a regression.
#[must_use]
pub(crate) fn supplies_the_turns_model(role: ModelCallRole, call_seq: u64) -> bool {
    role_supplies_the_turns_model(role) && call_seq == 0
}

/// The role half of [`supplies_the_turns_model`], for the folds whose event
/// carries a role but no `call_seq`.
///
/// [`stella_protocol::AgentEvent::StepUsage`] is the case: it is the metering
/// record for a committed call and carries `role`, `provider` and `model`, but
/// no sequence number — only `StepManifest` has one. A consumer of it can
/// therefore apply the role condition and nothing else, and the choice is
/// between asking this or spelling a second role list beside the first. Two
/// role lists that must agree is how the model on the AGENTS row came to be
/// taken from any call at all (#4307), so there is one list and it lives here.
///
/// Written as an exhaustive `match` rather than a [`matches!`]: a role added to
/// the vocabulary is an `E0004` at this one site, which is what makes "every
/// other role is auxiliary" a claim the compiler keeps rather than a comment.
#[must_use]
pub(crate) fn role_supplies_the_turns_model(role: ModelCallRole) -> bool {
    match role {
        ModelCallRole::Worker => true,
        ModelCallRole::Unknown
        | ModelCallRole::Triage
        | ModelCallRole::Research
        | ModelCallRole::Plan
        | ModelCallRole::PlanRepair
        | ModelCallRole::WitnessAuthor
        | ModelCallRole::WitnessRepair
        | ModelCallRole::DistressGuidance
        | ModelCallRole::Verdict
        | ModelCallRole::AgentAuthor
        | ModelCallRole::SkillAuthor
        | ModelCallRole::DomainInference
        | ModelCallRole::Reflection
        | ModelCallRole::Summarization => false,
    }
}

/// SPEC 6.3's `◐ model` row for one settled model call, or `None` for any
/// other event.
///
/// The rate is **derived, never invented**:
/// [`AgentEvent::StepUsage`] is the driver's own per-call metering record
/// (`stella-core`'s `driver::settlement::emit_step_usage`, and
/// `accounted_call` for the management roles), and it already carries the
/// output tokens the call produced and the wall clock it took to produce
/// them. tok/s is a division over those two, so no second signal goes on the
/// wire to say a thing the wire already says.
///
/// Total over [`AgentEvent`] so the metering record stays the only place a row
/// can come from. The fold calls this from its `StepUsage` arm alone; a caller
/// handing it anything else gets no row rather than a row about nothing.
pub(crate) fn model_call_row(event: &AgentEvent) -> Option<TranscriptEntry> {
    let AgentEvent::StepUsage {
        role,
        output_tokens,
        duration_ms,
        complete,
        sub_agent_id,
        ..
    } = event
    else {
        return None;
    };
    Some(TranscriptEntry::Model {
        activity: activity_of(*role),
        tokens_per_sec: tokens_per_sec(*output_tokens, *duration_ms, *complete),
        duration_ms: *duration_ms,
        sub_agent_id: sub_agent_id.clone(),
    })
}

/// The wire token for a call role (`plan_repair`, not `PlanRepair`), so the
/// word on screen is the word in `stella-events.jsonl` — the record anyone
/// checking the row will open.
///
/// `None` for [`ModelCallRole::Unknown`], which is the `serde` default for an
/// *absent* role and so covers every session recorded before call-role
/// attribution existed. `unknown` on the head would read as an activity the
/// engine has, and it has none by that name.
fn activity_of(role: ModelCallRole) -> Option<String> {
    if matches!(role, ModelCallRole::Unknown) {
        return None;
    }
    match serde_json::to_value(role) {
        Ok(serde_json::Value::String(token)) => Some(token),
        // `ModelCallRole` is a plain `#[serde(rename_all)]` enum, so no other
        // shape is reachable. Nothing branches on the absence — it renders as
        // a head with no activity, exactly as `Unknown` does.
        _ => None,
    }
}

/// Output tokens per second over the call's own wall clock, or `None` when the
/// division has no inputs to divide.
///
/// Three absences, one answer. A provider that supplied no truthful usage
/// envelope (`StepUsage::complete` false) reports zeros that are not
/// measurements; an untimed call has nothing to divide by; a call that emitted
/// no output tokens generated nothing to rate. Each renders as no column,
/// never `0 tok/s` — the substitution [`super::ReadSize`] and
/// [`crate::views::transcript::Extent`] both already refuse, because a zero
/// asserts a measurement where there was none (#2290, #4150).
///
/// Not a second copy of `deck::AgentEntry::live_tok_per_s`, which shares the
/// arithmetic and neither numerator nor denominator. That one is the **live**
/// rate of a turn still running: the turn's output so far over the deck's own
/// clock since the turn opened, tool time included, and it needs no settled
/// record because there is none yet. This one is one **call**, settled, over
/// the wall clock that call alone took. A turn's rate and its slowest call's
/// rate are different readings, and either standing in for the other would
/// answer a question nobody asked.
fn tokens_per_sec(output_tokens: u64, duration_ms: u64, complete: bool) -> Option<u32> {
    if !complete || output_tokens == 0 || duration_ms == 0 {
        return None;
    }
    u32::try_from(output_tokens.saturating_mul(1_000) / duration_ms).ok()
}

/// What SPEC 6.1's opening rule says out loud, stamped onto the stage boundary
/// that opens a turn.
///
/// # Why the facts ride the entry
///
/// The same reason [`super::TranscriptEntry::Complete`]'s `turn` does: `render::entry`
/// renders one entry at a time and holds no session state, so a fact that is not
/// on the entry is a fact the rule cannot say.
///
/// # Why one rule per turn and not one per stage
///
/// SPEC 6.1 draws a single labelled boundary and SPEC 2 makes the turn the
/// transcript's unit. A wrapped run has four or five stages inside one turn, so
/// a rule per stage would announce `turn 14` five times and the number would
/// stop reading as the turn's identity. Which boundary is the opening one is
/// decided at fold time (`SessionModel::turn_head_stamped`) rather than at
/// render time, because the renderer cannot see the entry before this one.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnOpening {
    /// This turn's 1-based ordinal — [`super::SessionModel::turns_completed`] plus the
    /// turn in flight, so it is the number the turn's own closing rule will
    /// carry.
    ///
    /// It counts turns that *completed*. A turn that died without one is
    /// therefore not counted, and the next attempt reopens under the same
    /// ordinal — which is the honest reading of a counter that means "turns
    /// this session has finished", not a renumbering.
    pub turn: u32,
    /// The model answering **this** turn, from a call that actually happened.
    ///
    /// Opens as `None` and is back-filled by the turn's own first worker call
    /// (`AgentEvent::StepManifest`, gated on `supplies_the_turns_model` below —
    /// crate-private, so named rather than linked),
    /// rather than being stamped from [`Hud::model`] when the boundary is
    /// pushed. Two defects made that the wrong source (#4183): `Hud::model` is
    /// written only by `TurnComplete` and `RunComplete`, both of which are
    /// terminal, so it was empty for the whole of turn 1 and named *the
    /// previous turn's* model from turn 2 on.
    ///
    /// Back-filled rather than deferred because the boundary is a settled
    /// transcript entry: the rule renders the moment the stage lands, and the
    /// deck's settled-prefix fold re-renders it when the manifest arrives — the
    /// same shape the head's measured size uses (#4154).
    ///
    /// Still `None` when nothing has committed a worker call, which is honest:
    /// a turn that ended before reaching the model has no model to name, and
    /// eliding beats substituting the configured default, which is not evidence
    /// of what a router picked (#4124).
    pub model: Option<String>,
    /// This turn's spend ceiling, from [`Hud::limit_usd`].
    ///
    /// The **turn's**, not the session's, which is what makes it the right
    /// number for a rule that opens one turn: `AgentEvent::BudgetTick`'s own
    /// doc states that `spent_usd`/`limit_usd` are turn-scoped and that the
    /// session axis rides separately on `session_spent_usd`/
    /// `session_limit_usd`, which the deck does not fold.
    ///
    /// `None` means **no budget is armed**, which is not a budget of `$0.00` —
    /// the same distinction [`Hud::deadline_remaining_ms`] draws, and for the
    /// same reason: a rule printing `budget $0.00` over an uncapped run would
    /// state a cap nobody set.
    pub budget_usd: Option<f64>,
    /// The steer **a person** made mid-turn that this turn consumed (SPEC
    /// 6.1's `queued:` label).
    ///
    /// The composer promises `⏎ queue (never blocks)`, and a queue that never
    /// blocks needs a visible payoff — the moment what you typed mid-turn
    /// actually got picked up. That is what this names (#4185).
    ///
    /// Back-filled by the turn's own first
    /// [`stella_protocol::AgentEvent::Steered`] whose
    /// [`SteerCause::is_from_a_person`] holds, exactly as [`Self::model`] is
    /// back-filled by the turn's first worker call (#4183) — a steer is
    /// consumed *after* the rule is drawn, so the field opens as `None` and
    /// the deck's settled-prefix fold re-renders the rule when it arrives.
    /// First write wins, for the same reason: a second steer does not rewrite
    /// what the turn opened by consuming.
    ///
    /// The [`SteerCause`] gate is the whole point of the field's existence.
    /// The engine's loop and stall rungs emit the same event, and a rule that
    /// labelled a stall-rung auto-steer as something the user queued would be
    /// worse than the blank it replaces — which is why this label sat unfed
    /// until the cause was on the wire (#3622). A legacy event decoding as
    /// [`SteerCause::Unknown`] keeps the blank, which is honest rather than a
    /// guess.
    pub queued_steer: Option<String>,
}

/// What SPEC 6.1's receipt says, stamped onto the entry that closes a turn.
///
/// Rides the entry for the same reason [`TurnOpening`] does: `render::entry`
/// renders one entry and holds no session state, so a fact that is not on the
/// entry is a fact the receipt cannot say.
///
/// Every field here was counted. A field with no source is **absent from this
/// struct** rather than present and zero — see [`TurnCounters`] for what is
/// counted and `crate::views::transcript::Receipt` for what is still missing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnReceipt {
    /// Tokens this turn spent, summed from its `StepUsage` events.
    ///
    /// `None` when no usage event arrived — a turn answered from cache, or one
    /// whose provider reported none. Never `Some(0)` from an absence.
    pub tokens: Option<u64>,
    /// Distinct paths the turn changed, from its `FileChange` events.
    ///
    /// A genuine `0` now — every mutation emits a `FileChange`, so a turn that
    /// changed nothing counted zero rather than failing to count.
    pub files: u32,
    /// Memories written, summed over the turn's `ContextWrite` upserts.
    pub memories: u32,
    /// Wall clock the turn took, in milliseconds.
    ///
    /// `None` inside the fold, always: [`super::SessionModel`] reads no clock
    /// by contract (L-T1, `replay(&log) == replay(&log)`). The deck stamps it
    /// from `deck::AgentEntry::turn_clock_ms` on the way past, which is the
    /// same shape `parked_since_ms` already uses.
    pub elapsed_ms: Option<u64>,
}

/// The per-turn counters [`TurnReceipt`] is stamped from, reset at each turn
/// boundary.
///
/// # What is not here
///
/// **Tests.** SPEC 6.1's `4/4 tests` has no source: nothing in the event
/// stream reports a test tally. `AgentEvent::Verdict` carries a
/// pass/fail plus prose, not counts, and a `bash` call running `cargo test` is
/// opaque to the fold — its output is a `ToolResult` string nobody parses, and
/// parsing one would be a scraper guessing at a harness rather than a
/// measurement. Feeding this field needs an event that states the counts:
/// either a verification plugin reporting its `EvidenceSet` per check, or a
/// test-runner tool that returns structured results instead of text.
///
/// **`det %`.** Removed from the design; see `crate::views::transcript::Receipt`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnCounters {
    /// Summed from `AgentEvent::StepUsage`'s **token** fields only.
    ///
    /// Never its `cost_usd`: the deck's spend is driven by `BudgetTick`, and
    /// folding usage cost as well would double-count it. That hazard is why
    /// the deck ignored `StepUsage` outright, and the token half was collateral.
    pub tokens: Option<u64>,
    /// Distinct paths this turn's `FileChange` events named, in first-touched
    /// order.
    ///
    /// Distinct, not a call count: the receipt says how much of the tree moved,
    /// not how many edits it took. Bounded by the turn, so unlike
    /// [`super::SessionModel::files`] it does not outlive a `/clear`.
    pub files: Vec<String>,
    /// Summed over `AgentEvent::ContextWrite::upserts`.
    pub memories: u32,
}

impl TurnCounters {
    /// Fold one usage event's tokens in, leaving its cost alone.
    pub fn add_tokens(&mut self, input: u64, output: u64) {
        let sum = input.saturating_add(output);
        self.tokens = Some(self.tokens.unwrap_or(0).saturating_add(sum));
    }

    /// Record a path this turn changed, once however often it is touched.
    pub fn touch(&mut self, path: &str) {
        if !self.files.iter().any(|p| p == path) {
            self.files.push(path.to_string());
        }
    }

    /// The settled receipt, minus the elapsed the fold may not measure.
    #[must_use]
    pub fn settle(&self) -> TurnReceipt {
        TurnReceipt {
            tokens: self.tokens,
            files: u32::try_from(self.files.len()).unwrap_or(u32::MAX),
            memories: self.memories,
            elapsed_ms: None,
        }
    }
}

/// Live HUD numbers, all folded from the event stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hud {
    /// Spend as the budget guard reports it on every `BudgetTick`.
    ///
    /// Note this is **cumulative for the life of the guard**, not per turn: the
    /// deck builds one `BudgetGuard` for the whole session and never calls
    /// `begin_turn`, so this only ever rises. Use [`Hud::turn_spent_usd`] for
    /// the number a reader would call "what this turn cost".
    pub spent_usd: f64,
    pub limit_usd: Option<f64>,
    pub budget_mode: Option<BudgetMode>,
    /// Wall clock left before the task deadline, as the last `BudgetTick`
    /// reported it (#2240, #2435). `None` is the decisive case and means
    /// **no deadline is armed** — never "no time left", which is
    /// `Some(0)`. The status bar renders the two differently for exactly that
    /// reason: a HUD showing `0s` for an unarmed run would put back into the
    /// UI the confusion #2240 took out of the journal. `None` draws no cell at
    /// all and `Some(0)` draws the word `expired`
    /// ([`crate::views::status_bar`]).
    ///
    /// Milliseconds rather than a `Duration` because that is the wire shape
    /// (`AgentEvent::BudgetTick::deadline_remaining_ms`), and this struct is a
    /// fold of the stream, not a reinterpretation of it.
    pub deadline_remaining_ms: Option<u64>,
    /// The stage the turn is in. [`StageName`], not [`StageKind`]: a
    /// contributed stage is what the statline must be able to name.
    pub stage: Option<StageName>,
    /// The most recent stage that was one of **this host's own** boundaries.
    ///
    /// Separate from [`Hud::stage`] because the two answer different questions,
    /// and one field cannot answer both once the vocabulary is open. `stage` is
    /// "what is happening right now", which is what the statline says out loud.
    /// This is "how far through its own shape the turn has got", which is what
    /// [`crate::views::pipeline`]'s three-segment bar draws — and the bar has
    /// only the host's three phases to draw with.
    ///
    /// Keeping it is what stops a contributed stage reading as a regression: a
    /// plugin stage arriving after `execute` leaves this at `Execute`, so the
    /// bar holds. Folding the contributed stage into the same field would make
    /// it phase-less, and a phase-less stage falls back to phase 0 — the bar
    /// would snap back to "plan" and claim the run had gone backwards.
    pub host_stage: Option<StageKind>,
    pub model: Option<String>,
    /// [`Hud::spent_usd`] as it stood when the current turn began, so live turn
    /// cost is the difference. Snapshotted in [`super::SessionModel::push_user_prompt`]
    /// — the earliest signal a turn has started.
    pub turn_start_spent_usd: f64,
    /// The final turn cost, set once a `Complete` event lands.
    pub final_cost_usd: Option<f64>,
    pub complete: bool,
}

impl Hud {
    /// What the turn in flight has cost so far.
    ///
    /// Once the turn settles this yields to `Complete`'s own `cost_usd`, which
    /// is authoritative — the driver totals it directly rather than differencing
    /// a cumulative gauge, so it also catches spend that never produced a tick.
    pub fn turn_spent_usd(&self) -> f64 {
        self.final_cost_usd
            .unwrap_or_else(|| (self.spent_usd - self.turn_start_spent_usd).max(0.0))
    }
}

/// The opening rule of the turn now in flight, for a late-arriving field to
/// fill.
///
/// Walks back to the **nearest** stage boundary that opened a turn and stops
/// there, whether or not the caller fills anything. That single stop is what
/// makes every field on the rule first-write-wins per turn, and it is doing
/// two jobs:
///
/// * an earlier turn's rule is already settled and must not be rewritten by a
///   later turn's event;
/// * a sub-agent's calls also carry [`ModelCallRole::Worker`], and a child may
///   well run on a different model. The lead has to call the model before it
///   can decide to delegate, so the lead's own first worker call always
///   precedes any child's and has already claimed the rule by the time one
///   arrives. Depth is not on the wire here, so the ordering is the guarantee
///   rather than a filter.
///
/// `None` for a turn whose rule was never stamped — no stage boundary has
/// arrived yet — which leaves each caller's own fallback carrying the fact.
///
/// One walk rather than one per field: the model and the queued steer had a
/// copy each, identical but for the field they wrote, so a fix to the stopping
/// rule had two places to land and no way to notice the second.
pub(super) fn open_turn(transcript: &mut [super::TranscriptEntry]) -> Option<&mut TurnOpening> {
    transcript.iter_mut().rev().find_map(|entry| match entry {
        super::TranscriptEntry::Stage {
            opens: Some(opening),
            ..
        } => Some(opening),
        _ => None,
    })
}
