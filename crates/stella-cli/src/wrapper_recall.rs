// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The CLI's half of the host-call channel: a plugin's `recall` reaches the
//! real context plane (#3561).
//!
//! # What was broken
//!
//! `doc:wrapper-socket` §6b's channel is served by
//! [`SubprocessWrapper::serving`](stella_runtime::wrapper::SubprocessWrapper::serving),
//! and **`stella-cli` never called it**. So a plugin declaring `[loop] calls =
//! ["recall"]` — `plugins/stella-research` does — had its stdin shut down after
//! the request, its `{"call":"recall",…}` had nowhere to go, and `converse`
//! returned `WrapperError::UnannouncedCall`. The dispatcher recorded the fault
//! and carried on, so the run was not lost; the recall stage simply contributed
//! nothing, every turn, on every host that installed the plugin.
//!
//! # Why the projection lives here
//!
//! [`RecallHost`] is deliberately **not** [`stella_protocol::ContextRecallPort`]
//! (née `stella_pipeline::ContextRecallPort`, retargeted to `stella-protocol`
//! by the removal census, `docs/spec/pipeline-as-plugins.md` §7 slice 1),
//! even though its signature mirrors it: it is `stella-runtime`'s own trait
//! for the wrapper socket's host-call channel, kept distinct from the door's
//! own recall port rather than merged with it, since a wrapper plugin's
//! `recall` call and a door's own turn-start context recall are two different
//! callers with two different lifecycles even where their payload shape
//! agrees. The frames therefore have to be projected by a driver that has
//! both, and that is this crate. The projection
//! is mechanical — a [`RecalledFrame`] carries strictly more than a
//! [`RecallFrame`] does, and the surplus (token cost, provenance method, the
//! content digest) is the host's accounting rather than anything a plugin acts
//! on.
//!
//! # A host with no context plane still attaches a gate
//!
//! [`SessionRecallHost::open`] on a workspace whose context plane will not open
//! keeps a `None` memory, and answers every recall with no frames. That is
//! the point: a plugin reads an empty frame list and degrades honestly, which
//! is what it is written to do. An **absent** gate is the one case it cannot be
//! told about — its call hangs until the point timeout — so the driver attaches
//! a gate unconditionally and lets the honest empty answer travel.
//!
//! # This handle is its own, and read-only
//!
//! The turn's [`SessionMemory`] is borrowed `&mut` by the driver for the whole
//! turn (the execution stamp, the skill-usage record), and the gate's plane has
//! to be `Send + Sync + 'static` because it lives inside the transport. So this
//! opens its own handle on the same workspace. It costs a second SQLite
//! connection on a plugin run that declared `recall` and nothing at all on one
//! that did not — the alternative, sharing one handle, would mean taking the
//! session's memory out from under the turn that is using it.

use async_trait::async_trait;
use std::path::Path;

use stella_pipeline::{ContextRecallPort, RecalledFrame};
use stella_plugin::RecallFrame;
use stella_runtime::wrapper::RecallHost;

use crate::memory::SessionMemory;

/// This session's context plane, as the host-call channel needs it.
pub(crate) struct SessionRecallHost {
    /// `None` when this workspace has no context plane that would open —
    /// answered as "no frames", never as a hang or an error.
    memory: Option<SessionMemory>,
}

impl SessionRecallHost {
    /// Open the workspace's context plane for a plugin's host calls.
    ///
    /// Silent on failure by design: the warning a user needs about an
    /// unopenable memory belongs to the *session's* own open, which has already
    /// happened by the time a plugin asks, and repeating it once per plugin
    /// would report one condition twice.
    pub(crate) fn open(workspace_root: &Path) -> Self {
        Self {
            memory: SessionMemory::open(workspace_root, false),
        }
    }

    /// A host with no context plane at all.
    #[cfg(test)]
    pub(crate) fn none() -> Self {
        Self { memory: None }
    }
}

#[async_trait]
impl RecallHost for SessionRecallHost {
    async fn recall(&self, goal: &str) -> Vec<RecallFrame> {
        let Some(memory) = &self.memory else {
            return Vec::new();
        };
        ContextRecallPort::recall(memory, goal)
            .await
            .frames
            .iter()
            .map(project)
            .collect()
    }
}

/// A boxed recall plane, so the driver can choose one at runtime.
///
/// [`HostPlanes::with_recall`](stella_runtime::wrapper::HostPlanes) takes its
/// plane by value, which is right for the crate that owns it — a host wiring
/// one concrete plane pays no dispatch — and wrong for a caller that picks
/// between this workspace's plane and a test's stub. `Box<dyn RecallHost>` does
/// not itself implement the trait, so this is the one line that says it does.
pub(crate) struct BoxedRecall(pub(crate) Box<dyn RecallHost>);

#[async_trait]
impl RecallHost for BoxedRecall {
    async fn recall(&self, goal: &str) -> Vec<RecallFrame> {
        self.0.recall(goal).await
    }
}

/// One recalled frame, as a plugin receives it.
///
/// The label is the citation the host already renders (L-C4 drops a frame
/// without one before it reaches here), and `source` is the record's original
/// provenance rather than the provider leg that answered — a plugin citing a
/// frame should cite where the content came from, not which adapter fetched it.
fn project(frame: &RecalledFrame) -> RecallFrame {
    RecallFrame {
        label: frame.citation_label.clone(),
        kind: frame.kind.clone(),
        source: frame.source.clone(),
        uri: frame.uri.clone(),
        content: frame.content.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recalled(label: &str, kind: &str) -> RecalledFrame {
        RecalledFrame {
            citation_label: label.to_string(),
            provider: "local".to_string(),
            source: "context.db".to_string(),
            kind: kind.to_string(),
            uri: Some("stella://memory/1".to_string()),
            method: Some("embedding".to_string()),
            content: "the parser rejects an empty header".to_string(),
            token_cost: 12,
            id: Some("nod_1".to_string()),
            content_digest: None,
        }
    }

    /// The projection carries the citation and the provenance a plugin can act
    /// on, and nothing that is only the host's accounting.
    #[test]
    fn a_recalled_frame_projects_onto_the_wire_view() {
        let wire = project(&recalled("parser lesson", "memory"));
        assert_eq!(wire.label, "parser lesson");
        assert_eq!(wire.kind, "memory");
        assert_eq!(
            wire.source, "context.db",
            "the record's own source, not the provider leg that answered"
        );
        assert_eq!(wire.uri.as_deref(), Some("stella://memory/1"));
        assert_eq!(wire.content, "the parser rejects an empty header");
    }

    /// A host with no context plane answers with no frames — the honest empty
    /// answer a plugin is written to degrade on. It must never be a hang, and
    /// it must never be an error the plugin has to special-case.
    #[tokio::test]
    async fn a_host_with_no_context_plane_answers_with_no_frames() {
        assert!(
            SessionRecallHost::none()
                .recall("anything")
                .await
                .is_empty()
        );
    }
}
