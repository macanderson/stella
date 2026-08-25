// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella fleet`'s child-turn metering (#4730).
//!
//! The property is the one `stella run`'s door already holds
//! (`crate::wrapper_plugin::tests::child_stream`, #3802), witnessed against
//! *this* door's stream: a plugin's `child_turn`, dispatched from a point that
//! runs between an attempt's rounds, reaches the attempt's own event stream
//! rather than the sink `SessionSubAgents::dispatch` falls back to.
//!
//! This door had the gap whole. Its lane published **nothing** on the registry
//! at any point in an attempt — [`super::AttemptDriver`] drives
//! `Engine::run_turn_with_sender` directly and never touches the slot — so
//! every child a plugin asked for on `stella fleet` metered into a sink, at
//! every point, on every round.
//!
//! The plugin, the metering dispatcher and the manifest are the shared ones in
//! [`crate::wrapper_plugin::child_turn_fixture`], so this door cannot pass
//! against a plugin the other doors never ask anything of.

use std::sync::Arc;

use super::*;
use crate::wrapper_plugin::child_turn_fixture::{asking_plugin, step_usage_count};

/// The round, stubbed so it clears the registry's event slot.
///
/// **The real [`super::AttemptDriver`] does not do this today**, and saying so
/// is the point of this comment rather than a caveat against it. That driver
/// drives the engine directly and never publishes a per-turn stream, so on
/// today's binary the slot survives a round untouched and the publication made
/// before the dispatch would be enough on its own.
///
/// The stub models the driver this door acquires the moment #3233 unblocks the
/// per-call measurement and `AttemptDriver` starts paying `turn_files`' seam
/// per turn like every other door's. Witnessing against the clearing shape is
/// what makes `RepublishingDriver` required here: a future maintainer who adds
/// that publication does not also have to rediscover that a wrapper's later
/// points would silently start metering into nothing.
struct RoundThatClearsItsStream {
    registry: Arc<stella_tools::ToolRegistry>,
}

#[async_trait(?Send)]
impl TurnDriver for RoundThatClearsItsStream {
    async fn run_turn(&mut self, _prelude: TurnPrelude) -> DrivenTurn {
        self.registry.detach_event_stream();
        DrivenTurn {
            outcome: WrapperTurnOutcome {
                completed: true,
                answer: "done".to_string(),
                tools: Some(Vec::new()),
                changed_files: None,
            },
            tamper: stella_plugin::TamperFinding::NotChecked,
        }
    }
}

/// **Witness (#4730, fleet door).** A plugin's `child_turn` at both of a
/// wrapper's points reaches the attempt's own event stream, and both spends are
/// drained by the attempt's renderer.
///
/// What each assertion distinguishes, because a green that cannot fail is not
/// evidence:
///
/// - **`found_a_stream[0]`** is the gap this issue is about, and it is `false`
///   on every build before this one: nothing published anything on this lane's
///   registry, so the first `before_turn` read an empty slot and its child's
///   `StepUsage` went to `EventSender::from_fn(|_| Ok(()))`. The money was
///   bounded by the worker's guard and reported through
///   `BoundWrapper::report_lines`, and `stella stats`, `stella usage report`
///   and the Observatory each under-reported a real spend.
/// - **`found_a_stream[1]`** is what `RepublishingDriver` answers under the
///   clearing round modelled above — see that type's doc for why the shape is
///   witnessed before the driver takes it.
/// - **`registry.events().is_none()`** is #960, and on this door it is the
///   assertion with teeth: `run_task` drops its own sender and then awaits the
///   renderer a few lines later, so a registry still holding the published
///   clone would hang a *finished* attempt. `AttemptPointStream::detach` is
///   what ends it.
///
/// No store: what this proves is which stream the events reach. That the
/// attempt's execution row is the one behind it is `run_task`'s
/// `begin_execution` call, and a store here would be testing
/// `Store::record_telemetry` rather than the wiring.
#[cfg(unix)]
#[tokio::test]
async fn a_plugins_child_turn_reaches_the_attempts_stream_at_both_points() {
    let dir = tempfile::tempdir().expect("a temp dir for the installed plugin");
    let cfg =
        crate::config::Config::for_tests(crate::config::PROVIDERS[0].clone(), "m".to_string());
    let registry = Arc::new(stella_tools::ToolRegistry::new(std::path::PathBuf::from(
        ".",
    )));
    let (wrapper, found_a_stream) = asking_plugin(dir.path(), &registry);

    // The attempt's channel and drain, assembled as `run_task` assembles them.
    // `Json` so the drain collects the events themselves rather than a frame.
    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let renderer = crate::agent::persistence::spawn_renderer(
        rx,
        crate::OutputFormat::Json,
        None,
        cfg.provider.id.to_string(),
        false,
        None,
    );

    // The publication `run_task` makes before the dispatch.
    let points = AttemptPointStream::new(&tx);
    points.publish(&registry, &cfg);

    let mut round = RoundThatClearsItsStream {
        registry: Arc::clone(&registry),
    };
    let report = {
        let mut rounds =
            crate::wrapper_plugin::RepublishingDriver::new(&mut round, &registry, &cfg, &points);
        wrapper
            .dispatch
            .run(
                RoundInput {
                    goal: "put the retry back".to_string(),
                    signals: crate::wrapper_plugin::pre_turn_signals(false, false),
                    candidate: None,
                },
                &mut rounds,
            )
            .await
            .expect("the declared stage program resolves")
    };
    assert!(report.faults.is_empty(), "{:?}", report.faults);

    // The teardown `run_task` performs: the stream comes down with the dispatch
    // that opened it, then this lane's own sender goes and the renderer drains.
    points.detach(&registry);
    assert!(
        registry.events().is_none(),
        "the stream comes down with the dispatch — a registry still holding a sender \
         keeps the renderer's channel open and hangs a finished attempt (#960)"
    );
    // `points` holds a sender over the attempt's channel, so it goes before the
    // drain — in `run_task` that happens on its own, because the binding lives
    // inside the wrapped match arm and dies with it well above the `drop(tx)`
    // this mirrors.
    drop(points);
    drop(tx);
    let drained = renderer.await.unwrap_or_default();

    assert_eq!(
        found_a_stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_slice(),
        [true, true],
        "both points read the registry's slot at dispatch time and found a stream — \
         the first because the lane publishes before the dispatch, the second because \
         the decorator puts it back after the round"
    );
    assert_eq!(
        step_usage_count(&drained.events),
        2,
        "both children's spend reached the attempt's drain: {:?}",
        drained.events
    );
}
