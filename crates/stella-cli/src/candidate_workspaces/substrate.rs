// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which of the two isolation substrates this session's fan-outs run on
//! (#1383).
//!
//! An enum rather than `Box<dyn CandidateWorkspaces>`, because a host asks its
//! own substrate two questions the port deliberately does not declare — what
//! earlier runs left on disk (#2813), and where unscored work went when a run
//! ended before anything scored it (#2651). Behind a trait object those
//! questions would either have to join the port, where a plugin would see
//! them, or be answered by a downcast. Two named variants answer both without
//! either.
//!
//! The choice is the operator's and is never inferred; see
//! [`CandidateIsolation`] for why.
//!
//! # Why the setting is read here and not carried on [`Config`]
//!
//! It is read once, at the moment the only code that can act on it is built,
//! and it is acted on immediately: after [`Self::for_session`] returns there is
//! a substrate, and no later value could change what it is. A field on
//! [`Config`] would therefore have to declare itself
//! `StartupOnly` to `/reload`'s completeness ledger — "re-deriving this
//! mid-session would be wrong" — which is the same fact this read states
//! directly, with one reader instead of a field every surface carries.
//!
//! Reading the scope chain a second time is what
//! [`Settings::load`](crate::settings::Settings::load) is already built for:
//! its trust refusals and format notices are latched process-wide, so the
//! several loads one launch performs speak once between them. The alternative —
//! a narrow re-reader like `Settings::load_tool_scopes` — would skip the
//! project trust boundary and `stella.toml`, which is the wrong trade for a
//! value that decides whether a promotion overwrites a tree.
//!
//! [`Config`]: crate::config::Config

use std::sync::Arc;

use async_trait::async_trait;
use stella_runtime::wrapper::{
    CandidateFanoutError, CandidateReport, CandidateWork, CandidateWorkspace, CandidateWorkspaces,
};

use super::SessionCandidateWorkspaces;
use super::copy_tree::CopyTreeCandidateWorkspaces;
use crate::config::Config;
use crate::settings::CandidateIsolation;
use crate::subagent::SessionSubAgents;

/// One session's candidate substrate, in whichever shape the workspace asked
/// for.
#[derive(Debug)]
pub(crate) enum CandidateSubstrate {
    /// One git worktree per candidate; promotion is a patch. The default.
    Worktree(SessionCandidateWorkspaces),
    /// One whole-tree copy per candidate; promotion is a replacement.
    CopyTree(CopyTreeCandidateWorkspaces),
}

impl CandidateSubstrate {
    /// Build the substrate this workspace's `candidate_isolation` names.
    ///
    /// A settings scope this host could not read gets the **worktree**
    /// substrate, which is the direction that cannot destroy anything: the
    /// copy substrate overwrites the tree it promotes onto, and nothing may
    /// select it except an operator who wrote it down.
    pub(crate) fn for_session(
        cfg: &Config,
        plugin: &str,
        sub_agents: Arc<SessionSubAgents>,
    ) -> Self {
        let isolation = crate::settings::Settings::load(&cfg.workspace_root)
            .map(|settings| settings.candidate_isolation())
            .unwrap_or_default();
        Self::with_isolation(isolation, cfg, plugin, sub_agents)
    }

    /// The half above that has already resolved the policy — the seam a test
    /// names a substrate through without writing a settings file.
    pub(crate) fn with_isolation(
        isolation: CandidateIsolation,
        cfg: &Config,
        plugin: &str,
        sub_agents: Arc<SessionSubAgents>,
    ) -> Self {
        match isolation {
            CandidateIsolation::Worktree => {
                Self::Worktree(SessionCandidateWorkspaces::new(cfg, plugin, sub_agents))
            }
            CandidateIsolation::CopyTree => {
                Self::CopyTree(CopyTreeCandidateWorkspaces::new(cfg, plugin, sub_agents))
            }
        }
    }

    /// What earlier runs in this workspace left behind, one line each.
    ///
    /// Both shapes keep their candidates in the same directory and write the
    /// same records, so either variant reports the other's residue — which is
    /// what a workspace that switched substrates between runs needs.
    pub(crate) fn orphaned_candidates(&self) -> Vec<String> {
        match self {
            Self::Worktree(inner) => inner.orphaned_candidates(),
            Self::CopyTree(inner) => inner.orphaned_candidates(),
        }
    }

    /// Keep every candidate the run ended before anything scored, and say
    /// where.
    ///
    /// The two shapes keep different artifacts, and each keeps the one that is
    /// recoverable: a patch for the worktree substrate, whose candidate is a
    /// diff against a commit, and the tree itself for the copy substrate,
    /// whose candidate already is one.
    pub(crate) async fn preserve_unscored(&self) -> Vec<String> {
        match self {
            Self::Worktree(inner) => inner.preserve_unscored().await,
            Self::CopyTree(inner) => inner.preserve_unscored(),
        }
    }
}

#[async_trait]
impl CandidateWorkspaces for CandidateSubstrate {
    async fn create(&self, label: &str) -> Result<CandidateWorkspace, CandidateFanoutError> {
        match self {
            Self::Worktree(inner) => inner.create(label).await,
            Self::CopyTree(inner) => inner.create(label).await,
        }
    }

    async fn work(
        &self,
        workspace: &CandidateWorkspace,
        work: CandidateWork,
    ) -> Result<CandidateReport, CandidateFanoutError> {
        match self {
            Self::Worktree(inner) => inner.work(workspace, work).await,
            Self::CopyTree(inner) => inner.work(workspace, work).await,
        }
    }

    async fn adopt(&self, workspace: &CandidateWorkspace) -> Result<(), CandidateFanoutError> {
        match self {
            Self::Worktree(inner) => inner.adopt(workspace).await,
            Self::CopyTree(inner) => inner.adopt(workspace).await,
        }
    }

    async fn remove(&self, workspace: &CandidateWorkspace) -> Result<(), CandidateFanoutError> {
        match self {
            Self::Worktree(inner) => inner.remove(workspace).await,
            Self::CopyTree(inner) => inner.remove(workspace).await,
        }
    }
}
