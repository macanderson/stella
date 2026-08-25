// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella run --pipeline <variant>`'s between-rounds event stream (#3802) —
//! what a plugin's own model calls meter into while the round's own channel is
//! closed.
//!
//! A sibling file rather than more lines in the parent suite, for the reason
//! AGENTS.md § "God files" gives: that file is the largest in this module and
//! this is a new property rather than a fix to one already covered there.
//!
//! The plugin, the metering dispatcher and the manifest live in
//! [`crate::wrapper_plugin::child_turn_fixture`], which the goal and fleet
//! doors' witnesses build from too (#4730).

use std::sync::Arc;

use super::*;
use crate::wrapper_plugin::child_turn_fixture::{asking_plugin, step_usage_count};

/// The engine, stubbed: it clears the registry's event slot the way this
/// door's real round does, and nothing else. [`TurnDriver`] is the seam the
/// socket is designed around, so everything between the plugin's stdout and
/// what the dispatcher reads is the shipping path.
///
/// Clearing rather than replacing, because that is what `stella run`'s round
/// does: it goes through `crate::agent::run_turn`, which closes its own
/// execution row's stream at the end of the turn
/// (`persistence::close_event_stream`). The goal door's round replaces the
/// slot instead — see that door's own witness.
struct RoundThatClosesItsStream {
    registry: Arc<stella_tools::ToolRegistry>,
}

#[async_trait(?Send)]
impl TurnDriver for RoundThatClosesItsStream {
    async fn run_turn(
        &mut self,
        _prelude: stella_runtime::wrapper::TurnPrelude,
    ) -> stella_runtime::wrapper::DrivenTurn {
        // What `crate::agent::run_turn` does at its close and the whole reason
        // the republishing decorator exists: without it, only the first point
        // of the first round would ever find a stream.
        self.registry.detach_event_stream();
        stella_runtime::wrapper::DrivenTurn {
            outcome: stella_plugin::TurnOutcome {
                completed: true,
                answer: "done".to_string(),
                tools: Some(Vec::new()),
                changed_files: Some(Vec::new()),
            },
            tamper: stella_plugin::TamperFinding::NotChecked,
        }
    }
}

/// **Witness (#3802).** A plugin's `child_turn`, dispatched from a point that
/// runs between the rounds, meters into a real event stream and its
/// `StepUsage` is drained by a renderer — rather than into the sink
/// `SessionSubAgents::dispatch` falls back to when the registry's slot is
/// empty.
///
/// Fails before this change with `found_a_stream == [false, false]` and an
/// empty drain: the slot is attached per turn and released at the end of it,
/// and a wrapper's points run *between* the turns they are about, so at the
/// moment either child was dispatched there was nothing there. The money was
/// bounded by the spend ledger and the user was told on stderr, and nothing
/// durable was written — `stella stats`, `stella usage report` and the
/// Observatory each under-reported a real spend.
///
/// Both points. `before_turn` is answered by the publication
/// `PluginChildStream::open` makes; `after_turn` is answered only by
/// `RepublishingDriver`, because the round in between clears the slot on its
/// way out. A version of this fix that opened the stream and never re-published
/// it passes on the first element of that vector and fails on the second.
///
/// No store: what this proves is that the events reach a stream with a drain
/// behind it. Which execution row that drain writes them to is
/// `super::super::child_stream`'s decision, and a store here would test
/// `Store::record_telemetry` rather than the wiring.
///
/// `Json` rather than `Text` for the same reason: it is the format under which
/// the drain *collects* what it saw (`persistence::spawn_renderer`), so the
/// assertion reads the events themselves rather than scraping a rendered frame.
#[cfg(unix)]
#[tokio::test]
async fn a_plugins_child_turn_meters_into_a_stream_rather_than_a_sink() {
    let dir = tempfile::tempdir().expect("a temp dir for the installed plugin");
    let cfg =
        crate::config::Config::for_tests(crate::config::PROVIDERS[0].clone(), "m".to_string());
    let registry = Arc::new(stella_tools::ToolRegistry::new(PathBuf::from(".")));
    let (wrapper, found_a_stream) = asking_plugin(dir.path(), &registry);

    let stream = super::super::child_stream::PluginChildStream::open(
        &registry,
        &cfg,
        &None,
        crate::OutputFormat::Json,
        "put the retry back",
        "session-1",
        "grading-v1",
    );
    let mut round = RoundThatClosesItsStream {
        registry: Arc::clone(&registry),
    };
    let report = {
        let mut rounds = super::super::child_stream::RepublishingDriver::new(
            &mut round, &registry, &cfg, &stream,
        );
        super::super::dispatch_under_turn_controls(
            &wrapper.dispatch,
            RoundInput {
                goal: "put the retry back".to_string(),
                signals: pre_turn_signals(false, false),
                candidate: None,
            },
            &registry,
            stella_core::ports::TurnControls::none(),
            &mut rounds,
        )
        .await
        .expect("the declared stage program resolves")
    };
    assert!(report.faults.is_empty(), "{:?}", report.faults);

    let drained = stream.close(&registry, "completed", 0.04).await;
    assert_eq!(
        found_a_stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_slice(),
        [true, true],
        "both points read the registry's slot at dispatch time and found a stream — \
         the second only because the round between them re-published it"
    );
    assert_eq!(
        step_usage_count(&drained.events),
        2,
        "both children's spend reached the drain: {:?}",
        drained.events
    );
    assert!(
        registry.events().is_none(),
        "and the stream comes down with the dispatch — a registry still holding a \
         sender keeps the renderer's channel open and hangs a completed run (#960)"
    );
}
