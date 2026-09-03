// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The mounted skill-invocation plane. [`SkillInvocationPlane`] tracks the
//! skill invocations live in a session's tool stack, each carrying its
//! frontmatter `allowed-tools` grant as [`ToolPolicy`] algebra;
//! [`SkillScopedTools`], composed above the operator's `PolicyToolSet`,
//! advertises and executes only `operator ∧ grant` per concrete name, so a
//! grant can select within the operator surface but never re-enable a tool
//! the operator denied below.
//!
//! No new callable tool: `invoke_skill` stays in
//! [`crate::catalog::RETIRED_TOOL_NAMES`]; a skill is invoked by the human
//! (`stella skill run <slug>` or an in-session `/slug`), never by the model
//! calling a tool. [`SkillScopedTools::active_skill_slugs`] is the first
//! shipped `ToolExecutor::active_skill_slugs` to answer non-empty, and the
//! decorator pass-throughs forward it, so the engine's
//! overflow-summarization seam sees a live invocation's slug through any
//! shipped composition.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::{DispatchGate, LiveService, ToolExecutor};
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::policy::ToolPolicy;
use crate::skill_grant::grant_policy;

/// One live invocation: the slug (what `active_skill_slugs` reports) and the
/// grant its frontmatter declared, resolved to policy algebra at `begin` so
/// the per-call answer is a lookup, not a re-parse.
struct Span {
    id: u64,
    slug: String,
    /// `None` when the skill declared no `allowed-tools:` — the invocation
    /// inherits the session surface unchanged.
    grant: Option<ToolPolicy>,
}

#[derive(Default)]
struct Inner {
    /// A Vec, not a set: the same skill invoked twice is two spans, and the
    /// narrowing holds until the *last* one ends — mirroring
    /// `ActiveSkillInvocations`.
    spans: Mutex<Vec<Span>>,
    next_id: AtomicU64,
}

/// Which skill invocations are live in a session's tool stack, with their
/// grants. Cloneable handle over shared state, so the driver that begins a
/// span and the [`SkillScopedTools`] view that enforces it hold the same set
/// without naming each other.
#[derive(Clone, Default)]
pub struct SkillInvocationPlane {
    inner: Arc<Inner>,
}

impl SkillInvocationPlane {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark `slug` invoked until the returned guard drops, scoped to
    /// `allowed_tools` when the skill declared a grant. Spans end
    /// structurally — on the ordinary path, on an unwind, and when the
    /// per-turn stack holding the guard is torn down.
    #[must_use = "the invocation ends (and its narrowing lifts) the moment this guard drops"]
    pub fn begin(&self, slug: &str, allowed_tools: Option<&[String]>) -> SkillSpanGuard {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .spans
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Span {
                id,
                slug: slug.to_string(),
                grant: allowed_tools.map(grant_policy),
            });
        SkillSpanGuard {
            inner: self.inner.clone(),
            id,
        }
    }

    /// Every live slug, oldest span first (duplicates preserved) — the answer
    /// [`SkillScopedTools::active_skill_slugs`] gives the engine.
    #[must_use]
    pub fn active_slugs(&self) -> Vec<String> {
        self.inner
            .spans
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|span| span.slug.clone())
            .collect()
    }

    /// Whether every live grant allows `name`. A conjunction across spans —
    /// two overlapping invocations narrow to the intersection of their
    /// grants — and vacuously true with no grant-carrying span live, so the
    /// plane is inert exactly when nothing is invoked.
    #[must_use]
    pub fn allows(&self, name: &str) -> bool {
        self.inner
            .spans
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter_map(|span| span.grant.as_ref())
            .all(|grant| grant.allows(name))
    }

    /// The slugs whose grants are currently narrowing the surface, for the
    /// refusal message — a denial that names its cause is diagnosable, one
    /// that does not reads as a broken tool.
    fn narrowing_slugs(&self) -> Vec<String> {
        self.inner
            .spans
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|span| span.grant.is_some())
            .map(|span| span.slug.clone())
            .collect()
    }
}

/// A live invocation span. Dropping it ends the span and lifts its narrowing.
pub struct SkillSpanGuard {
    inner: Arc<Inner>,
    id: u64,
}

impl Drop for SkillSpanGuard {
    fn drop(&mut self) {
        self.inner
            .spans
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|span| span.id != self.id);
    }
}

/// The enforcement view over a session stack: advertises and executes only
/// what every live grant on `plane` allows, and reports the plane's live
/// slugs as [`ToolExecutor::active_skill_slugs`].
///
/// Composed **above** the operator's policy layer (the driver wraps the
/// assembled session stack), so the effective per-name answer is
/// `operator ∧ grant` structurally — the shape
/// [`crate::skill_grant`]'s module docs specify, with no third object holding
/// a copy of the intersection.
pub struct SkillScopedTools<'a> {
    inner: &'a dyn ToolExecutor,
    plane: SkillInvocationPlane,
}

impl<'a> SkillScopedTools<'a> {
    pub fn new(inner: &'a dyn ToolExecutor, plane: SkillInvocationPlane) -> Self {
        Self { inner, plane }
    }
}

#[async_trait]
impl ToolExecutor for SkillScopedTools<'_> {
    /// Filtered per call, not snapshotted at construction: spans begin and
    /// end mid-session, and the engine re-reads schemas each step, so the
    /// advertised surface tracks the live grants — a granted step sees the
    /// narrowed set, the step after the span ends sees the full one.
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner
            .schemas()
            .into_iter()
            .filter(|schema| self.plane.allows(&schema.name))
            .collect()
    }

    /// Forwarded with exactly the filter `schemas()` applies — the two
    /// advertisements must not disagree about which tools exist here.
    fn contracts(&self) -> Vec<stella_protocol::ToolContract> {
        self.inner
            .contracts()
            .into_iter()
            .filter(|contract| self.plane.allows(contract.name()))
            .collect()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        if !self.plane.allows(name) {
            return ToolOutput::classified_error(
                stella_protocol::ErrorClass::RefusedByPolicy,
                format!(
                    "`{name}` is not available here: an active skill invocation's \
                     allowed-tools grant scopes this context (live: {})",
                    self.plane.narrowing_slugs().join(", ")
                ),
            );
        }
        self.inner.execute(name, input).await
    }

    /// Forwarded: a view that restricts *which* tools may run is not a
    /// narrower policy plane.
    fn dispatch_gate(&self) -> Option<&dyn DispatchGate> {
        self.inner.dispatch_gate()
    }

    /// Forwarded unfiltered: a skill's allowed-tools grant says what may run
    /// next, and the loop detector is asking about a call that already ran.
    fn tool_origin(&self, name: &str) -> Option<stella_core::loop_detect::ToolOrigin> {
        self.inner.tool_origin(name)
    }

    /// Forwarded, not zeroed — a grandchild dispatched behind this view
    /// still settles into the carve that bounds it (see `ReadOnlyTools`).
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }

    /// Forwarded: a granted tool legitimately parks on external state, and
    /// its probe replays through this same view, so the narrowing holds
    /// while parked too.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        self.inner.drain_wait_request()
    }

    /// Forwarded filtered: a name this view refuses to execute must not be
    /// advertised as safe to run concurrently.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.inner
            .parallel_safe_names()
            .into_iter()
            .filter(|name| self.plane.allows(name))
            .collect()
    }

    /// The plane's live slugs, then whatever the inner stack reports — the
    /// production answer to the port whose every shipped implementation was
    /// empty until this plane existed (see the module docs).
    fn active_skill_slugs(&self) -> Vec<String> {
        let mut slugs = self.plane.active_slugs();
        slugs.extend(self.inner.active_skill_slugs());
        slugs
    }

    /// Forwarded unfiltered: a live service is state the workspace is in,
    /// not a capability this view hands out.
    fn live_services(&self) -> Vec<LiveService> {
        self.inner.live_services()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A leaf advertising one read and one write tool, recording what
    /// actually executed — so a denial is proven at the process, not by the
    /// text that came back.
    struct Leaf {
        reached: Mutex<Vec<String>>,
    }

    impl Leaf {
        fn new() -> Self {
            Self {
                reached: Mutex::new(Vec::new()),
            }
        }
        fn reached(&self) -> Vec<String> {
            self.reached.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ToolExecutor for Leaf {
        fn schemas(&self) -> Vec<ToolSchema> {
            ["read_file", "write_file", "task_list"]
                .into_iter()
                .map(|name| ToolSchema {
                    name: name.into(),
                    description: "d".into(),
                    input_schema: json!({}),
                    read_only: name != "write_file",
                    speculation_safe: false,
                })
                .collect()
        }
        async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
            self.reached.lock().unwrap().push(name.to_string());
            ToolOutput::Ok {
                content: format!("ran {name}"),
                data: None,
            }
        }
    }

    /// **The skill-invocation grant witness.** While a skill invocation with an
    /// `allowed-tools` grant is live, a disallowed tool call is DENIED at
    /// execution time (the leaf never sees it), an allowed call runs
    /// (anti-vacuity), the narrowed surface is what `schemas()` advertises,
    /// and the whole narrowing lifts structurally when the span guard drops.
    #[tokio::test]
    async fn a_live_skill_grant_denies_a_disallowed_tool_and_lifts_when_the_span_ends() {
        let leaf = Leaf::new();
        let plane = SkillInvocationPlane::new();
        let view = SkillScopedTools::new(&leaf, plane.clone());

        // Inert with nothing invoked: full surface, everything runs.
        assert_eq!(view.schemas().len(), 3);
        assert!(matches!(
            view.execute("write_file", &json!({})).await,
            ToolOutput::Ok { .. }
        ));

        let span = plane.begin("generate-quarter-seed", Some(&["task_list".to_string()]));
        assert_eq!(view.active_skill_slugs(), vec!["generate-quarter-seed"]);
        assert_eq!(
            view.schemas()
                .into_iter()
                .map(|s| s.name)
                .collect::<Vec<_>>(),
            vec!["task_list"],
            "advertisement narrows to the grant"
        );

        match view.execute("write_file", &json!({})).await {
            ToolOutput::Error { message, .. } => {
                assert!(
                    message.contains("write_file") && message.contains("generate-quarter-seed"),
                    "the denial names the tool and the invoking skill: {message}"
                );
            }
            other => panic!("a disallowed call must be denied, got {other:?}"),
        }
        assert!(
            matches!(
                view.execute("task_list", &json!({})).await,
                ToolOutput::Ok { .. }
            ),
            "the granted tool still runs through the same view"
        );
        assert_eq!(
            leaf.reached(),
            vec!["write_file".to_string(), "task_list".to_string()],
            "the denied call never reached the leaf"
        );

        drop(span);
        assert!(view.active_skill_slugs().is_empty());
        assert!(
            matches!(
                view.execute("write_file", &json!({})).await,
                ToolOutput::Ok { .. }
            ),
            "the narrowing lifts structurally with the guard"
        );
    }

    /// The grant can never widen: a name the inner stack does not advertise
    /// (or that a layer below denies) stays unavailable whatever the grant
    /// says — here, the granted name is simply not in the leaf's surface,
    /// and the view's advertisement is the intersection.
    #[tokio::test]
    async fn a_grant_naming_a_tool_the_stack_lacks_advertises_nothing_for_it() {
        let leaf = Leaf::new();
        let plane = SkillInvocationPlane::new();
        let view = SkillScopedTools::new(&leaf, plane.clone());

        let _span = plane.begin("phantom", Some(&["deploy_to_prod".to_string()]));
        assert!(
            view.schemas().is_empty(),
            "a grant selects within the surface; it mints nothing"
        );
    }

    /// Overlapping invocations narrow to the intersection of their grants,
    /// and a grant-less span (no `allowed-tools:`) narrows nothing while
    /// still reporting its slug.
    #[tokio::test]
    async fn overlapping_grants_intersect_and_a_grantless_span_only_reports() {
        let leaf = Leaf::new();
        let plane = SkillInvocationPlane::new();
        let view = SkillScopedTools::new(&leaf, plane.clone());

        let _grantless = plane.begin("context-only", None);
        assert_eq!(view.schemas().len(), 3, "no grant, no narrowing");
        assert_eq!(view.active_skill_slugs(), vec!["context-only"]);

        let _a = plane.begin(
            "a",
            Some(&["task_list".to_string(), "read_file".to_string()]),
        );
        let _b = plane.begin(
            "b",
            Some(&["task_list".to_string(), "write_file".to_string()]),
        );
        assert_eq!(
            view.schemas()
                .into_iter()
                .map(|s| s.name)
                .collect::<Vec<_>>(),
            vec!["task_list"],
            "two live grants intersect"
        );
        assert!(matches!(
            view.execute("read_file", &json!({})).await,
            ToolOutput::Error { .. }
        ));
    }
}
