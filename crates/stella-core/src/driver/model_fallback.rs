// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Mid-turn provider fallback on an exhausted retry ladder (#2679).
//!
//! The engine holds one provider for the whole turn, so before this module a
//! wedged provider ended the turn as `Aborted { Failure }` even when a
//! healthy fallback was configured and resolvable — every completed step's
//! work stranded in the transcript. Its first class of failure is everything
//! that surfaces as `ModelCallFailure::Exhausted`: transport faults, 5xx,
//! auth revocation, and rate limiting that outlived the parked ladder
//! (`driver/rate_limit.rs`, #2677). Its second is the one rejection
//! [`overflow_recovery`](super::overflow_recovery) cannot compact its way out
//! of — a transcript still too large once that ladder is spent, which a
//! provider with a wider window may accept unchanged (#2770). A child module
//! of `driver` (the `settlement.rs` pattern), so the seam stays out of the
//! god-file ceiling.
//!
//! # How the fallback is chosen
//!
//! Through the same routing that chose the primary, never a hardcoded model
//! list: the engine asks the [`crate::ports::FallbackResolver`] port, whose
//! production impl re-resolves the engine's role via
//! [`crate::router::Router::resolve`]. The failing calls already fed the
//! router's circuit breaker through [`crate::ports::ProviderOutcomes`]
//! (#2673) — the ladder records the failure *before* settlement asks for a
//! fallback — so resolution naturally routes around the sick provider. A
//! resolution that lands back on the failed provider is treated as "no
//! fallback" and the turn aborts exactly as it did before this module.
//!
//! # The latch, and why its scope is the engine rather than the turn
//!
//! `Engine::provider_override` is a set-once cell, and the set IS the
//! bound: at most one swap per engine, ever, so two sick providers can
//! never ping-pong a turn between them. The override persists across the
//! engine's remaining turns — the swap is a routing decision, not a
//! one-call patch — and the router's breaker (cooldown, then a half-open
//! trial) is what eventually routes fresh engines back to a recovered
//! primary. Like the overflow ladder, the latch is deliberately not
//! checkpointed: a resumed turn starts the allowance over, which only
//! re-permits a bounded amount of work.
//!
//! "Persists across the engine's remaining turns" was the stated intent from
//! #2679 and was **not** what the code did until #2918. The cell was a plain
//! `OnceLock` that [`Engine::with_turn_halt`] and
//! [`Engine::with_turn_instance`] *copied*, and the pipeline's execute stage
//! builds one such copy per turn off a single `&Engine`. A swap latched
//! inside turn one therefore died with turn one: turn two started on the
//! roster's original choice, paid the dead provider's full retry ladder over
//! again, and announced a second swap of its own. That is one wasted round
//! trip and one extra user-visible `Error` per turn on a run that had
//! already established the provider is down — and #2915, which buys the
//! worker extra turns on an unproven result, made it the ordinary case
//! rather than a rarity. The cell is now an `Arc<OnceLock<_>>`, so every
//! copy is the same cell.
//!
//! **The scope is the engine, which is the candidate — never the session and
//! never the process.** That is the deliberate answer to "a transient outage
//! should not permanently re-route a long run": an engine is constructed per
//! candidate (`Pipeline::run_shared_candidates`, `fanout_stage`) and per CLI
//! session loop, so a latch expires with the unit of work that observed the
//! failure. Nothing carries a swap into the next candidate, the next `stella
//! run`, or a resumed turn. What routes a *fresh* engine is the router's
//! breaker, which is where recovery belongs, because unlike this latch it
//! actually re-probes the provider (cooldown, then a half-open trial)
//! instead of remembering one bad minute forever. Widening the latch to the
//! session would remove that re-probe; narrowing it back to the turn is the
//! behaviour #2918 removed.
//!
//! # Transcript repair
//!
//! The failed call appended nothing, so the engine's own path is already
//! well-paired at this boundary; history a caller handed in (or appended
//! to) mid-turn can still carry an unanswered `tool_use`, which the next
//! provider would reject outright. The swap closes those through the same
//! [`crate::step::close_open_tool_calls`] repair the cancel and soft-stop
//! exits use — deterministic, and mirrored onto the event stream. The other
//! thing a naive provider switch replays into a 400 — model-signed
//! thinking/reasoning blocks — cannot occur here structurally:
//! `stella_protocol::CompletionMessage` carries only
//! `role`/`content`/`tool_calls`/`tool_results`/`attachments`, so signed
//! reasoning never enters the transcript the fallback replays. (The prompt
//! cache is necessarily cold on the replacement provider; that is the cost
//! of finishing the turn at all.)
//!
//! # What consumers see
//!
//! While the fallback is deciding, the terminal channels stay silent — the
//! ladder withholds `RetriesExhausted`/`Error { retryable: false }`,
//! exactly as overflow recovery withholds its own — so an observer never
//! tears down a session that is about to recover. A latched swap announces
//! itself as `AgentEvent::ProviderFallback` (L-M7: never a silent mid-turn
//! family switch) plus a retryable `Error` notice on the same channel the
//! overflow rungs use; a refused swap surfaces the terminal pair
//! byte-identical to the pre-#2679 abort. Every discarded attempt was
//! already billed through the ladder's per-attempt `UsageIncomplete`
//! observer — the swap changes what happens *next*, never whether a failed
//! attempt is accounted.

use stella_protocol::AgentEvent;

use super::Engine;
use crate::event_sender::EventSender;
use crate::step::TurnState;

/// The `ToolOutput::Error` that closes a `tool_use` left unanswered by
/// caller-supplied history when the turn switches providers — the same
/// repair, at the same safe boundary, as `BUDGET_ABORT_TOOL_RESULT` and
/// `SOFT_STOP_TOOL_RESULT`, with wording that names why.
pub(crate) const FALLBACK_TOOL_RESULT: &str =
    "not executed — the turn switched to a fallback provider after exhausted retries";

/// Why the turn is asking for a replacement provider.
///
/// The two failure classes that reach the fallback are true of different
/// things, and the notice has to say which: an exhausted ladder is a claim
/// about the provider's *health*, an overflow is a claim about its *window*.
/// A swap announced as "exhausted its retries" when the ladder never ran once
/// is the misclassification #2743 names elsewhere, arriving by another route.
#[derive(Clone, Copy)]
pub(crate) enum FallbackCause {
    /// The retry ladder ran out against the active provider (#2679).
    RetriesExhausted,
    /// The transcript still exceeds the active provider's context window
    /// after the compaction ladder is spent (#2770).
    ContextOverflow,
}

impl FallbackCause {
    /// What this cause says about the provider being left, for the notice.
    fn clause(self, from: &str) -> String {
        match self {
            Self::RetriesExhausted => format!("`{from}` exhausted its retries"),
            Self::ContextOverflow => {
                format!("`{from}` rejected the transcript as too large and compaction is spent")
            }
        }
    }
}

impl<'a> Engine<'a> {
    /// The provider this engine's calls go to: the constructor-time primary
    /// until a fallback latches, the replacement afterwards. Every site that
    /// dispatches or attributes a model call reads through this — a site
    /// reading `provider` directly after a swap would call (or bill) the
    /// dead primary.
    pub(crate) fn active_provider(&self) -> &'a dyn stella_protocol::Provider {
        self.provider_override
            .get()
            .copied()
            .unwrap_or(self.provider)
    }

    /// Try to continue the turn on a replacement provider after `message`
    /// ended the call against the active one for `cause`. `true` latched the
    /// swap — the caller re-runs the step against the replacement, terminal
    /// events withheld. `false` means the turn must surface the failure
    /// terminally: no resolver attached, the latch already spent, no healthy
    /// alternative, or resolution landing back on the failed provider.
    ///
    /// The set-once latch is the entire bound for both causes, which is why
    /// the overflow rung needs no bound of its own: it can burn at most one
    /// paid call on a replacement whose window may be no larger, and a
    /// replacement that also overflows finds both the compaction ladder and
    /// this latch spent, so it is terminal with no loop.
    pub(crate) fn attempt_provider_fallback(
        &self,
        message: &str,
        cause: FallbackCause,
        state: &mut TurnState,
        events: &EventSender,
    ) -> bool {
        let Some(resolver) = self.fallback else {
            return false;
        };
        // The latch is the bound (module docs): one swap per engine.
        if self.provider_override.get().is_some() {
            return false;
        }
        let from = self.active_provider().id().to_string();
        let Some(resolved) = resolver.resolve_fallback(&from) else {
            return false;
        };
        let to = resolved.provider.id().to_string();
        if to == from {
            return false;
        }
        if self.provider_override.set(resolved.provider).is_err() {
            // Unreachable single-threaded (the step loop is the only
            // writer), but a lost race must refuse rather than announce a
            // swap that did not take.
            return false;
        }
        crate::step::close_open_tool_calls(&mut state.messages, FALLBACK_TOOL_RESULT, events);
        let _ = events.send(AgentEvent::ProviderFallback {
            from: from.clone(),
            to: to.clone(),
            reason: resolved.reason.clone(),
        });
        // The same retryable-notice channel the overflow rungs announce on:
        // an observer must read this as the turn continuing, never ending.
        let _ = events.send(AgentEvent::Error {
            message: format!(
                "{message} — {clause}; continuing the turn on `{to}` ({reason})",
                clause = cause.clause(&from),
                reason = resolved.reason
            ),
            retryable: true,
        });
        // The retried call is a new step, mirroring overflow recovery: its
        // receipts can never collide with what the failed step emitted.
        state.step += 1;
        true
    }
}
