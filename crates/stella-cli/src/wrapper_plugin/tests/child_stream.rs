// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The between-rounds event stream (#3802) — what a plugin's own model calls
//! meter into while the round's own channel is closed.
//!
//! A sibling file rather than more lines in the parent suite, for the reason
//! AGENTS.md § "God files" gives: that file is the largest in this module and
//! this is a new property rather than a fix to one already covered there. It
//! reads the parent's fixtures through `use super::*` — the grading manifest,
//! the roster helpers and the `sh` plugin shape are the same ones every other
//! `stella run --pipeline` property is tested against.

use std::sync::{Arc, Mutex};

use stella_core::EventSender;
use stella_protocol::AgentEvent;

use super::*;

/// A dispatcher that meters its child exactly the way
/// [`crate::subagent::SessionSubAgents`] does — by reading the *current*
/// stream off the registry at dispatch time and falling back to a sink when
/// the slot is empty.
///
/// The two lines it shares with the shipping dispatcher are the two the
/// property is about: `ToolRegistry::events`, and
/// `unwrap_or_else(|| EventSender::from_fn(..))`. What it sends is one
/// `StepUsage`, which is the event the child's spend actually rides on.
struct MeteringDispatcher {
    registry: Arc<stella_tools::ToolRegistry>,
    /// Whether each dispatch found a real stream or the sink, in order.
    found_a_stream: Arc<Mutex<Vec<bool>>>,
}

impl MeteringDispatcher {
    /// One child's metering record, built through the wire shape rather than
    /// the variant literal: `StepUsage` carries two dozen fields, most of them
    /// `serde(default)`, and a literal here would need editing every time one
    /// is added without saying anything more than this does.
    fn step_usage(spec: &SubAgentSpec) -> AgentEvent {
        serde_json::from_value(serde_json::json!({
            "type": "step_usage",
            "step": 0,
            "provider": "test",
            "model": "m",
            "input_tokens": 10,
            "output_tokens": 5,
            "cached_input_tokens": 0,
            "cost_usd": 0.02,
            "duration_ms": 1,
            "retries": 0,
            "tool_calls": 0,
            "complete": true,
            "sub_agent_id": spec.agent_id,
        }))
        .expect("the metering record parses")
    }
}

#[async_trait]
impl SubAgentDispatcher for MeteringDispatcher {
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
        let attached = self.registry.events();
        self.found_a_stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(attached.is_some());
        let events = attached.unwrap_or_else(|| EventSender::from_fn(|_| Ok(())));
        let _ = events.send(Self::step_usage(&spec));
        SubAgentOutcome::Completed(SubAgentReport {
            summary: "read it".to_string(),
            truncated: false,
            cost_usd: 0.02,
            steps: 1,
            absorbed_messages: 0,
        })
    }
}

/// A wrapper that asks for a model call at **both** points, which is what
/// makes the round in between load-bearing: `before_turn` runs before the
/// round's own stream exists and `after_turn` runs after that stream has been
/// closed, so the two points are on opposite sides of the slot being cleared.
const ASKING_MANIFEST: &str = r#"
name = "asking-wrapper"
[loop]
participation = "steering"
points = ["before_turn", "after_turn"]
calls = ["child_turn"]
[subloop]
stages = ["research"]
[roles.reviewer]
tier = "research"
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
[wrapper]
id = "asking-v1"
[[wrapper.stages]]
name = "execute"
"#;

/// The plugin: one child turn per point, and an answer shaped for whichever
/// point it was asked at. One process per point, so the request line it reads
/// is the only thing that tells it which one it is serving.
const ASKS_FOR_A_CHILD: &str = r#"
read -r request
printf '%s\n' '{"call":"child_turn","id":7,"args":{"role":"reviewer","instruction":"read the diff"}}'
read -r answer
case "$request" in
  *'"point":"after_turn"'*)
    printf '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"not-attempted"}}}\n' ;;
  *)
    printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"reviewer","text":"read"}]}}\n' ;;
esac
"#;

/// The engine, stubbed: it clears the registry's event slot the way a real
/// round does, and nothing else. [`TurnDriver`] is the seam the socket is
/// designed around, so everything between the plugin's stdout and what the
/// dispatcher reads is the shipping path.
struct RoundThatClosesItsStream {
    registry: Arc<stella_tools::ToolRegistry>,
}

#[async_trait(?Send)]
impl TurnDriver for RoundThatClosesItsStream {
    async fn run_turn(
        &mut self,
        _prelude: stella_runtime::wrapper::TurnPrelude,
    ) -> stella_runtime::wrapper::DrivenTurn {
        // What `crate::agent::run_turn` does at its close
        // (`persistence::close_event_stream`) and the whole reason the
        // republishing decorator exists: without it, only the first point of
        // the first round would ever find a stream.
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

/// What the witness assembles: a real `sh` plugin on disk, a metering
/// dispatcher over a live registry, and the wrapper bound to both.
fn asking_plugin(
    dir: &std::path::Path,
    registry: &Arc<stella_tools::ToolRegistry>,
) -> (BoundWrapper, Arc<Mutex<Vec<bool>>>) {
    std::fs::write(dir.join("main.sh"), ASKS_FOR_A_CHILD).expect("the plugin script is written");
    let manifest = PluginManifest::from_toml_str(ASKING_MANIFEST).expect("fixture must load");
    let found_a_stream: Arc<Mutex<Vec<bool>>> = Arc::default();
    let plane = Arc::new(child_turn_plane(
        &manifest,
        Arc::new(MeteringDispatcher {
            registry: Arc::clone(registry),
            found_a_stream: Arc::clone(&found_a_stream),
        }) as Arc<dyn SubAgentDispatcher>,
    ));
    let roster = roster(vec![installed(ASKING_MANIFEST, &dir.to_string_lossy())]);
    let wrapper = bind_installed(&roster, "asking-v1", &mut |_| {})
        .expect("the installed plugin declares this variant")
        .serving(|_| WrapperHost::recalling(no_recall()).with_child_turns(Arc::clone(&plane)))
        .expect("it binds");
    (wrapper, found_a_stream)
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
/// Both points, deliberately. `before_turn` is answered by the publication
/// [`PluginChildStream::open`] makes; `after_turn` is answered only by
/// [`RepublishingDriver`], because the round in between clears the slot on its
/// way out. A version of this fix that opened the stream and never re-published
/// it passes on the first element of that vector and fails on the second.
///
/// No store, deliberately: what this proves is that the events reach a stream
/// with a drain behind it. Which execution row that drain writes them to is
/// [`super::super::child_stream`]'s decision, and a store here would test
/// `Store::record_telemetry` rather than the wiring.
///
/// `Json` rather than `Text` for the same reason: it is the format under which
/// the drain *collects* what it saw
/// (`persistence::spawn_renderer`), so the assertion reads the events
/// themselves rather than scraping a rendered frame.
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

    let drained = stream.close(&registry, 0.02).await;
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
        drained
            .events
            .iter()
            .filter(|event| matches!(event, AgentEvent::StepUsage { .. }))
            .count(),
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
