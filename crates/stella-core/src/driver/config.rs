// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! [`EngineConfig`] -- what a turn is configured to do, as opposed to what
//! drives it.
//!
//! Split out of `driver.rs` (#3410). That file is a grandfathered god file
//! sitting exactly on its ceiling, and the lane stamp needed room; this is
//! the coherent unit to move rather than the smallest one. The config is a
//! genuinely separate concern from the step loop -- it is the *declaration*
//! a caller makes, where `driver.rs` is the machinery that honours it -- and
//! `driver/settlement.rs` and `driver/capabilities.rs` are the precedent for
//! a sibling submodule reaching the same private state.
//!
//! Behaviour is unchanged: the struct, its field docs, [`TurnHalt`] and the
//! `Default` impl moved whole, and `driver` re-exports them, so every
//! existing path (`stella_core::EngineConfig`, `driver::EngineConfig`) still
//! resolves.

use std::sync::Arc;
use std::time::Duration;

use stella_protocol::ReasoningEffort;

use crate::loop_detect::LoopDetectionConfig;
use crate::retry::RetryPolicy;

// Named only by this module's doc comments, which moved here with the fields
// that carry them. Importing them keeps those intra-doc links resolving
// instead of degrading the prose to plain text.
//
// The reason rides the attribute rather than this comment alone: two imports
// share one paragraph, so a reader (and `check-dead-code-allows`) can only
// associate it with the first (#3949).
#[allow(unused_imports, reason = "named by this module's intra-doc links")]
use super::Engine;
#[allow(unused_imports, reason = "named by this module's intra-doc links")]
use crate::budget::BudgetGuard;

/// Everything about a turn's execution that isn't the provider/tools
/// themselves: prompt shape, retry/compaction/loop tuning, and hard
/// backstops. `Default` gives sensible starting values for `stella-cli`.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub effort: Option<ReasoningEffort>,
    /// Thinking-mode enable/disable forwarded to every completion —
    /// `CompletionRequest::reasoning` semantics (`None` = provider default).
    pub reasoning: Option<bool>,
    /// Sampling/routing overrides forwarded to every completion —
    /// `CompletionRequest::params` semantics (each adapter forwards the
    /// subset its dialect supports).
    pub params: Option<stella_protocol::GenerationParams>,
    pub retry_policy: RetryPolicy,
    pub loop_detection: LoopDetectionConfig,
    /// Compaction fires once the estimated conversation size exceeds this
    /// many tokens (`crate::estimator`). When calibration is attached
    /// ([`Engine::with_calibration`]) the comparison uses the
    /// drift-corrected estimate, so this budget is honored in the model's
    /// own observed tokens rather than raw heuristic tokens.
    pub compaction_budget_tokens: u64,
    /// Age-based tool-result retention (#1285): results older than this many
    /// tool-bearing steps are middle-out aged on every step, gated so the
    /// prompt-cache prefix is rewritten only when real bytes come back
    /// (`compaction::RETENTION_MIN_RECLAIM_CHARS`). This is what holds the
    /// standing context roughly flat through a long turn — the budget above
    /// fires only near the context ceiling, which on a long trial is the very
    /// end of exactly the runs it should have shaped from the middle
    /// (measured: 4× more input per step than a comparator that trims old
    /// tool output, and ~quadratic total input growth). `None` disables the
    /// pass, restoring pure budget-triggered compaction.
    pub tool_result_horizon_steps: Option<usize>,
    /// When eviction/dedup/aging alone cannot reach the compaction budget
    /// (the oversized content is protected user/assistant text, or already
    /// stubbed), replace the oldest span of the conversation with a
    /// model-written summary instead of letting the next call overflow the
    /// provider's context window. Costs one cheap completion, metered into
    /// the same [`BudgetGuard`] as every other call.
    pub summarize_overflow: bool,
    /// Messages at the conversation tail the summarizer never touches —
    /// the recent work the model is actively reasoning over.
    pub summarize_keep_recent: usize,
    /// Hard backstop on step count, independent of loop detection — belt
    /// and suspenders, never the *primary* stuck-loop defense (that's
    /// `crate::loop_detect`).
    pub max_steps: usize,
    /// Hard backstop on how long one tool dispatch may run, independent of
    /// each tool's own bound — the same belt-and-suspenders philosophy as
    /// [`Self::max_steps`], and never the *primary* mechanism.
    ///
    /// Per-tool bounds are the real defense (bash clamps its own timeout,
    /// scripts cap out, MCP bounds each call), but each is that tool's own
    /// responsibility. A tool that misses its bound — a future native tool,
    /// an MCP server that overrides its timeout upward, a wedged blocking
    /// read — would otherwise park the step forever: loop detection, budget
    /// checks and soft-stop all fire at *step boundaries*, so a headless
    /// `stella run` hangs indefinitely with no way out but a hard cancel.
    ///
    /// When the ceiling trips, the call resolves to a `ToolOutput::Error`
    /// the model can react to, turning "hung forever" into one failed call
    /// the loop routes around. Deliberately generous (15 minutes) so it
    /// never pre-empts a legitimately slow tool. `None` disables the
    /// backstop entirely, restoring the unbounded await.
    ///
    /// Note this is the one place the engine may interrupt a tool in
    /// flight; the budget-abort invariant (clean aborts at step boundaries,
    /// never mid-tool) is unaffected, because a trip is surfaced as a tool
    /// *result*, not as a turn abort.
    pub tool_timeout: Option<Duration>,
    /// Backstop on a **single generation** — one provider dispatch, excluding
    /// the backoff sleeps between attempts. `None` restores the unbounded
    /// await.
    ///
    /// Without it nothing above the transport bounds a model call: the only
    /// limit is whichever reqwest deadline the adapter happens to carry, and
    /// those are per-read stalls, not whole-response bounds. Bedrock is the
    /// worst case — it is unary, so its 600s `UNARY_READ_TIMEOUT` covers the
    /// entire generation, and its expiry used to classify as retryable
    /// `Transport`, so one wedged request cost four full 600s attempts
    /// (~40 minutes) before surfacing.
    ///
    /// A trip is reported as `ProviderError::Terminal`, never `Transport`,
    /// following `stella-serve`'s reverse-RPC deadline: a provider that is
    /// simply not answering must not be handed the same unbounded wait again
    /// once per retry, which would multiply the very window the deadline
    /// exists to close.
    ///
    /// 13.6 minutes of SILENCE, not of elapsed time: `bounded_generation`
    /// measures idle gaps between stream fragments, so a generation that
    /// keeps producing is never cut however long it runs. The number is
    /// measured rather than chosen. It was 10 minutes (matching
    /// `UNARY_READ_TIMEOUT`) on the reasoning that no generation a caller
    /// still wants goes longer without progress. Context for the size: on
    /// the Terminal-Bench 2.1 gate a comparator on the same model, same API
    /// and same effort earned reward `1.0` on single steps of 624s and 756s
    /// — steps that stream throughout and are untouched by an idle bound,
    /// but that shaped the margin chosen here for a provider that has
    /// genuinely stopped answering.
    ///
    /// The rule fixing the constant is "never be the side that stops first":
    /// 60s above the longest single step the comparator was ever rewarded for
    /// (756s). Raising it is what makes a raised output ceiling reachable —
    /// the two are one budget, and moving either alone provably does nothing,
    /// which is why the earlier 16384 → 32000 → 64000 attempts each traded
    /// one truncation mode for another instead of clearing it.
    ///
    /// This bounds the *streaming* path, which is the real request path:
    /// adapters bound their streams by `STREAM_IDLE_TIMEOUT` (120s between
    /// chunks, not a total), so nothing under this cuts a generation that
    /// keeps producing. A non-streaming adapter still carries its own
    /// `UNARY_READ_TIMEOUT` (600s) and will bind before this does — that
    /// ceiling is untouched here and remains a per-adapter transport concern.
    pub model_timeout: Option<Duration>,
    /// Wall clock one turn may spend, used to decide whether a length
    /// continuation is affordable. `None` — the default — leaves the
    /// continuation allowance a pure count, exactly as before.
    ///
    /// This is not a timeout and nothing enforces it: no future is cancelled
    /// when it elapses. It exists so the engine can decline to *start* work it
    /// cannot finish. The constraint being modelled is external — a harness
    /// kills a trial on elapsed time (Terminal-Bench at 900s) — and an engine
    /// blind to it will spend its whole continuation allowance and be
    /// destroyed mid-continuation, turning a truthful truncated answer into an
    /// `AgentTimeoutError` that reads at the results layer exactly like the
    /// agent failing the task.
    ///
    /// Set it slightly below the real deadline: the useful behaviour is
    /// stopping with an honest partial while time remains, not racing the
    /// harness to the last second.
    pub turn_budget: Option<Duration>,
    /// Working directory reported to lifecycle hooks (`crate::hooks`) as the
    /// `cwd` of every [`crate::hooks::HookPayload`]. Kept here — rather than sniffed via
    /// `std::env::current_dir()` inside the engine — so `stella-core`
    /// performs no I/O of its own: the caller
    /// (which already knows the workspace root) supplies the real path, and
    /// the `"."` default keeps hook-free turns unaffected. Only read when
    /// hooks are actually configured.
    pub cwd: String,
    /// Monotonic per-session turn index stamped onto this turn's context
    /// receipts (`BlockRegistered`/`StepManifest`, spec §3). Groups a
    /// `run_turn`'s steps across executions in one session without an
    /// event-order correlation. `0` when the caller does not track turns
    /// (the receipt is still valid — `(execution_id, step)` disambiguates
    /// within an execution).
    pub turn_instance: u32,
    /// `context.lifecycle.enabled` — Phase 2 (#713). Gates the compiled-frame
    /// identity on this turn's step manifests. The setting ships on; this
    /// field still defaults `false` so a programmatically-built config opts in
    /// deliberately rather than inheriting a product default it never read.
    pub lifecycle_enabled: bool,
    /// Where this engine's turns write their resume point, if anywhere.
    ///
    /// `None` — the default — is the behaviour every caller had before: a turn
    /// interrupted mid-flight is gone, and the host recovers by re-dispatching
    /// the prompt and paying for the work again. Attaching a sink makes the
    /// turn itself recoverable; see [`crate::step::CheckpointSink`] for the failure
    /// contract, and [`Engine::resume_turn`] for the other half.
    ///
    /// Kept here rather than passed to `run_turn` because it is a property of
    /// the host's durability arrangement, not of any one turn — the same
    /// reasoning that puts [`Self::cwd`] here rather than making the engine
    /// sniff it.
    pub checkpoint_sink: Option<Arc<dyn crate::step::CheckpointSink>>,
    /// What this session has already learned about which output ceilings its
    /// providers will fund (`super::output_budget_recovery`, #3307).
    ///
    /// `None` — the default — is exactly the behaviour every caller had
    /// before: the clamp dies with the turn that learned it, so the next turn
    /// re-sends the original ceiling and pays another 402 round-trip to be
    /// told the same thing. Attaching a handle carries the clamp across the
    /// turns of one session while leaving the per-turn rung allowance to reset
    /// with the turn, as it must.
    ///
    /// Kept here rather than passed to `run_turn` for the same reason as
    /// [`Self::checkpoint_sink`]: it is a property of the session the host is
    /// running, not of any one turn. A host builds one handle per session and
    /// clones it into each turn's config — `stella-cli` does this from
    /// `Config`, so every role of a session (default, worker, verifier) shares
    /// what any of them learned about the account paying for all of them.
    pub session_output_ceilings: Option<Arc<super::output_budget_recovery::SessionOutputCeilings>>,
    /// A host-supplied "the goal is already met — stop now" signal, consulted
    /// at every step boundary.
    ///
    /// `None` — the default — is exactly the behaviour every caller had
    /// before: the turn runs until the model stops asking for tools, the
    /// budget bites, or the step cap does.
    ///
    /// This exists because those are all *external* stopping conditions, and
    /// an agent that has already met its goal will happily keep spending
    /// against them — the measured story (Terminal-Bench run `or1`, task
    /// `pypi-server`) was recorded in the staged pipeline's `flip_halt` module
    /// doc (`crates/stella-pipeline`, deleted in #3865).
    ///
    /// The host owns the predicate because only the host knows what "done"
    /// means. The staged pipeline supplied one that fired when its flip oracle
    /// observed the tracked test go fail→pass; a wrapper plugin's own oracle
    /// is what supplies one now.
    pub turn_halt: Option<Arc<dyn TurnHalt>>,
    /// Task-mode opt-in (#2663): a completing turn that mutated the workspace
    /// and was not yet challenged is nudged once to prove its work
    /// before the declaration is accepted (`confident_zero::check`, rung 4).
    pub completion_gate: bool,
    /// How many times one turn's `Stop` hooks may hold the completion open —
    /// the host's answer to a verifying extension's *ask* (#3380).
    ///
    /// `None` is [`DEFAULT_STOP_HOLDS`], the bound every caller had before a
    /// host could say otherwise. Read through [`Self::stop_holds`], never
    /// directly, because the value is **clamped to [`STOP_HOLD_CEILING`]**:
    /// the engine spends this allowance, so the engine is the last line, and
    /// an ask is never an authority. A manifest that declares `max_holds =
    /// 1000` must not be able to buy an unbounded deny → revise → re-check
    /// loop, which is the compact→error→stop-hook→retry spiral the bound
    /// exists to cap (`driver::user_hooks` module docs).
    ///
    /// Zero is not expressible: [`clamp_stop_holds`] floors at one, because
    /// "consult this gate zero times" is how you disable a gate you
    /// configured, silently. Withhold the hook instead.
    ///
    /// `stella-core` never learns plugins exist (invariant #1), so this is a
    /// plain number: the *host* reads `LoopGrant::max_holds` off a manifest,
    /// clamps it with [`clamp_stop_holds`] so it can report the clamp, and
    /// sets this field.
    pub stop_hold_allowance: Option<u32>,
}

/// The `Stop`-hold allowance a turn gets when no host asks for one.
///
/// Three, not one, because the gate's primary tenant is verification (#3246):
/// a verifier needs at least two consultations to observe a fail→pass flip,
/// and the third admits one more revision round — the empirically common case
/// of a model landing the fix within three attempts.
pub const DEFAULT_STOP_HOLDS: u32 = 3;

/// The most `Stop` holds any host may buy, however large its ask.
///
/// Eight is a bound, not a target: it is comfortably above the four rounds a
/// verification extension has actually been observed to want, and low enough
/// that a held-open turn cannot grow the transcript indefinitely. It moves
/// only as a reviewed edit here, never as a value some manifest supplies.
pub const STOP_HOLD_CEILING: u32 = 8;

/// Clamp a declared `Stop`-hold ask into `1..=`[`STOP_HOLD_CEILING`].
///
/// Public so a host can clamp a plugin's `max_holds` at load time and *tell
/// the user* it narrowed the ask, rather than discovering the ceiling by
/// watching a turn end early. The engine applies the identical clamp again
/// when it spends the allowance ([`EngineConfig::stop_holds`]) — one
/// implementation, two callers, and the engine-side call is what makes the
/// bound a property of the engine rather than of every host's diligence.
#[must_use]
pub fn clamp_stop_holds(requested: u32) -> u32 {
    requested.clamp(1, STOP_HOLD_CEILING)
}

impl EngineConfig {
    /// This turn's effective `Stop`-hold allowance: the configured ask
    /// clamped to [`STOP_HOLD_CEILING`], or [`DEFAULT_STOP_HOLDS`] when no
    /// host asked.
    #[must_use]
    pub fn stop_holds(&self) -> u32 {
        self.stop_hold_allowance
            .map_or(DEFAULT_STOP_HOLDS, clamp_stop_holds)
    }
}

/// A caller's answer to "is the goal already met?", asked once per step
/// boundary.
///
/// Debug is a supertrait for the same reason [`crate::step::CheckpointSink`]
/// carries it: [`EngineConfig`] derives `Debug`, and a config that cannot be
/// printed is a config nobody can diagnose.
pub trait TurnHalt: Send + Sync + std::fmt::Debug {
    /// `Some(reason)` ends the turn cleanly at the next step boundary;
    /// `None` lets it continue.
    ///
    /// Asked per committed step AND inside the dispatch loop (#2661): cheap,
    /// never blocking. A mid-dispatch fire kills in-flight sibling tools and
    /// answers their calls synthetically — the transcript stays paired.
    fn halt_reason(&self) -> Option<String>;
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            // 16k, not 8k: reasoning models (e.g. glm-5.2) can spend their whole
            // output budget on chain-of-thought and get cut off before emitting
            // any answer. 16k gives the answer room to land after reasoning and
            // is within every seeded catalog model's output ceiling — and it is
            // only the fallback: a model whose catalog entry declares its own
            // output ceiling overrides this at engine assembly
            // (stella-cli::agent::engine::tuned_engine_config).
            max_output_tokens: Some(16384),
            temperature: Some(0.0),
            effort: None,
            reasoning: None,
            params: None,
            retry_policy: RetryPolicy::standard(),
            loop_detection: LoopDetectionConfig::default(),
            compaction_budget_tokens: 150_000,
            // Eight tool-bearing steps of verbatim results, matching
            // `summarize_keep_recent`'s notion of "the recent work the model
            // is actively reasoning over". Older results keep head+tail; the
            // stub says how to re-fetch the rest.
            tool_result_horizon_steps: Some(8),
            summarize_overflow: true,
            summarize_keep_recent: 8,
            max_steps: 200,
            tool_timeout: Some(Duration::from_secs(15 * 60)),
            model_timeout: Some(Duration::from_secs(816)),
            // Off by default: only a caller that knows its own deadline can
            // supply a true one, and a guessed budget would decline work the
            // caller had time for.
            turn_budget: None,
            cwd: ".".to_string(),
            turn_instance: 0,
            lifecycle_enabled: false,
            // Off by default for the same reason `turn_budget` is: only a
            // caller that owns durable storage can say where a resume point
            // belongs, and a default location invented here would write one
            // process's turns into another's session.
            checkpoint_sink: None,
            session_output_ceilings: None,
            // Off by default: a turn with no host-supplied notion of "done"
            // must behave exactly as it always has. Only a caller holding a
            // real completion signal — the pipeline's flip oracle — can say
            // the goal is met, and inventing one here would end turns that
            // had more to do.
            turn_halt: None,
            completion_gate: false,
            // No host has asked, so the turn gets `DEFAULT_STOP_HOLDS`.
            stop_hold_allowance: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stop_hold_allowance_is_clamped_not_honoured() {
        assert_eq!(EngineConfig::default().stop_holds(), DEFAULT_STOP_HOLDS);
        for (asked, granted) in [(1, 1), (5, 5), (STOP_HOLD_CEILING, STOP_HOLD_CEILING)] {
            let config = EngineConfig {
                stop_hold_allowance: Some(asked),
                ..EngineConfig::default()
            };
            assert_eq!(config.stop_holds(), granted, "asked for {asked}");
        }
        // An ask is never an authority: above the ceiling, and below the
        // floor, the host gets the bound rather than the number it named.
        for asked in [STOP_HOLD_CEILING + 1, 1000, u32::MAX] {
            let config = EngineConfig {
                stop_hold_allowance: Some(asked),
                ..EngineConfig::default()
            };
            assert_eq!(config.stop_holds(), STOP_HOLD_CEILING, "asked for {asked}");
        }
        assert_eq!(clamp_stop_holds(0), 1, "zero disables a configured gate");
    }
}
