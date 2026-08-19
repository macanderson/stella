// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The host half of a candidate worktree that crosses a process — #3380,
//! `doc:pipeline-as-plugins` §4 A10, `doc:wrapper-socket` §6.
//!
//! [`super::CandidateWorkspace`] is nineteen methods returning borrowed trait
//! objects, and an out-of-process plugin cannot be handed a `&dyn` anything.
//! [`stella_protocol::CandidateHandle`] is what crosses instead; this module
//! is the registry that mints one, resolves it back, and answers exactly the
//! six operations [`stella_protocol::CandidateOp`] declares.
//!
//! # One implementation, not two
//!
//! [`CandidateHandles`] owns no isolation logic whatever. It borrows the same
//! [`super::CandidateWorkspacePort`] the pipeline already drives — in
//! production `stella-cli`'s real-git `GitCandidateWorkspaces` — and every one
//! of the six operations is a delegation to the workspace that port created.
//! A second snapshot/seal/adopt implementation living behind the wire path is
//! precisely the drift shape #1613 already cost this repository once, so the
//! handle path is a *lookup table over the existing port*, and nothing else.
//!
//! # The grant is what crosses, and the fence is what makes that safe
//!
//! [`CandidateHandles::grant`] mints the [`stella_plugin::CandidateGrant`] an out-of-process
//! plugin actually receives in its request: the handle, the workspace's
//! **canonical** root, and the test invocation the host would run. Before it,
//! `AfterTurnRequest` carried a bare handle whose only implementation was the
//! in-process async Rust below — so a plugin holding one could do nothing with
//! it, and Track C's three reference plugins took their test command out of
//! `[runtime] env` instead (#3498).
//!
//! Handing out a root is not handing out trust, and the two halves are
//! deliberately different directions:
//!
//! - **Outbound**, the root is canonical *before* it is minted, so the path the
//!   plugin is told and the path the host fences against are the same one. A
//!   root that will not canonicalise mints no grant at all
//!   ([`stella_protocol::CandidateDenial::RootUnavailable`]) rather than a
//!   grant nothing can be judged against.
//! - **Inbound**, every path that comes *back* — a withheld adoption path, a
//!   scope a plugin proposes, an argument inside a test command — is resolved
//!   by [`CandidateHandles::resolve_path`] against that same root and refused
//!   if it lands anywhere else. Two layers, deliberately:
//!
//! 1. **Lexical** (`fence_lexical`) — absolute paths, `..` components, NUL
//!    bytes and backslashes are refused without touching the filesystem, as
//!    written and never normalised first. Normalising before checking is how
//!    a fence is talked past.
//! 2. **On disk** (`fence_on_disk`) — the surviving relative path is joined
//!    to the *canonical* root and the deepest existing ancestor is
//!    canonicalised, so a symlink anywhere along the way is followed before
//!    containment is judged. It fails closed: a root that will not
//!    canonicalise, or a broken link, refuses rather than admits.
//!
//! Test-command arguments get the fence they already had: `run_test` parses
//! through [`crate::witness::parse_test_invocation`], whose `validate_local_args`
//! refuses absolute and `..` arguments and the per-runner flags that redirect a
//! runner out of its own directory. That parser is not re-implemented here.
//!
//! **What this is not.** The resolved path is true at the instant it is
//! checked; a sufficiently determined local attacker can swap a component
//! afterwards (TOCTOU). Closing that needs `openat`-style resolution held
//! across the use, which is a substrate change, not a caller change — #3483,
//! declared rather than pretended away.
//!
//! # Who calls this, and who deliberately does not
//!
//! `stella-cli`'s wrapper driver mints the grant its plugins receive — through
//! [`host_tree_grant`], not through the table, because `stella run --pipeline
//! <variant>` runs its turn in the **shared work tree** and a grant naming a
//! worktree the turn never touched would be a lie a plugin cannot detect
//! (#3553). The table itself still has no production caller: its consumer is a
//! fan-out that exposes the workspaces it already creates, and
//! [`CandidateHandles::register`] is the seam for that — #3485.
//!
//! # Tamper snapshotting stays host-side
//!
//! Nothing here reads or writes witness-artifact identity. That remains
//! `crate::witness`'s job, exactly as `stella_plugin::TamperPolicy::ArtifactIdentity`
//! already assumes ("the host snapshots artifact identity at authoring time").

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use stella_plugin::{CandidateGrant, TestPlan};
// Not used by this file's own code — `test_plan` (now in `stella-plugin`
// too) builds a `TestBaseline` internally — but `ports/handle/tests.rs`'s
// `use super::*;` still needs it in scope to assert against.
#[cfg(test)]
use stella_plugin::TestBaseline;
use stella_protocol::{CandidateDenial, CandidateHandle, CandidateOp};

use super::{
    AdoptedChange, CandidateWorkspace, CandidateWorkspacePort, CmdOutcome, WorkspaceError,
};
use crate::witness::parse_test_invocation;

/// The prefix every minted handle carries. Present so a handle is
/// recognisable in a log line, and deliberately not a namespace: identity of
/// the *principal* holding a handle is #3380 A1's job, not this string's
/// (#3484).
const HANDLE_PREFIX: &str = "candidate-";

/// Why one handle-addressed operation did not happen.
///
/// Serializable in full, because the answer has to reach the plugin that
/// asked — in whatever language it is written in. The two arms are the two
/// genuinely different questions a caller has: [`Self::Denied`] means the host
/// refused the request (a bad handle, a path outside the root, a test command
/// the parser will not accept), while [`Self::Workspace`] means the request
/// was accepted and the isolation substrate itself failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum CandidateOpError {
    /// The host refused the request. Nothing was run and nothing was changed.
    #[error("candidate operation `{op}` was refused: {denial}")]
    Denied {
        /// Which of the six was asked for.
        op: CandidateOp,
        /// Why the host said no.
        denial: CandidateDenial,
    },
    /// The isolation substrate failed while serving an accepted request.
    #[error("candidate operation `{op}` failed: {source}")]
    Workspace {
        /// Which of the six was asked for.
        op: CandidateOp,
        /// The substrate's own typed failure, unflattened.
        source: WorkspaceError,
    },
}

impl CandidateOpError {
    /// Which of the six operations this failure belongs to.
    #[must_use]
    pub fn op(&self) -> CandidateOp {
        match self {
            CandidateOpError::Denied { op, .. } | CandidateOpError::Workspace { op, .. } => *op,
        }
    }

    fn denied(op: CandidateOp, denial: CandidateDenial) -> Self {
        CandidateOpError::Denied { op, denial }
    }
}

/// The host's table of live candidate workspaces, addressed by
/// [`CandidateHandle`].
///
/// Borrows the port for the run's lifetime exactly as the pipeline's other
/// port consumers do, and holds each created workspace in an `Arc` so an
/// operation can await without holding the table's lock.
pub struct CandidateHandles<'p> {
    port: &'p dyn CandidateWorkspacePort,
    live: Mutex<BTreeMap<CandidateHandle, Arc<dyn CandidateWorkspace>>>,
    minted: AtomicU64,
}

impl<'p> CandidateHandles<'p> {
    /// A fresh, empty table over `port`.
    #[must_use]
    pub fn new(port: &'p dyn CandidateWorkspacePort) -> Self {
        Self {
            port,
            live: Mutex::new(BTreeMap::new()),
            minted: AtomicU64::new(0),
        }
    }

    /// [`CandidateOp::Create`] — snapshot the tree into a fresh isolated
    /// workspace and return the handle that names it.
    pub async fn create(&self) -> Result<CandidateHandle, CandidateOpError> {
        let workspace = self
            .port
            .create()
            .await
            .map_err(|source| CandidateOpError::Workspace {
                op: CandidateOp::Create,
                source,
            })?;
        Ok(self.register(workspace))
    }

    /// Take a workspace this host already created and give it a handle.
    ///
    /// **Host-side plumbing, not a seventh operation**: a plugin cannot ask
    /// for this, because it has no workspace to offer. It exists so a caller
    /// that already fanned out through [`super::CandidateWorkspacePort`] can
    /// expose those same workspaces over the handle surface instead of
    /// creating a second set beside them.
    pub fn register(&self, workspace: Box<dyn CandidateWorkspace>) -> CandidateHandle {
        let ordinal = self.minted.fetch_add(1, Ordering::Relaxed) + 1;
        let handle = CandidateHandle::new(format!("{HANDLE_PREFIX}{ordinal}"));
        self.table().insert(
            handle.clone(),
            Arc::<dyn CandidateWorkspace>::from(workspace),
        );
        handle
    }

    /// [`CandidateOp::Root`] — the workspace's absolute root path.
    ///
    /// Owned rather than borrowed: a borrow out of the table is exactly the
    /// shape that cannot cross a process, which is what this module exists to
    /// avoid.
    pub fn root(&self, handle: &CandidateHandle) -> Result<String, CandidateOpError> {
        let workspace = self.live(handle, CandidateOp::Root)?;
        Ok(workspace.root().to_string())
    }

    /// Mint the [`CandidateGrant`] a plugin receives in its request — the
    /// serializable form of [`CandidateOp::Root`], not a seventh operation
    /// (the closed set of six is the design, `stella_protocol::candidate`).
    ///
    /// The root it carries is **canonical**, so what the plugin is told and
    /// what [`Self::resolve_path`] fences against are the same path even when
    /// the workspace was created through a symlink. It fails closed on both
    /// ways that can go wrong: a root the filesystem will not resolve, and a
    /// root this host cannot spell as UTF-8, each refuse to mint rather than
    /// hand out a path nothing can be judged against.
    ///
    /// `test` is the invocation the host would run there — build it with
    /// [`test_plan`] so the argv a plugin receives is the one the pipeline's
    /// own parser already accepted. `None` says the host has no test to give,
    /// which a plugin reports as unobservable rather than guessing at.
    ///
    /// # Errors
    ///
    /// [`CandidateOpError::Denied`] with [`CandidateDenial::UnknownHandle`]
    /// for a handle this table never minted or has already retired, and with
    /// [`CandidateDenial::RootUnavailable`] for a root that cannot be
    /// resolved.
    pub fn grant(
        &self,
        handle: &CandidateHandle,
        test: Option<TestPlan>,
    ) -> Result<CandidateGrant, CandidateOpError> {
        let workspace = self.live(handle, CandidateOp::Root)?;
        let deny = |denial| CandidateOpError::denied(CandidateOp::Root, denial);
        let canonical = canonical_root(workspace.root())
            .map_err(deny)?
            .into_os_string()
            .into_string()
            .map_err(|_| {
                deny(CandidateDenial::RootUnavailable {
                    reason: "the candidate root is not valid UTF-8".to_string(),
                })
            })?;
        Ok(CandidateGrant {
            handle: handle.clone(),
            root: canonical,
            test,
        })
    }

    /// [`CandidateOp::RunTest`] — run one test command inside the workspace.
    ///
    /// `command` is untrusted text and crosses [`crate::witness::parse_test_invocation`]
    /// before a process is spawned, so the handle path inherits the same
    /// closed runner vocabulary and per-argument confinement every other test
    /// run in the pipeline has. A command the parser refuses is
    /// [`CandidateDenial::TestCommandRefused`] and spawns nothing.
    pub async fn run_test(
        &self,
        handle: &CandidateHandle,
        command: &str,
    ) -> Result<CmdOutcome, CandidateOpError> {
        let workspace = self.live(handle, CandidateOp::RunTest)?;
        let invocation = parse_test_invocation(command).map_err(|reason| {
            CandidateOpError::denied(
                CandidateOp::RunTest,
                CandidateDenial::TestCommandRefused {
                    reason: reason.to_string(),
                },
            )
        })?;
        Ok(workspace.tests().run_test(&invocation).await)
    }

    /// [`CandidateOp::Seal`] — commit the workspace's current bytes into its
    /// private history, so a verification observation and an adoption see the
    /// same tree.
    pub async fn seal(&self, handle: &CandidateHandle) -> Result<(), CandidateOpError> {
        let workspace = self.live(handle, CandidateOp::Seal)?;
        workspace
            .seal()
            .await
            .map_err(|source| CandidateOpError::Workspace {
                op: CandidateOp::Seal,
                source,
            })
    }

    /// [`CandidateOp::Adopt`] — apply the workspace's changes to the real
    /// tree, all-or-nothing.
    ///
    /// Every `withhold` path is fenced against this handle's root *before*
    /// the substrate is called, so a path that escapes refuses the whole
    /// adoption rather than being silently dropped from the withheld set —
    /// which would deliver the very file the caller asked to keep back.
    pub async fn adopt(
        &self,
        handle: &CandidateHandle,
        withhold: &[String],
    ) -> Result<Vec<AdoptedChange>, CandidateOpError> {
        let workspace = self.live(handle, CandidateOp::Adopt)?;
        for path in withhold {
            fence(workspace.root(), path)
                .map_err(|denial| CandidateOpError::denied(CandidateOp::Adopt, denial))?;
        }
        workspace
            .adopt(withhold)
            .await
            .map_err(|source| CandidateOpError::Workspace {
                op: CandidateOp::Adopt,
                source,
            })
    }

    /// [`CandidateOp::Remove`] — discard the workspace and retire its handle.
    ///
    /// The handle is dropped from the table before the removal is awaited, so
    /// a concurrent operation cannot reach a workspace that is being torn
    /// down; every later use of that handle is
    /// [`CandidateDenial::UnknownHandle`].
    pub async fn remove(&self, handle: &CandidateHandle) -> Result<(), CandidateOpError> {
        let workspace = self.table().remove(handle).ok_or_else(|| {
            CandidateOpError::denied(
                CandidateOp::Remove,
                CandidateDenial::UnknownHandle {
                    handle: handle.clone(),
                },
            )
        })?;
        workspace.remove().await;
        Ok(())
    }

    /// Resolve one caller-supplied relative path inside a handle's workspace,
    /// or refuse it.
    ///
    /// **This is the funnel.** Any path a plugin names must reach the
    /// filesystem through here — the module docs describe the two layers and
    /// what the answer does not promise.
    pub fn resolve_path(
        &self,
        handle: &CandidateHandle,
        relative: &str,
    ) -> Result<PathBuf, CandidateDenial> {
        let workspace = self.lookup(handle)?;
        fence(workspace.root(), relative)
    }

    /// The live workspace for `handle`, or the denial to report for `op`.
    fn live(
        &self,
        handle: &CandidateHandle,
        op: CandidateOp,
    ) -> Result<Arc<dyn CandidateWorkspace>, CandidateOpError> {
        self.lookup(handle)
            .map_err(|denial| CandidateOpError::denied(op, denial))
    }

    fn lookup(
        &self,
        handle: &CandidateHandle,
    ) -> Result<Arc<dyn CandidateWorkspace>, CandidateDenial> {
        self.table()
            .get(handle)
            .map(Arc::clone)
            .ok_or_else(|| CandidateDenial::UnknownHandle {
                handle: handle.clone(),
            })
    }

    /// The table, recovering from a poisoned lock rather than panicking.
    ///
    /// Invariant #5: a panic in some other operation's thread must not turn
    /// every later handle lookup into a second panic. The map's own
    /// invariants cannot be broken by an unwind — insertion and removal are
    /// atomic with respect to it — so the contents behind a poisoned guard
    /// are exactly as sound as before.
    fn table(
        &self,
    ) -> std::sync::MutexGuard<'_, BTreeMap<CandidateHandle, Arc<dyn CandidateWorkspace>>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// [`HOST_TREE_HANDLE`], [`host_tree_grant`], [`resolve_in_root`],
/// [`test_plan`], and the path fence itself ([`fence`]/`fence_lexical`/
/// `fence_on_disk`/`canonical_root`) moved to `stella_plugin::candidate_grant`
/// (removal census for `stella-pipeline`,
/// `docs/spec/pipeline-as-plugins.md` §7 slice 1, §1.5.6) — the surviving
/// wrapper-socket driver mints the shared-work-tree grant through exactly
/// these functions, for every currently-installed witness-flavoured plugin,
/// and [`CandidateHandles::grant`]/[`CandidateHandles::resolve_path`] above
/// now call the same [`fence`]/[`canonical_root`] rather than keeping a
/// second copy — "one minting implementation and one fence rather than two"
/// stays true across the crate boundary, not just within this file.
/// Re-exported so every `crate::ports::HOST_TREE_HANDLE` (etc.) path in this
/// crate still resolves unchanged.
pub use stella_plugin::{
    HOST_TREE_HANDLE, canonical_root, fence, host_tree_grant, resolve_in_root, test_plan,
};

#[cfg(test)]
mod tests;
