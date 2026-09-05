// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What this host does when a driver asks.
//!
//! [`crate::driver_plugin`] binds the gate. The gate says which asks are let
//! through. This is what runs after it says yes.
//!
//! # As the plugin, not as you
//!
//! Each ask is run as [`Principal::Plugin`]. Before the host reads the
//! tracker, it asks the rule [`crate::plugin_authz`] built from the list you
//! accepted at install. A grant with `bash` on it gets the read. One without
//! `bash` is turned down, and the reason names the plugin.
//!
//! That is the point of the two names. What you read at install and what the
//! loop may do are one thing here.
//!
//! # One verb, and the rest say so
//!
//! The host serves `backlog_next`. It reads the queue through
//! [`IssueProvider`], in the order `self_driving_cmd::ready` folds.
//!
//! Each other verb answers [`HostCallRefusal::Unsupported`] and names its
//! family. That is a stated gap, not a silence. The match below covers every
//! verb, so a new one is a build error here.

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::{AuthzDecision, AuthzGate, Principal};
use stella_plugin::{
    BacklogEntry, BacklogPage, DriverCall, DriverOk, HostCallFailure, HostCallRefusal,
};
use stella_protocol::issue::{Issue, IssueProvider};
use stella_runtime::wrapper::DriverCapabilities;
use stella_tools::registry::Tool;

use crate::plugin_authz::PluginGates;
use crate::self_driving_cmd::config::LoopConfig;

/// How many ranked issues one `backlog_next` answer carries.
///
/// A page, not the whole queue. A driver takes one unit per cycle. It needs a
/// few behind that one, in case the top one is already claimed. A thousand
/// would push the whole open set through a plugin's stdin for no use.
const BACKLOG_PAGE: usize = 20;

/// What this host will do for one installed plugin.
pub(crate) struct HostDriverCapabilities {
    /// Who these calls are run as. Always [`Principal::Plugin`].
    principal: Principal,
    /// The rule from install time that this plugin is held to.
    ///
    /// `None` when no installed plugin asked for a tool at all. That is what
    /// [`PluginGates::from_roster`] says by giving nothing back, and this path
    /// then narrows nothing rather than turning down every ask.
    gates: Option<PluginGates>,
    /// The tracker, behind its port. A test hands over a fake, and no `gh`
    /// ever runs.
    issues: Box<dyn IssueProvider>,
    /// This workspace's own loop settings: the label names, and the rule that
    /// says which issue is ready.
    config: LoopConfig,
}

impl HostDriverCapabilities {
    /// What `plugin` may be served in this workspace.
    pub(crate) fn new(
        plugin: &str,
        gates: Option<PluginGates>,
        issues: Box<dyn IssueProvider>,
        config: LoopConfig,
    ) -> Self {
        Self {
            principal: Principal::Plugin(plugin.to_string()),
            gates,
            issues,
            config,
        }
    }

    /// Who every ask here is run as.
    ///
    /// Only a test reads it back: the shipping path uses the field itself, in
    /// [`Self::may_shell`]. `#[cfg(test)]` rather than an `allow` because the
    /// lint is right — nothing ships a call to this — and this way a later
    /// production caller is a build error instead of a suppression somebody
    /// has to re-justify (AGENTS.md, "Code style and conventions").
    #[cfg(test)]
    pub(crate) fn principal(&self) -> &Principal {
        &self.principal
    }

    /// Whether the grant this plugin was given covers running a shell.
    ///
    /// The tracker is read by running its own client. That is what the
    /// manifest's `bash` line asks for: "`gh` — read the defect queue". So the
    /// read is held to that line, at the grade this host gives `bash`. It goes
    /// through the same `AuthzGate` a tool call does, not a second rule.
    ///
    /// A `RequireApproval` is turned down, not parked. A driver session has
    /// nobody to ask. Reading "a human must say yes" as "yes" is the one
    /// answer this must never give.
    fn may_shell(&self) -> Result<(), HostCallFailure> {
        let Some(gates) = &self.gates else {
            return Ok(());
        };
        let contract =
            stella_tools::contracts::contract_for(&stella_tools::bash::Bash::new(None).schema());
        match gates.check(&contract, &self.principal, &Value::Null) {
            Ok(AuthzDecision::Allow) => Ok(()),
            Ok(AuthzDecision::Deny { reason }) => Err(HostCallFailure::new(
                HostCallRefusal::Forbidden,
                format!("{reason} — so this host did not read the tracker for it"),
            )),
            Ok(AuthzDecision::RequireApproval { reason }) => Err(HostCallFailure::new(
                HostCallRefusal::Forbidden,
                format!(
                    "{reason} — a driver session has no human to ask, so the answer is no rather \
                     than a prompt nobody would see"
                ),
            )),
            Err(error) => Err(HostCallFailure::new(
                HostCallRefusal::Failed,
                format!("the plugin's capability rule could not be evaluated: {error}"),
            )),
        }
    }

    /// The ranked queue — `backlog_next`.
    ///
    /// Awaited, not blocked on. This runs inside the runtime the driver
    /// session holds, where `ready_full`'s own `block_on` would panic.
    /// `ready_full_async` is the same read, the same page limit and the same
    /// fold, with no runtime of its own.
    async fn backlog_next(&self) -> Result<DriverOk, HostCallFailure> {
        self.may_shell()?;
        let ready =
            crate::self_driving_cmd::ready::ready_full_async(self.issues.as_ref(), &self.config)
                .await
                .map_err(|reason| HostCallFailure::new(HostCallRefusal::Failed, reason))?;
        Ok(DriverOk {
            backlog: Some(BacklogPage {
                issues: ready.iter().take(BACKLOG_PAGE).map(entry).collect(),
            }),
        })
    }
}

/// One tracker record, as the channel carries it. No body: [`BacklogEntry`]
/// says why.
fn entry(issue: &Issue) -> BacklogEntry {
    BacklogEntry {
        key: issue.key.as_str().to_string(),
        title: issue.title.clone(),
        labels: issue
            .labels
            .iter()
            .map(|label| label.name.clone())
            .collect(),
        url: issue.url.clone(),
    }
}

#[async_trait]
impl DriverCapabilities for HostDriverCapabilities {
    async fn perform(&self, call: DriverCall) -> Result<DriverOk, HostCallFailure> {
        match call {
            DriverCall::BacklogNext => self.backlog_next().await,
            DriverCall::BacklogClaim
            | DriverCall::BacklogFile
            | DriverCall::BacklogClose
            | DriverCall::BacklogLink
            | DriverCall::WorkStart
            | DriverCall::WorkStatus
            | DriverCall::WorkAbandon
            | DriverCall::DeliverOpen
            | DriverCall::DeliverObserve
            | DriverCall::DeliverNext
            | DriverCall::DeliverMerge
            | DriverCall::SweepAudit
            | DriverCall::SweepRegress
            | DriverCall::SweepMeta
            | DriverCall::CuratePropose
            | DriverCall::CurateList
            | DriverCall::CurateAccept => Err(HostCallFailure::new(
                HostCallRefusal::Unsupported,
                format!(
                    "this host does not perform \"{call}\" yet; the {} capabilities are still \
                     unbuilt",
                    call.family()
                ),
            )),
        }
    }
}
