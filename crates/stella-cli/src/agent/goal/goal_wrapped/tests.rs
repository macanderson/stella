// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella goal --pipeline <variant>`'s child-turn metering (#4730).
//!
//! The property is the one `stella run`'s door already holds
//! (`crate::wrapper_plugin::tests::child_stream`, #3802), witnessed against
//! *this* door's stream: a plugin's `child_turn`, dispatched from a point that
//! runs between the rounds, reaches the goal run's own event stream and its
//! execution row — not the sink `SessionSubAgents::dispatch` falls back to, and
//! not the round's own per-turn sender.
//!
//! The plugin, the metering dispatcher and the manifest are the shared ones in
//! [`crate::wrapper_plugin::child_turn_fixture`], so this door cannot pass
//! against a plugin the other doors never ask anything of.

use std::sync::{Arc, Mutex};

use super::*;
use crate::wrapper_plugin::child_turn_fixture::{asking_plugin, step_usage_count};

/// The round, stubbed: it **replaces** the registry's event slot with a sender
/// of its own, which is what this door's real round does.
///
/// Replacing rather than clearing is the difference from `stella run`'s
/// witness. [`GoalRoundDriver::run_turn`] opens one observed sender per
/// internal turn and publishes it with
/// `persistence::attach_run_streams`; nothing on this door detaches between
/// rounds, so after a round the slot holds *that round's* sender rather than
/// nothing.
///
/// **The stand-in deliberately does not forward to the run's channel**, and
/// that is the instrument the assertion turns on. The real per-turn sender is a
/// `TurnFacts` tap over a clone of the run's channel, so a child that metered
/// into it would still reach the drain and be indistinguishable from one that
/// metered into the run's own stream. Cutting the forward is what lets this
/// witness say *which* stream each child actually found: anything landing in
/// `caught` is a child that was metered against a round instead of the run.
struct RoundThatReplacesItsStream {
    registry: Arc<stella_tools::ToolRegistry>,
    cfg: Config,
    /// Everything the round's own sender received — expected to stay empty.
    caught: Arc<Mutex<Vec<AgentEvent>>>,
}

#[async_trait(?Send)]
impl TurnDriver for RoundThatReplacesItsStream {
    async fn run_turn(&mut self, _prelude: TurnPrelude) -> DrivenTurn {
        let caught = Arc::clone(&self.caught);
        let round_sender = stella_core::EventSender::from_fn(move |event| {
            caught
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event);
            Ok(())
        });
        persistence::attach_run_streams(&self.registry, &self.cfg, &round_sender, None);
        DrivenTurn {
            outcome: WrapperTurnOutcome {
                completed: true,
                answer: "done".to_string(),
                tools: Some(Vec::new()),
                changed_files: Some(Vec::new()),
            },
            tamper: stella_plugin::TamperFinding::NotChecked,
        }
    }
}

/// **Witness (#4730, goal door).** A plugin's `child_turn` at both of a
/// wrapper's points reaches the goal run's own event stream, and both spends
/// are drained by the run's renderer.
///
/// What each assertion distinguishes, because a green that cannot fail is not
/// evidence:
///
/// - **`found_a_stream[0]`** is the gap this issue is about. Before the fix
///   this door published nothing before its first round, so `before_turn` —
///   which runs before any turn has claimed the registry's slot — read an empty
///   slot and its child's `StepUsage` went to
///   `EventSender::from_fn(|_| Ok(()))`. It is `false` without
///   `GoalPointStream::publish` above the loop.
/// - **`caught` staying empty** is the half `RepublishingDriver` answers, and
///   it is why the stub above does not forward. Without the decorator the slot
///   after round one still holds *that round's* sender, so `after_turn`'s child
///   is metered against a round that has already reported and closed its facts
///   — money attributed to the wrong stream rather than lost. Both events land
///   in `caught` and the drain sees one instead of two.
/// - **`registry.events().is_none()`** is #960: while a stream is published the
///   registry holds a live sender on the renderer's channel, so a goal run that
///   finished would hang on a `recv()` that never ends.
///
/// No store (`execution: None`), for the reason `stella run`'s witness gives:
/// what this proves is which stream the events reach. That the goal run's
/// execution row is the one behind that stream is
/// [`run_goal_wrapped_turn`]'s `begin_execution` call, and a store here would
/// be testing `Store::record_telemetry` rather than the wiring.
#[cfg(unix)]
#[tokio::test]
async fn a_plugins_child_turn_reaches_the_goal_runs_stream_at_both_points() {
    let dir = tempfile::tempdir().expect("a temp dir for the installed plugin");
    let cfg =
        crate::config::Config::for_tests(crate::config::PROVIDERS[0].clone(), "m".to_string());
    let registry = Arc::new(stella_tools::ToolRegistry::new(std::path::PathBuf::from(
        ".",
    )));
    let (wrapper, found_a_stream) = asking_plugin(dir.path(), &registry);

    // The run's channel and drain, assembled exactly as `run_goal_wrapped_turn`
    // assembles them above its round loop. `Json` so the drain collects the
    // events themselves rather than a rendered frame.
    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let renderer = persistence::spawn_renderer(
        rx,
        crate::OutputFormat::Json,
        None,
        cfg.provider.id.to_string(),
        false,
        Some("put the retry back".to_string()),
    );

    // The publication the round loop makes before its first round.
    let points = GoalPointStream {
        tx: stella_core::EventSender::new(tx.clone()),
        execution: None,
    };
    points.publish(&registry, &cfg);

    let caught: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let mut round = RoundThatReplacesItsStream {
        registry: Arc::clone(&registry),
        cfg: cfg.clone(),
        caught: Arc::clone(&caught),
    };
    let report = {
        let mut rounds = RepublishingDriver::new(&mut round, &registry, &cfg, &points);
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

    // The teardown `run_goal_wrapped_turn` performs below its loop, in its
    // order. `points` first: it holds a sender over the run's channel, so a
    // stream still alive here keeps that channel open and hangs the drain
    // below — which is exactly what this test caught when the door dropped it
    // last (#960).
    drop(points);
    let events = stella_core::EventSender::new(tx.clone());
    drop(tx);
    let drained = persistence::close_event_stream(&registry, events, renderer).await;

    assert_eq!(
        found_a_stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_slice(),
        [true, true],
        "both points read the registry's slot at dispatch time and found a stream — \
         the first only because the loop publishes before its first round"
    );
    assert!(
        caught
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "no child was metered against a round's own sender — the decorator put the \
         run's stream back after the round: {:?}",
        caught
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    );
    assert_eq!(
        step_usage_count(&drained.events),
        2,
        "both children's spend reached the goal run's drain: {:?}",
        drained.events
    );
    assert!(
        registry.events().is_none(),
        "and the stream comes down with the run — a registry still holding a sender \
         keeps the renderer's channel open and hangs a completed goal (#960)"
    );
}
