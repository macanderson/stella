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
//! in the order `self_driving_cmd::ready` folds — `backlog_claim`, the three
//! `work` verbs, the `deliver` verbs, and the two `sweep` verbs that need no
//! lens.
//!
//! Each other verb answers [`HostCallRefusal::Unsupported`] and names its
//! family. That is a stated gap, not a silence. The match below covers every
//! verb, so a new one is a build error here.
//!
//! # A sweep draws only from a supply the workspace opened
//!
//! `sweep_regress` and `sweep_meta` run the code the cycle driver runs, in
//! `self_driving_cmd::supply`. Each supply is shut until an operator opens it
//! in `stella.toml`. Granting the verb does not open one. An ask against a
//! shut supply is refused by the switch's own name.
//!
//! `sweep_audit` runs a lens's own tooling. The host cannot yet do that for a
//! driver (`#6182`), so it keeps refusing and names that issue. The gap is one
//! somebody chose.
//!
//! # A merge is decided on what this host read
//!
//! `deliver_next` decides over a reading the driver sends. That is arithmetic.
//! A loop that already read the forge should not pay to read it twice.
//!
//! `deliver_merge` reads the forge itself. It runs the same machine over its
//! own answer. So a driver cannot merge by describing a build nobody saw.
//! [`super::deliver`] carries the case.
//!
//! # An ask carries its own verb's arguments and nobody else's
//!
//! [`DriverArgs`] has one member per verb that reads one, and an ask naming
//! `work_start` while carrying a `backlog_claim` table is two claims about
//! what the driver wants. [`table_for`] refuses that rather than reading one
//! and dropping the other, and [`no_args`] refuses a table sent to a verb that
//! reads none — the rule `DriverMessage`'s own envelope applies to `point`
//! against `call`, one layer in.

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::{AuthzDecision, AuthzGate, Principal};
use stella_plugin::{
    BacklogEntry, BacklogPage, ClaimReport, DriverArgs, DriverCall, DriverOk, HostCallFailure,
    HostCallRefusal, PullRequestArgs, SweepReceipts, SweepReport, SweepSkip, SweepSkipReason,
    SweptSupply, UnitArgs,
};
use stella_protocol::issue::{Issue, IssueProvider};
use stella_runtime::wrapper::DriverCapabilities;
use stella_tools::registry::Tool;

use super::deliver::DeliverDesk;
use super::work::WorkSlot;
use crate::plugin_authz::PluginGates;
use crate::self_driving_cmd::claim::Claim;
use crate::self_driving_cmd::config::LoopConfig;
use crate::self_driving_cmd::state::LoopState;
use crate::self_driving_cmd::supply::{self, Drawn};

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
    /// `None` when the workspace has no plugin installed at all. That is what
    /// [`PluginGates::from_roster`] says by giving nothing back, and this path
    /// then narrows nothing rather than turning down every ask.
    ///
    /// A plugin that installed and declared no `[[capabilities]]` is a
    /// different answer: it gets a rule granting nothing, so a shell it asks
    /// this host to run for it is refused by name (ADR 0034).
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
    /// The one pull request this session may open for that unit.
    deliver: DeliverDesk,
    /// The lease this session holds while it works a unit.
    ///
    /// Held for the length of the session rather than the call, because that
    /// is what a lease is: dropping it releases the key, so a claim that lived
    /// only as long as `perform` would be free again before the turn it exists
    /// to protect had started.
    lease: Mutex<Option<crate::self_driving_cmd::claim::Lease>>,
    /// The loop's own state directory: the receipts a regress sweep re-checks
    /// and the ledger a meta sweep folds.
    ///
    /// `None` when nothing bound one, which is what [`Self::new`] leaves
    /// behind. Resolving it reads the stella home and seeds files, and a
    /// session that never sweeps should pay for neither. A sweep asked for
    /// without one is refused, never answered over an empty ledger.
    state: Option<LoopState>,
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
        deliver: DeliverDesk,
    ) -> Self {
        Self {
            principal: Principal::Plugin(plugin.to_string()),
            gates,
            issues,
            config,
            root,
            work,
            deliver,
            lease: Mutex::new(None),
            state: None,
        }
    }

    /// Bind the loop's state directory. The two served `sweep` verbs read the
    /// receipts and the ledger in it.
    ///
    /// Apart from [`Self::new`] because it is the one argument a caller can
    /// fail to get: `LoopState::open` reads the stella home and seeds files.
    /// A host that cannot get one still serves every other verb.
    #[must_use]
    pub(crate) fn sweeping(mut self, state: Option<LoopState>) -> Self {
        self.state = state;
        self
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

    /// The pull request for the session's unit — `deliver_open`.
    ///
    /// It reads the session's own slot, not an argument. A driver delivers
    /// what it worked. A key here could only name a branch this session does
    /// not hold.
    ///
    /// The title comes back through the tracker port. So the pull request a
    /// human reads in the list carries the issue's real title, under this
    /// workspace's own prefix.
    async fn deliver_open(&self) -> Result<stella_plugin::OpenReport, HostCallFailure> {
        let Some((issue, branch)) = self.work.deliverable() else {
            return Err(HostCallFailure::new(
                HostCallRefusal::Unavailable,
                "this session holds no worked unit with a change on a branch, so there is nothing                  to open a pull request for — ask for `work_start` first",
            ));
        };
        self.may_shell()?;
        let resolved = self
            .issues
            .get(&stella_protocol::issue::IssueKey::from(issue.as_str()))
            .await
            .map_err(|error| {
                HostCallFailure::new(
                    HostCallRefusal::Failed,
                    format!("this host could not read issue {issue}: {error}"),
                )
            })?;
        let title = self
            .config
            .attribution
            .title(&format!("{} (#{issue})", resolved.title));
        self.deliver.open(&branch, &issue, &title).await
    }

    /// One draw from one supply — `sweep_regress` and `sweep_meta`.
    ///
    /// It shells out twice. Git says whether a cited fix is still on the base,
    /// and the tracker takes what the sweep files. So it is held to the grant
    /// [`Self::backlog_next`] is held to.
    ///
    /// The switch under `[self_driving.supply]` is read first. Granting the
    /// verb opens no supply: an operator's `stella.toml` is the only thing
    /// that does, and a driver asking for a shut one is told which line to
    /// edit. `supply::draw_regress` itself asks no switch, because
    /// `stella self-driving sweep` is a person running one draw by hand and a
    /// person needs no permission from a file they own.
    async fn sweep(&self, supply: SweptSupply) -> Result<DriverOk, HostCallFailure> {
        let open = match supply {
            SweptSupply::Regress => self.config.supply.regress,
            SweptSupply::Meta => self.config.supply.meta,
        };
        if !open {
            return Err(shut(supply));
        }
        self.may_shell()?;

        let Some(state) = &self.state else {
            return Err(HostCallFailure::new(
                HostCallRefusal::Unavailable,
                "this host could not resolve the loop's own state directory, so it has no \
                 receipts to re-check and no ledger to fold",
            ));
        };
        let drawn = match supply {
            SweptSupply::Regress => supply::draw_regress(state, &self.root),
            SweptSupply::Meta => supply::draw_meta(state),
        };
        let offered = drawn.findings.len() as u64;
        let filed = supply::offer(
            state,
            self.issues.as_ref(),
            &self.config,
            &self.root,
            &drawn.findings,
        )
        .await;

        Ok(DriverOk {
            sweep: Some(report(supply, offered, drawn, filed)),
            ..DriverOk::default()
        })
    }
}

/// The refusal for a sweep of a supply this workspace has not opened.
///
/// It names the line an operator edits. A driver told only "no" would read a
/// shut switch as a host that cannot sweep. Those two call for opposite
/// answers.
fn shut(supply: SweptSupply) -> HostCallFailure {
    HostCallFailure::new(
        HostCallRefusal::Unavailable,
        format!(
            "this workspace has not opened the `{supply}` supply, so nothing was swept — set \
             `[self_driving.supply] {supply} = \"on\"` in stella.toml to open it"
        ),
    )
}

/// One draw, in the channel's own words.
///
/// The map from `stella_autonomy`'s skip reasons to the wire's is total. So
/// the two sets of words cannot drift apart.
fn report(supply: SweptSupply, offered: u64, drawn: Drawn, filed: supply::Offered) -> SweepReport {
    SweepReport {
        supply,
        offered,
        fresh: filed.fresh as u64,
        filed: filed.filed,
        receipts: drawn.receipts.map(|counts| SweepReceipts {
            total: counts.total as u64,
            checked: counts.checked,
            skipped: tally(&counts.skipped),
        }),
    }
}

/// Every skip reason the draw met, once each, with how many receipts carried
/// it.
fn tally(skipped: &[stella_autonomy::regress::Skip]) -> Vec<SweepSkip> {
    use stella_autonomy::regress::Skip;

    let mut counted: Vec<SweepSkip> = Vec::new();
    for skip in skipped {
        let reason = match skip {
            Skip::NoChangeCited => SweepSkipReason::NoChangeCited,
            Skip::UnknownAtClose => SweepSkipReason::UnknownAtClose,
            Skip::AbsentAtClose => SweepSkipReason::AbsentAtClose,
        };
        match counted.iter_mut().find(|held| held.reason == reason) {
            Some(held) => held.count += 1,
            None => counted.push(SweepSkip { reason, count: 1 }),
        }
    }
    counted
}

/// The pull request an ask named, or a refusal that says it named none.
///
/// A blank number is refused rather than guessed at, on [`named_unit`]'s
/// terms. The only guess on offer is the one this session opened. Putting that
/// in would answer about a pull request the driver never asked about.
fn named_pr(named: &PullRequestArgs) -> Result<&str, HostCallFailure> {
    let pr = named.pr.trim();
    if pr.is_empty() {
        return Err(HostCallFailure::new(
            HostCallRefusal::Failed,
            "this ask names no pull request, and this host does not choose one for a driver —              send the `pr` a `deliver_open` answer gave you",
        ));
    }
    Ok(pr)
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
            DriverCall::DeliverOpen => {
                no_args(call, args.as_ref())?;
                Ok(DriverOk {
                    pull_request: Some(self.deliver_open().await?),
                    ..DriverOk::default()
                })
            }
            DriverCall::DeliverObserve => {
                let table = table_for(call, args.as_ref())?;
                let named = table
                    .deliver_observe
                    .as_ref()
                    .ok_or_else(|| no_table(call))?;
                self.may_shell()?;
                Ok(DriverOk {
                    observation: Some(self.deliver.observe(named_pr(named)?).await?),
                    ..DriverOk::default()
                })
            }
            // No shell, and so no grant beyond `[driver] calls`: this reads no
            // forge and spends no model call. It is arithmetic over facts the
            // driver already holds, and demanding `bash` for it would ask a
            // human to grant a power it never uses.
            DriverCall::DeliverNext => {
                let table = table_for(call, args.as_ref())?;
                let asked = table.deliver_next.as_ref().ok_or_else(|| no_table(call))?;
                Ok(DriverOk {
                    decision: Some(super::deliver::decide_from(asked)),
                    ..DriverOk::default()
                })
            }
            DriverCall::DeliverReady => {
                let table = table_for(call, args.as_ref())?;
                let named = table.deliver_ready.as_ref().ok_or_else(|| no_table(call))?;
                self.may_shell()?;
                Ok(DriverOk {
                    ready: Some(self.deliver.ready(named_pr(named)?).await?),
                    ..DriverOk::default()
                })
            }
            DriverCall::DeliverMerge => {
                let table = table_for(call, args.as_ref())?;
                let named = table.deliver_merge.as_ref().ok_or_else(|| no_table(call))?;
                self.may_shell()?;
                Ok(DriverOk {
                    merge: Some(self.deliver.merge(named_pr(named)?).await?),
                    ..DriverOk::default()
                })
            }
            // No arguments, and none accepted. A sweep re-checks every
            // receipt the loop holds and folds the whole ledger. Neither is
            // about one unit, so a key here could only name something the
            // draw does not read.
            DriverCall::SweepRegress => {
                no_args(call, args.as_ref())?;
                self.sweep(SweptSupply::Regress).await
            }
            DriverCall::SweepMeta => {
                no_args(call, args.as_ref())?;
                self.sweep(SweptSupply::Meta).await
            }
            // Its own arm, because its gap has a name. Running a lens's own
            // tooling on a driver's behalf is `#6182`, and a refusal saying
            // so is what tells a driver author which work they are waiting on.
            DriverCall::SweepAudit => Err(HostCallFailure::new(
                HostCallRefusal::Unsupported,
                format!(
                    "this host does not perform \"{call}\" yet: running an open lens's own \
                     tooling for a driver is `#6182`. The other two {} verbs are served",
                    call.family()
                ),
            )),
            DriverCall::BacklogFile
            | DriverCall::BacklogClose
            | DriverCall::BacklogLink
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
