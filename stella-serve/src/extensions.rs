// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Server-side hooks: the operator's seam into a served turn (#1298).
//!
//! `stella-core` ships two hook systems and this module attaches exactly one
//! of them.
//!
//! - `stella_core::hooks` is the **settings-declared shell command** engine.
//!   It runs operator-authored `bash -c` at lifecycle moments, and it is
//!   deliberately **not** reachable from this crate. The sidecar's whole
//!   containment story (ADR-033 Option B, `crate::remote`) is that the engine
//!   holds no ambient authority: every model and tool call is remoted to the
//!   host. A hook system that spawned shells here would hand back, through a
//!   side door, precisely the local execution the front door refuses.
//! - `stella_core::bus` is the **in-process** hook bus: typed handlers
//!   registered against a catalog of dotted event names, observing the turn
//!   and — on an explicit allowlist — intercepting it. That is what a served
//!   turn attaches, through [`ServeExtension`].
//!
//! # Who may register a hook
//!
//! **The operator, in process, before the server starts. Nobody else, ever.**
//!
//! A [`ServeExtension`] is Rust code compiled into whatever binary calls
//! [`serve`](crate::serve), installed on
//! [`ServeConfig::extensions`](crate::ServeConfig::extensions) at construction
//! time. There is no route that registers, uploads, names, or configures one,
//! and adding one would be a security change, not a feature: a remote caller
//! that could install a handler could read every tool call's raw input —
//! including file contents and shell command lines — and `Deny` its way into
//! controlling what the engine does. The bearer token authenticates *a host*,
//! not the operator, and those are not the same principal.
//!
//! The shipping `stella-serve` binary registers none, so its behavior is
//! unchanged; extensions exist for a host that embeds this crate and wants its
//! own audit or policy plane inside the sidecar. See the module docs in
//! `crate::server` for what the *transport* authenticates.
//!
//! # What an extension can and cannot do
//!
//! Registration is by kind, and the two kinds have different powers — the
//! split is `stella_core::bus`'s, not this crate's:
//!
//! - `HookBus::on` — an **observer**. It watches; it can never veto, delay or
//!   corrupt the turn. A handler that returns `Err` or overruns its latency
//!   budget is isolated and, if it keeps overrunning, quarantined.
//! - `HookBus::on_blocking` — a **policy hook** on the interceptable
//!   allowlist. Server-side that is `tool.call.requested`: the chain runs
//!   before the tool request frame reaches the host, and may `Deny` (the model
//!   sees a named error instead of a result) or `Modify` (the input the host
//!   is asked to run is rewritten). The file/command chains the CLI's
//!   `ToolRegistry` also gates are absent here for the same reason the shell
//!   engine is: the sidecar does not touch the filesystem, so it has no
//!   file operation of its own to intercept.
//!
//! # Lifetime
//!
//! One [`HookBus`] per turn, minted on the session thread and dropped with it.
//! [`ServeExtension::install`] is called once per turn with that bus, before
//! the first step runs — so a subscription registered there sees the turn's
//! opening event, and no subscription outlives the turn it was made for.

use stella_core::bus::HookBus;

/// An operator-installed hook plane for served turns.
///
/// Implementors register handlers in [`install`](ServeExtension::install) and
/// keep them alive for the turn by calling
/// `HookSubscription::detach` — the bus is per-turn, so a detached handler
/// dies with it rather than leaking across turns.
///
/// ```no_run
/// use stella_core::bus::{HookBus, HookDecision, names};
/// use stella_serve::ServeExtension;
///
/// /// Refuse one tool outright, whatever the host advertised.
/// struct NoDeploys;
///
/// impl ServeExtension for NoDeploys {
///     fn name(&self) -> &str {
///         "no-deploys"
///     }
///
///     fn install(&self, bus: &HookBus) {
///         bus.on_blocking(names::TOOL_CALL_REQUESTED, |event| {
///             match event.payload["tool"].as_str() {
///                 Some("deploy") => HookDecision::Deny {
///                     reason: "deploys are not served from this sidecar".to_string(),
///                 },
///                 _ => HookDecision::Allow,
///             }
///         })
///         .detach();
///     }
/// }
/// ```
pub trait ServeExtension: Send + Sync + 'static {
    /// A stable, human-readable id. Used in the extension-error events an
    /// isolated handler failure produces, so a broken extension is
    /// identifiable rather than anonymous.
    fn name(&self) -> &str;

    /// Register this extension's handlers on one turn's bus.
    ///
    /// Called on the session thread, before the turn's first step. It must
    /// not block: the turn does not start until it returns.
    fn install(&self, bus: &HookBus);
}

/// Every extension the operator installed, in registration order.
///
/// A type alias rather than a wrapper struct: the ordering *is* the semantics
/// (blocking handlers run in registration order and the first `Deny` wins), so
/// a `Vec` says exactly what the bus will do with it.
pub type Extensions = Vec<std::sync::Arc<dyn ServeExtension>>;

/// Mint this turn's bus, install every extension on it, and announce the
/// session — in that order, which is the order the bus's own contract
/// requires: `HookBus::session_started` is documented as construct, subscribe,
/// *then* announce, because an observer registered after the announcement
/// would miss the one event that opens its fold.
///
/// Returns `None` when nothing is installed, so a turn on a serve deployment
/// with no extensions never mints a bus and the engine's every emit site stays
/// behind its `if let Some(bus)` — the "`None` adds zero work" contract
/// `Engine::with_bus` is written to.
pub(crate) fn install_for_turn(
    extensions: &Extensions,
    turn_id: impl Into<String>,
) -> Option<HookBus> {
    if extensions.is_empty() {
        return None;
    }
    let bus = HookBus::new(turn_id);
    for extension in extensions {
        extension.install(&bus);
    }
    bus.session_started();
    Some(bus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use stella_core::bus::{HookEvent, names};

    /// An extension that records every event it sees.
    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl ServeExtension for Recorder {
        fn name(&self) -> &str {
            "recorder"
        }

        fn install(&self, bus: &HookBus) {
            let seen = Arc::clone(&self.0);
            bus.on("*", move |event: &HookEvent| {
                seen.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(event.name.clone());
                Ok(())
            })
            .detach();
        }
    }

    /// No extensions means no bus at all — not an empty one. The engine's
    /// emit sites are all behind `if let Some(bus)`, so this is what keeps a
    /// deployment that installed nothing from paying to build payloads
    /// nobody reads.
    #[test]
    fn no_extensions_mints_no_bus() {
        assert!(install_for_turn(&Extensions::new(), "turn-abc").is_none());
    }

    /// Ordering is the contract: an observer installed here must see
    /// `session.started`, which is emitted after every install and would be
    /// missed if the announcement came first.
    #[test]
    fn installed_extensions_see_the_opening_event() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let extensions: Extensions = vec![Arc::new(Recorder(Arc::clone(&seen)))];
        let bus = install_for_turn(&extensions, "turn-abc").expect("a bus was minted");
        assert_eq!(bus.session_id(), "turn-abc");
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [names::SESSION_STARTED.to_string()]
        );
    }
}
