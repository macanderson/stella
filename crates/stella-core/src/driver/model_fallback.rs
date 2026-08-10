// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Mid-turn provider fallback on an exhausted retry ladder (#2679).
//!
//! The engine holds one provider for the whole turn, so before this module a
//! wedged provider ended the turn as `Aborted { Failure }` even when a
//! healthy fallback was configured and resolvable — every completed step's
//! work stranded in the transcript. This is the recovery rung for the
//! failure classes [`overflow_recovery`](super::overflow_recovery) does not
//! own: transport faults, 5xx, auth revocation, and rate limiting that
//! outlived the parked ladder (`driver/rate_limit.rs`, #2677) — everything
//! that surfaces as `ModelCallFailure::Exhausted`. A child module of
//! `driver` (the `settlement.rs` pattern), so the seam stays out of the
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
//! # The latch
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
    /// exhausted the retry ladder against the active one. `true` latched the
    /// swap — the caller re-runs the step against the replacement, terminal
    /// events withheld. `false` means the turn must surface the failure
    /// terminally: no resolver attached, the latch already spent, no healthy
    /// alternative, or resolution landing back on the failed provider.
    pub(crate) fn attempt_provider_fallback(
        &self,
        message: &str,
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
                "{message} — `{from}` exhausted its retries; continuing the turn on `{to}` \
                 ({reason})",
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
