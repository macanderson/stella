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
//! # What is served, and what says so
//!
//! The host serves `backlog_next` — the queue, read through [`IssueProvider`]
//! in the order `self_driving_cmd::ready` folds — `backlog_claim`, and the
//! three `work` verbs.
//!
//! Each other verb answers [`HostCallRefusal::Unsupported`] and names its
//! family. That is a stated gap, not a silence. The match below covers every
//! verb, so a new one is a build error here.
//!
//! # An ask carries its own verb's arguments and nobody else's
//!
//! [`DriverArgs`] has one member per verb that reads one, and an ask naming
//! `work_start` while carrying a `backlog_claim` table is two claims about
//! what the driver wants. [`mismatched_args`] refuses that rather than reading
//! one and dropping the other — the rule `DriverMessage`'s own envelope
//! applies to `point` against `call`, one layer in.

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::{AuthzDecision, AuthzGate, Principal};
use stella_plugin::{
    BacklogEntry, BacklogPage, ClaimReport, DriverArgs, DriverCall, DriverOk, HostCallFailure,
    HostCallRefusal, UnitArgs,
};
use stella_protocol::issue::{Issue, IssueProvider};
use stella_runtime::wrapper::DriverCapabilities;
use stella_tools::registry::Tool;

use super::work::WorkSlot;
use crate::plugin_authz::PluginGates;
use crate::self_driving_cmd::claim::Claim;
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
    /// The workspace this session drives — where a worktree is cut and where
    /// the lease ledger lives.
    root: PathBuf,
    /// The one unit of work this session may hold.
    work: WorkSlot,
    /// The lease this session holds while it works a unit.
    ///
    /// Held for the length of the session rather than the call, because that
    /// is what a lease is: dropping it releases the key, so a claim that lived
    /// only as long as `perform` would be free again before the turn it exists
    /// to protect had started.
    lease: Mutex<Option<crate::self_driving_cmd::claim::Lease>>,
}

impl HostDriverCapabilities {
    /// What `plugin` may be served in this workspace.
    pub(crate) fn new(
        plugin: &str,
        gates: Option<PluginGates>,
        issues: Box<dyn IssueProvider>,
        config: LoopConfig,
        root: PathBuf,
        work: WorkSlot,
    ) -> Self {
        Self {
            principal: Principal::Plugin(plugin.to_string()),
            gates,
            issues,
            config,
            root,
            work,
            lease: Mutex::new(None),
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
            ..DriverOk::default()
        })
    }

    /// The cooperative lease — `backlog_claim`.
    ///
    /// Over the `dispatch_claims` table in `.stella/private/fleet.db`, which is
    /// the same table `stella self-driving drive` claims through, so a driver
    /// session and a hand-started loop against one clone can see each other.
    ///
    /// A ledger that cannot answer is **not** a peer holding the key. It fails
    /// open, like every other contention probe: `held` is true with no holder
    /// named, and the reason is in [`Claim::Unavailable`]'s own words — a loop
    /// meant to run for days cannot treat a local I/O error as somebody else's
    /// work.
    ///
    /// It does not ask [`Self::may_shell`], because nothing here shells out.
    /// The lease is a row this process writes into the workspace's own ledger,
    /// so `[driver] calls` is the whole gate on it. Demanding `bash` for a
    /// local write would be asking a human to grant a power this never uses.
    fn backlog_claim(&self, unit: &UnitArgs) -> Result<DriverOk, HostCallFailure> {
        let key = named_unit(unit)?;
        let taken = crate::self_driving_cmd::claim::acquire(&self.root, key);
        let report = match taken {
            Claim::Granted(lease) => {
                *self
                    .lease
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(lease);
                ClaimReport {
                    issue: key.to_string(),
                    held: true,
                    holder: String::new(),
                }
            }
            Claim::HeldBy(who) => ClaimReport {
                issue: key.to_string(),
                held: false,
                holder: who,
            },
            Claim::Unavailable => ClaimReport {
                issue: key.to_string(),
                held: true,
                holder: String::new(),
            },
        };
        Ok(DriverOk {
            claim: Some(report),
            ..DriverOk::default()
        })
    }

    /// One unit of backlog through the turn loop — `work_start`.
    ///
    /// The issue is resolved through the port before anything is cut, so a key
    /// no tracker knows is a refusal rather than an empty worktree. Reading it
    /// spends the shell the same way [`Self::backlog_next`] does, so it is held
    /// to the same grant.
    async fn work_start(&self, unit: &UnitArgs) -> Result<DriverOk, HostCallFailure> {
        let key = named_unit(unit)?;
        self.may_shell()?;
        let issue = self
            .issues
            .get(&stella_protocol::issue::IssueKey::from(key))
            .await
            .map_err(|error| {
                HostCallFailure::new(
                    HostCallRefusal::Failed,
                    format!("this host could not read issue {key}: {error}"),
                )
            })?;
        let report = self.work.start(&issue).await?;
        Ok(DriverOk {
            work: Some(report),
            ..DriverOk::default()
        })
    }
}

/// The key an ask named, or a refusal that says it named none.
///
/// A blank key is refused rather than guessed at. There is exactly one
/// plausible guess — the top of the queue — and it is the wrong one: what to
/// work is the driver's decision, and a host that picks for it has taken the
/// judgement the channel exists to leave with the plugin.
fn named_unit(unit: &UnitArgs) -> Result<&str, HostCallFailure> {
    let key = unit.issue.trim();
    if key.is_empty() {
        return Err(HostCallFailure::new(
            HostCallRefusal::Failed,
            "this ask names no issue, and this host does not choose one for a driver — send the \
             `key` a `backlog_next` answer gave you",
        ));
    }
    Ok(key)
}

/// Refuse an ask that carries arguments for a verb that reads none.
///
/// A dropped table is a driver believing it said something the host never
/// heard, which is the failure the message envelope refuses one layer out.
fn no_args(call: DriverCall, args: Option<&DriverArgs>) -> Result<(), HostCallFailure> {
    let named = args.map(DriverArgs::tables).unwrap_or_default();
    if named.is_empty() {
        return Ok(());
    }
    Err(HostCallFailure::new(
        HostCallRefusal::Failed,
        format!(
            "\"{call}\" reads no arguments, and this ask carries the arguments of {}",
            describe(&named)
        ),
    ))
}

/// The refusal for an ask whose table is not the one its verb reads.
///
/// Unreachable behind [`table_for`], which has already established that the
/// table names this verb and no other. Written as a value rather than an
/// `expect` because AGENTS.md #5 makes no exception for "I checked earlier".
fn no_table(call: DriverCall) -> HostCallFailure {
    HostCallFailure::new(
        HostCallRefusal::Failed,
        format!("\"{call}\" reads an argument table of its own name, and this ask carries none"),
    )
}

/// The ask's table, once it is established that it names `call` and nothing
/// else.
fn table_for(call: DriverCall, args: Option<&DriverArgs>) -> Result<&DriverArgs, HostCallFailure> {
    let args = args.ok_or_else(|| {
        HostCallFailure::new(
            HostCallRefusal::Failed,
            format!("\"{call}\" reads arguments, and this ask carries none"),
        )
    })?;
    let named = args.tables();
    if named != vec![call] {
        return Err(HostCallFailure::new(
            HostCallRefusal::Failed,
            format!(
                "this ask names \"{call}\" and carries the arguments of {}; an ask carries the \
                 table of the verb it names and nothing else",
                describe(&named)
            ),
        ));
    }
    Ok(args)
}

/// How a mismatched table is named in a refusal.
fn describe(named: &[DriverCall]) -> String {
    if named.is_empty() {
        return "no verb".to_string();
    }
    named
        .iter()
        .map(|call| format!("\"{call}\""))
        .collect::<Vec<_>>()
        .join(", ")
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
    async fn perform(
        &self,
        call: DriverCall,
        args: Option<DriverArgs>,
    ) -> Result<DriverOk, HostCallFailure> {
        match call {
            DriverCall::BacklogNext => self.backlog_next().await,
            DriverCall::BacklogClaim => {
                let table = table_for(call, args.as_ref())?;
                let unit = table.backlog_claim.as_ref().ok_or_else(|| no_table(call))?;
                self.backlog_claim(unit)
            }
            DriverCall::WorkStart => {
                let table = table_for(call, args.as_ref())?;
                let unit = table.work_start.as_ref().ok_or_else(|| no_table(call))?;
                self.work_start(unit).await
            }
            // No arguments, and none accepted: the session holds one unit, so
            // there is nothing for a key here to name that the session does not
            // already know. An ask that sent one would be describing a
            // different unit from the one this would report on.
            DriverCall::WorkStatus => {
                no_args(call, args.as_ref())?;
                Ok(DriverOk {
                    work: Some(self.work.status()),
                    ..DriverOk::default()
                })
            }
            DriverCall::WorkAbandon => {
                let table = table_for(call, args.as_ref())?;
                let reason = table
                    .work_abandon
                    .as_ref()
                    .map(|abandon| abandon.reason.trim())
                    .filter(|reason| !reason.is_empty())
                    .ok_or_else(|| {
                        HostCallFailure::new(
                            HostCallRefusal::Failed,
                            "abandoning a unit records why, and this ask gives no reason — a \
                             release with nothing said about it teaches the next cycle nothing",
                        )
                    })?;
                Ok(DriverOk {
                    work: Some(self.work.abandon(reason).await?),
                    ..DriverOk::default()
                })
            }
            DriverCall::BacklogFile
            | DriverCall::BacklogClose
            | DriverCall::BacklogLink
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
                    "this host does not perform \"{call}\" yet; the rest of the {} family is \
                     still unbuilt",
                    call.family()
                ),
            )),
        }
    }
}
