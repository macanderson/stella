// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The one fixture every door's child-turn witness is built from (#3802,
//! #4730).
//!
//! Three doors drive a wrapper — `stella run --pipeline <variant>`,
//! `stella goal --pipeline <variant>` and `stella fleet` — and each owes the
//! same property: a plugin's `child_turn`, dispatched from a point that runs
//! *between* the rounds, meters into a real event stream rather than into the
//! sink [`crate::subagent::SessionSubAgents::dispatch`] falls back to when the
//! registry's slot is empty.
//!
//! A shared module rather than three copies, for the reason the property is
//! shared: a fixture that drifted per door would let one door's witness pass
//! against a plugin the other two never ask anything of, which is exactly the
//! failure a per-door witness exists to catch. What each door supplies is its
//! own [`super::PointStream`] and its own stub round; everything from the
//! plugin's stdout to what the dispatcher reads is this file.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use stella_core::EventSender;
use stella_core::subagent::{SubAgentDispatcher, SubAgentOutcome, SubAgentReport, SubAgentSpec};
use stella_plugin::PluginManifest;
use stella_protocol::AgentEvent;

use super::{BoundWrapper, WrapperHost, bind_installed, child_turn_plane};
use crate::plugin_cmd::roster::{InstalledPlugin, PluginRoster, PluginScope};

/// A dispatcher that meters its child exactly the way
/// [`crate::subagent::SessionSubAgents`] does — by reading the *current*
/// stream off the registry at dispatch time and falling back to a sink when
/// the slot is empty.
///
/// The two lines it shares with the shipping dispatcher are the two every
/// witness here is about: `ToolRegistry::events`, and
/// `unwrap_or_else(|| EventSender::from_fn(..))`. What it sends is one
/// `StepUsage`, which is the event a child's spend actually rides on.
pub(crate) struct MeteringDispatcher {
    registry: Arc<stella_tools::ToolRegistry>,
    /// Whether each dispatch found a real stream or the sink, in order.
    found_a_stream: Arc<Mutex<Vec<bool>>>,
}

impl MeteringDispatcher {
    /// One child's metering record, built through the wire shape rather than
    /// a struct literal: `StepUsage` carries two dozen fields, most of them
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

/// A wrapper that asks for a model call at **both** points, which is what the
/// round in between is here to test: `before_turn` runs before the round's own
/// stream exists and `after_turn` runs after that stream has been replaced or
/// closed, so the two points sit on opposite sides of the slot changing hands.
pub(crate) const ASKING_MANIFEST: &str = r#"
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
pub(crate) const ASKS_FOR_A_CHILD: &str = r#"
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

/// What every witness assembles: a real `sh` plugin on disk, a metering
/// dispatcher over a live registry, and the wrapper bound to both.
///
/// The returned vector is the record of what each dispatch found — one `bool`
/// per point, in the order the points ran.
pub(crate) fn asking_plugin(
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
    let roster = PluginRoster::compose(
        vec![InstalledPlugin {
            manifest: PluginManifest::from_toml_str(ASKING_MANIFEST).expect("fixture must load"),
            dir: PathBuf::from(dir),
            scope: PluginScope::User,
            consent: crate::plugin_cmd::receipt::ConsentState::Receipted,
        }],
        Vec::new(),
        &BTreeMap::new(),
    );
    let wrapper = bind_installed(&roster, "asking-v1", &mut |_| {})
        .expect("the installed plugin declares this wrapper id")
        .serving(|_| {
            WrapperHost::recalling(Box::new(crate::wrapper_recall::SessionRecallHost::none()))
                .with_child_turns(Arc::clone(&plane))
        })
        .expect("it binds");
    (wrapper, found_a_stream)
}

/// What each door's witness reads at the end: how many children's spend
/// actually reached the drain.
pub(crate) fn step_usage_count(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, AgentEvent::StepUsage { .. }))
        .count()
}
