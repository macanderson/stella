// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Mid-turn provider fallback for the engines this pipeline builds (#2765) —
//! the pipeline's side of the [`stella_core::ports::FallbackResolver`] seam
//! `stella-core`'s `driver::model_fallback` opened (#2679).
//!
//! #2679 shipped the engine seam and wired the CLI's bare loops
//! (`agent/engine.rs::SessionFallback`). The pipeline was left out, and it is
//! the default `stella run` path: on it, a retry ladder exhausting mid-turn
//! still aborted the turn exactly as it did before that seam existed, with
//! every completed step's work stranded in the transcript.
//!
//! # Why this resolver is cheaper than the CLI's
//!
//! [`SessionFallback`] has to *build* an adapter on first use (the bare loops
//! know only their own primary) and park it in a set-once slot so the engine
//! can borrow it. A pipeline run already holds the whole adapter set for its
//! lifetime — that is exactly what [`ProviderResolver`] is — so this resolver
//! only ever hands back a borrow of something the caller already owns. There
//! is no slot, no build, and no failure mode where the *first* resolution
//! decides which adapter every later one sees.
//!
//! [`SessionFallback`]: https://github.com/macanderson/stella/blob/main/crates/stella-cli/src/agent/engine.rs
//!
//! # Re-resolving the stage's own role
//!
//! Each engine gets its provider from one [`ResolvedRole`], and the
//! [`StageFallback`] attached beside it re-resolves **that same role**. The
//! tie is structural rather than a second declaration at the call site:
//! [`super::Pipeline::attach`] takes the role from the resolution the engine
//! was built from, so a stage cannot fail over along a route it never
//! resolved along in the first place. The failing calls already fed the
//! router's breaker through [`stella_core::ports::ProviderOutcomes`] (#2673,
//! attached by the same `attach`), so the re-resolution naturally routes
//! around the sick provider — and a resolution landing back on it is refused
//! by the engine as "no fallback".
//!
//! # Every engine states a posture
//!
//! [`FallbackPosture`] is a required argument of `attach` rather than a
//! default, so a new engine site has to answer "what happens when this turn's
//! provider dies?" while it is being written. `Withheld` is a legitimate
//! answer — it carries the reason a reviewer can check, the same declared-gap
//! discipline `stella-protocol`'s consumer ledger applies to events — but
//! silence is not an answer this signature admits.

use stella_core::Router;
use stella_core::ports::{FallbackResolver, ResolvedFallback};
use stella_protocol::{Provider, Role};

use crate::ports::ProviderResolver;

/// What one engine does when its retry ladder exhausts mid-turn.
///
/// Passed to [`super::Pipeline::attach`] by every engine construction site.
#[derive(Debug, Clone, Copy)]
pub(super) enum FallbackPosture {
    /// Re-resolve this role through the run's router and continue the turn on
    /// the replacement (#2679/#2765). Always the role the engine's own
    /// primary provider was resolved from — see the module doc.
    ReResolve(Role),
    /// No mid-turn swap for this engine, with the reason a reviewer can
    /// check. The turn aborts on an exhausted ladder exactly as it did
    /// before the seam existed.
    Withheld(&'static str),
}

/// Re-resolves one role through the run's router, mapping the decision to an
/// adapter the run already owns.
///
/// Every miss is soft, matching the CLI's resolver and the pipeline's own
/// [`super::Pipeline::resolve_provider`]: a resolve error (every profile
/// breaker-open), a resolution landing back on the failed provider, or a
/// model the resolver has no adapter for all yield `None`, and the turn then
/// aborts exactly as it did before this seam existed.
pub(super) struct StageFallback<'a> {
    router: &'a Router,
    providers: &'a dyn ProviderResolver,
    role: Role,
}

impl<'a> StageFallback<'a> {
    fn new(router: &'a Router, providers: &'a dyn ProviderResolver, role: Role) -> Self {
        Self {
            router,
            providers,
            role,
        }
    }
}

impl<'a> FallbackResolver for StageFallback<'a> {
    fn resolve_fallback(&self, failed_provider_id: &str) -> Option<ResolvedFallback<'_>> {
        let decision = self.router.resolve(self.role).ok()?;
        // Refused here as well as in the engine. The engine's own check is on
        // `Provider::id()` and is the authoritative one; this one is on the
        // routing decision, and skips the adapter lookup for the commonest
        // miss — a single-profile run, where the only route back is the
        // provider that just died.
        if decision.model_ref.provider == failed_provider_id {
            return None;
        }
        // Read as a copy of the `&'a` field (the same move
        // `Pipeline::resolve_provider` makes) so the adapter borrow carries
        // the run's lifetime rather than this call's `&self`.
        let providers: &'a dyn ProviderResolver = self.providers;
        let provider: &'a dyn Provider = providers.provider_for(&decision.model_ref)?;
        // The breaker's own account when it forced the substitution, and a
        // plain statement of the re-resolution otherwise (a router whose
        // preference order simply names another provider first). Either way it
        // rides `AgentEvent::ProviderFallback` verbatim — L-M7: a mid-turn
        // family switch is never silent.
        let reason = match decision.fallback {
            Some(fallback) => fallback.reason,
            None => format!(
                "the {} role re-resolved to `{}` after `{failed_provider_id}` exhausted its \
                 retries",
                role_label(self.role),
                decision.model_ref,
            ),
        };
        Some(ResolvedFallback { provider, reason })
    }
}

/// The pipeline's per-role resolvers, built once per run and lent to every
/// engine [`super::Pipeline::attach`] touches.
///
/// One slot per [`Role`] rather than only the roles that back an engine
/// today: the roster can reassign a responsibility to any role
/// (`Roster::role`), so "which roles reach an engine" is a runtime fact, and
/// [`Self::get`] is exhaustive so a new role is a compile error here rather
/// than a resolution that silently finds no slot.
///
/// Held by value on the pipeline (three words per slot: two borrows and a
/// role) because the engine borrows the resolver for the rest of its turn,
/// and a resolver built inside `attach` would not outlive the engine it was
/// built for.
pub(super) struct RoleFallbacks<'a> {
    worker: StageFallback<'a>,
    triage: StageFallback<'a>,
    plan: StageFallback<'a>,
    research: StageFallback<'a>,
    verifier: StageFallback<'a>,
    embed: StageFallback<'a>,
    vision: StageFallback<'a>,
    image: StageFallback<'a>,
    video: StageFallback<'a>,
}

impl<'a> RoleFallbacks<'a> {
    pub(super) fn new(router: &'a Router, providers: &'a dyn ProviderResolver) -> Self {
        let slot = |role| StageFallback::new(router, providers, role);
        Self {
            worker: slot(Role::Worker),
            triage: slot(Role::Triage),
            plan: slot(Role::Plan),
            research: slot(Role::Research),
            verifier: slot(Role::Verifier),
            embed: slot(Role::Embed),
            vision: slot(Role::Vision),
            image: slot(Role::Image),
            video: slot(Role::Video),
        }
    }

    /// The resolver for `role`. Exhaustive on purpose — see the type doc.
    pub(super) fn get(&self, role: Role) -> &StageFallback<'a> {
        match role {
            Role::Worker => &self.worker,
            Role::Triage => &self.triage,
            Role::Plan => &self.plan,
            Role::Research => &self.research,
            Role::Verifier => &self.verifier,
            Role::Embed => &self.embed,
            Role::Vision => &self.vision,
            Role::Image => &self.image,
            Role::Video => &self.video,
        }
    }
}

/// The role's name for the fallback reason string. Exhaustive so a new role
/// names itself rather than inheriting a neighbour's label.
fn role_label(role: Role) -> &'static str {
    match role {
        Role::Worker => "worker",
        Role::Triage => "triage",
        Role::Plan => "plan",
        Role::Research => "research",
        Role::Verifier => "verifier",
        Role::Embed => "embed",
        Role::Vision => "vision",
        Role::Image => "image",
        Role::Video => "video",
    }
}
