// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The execution context a tool runs in (#3284): the workspace root it has
//! always had, plus the event handle it never had.
//!
//! `tool.call.progress` was declared in `stella-core`'s bus names and emitted
//! nowhere, structurally: `Tool::execute(&self, input, root)` carried no way
//! to author an event, so only the registry itself could put anything on the
//! stream. [`ToolCtx`] is that channel — and #2694's journal discipline gets
//! its carrier: an infrastructure-mutating tool can now emit replayable
//! events the way file mutations already do.
//!
//! # The contract is the allowlist
//!
//! [`crate::contracts::contract_for`] resolves each tool's declared
//! `events` (#2716 §4), and [`ToolCtx::emit`] **drops** any name the
//! contract does not declare — a tool cannot improvise a vocabulary its
//! reviewers never saw. The drop is reported (`emit` returns whether the
//! event went out) but deliberately not an error: an event is telemetry, and
//! failing the call over it would let an observability bug break a working
//! tool.
//!
//! # Events never ride the output
//!
//! The loop detector keys on output bytes, so a timing or progress marker in
//! [`stella_protocol::tool::ToolOutput`] makes identical calls look distinct
//! and defeats it — the lesson that cost a bench run. Emission goes to the
//! hook bus, never into `content`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use stella_core::bus::HookBus;

/// What a tool may touch while it runs: the workspace root for path
/// resolution, and the session's event channel scoped to this call's
/// declared vocabulary.
///
/// Owned rather than borrowed because it crosses an `async_trait` boundary
/// on every dispatch; the bus is a cheap shared-inner clone and the
/// allowlist is a handful of names.
pub struct ToolCtx {
    root: PathBuf,
    tool: String,
    bus: Option<HookBus>,
    allowed: Vec<String>,
}

impl ToolCtx {
    /// The context [`crate::registry::ToolRegistry::execute`] assembles per
    /// call: this tool's name, its contract-declared event names, and the
    /// session bus (when one is attached).
    #[must_use]
    pub fn new(
        root: PathBuf,
        tool: impl Into<String>,
        bus: Option<HookBus>,
        allowed: Vec<String>,
    ) -> Self {
        Self {
            root,
            tool: tool.into(),
            bus,
            allowed,
        }
    }

    /// A context with no event channel — for tests and for callers that
    /// execute a tool outside a registry. Every `emit` is a no-op that
    /// reports the drop.
    #[must_use]
    pub fn bare(root: PathBuf) -> Self {
        Self {
            root,
            tool: String::new(),
            bus: None,
            allowed: Vec::new(),
        }
    }

    /// The workspace root — exactly the `root` parameter `Tool::execute`
    /// carried before #3284. Tools must never read or write outside it
    /// without explicit opt-in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Emit one named event onto the session bus, if this tool's contract
    /// declares the name.
    ///
    /// The payload must be a JSON object; a `"tool"` field naming the
    /// emitter is stamped in so an observer never has to guess attribution.
    /// Returns whether the event actually went out — `false` when the name
    /// is undeclared (dropped: the contract is the allowlist), when no bus
    /// is attached, or when the payload is not an object.
    pub fn emit(&self, event: &str, payload: Value) -> bool {
        let Some(bus) = &self.bus else {
            return false;
        };
        if !self.allowed.iter().any(|name| name == event) {
            return false;
        }
        let Value::Object(mut fields) = payload else {
            return false;
        };
        fields.insert("tool".into(), Value::String(self.tool.clone()));
        bus.emit_named(event, Value::Object(fields));
        true
    }

    /// Emit `tool.call.progress` — sugar for the one event most tools that
    /// emit anything will emit.
    pub fn progress(&self, payload: Value) -> bool {
        self.emit(stella_core::bus::names::TOOL_CALL_PROGRESS, payload)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    fn recording_bus() -> (HookBus, Seen) {
        let bus = HookBus::new("test-session");
        let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let sink = seen.clone();
        bus.on("tool.call.*", move |event| {
            sink.lock()
                .unwrap()
                .push((event.name.clone(), event.payload.clone()));
            Ok(())
        })
        .detach();
        (bus, seen)
    }

    /// The #3284 witness's other half: an undeclared event name is dropped
    /// rather than forwarded — the contract is the allowlist.
    #[test]
    fn an_undeclared_event_name_is_dropped() {
        let (bus, seen) = recording_bus();
        let ctx = ToolCtx::new(
            PathBuf::from("."),
            "task",
            Some(bus),
            vec!["tool.call.progress".into()],
        );
        assert!(!ctx.emit("tool.call.invented", json!({ "stage": "x" })));
        assert!(seen.lock().unwrap().is_empty(), "nothing may reach the bus");
    }

    #[test]
    fn a_declared_event_reaches_the_bus_stamped_with_its_tool() {
        let (bus, seen) = recording_bus();
        let ctx = ToolCtx::new(
            PathBuf::from("."),
            "task",
            Some(bus),
            vec!["tool.call.progress".into()],
        );
        assert!(ctx.progress(json!({ "stage": "dispatched" })));
        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "tool.call.progress");
        assert_eq!(events[0].1["tool"], "task");
        assert_eq!(events[0].1["stage"], "dispatched");
    }

    #[test]
    fn a_bare_context_drops_everything_quietly() {
        let ctx = ToolCtx::bare(PathBuf::from("."));
        assert!(!ctx.progress(json!({ "stage": "x" })));
    }

    /// A non-object payload cannot carry the `tool` attribution stamp, so it
    /// is dropped rather than emitted anonymous.
    #[test]
    fn a_non_object_payload_is_dropped() {
        let (bus, seen) = recording_bus();
        let ctx = ToolCtx::new(
            PathBuf::from("."),
            "task",
            Some(bus),
            vec!["tool.call.progress".into()],
        );
        assert!(!ctx.progress(json!("just a string")));
        assert!(seen.lock().unwrap().is_empty());
    }
}
