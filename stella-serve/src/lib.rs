// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-serve` — the Stella engine as a headless, host-driven service.
//!
//! This crate lets a host (e.g. Oxagen's platform) run the Stella Rust engine
//! "under the hood": the host assembles a turn ([`SessionSpec`]), the engine
//! orchestrates it, and **every governed side effect — model calls and tool
//! calls — is remoted back to the host** over a wire protocol. The engine never
//! holds ambient authority; the host runs each effect through its own kernel and
//! metering and reports the result back. This is ADR-033 Option B (the Rust
//! sidecar), whose port surface is identical to the in-process embed, so the
//! transport is swappable.
//!
//! **Where ADR-033 lives:** in the private `oxagen-platform` repository
//! (`docs/specs/agent-engine-v2/`), not in this one — Stella's own `docs/adr/`
//! is a separate 0001-0009 series scoped to Phase 0 adaptive-context. Every
//! bare "ADR-033" in this crate means that Oxagen ADR;
//! `docs/design/serve-surface.md` is the self-contained Stella-side account.
//!
//! # Shape
//!
//! - [`Session`] drives one turn on a dedicated OS thread (the engine turn
//!   future is `!Send`), emitting a stream of [`ServerFrame`]s.
//! - Most frames are [`ServerFrame::Event`] (agent events for the UI). The
//!   reverse-RPC frames — [`ServerFrame::ProviderRequest`] and
//!   [`ServerFrame::ToolRequest`] — each carry a `request_id`; the host runs the
//!   effect and answers with [`Session::resolve_provider`] /
//!   [`Session::resolve_tool`], unblocking the parked engine step.
//! - The terminal frame is [`ServerFrame::TurnComplete`].
//!
//! The HTTP/SSE transport that exposes this over a socket is a thin layer on top
//! of [`Session`] (SSE = the frame stream; POST endpoints = the resolve calls);
//! it is deliberately a separate slice so the `!Send` concurrency bridge here is
//! provable in isolation.
//!
//! # Bounding a turn that stops making progress
//!
//! A reverse request hands control to the host, and the parked engine step holds
//! an OS thread while it waits — so a host that never answers used to wedge a
//! turn, and its thread, forever. Two bounds close that:
//!
//! - **A deadline on every reverse request**
//!   ([`SessionSpec::reverse_request_timeout`], five minutes by default). The
//!   automatic bound: an unanswered model call fails the turn, an unanswered
//!   tool call becomes a tool error the model can react to.
//! - **Cancellation** ([`Session::cancel`], or `POST /v1/turns/{id}/cancel` over
//!   HTTP). The manual bound: the caller gives up, the parked step wakes at once,
//!   and the turn unwinds to an `aborted` outcome rather than being killed — so
//!   its settled cost still reaches the host.
//!
//! See the module docs in `src/server.rs` for the HTTP shape of cancellation
//! (the route table, and why `cancel` is a path segment rather than a `DELETE`).

mod accept;
mod controls;
mod error;
mod frame;
mod history;
mod hostguard;
mod http;
pub mod observe;
mod pending;
mod remote;
mod routes;
#[cfg(feature = "schema")]
pub mod schema_export;
mod server;
mod session;
mod sessions;

pub use error::ServeError;
pub use hostguard::HostMode;
pub use frame::{
    ProviderErrorWire, ProviderOutcomeIn, ProviderResultIn, ServerFrame, ToolResultIn,
    TurnOutcomeWire,
};
pub use observe::{Metrics, Observer, ServeEvent, SharedObserver};
pub use pending::Pending;
pub use server::{
    DEFAULT_RESUME_GRACE, DEFAULT_SESSION_IDLE_TTL, MAX_RESUME_GRACE, ServeConfig, serve,
};
pub use session::{Session, SessionSpec, SettleHook, TurnSettlement};
