// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The event stream a wrapped run holds open **between** its rounds, so a
//! plugin's own model calls reach the store instead of a sink (#3802).
//!
//! # The gap this closes
//!
//! `SessionSubAgents::dispatch` meters a child against the *current* turn's
//! event channel, read off the registry at dispatch time
//! ([`ToolRegistry::events`]), and falls back to a sink when the slot is empty.
//! A wrapper's points run between the turns they are about — `before_turn`
//! before `crate::agent::run_turn` is called at all, `after_turn` after it
//! returned — so at the moment a plugin's `child_turn` is dispatched the slot is
//! empty and every `StepUsage` the child emits goes nowhere durable. The money
//! was bounded (the spend ledger) and the user was told (`spend_lines`), but
//! `stella stats`, `stella usage report` and the Observatory all under-reported
//! a real spend.
//!
//! # The two decisions #3802 asked for
//!
//! **Which execution row a between-turns child belongs to: its own.** Each
//! wrapped round opens an execution of its own, so a `before_turn` child
//! precedes the row its work belongs to and an `after_turn` child follows one
//! that is already finished — attributing either to the round it brackets
//! records a call as a step of a turn it was not part of, in one direction or
//! the other. One run-scoped `plugin` row for the whole dispatch is the shape
//! that is true: it says "this run's plugin-side model calls", it carries the
//! plugin's declared `[wrapper] id` and the session id like every other row
//! this door opens, and it joins to the rounds beside it through
//! `executions.session_id`.
//!
//! **Who owns the channel: this type, for the span of the dispatch.** It is
//! opened before the first point can run and closed after the last one has,
//! with [`RepublishingDriver`] re-publishing it after every round — because a
//! round's own turn replaces the slot on the way in and clears it on the way
//! out ([`crate::agent::persistence::close_event_stream`]), so without the
//! re-publish only the very first `before_turn` would have found a stream.
//!
//! `PLUGIN_CHILD_TURN_SLOT` is untouched: a child's receipts stay off the
//! parent's `(turn_instance, step, call_seq)` key exactly as before, and this
//! changes only which execution row they land in.
//!
//! # Closing it is not optional
//!
//! The registry holds an `EventSender` while the stream is published, and an
//! `EventSender` is a live sender on the renderer's channel — a registry that
//! keeps one past the end of the run leaves the renderer's `recv()` loop
//! pending and hangs a *completed* `stella run` (#960).
//! [`PluginChildStream::close`] is what
//! actually ends it, and it is the reason this type hands its parts out rather
//! than letting a caller assemble them.

use std::sync::Arc;

use stella_core::EventSender;
use stella_protocol::AgentEvent;
use stella_runtime::wrapper::{DrivenTurn, TurnDriver, TurnPrelude};
use stella_store::Store;
use stella_tools::ToolRegistry;
use tokio::sync::mpsc;

use crate::OutputFormat;
use crate::agent::persistence::RendererOutcome;
use crate::config::Config;

/// The execution kind recorded for a wrapped run's plugin-side model calls.
///
/// A kind of its own rather than `"run"`: these rows are not turns, and a
/// consumer counting a session's turns must be able to tell them apart without
/// reading the `pipeline_variant` column.
pub(crate) const PLUGIN_EXECUTION_KIND: &str = "plugin";

/// One wrapped run's between-rounds event stream, with the execution row its
/// events are journalled against.
pub(crate) struct PluginChildStream {
    /// The channel every point's children meter into.
    tx: EventSender,
    /// The row `tx`'s events are persisted under, when this run has a store.
    execution: Option<(Arc<Store>, i64)>,
    /// The drain task: renders and persists, and is awaited by [`Self::close`].
    renderer: tokio::task::JoinHandle<RendererOutcome>,
}

impl PluginChildStream {
    /// Open the stream and publish it on `registry`, ready for the first point.
    ///
    /// `prompt` is the run's goal, which is what the execution row records —
    /// the plugin's children are bought in service of it, and a row with no
    /// prompt reads in `stella stats` as a call nobody asked for.
    pub(crate) fn open(
        registry: &ToolRegistry,
        cfg: &Config,
        store: &Option<Arc<Store>>,
        format: OutputFormat,
        prompt: &str,
        session: &str,
        wrapper_id: &str,
    ) -> Self {
        let execution = crate::agent::persistence::begin_execution(
            store,
            PLUGIN_EXECUTION_KIND,
            prompt,
            cfg,
            Some(session),
            Some(wrapper_id),
        );
        let (raw_tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let tx = EventSender::new(raw_tx);
        let renderer = crate::agent::persistence::spawn_renderer(
            rx,
            format,
            execution.clone(),
            cfg.provider.id.to_string(),
            // This stream is assembled here rather than through
            // `output::raw_event_sender_for_run`, so nothing has pre-appended
            // its events to a durable sink.
            false,
            // No `you` row: the goal is already the anchor of every round's own
            // frame, and repeating it above the plugin's calls would read as a
            // second turn against the same prompt rather than as what it is.
            // The execution row still records it — that is what `stella stats`
            // reads.
            None,
        );
        let stream = Self {
            tx,
            execution,
            renderer,
        };
        stream.publish(registry, cfg);
        stream
    }

    /// Re-publish the stream on `registry` after a round has cleared the slot.
    ///
    /// Through [`crate::agent::persistence::attach_run_streams`] rather than
    /// `attach_events` beside it, for the reason
    /// `turn_files::open_turn_streams` exists: the registry's own events are
    /// the loud debt and the per-call work-tree measurement is the silent one,
    /// and a stream opened without the second renders every mutating row
    /// diffless rather than failing.
    pub(crate) fn publish(&self, registry: &ToolRegistry, cfg: &Config) {
        crate::agent::persistence::attach_run_streams(
            registry,
            cfg,
            &self.tx,
            self.execution.as_ref(),
        );
    }

    /// Close the stream, finish its execution row, and return what the renderer
    /// drained.
    ///
    /// Detaching, dropping this type's own sender and awaiting the drain are
    /// one operation for #960's reason: any of the three left undone keeps the
    /// channel alive and the renderer pending.
    ///
    /// `ended` is the *run's* ending, not the plugin's: these calls were bought
    /// in service of the rounds beside them, and a row that always read
    /// `completed` would describe an aborted run's plugin spend as a finished
    /// piece of work. `cost_usd` is every model call this host made on the
    /// plugin's behalf. Both are written best-effort — a run whose work is done
    /// is not made less done by a store that would not write.
    pub(crate) async fn close(
        self,
        registry: &ToolRegistry,
        ended: &str,
        cost_usd: f64,
    ) -> RendererOutcome {
        let Self {
            tx,
            execution,
            renderer,
        } = self;
        let outcome = crate::agent::persistence::close_event_stream(registry, tx, renderer).await;
        if let Some((store, id)) = execution {
            let _ = store.finish_execution(id, ended, cost_usd);
        }
        outcome
    }
}

/// A [`TurnDriver`] that re-publishes `stream` after every round it drives.
///
/// A decorator rather than a line inside `RawTurnDriver::run_turn` because the
/// property is about *any* driver the dispatch is given: `crate::agent::goal`'s
/// round driver and `crate::fleet_cmd`'s attempt driver each own a turn that
/// clears the slot the same way, and a decorator states the rule once where the
/// dispatch is assembled instead of once per door.
pub(crate) struct RepublishingDriver<'a> {
    inner: &'a mut dyn TurnDriver,
    registry: &'a ToolRegistry,
    cfg: &'a Config,
    stream: &'a PluginChildStream,
}

impl<'a> RepublishingDriver<'a> {
    /// Wrap `inner` so `stream` is back on `registry` after each round it
    /// drives.
    pub(crate) fn new(
        inner: &'a mut dyn TurnDriver,
        registry: &'a ToolRegistry,
        cfg: &'a Config,
        stream: &'a PluginChildStream,
    ) -> Self {
        Self {
            inner,
            registry,
            cfg,
            stream,
        }
    }
}

#[async_trait::async_trait(?Send)]
impl TurnDriver for RepublishingDriver<'_> {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        let driven = self.inner.run_turn(prelude).await;
        // After, never before: the round's own turn published its stream on the
        // way in and cleared the slot on the way out, so this is the first
        // instant at which re-publishing does not overwrite a live turn's
        // channel with a run-scoped one.
        self.stream.publish(self.registry, self.cfg);
        driven
    }
}
