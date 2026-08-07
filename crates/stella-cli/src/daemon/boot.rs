// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella daemon resume-all` and `stella daemon install --resume-all`:
//! resume-at-boot (#1627).
//!
//! [`crate::daemon::service`] (#1587) registers one explicit invocation with launchd /
//! `systemd --user`, and a service manager re-**starts** what it registered —
//! a fresh turn and a fresh spend at every boot. That is the wrong verb for
//! the machine that rebooted mid-turn: the killed run left a resume point at
//! its last committed step, and [`crate::daemon::resume_supervised`] (#1586) continues
//! from it. This module is the wire between the two, so a rebooted machine
//! comes back **continuing** its work rather than repeating it.
//!
//! # Which of the two shapes #1627 offered, and why
//!
//! The issue offered a one-shot `install --resume <id>` or a standing sweep.
//! It is the **sweep**: nobody knows, before the crash, which run the crash
//! will take, so an id chosen at install time is an id chosen for the wrong
//! run. `stella daemon install --resume-all` registers exactly one thing —
//! `stella daemon resume-all` — and that verb decides at load, from the
//! registry, what this boot should continue.
//!
//! # Which runs it continues — the conservative rule
//!
//! Exactly the rows `stella daemon list` paints `Crashed ↩`: a **supervised**
//! run whose stored status is still *live* (`SessionStatus::is_live`) — or is
//! `Error`, a crash that lived just long enough to say so (#1696) — while its
//! liveness lock is not held, whose workspace still exists, and which left a
//! resume point this build can see.
//!
//! The load-bearing half is the stored status. Every deliberate end writes
//! one on the way out — `Complete` when it finished, `Cancelled` when `stella
//! daemon stop` or a Ctrl-C ended it, `Stopped` when the run ended itself by
//! policy, `Paused` when a deck set it aside. A process the kernel took
//! mid-turn writes nothing, so a live status with a dead lock is the
//! signature of interruption.
//!
//! # Why an `Error` is continued too, and why that is safe (#1696)
//!
//! This rule used to skip *every* terminal status, `Error` included. That was
//! a compromise forced by #1653: a deliberate policy stop (a stuck-loop
//! escalation, the step cap, an enforced budget, an ended scope review) was
//! recorded as `Error`, identical to a genuine crash, so continuing an
//! `Error` could have restarted — unattended, at the operator's expense —
//! work the operator ended on purpose. The cost was the honest one: a real
//! crash that *did* manage to write `Error` before dying was left stranded.
//!
//! #1653 removed the ambiguity. A policy stop now records
//! [`SessionStatus::Stopped`] with every other deliberate ending, which
//! leaves `Error` meaning only "it fell over" — a run exactly as entitled to
//! be continued as one the kernel took without warning. Two independent
//! facts make the widening safe rather than merely intended:
//!
//! - **A deliberate stop has no resume point.** The engine driver discards
//!   the checkpoint on every terminal path, abort included, so a policy stop
//!   retracts its resume point on the way out. A row written by a build that
//!   predates #1653 — where a policy stop really did store `Error` — is
//!   therefore filtered by `SkipReason::NoResumePoint` anyway (named, not
//!   linked: this module is private, so rustdoc cannot resolve it), without
//!   this module having to trust its status.
//! - **The attempt bound still applies.** An `Error` that resumes into
//!   another `Error` is counted like any other continuation and retired
//!   after `MAX_BOOT_ATTEMPTS`.
//!
//! # What stops a boot loop
//!
//! A run that wedges the machine on resume would otherwise resume on every
//! boot forever. Two brakes, and the second is the load-bearing one:
//!
//! - The resume point is consumed by the turn that continues (#1586), so the
//!   ordinary case self-terminates after one pass.
//! - That is not enough when the *resumed* turn is itself killed, because it
//!   writes a fresh resume point before it dies. So every continued run is
//!   counted in a durable ledger at `~/.stella/services/resume-boot.json`
//!   **before** it is spawned, and the `MAX_BOOT_ATTEMPTS`th failure retires
//!   it: the row is still listed, still says why, and is still resumable by
//!   hand with `stella daemon resume <id>` — a human deciding to try again is
//!   exactly what the bound exists to require.
//!
//! # What stops a stalled *sweep* — the per-run ceiling (#1921)
//!
//! The brakes above bound how many boots one run can spend; nothing in them
//! bounds how long one run can hold *this* boot. The sweep is sequential and
//! streams each resumed turn to completion, so any turn that never ends — a
//! wedged tool, a provider that never returns, a model call retrying forever
//! — used to stall every id after it, silently. (#1920 fixed the one such
//! state visible on disk before spawning, a parked approval; the ceiling
//! covers every way a turn fails to end that only running it reveals.)
//!
//! So each resumed run gets a wall-clock ceiling (`--ceiling`, minutes).
//! Expiry never kills the child outright — a `SIGKILL` at the ceiling would
//! land mid-edit, which is worse than the stall it fixes. It goes through the
//! same graceful discipline as Ctrl-C and `stella daemon stop`: `SIGTERM`,
//! then `STOP_GRACE` for the engine to abort at a safe boundary (invariant 6)
//! and write its own terminal status, escalation only then. The two ways that
//! ends compose with the brakes above rather than needing new ones:
//!
//! - A child that **responds** to the stop ends deliberately — checkpoint
//!   discarded, `Cancelled` recorded, exactly as if an operator had stopped
//!   it — so the next sweep skips it as ended and returns its attempts.
//! - A child so wedged it had to be **killed** wrote nothing, keeps its
//!   resume point, and is swept again next boot — where the attempt already
//!   charged for this resume counts against `MAX_BOOT_ATTEMPTS`.
//!
//! That is also the answer to whether a timed-out run is **charged a boot
//! attempt: yes**. The attempt was recorded before the spawn (as every
//! attempt is), and it stays spent, because the only run still resumable
//! after a timeout is the wedged one — the exact recurring failure the
//! three-attempt bound exists to stop. A run that was merely slower than the
//! ceiling ended deliberately at a safe boundary, and slower-than-the-ceiling
//! by hand is what `stella daemon resume <id>` — which has no ceiling — is
//! for.
//!
//! # What the operator sees
//!
//! Every candidate, continued or skipped, prints one line with its reason —
//! into `~/.stella/services/resume-boot.log` when a service is driving, into
//! the terminal when a human is. A run silently resumed at boot would be as
//! bad as one silently lost, so nothing here is quiet, and
//! `stella daemon resume-all --dry-run` prints the same decisions while
//! spawning and spending nothing.
//!
//! The resumed child is an ordinary `stella daemon resume <id> --foreground`,
//! so it inherits that path's own honesty unchanged: #1615's resume frame
//! still names the pipeline stages a resumed turn is not restoring, into the
//! service console rather than a terminal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use colored::Colorize;
use stella_store::{SessionRecord, SessionRegistry, SessionStatus};

/// How many boot-time resumes one run gets before it is retired from the
/// sweep.
///
/// Three, not one: the honest interrupted-turn case deserves a second chance
/// (a machine can reboot twice for reasons that have nothing to do with the
/// run), while a turn that takes the machine down with it burns three boots
/// of budget rather than every boot until somebody notices.
const MAX_BOOT_ATTEMPTS: u32 = 3;

/// The ledger's file name under `~/.stella/services/`.
const LEDGER_FILE: &str = "resume-boot.json";

/// The service label `install --resume-all` registers under, and therefore
/// the console name (`~/.stella/services/resume-boot.log`).
pub(super) const BOOT_LABEL: &str = "resume-boot";

/// The stella argv `install --resume-all` registers.
///
/// Named here rather than spelled at the installer's call site so the
/// registered command and the verb it is meant to run cannot drift apart.
pub(super) fn registered_argv() -> Vec<String> {
    vec!["daemon".to_string(), "resume-all".to_string()]
}

/// One registry row reduced to the facts the decision is a function of.
///
/// A plain value with no registry, filesystem or lock behind it: the whole
/// selection rule is then a pure function the tests can drive over
/// hand-written rows, which is the only way to witness "a stopped run is not
/// resumed at boot" without stopping a real run and rebooting a real machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BootCandidate {
    pub(super) id: String,
    pub(super) title: String,
    /// Whether the record was written by a supervised run (#1552).
    pub(super) supervised: bool,
    /// The status **stored** in the registry — never `list`'s presented one,
    /// which downgrades a live status with a dead pid to `Error` and would
    /// erase the exact distinction this decision rests on.
    pub(super) stored_status: SessionStatus,
    /// Whether the run's liveness lock is currently held.
    pub(super) lock_held: bool,
    /// Whether the run left a resume point this build can see.
    pub(super) has_resume_point: bool,
    /// Whether the workspace the turn must continue in still exists.
    pub(super) workspace_exists: bool,
    /// Whether the run left an unanswered approval request in its sidecar
    /// ([`stella_store::supervised::APPROVAL_REQUEST`]).
    ///
    /// Such a run does not fail on resume — it *parks*, waiting for a human
    /// who is not there. The sweep resumes one run at a time and streams each
    /// to completion, so a parked run never returns and every later id stays
    /// unresumed with nothing said about why (#1698).
    pub(super) parked: bool,
    /// Boot-time resumes already spent on this run.
    pub(super) attempts: u32,
}

/// Why a candidate is not being continued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SkipReason {
    /// Not a supervised run — there is no supervisor to relaunch.
    NotSupervised,
    /// Still running: the sweep found a run that survived, and starting a
    /// second copy of it is the one outcome worse than not resuming.
    StillRunning,
    /// The run recorded a terminal status that says it *ended* rather than
    /// broke — `Complete`, `Cancelled`, `Stopped`, `Paused`, `Archived`.
    /// Since #1653 this no longer covers `Error`; see the module docs on
    /// #1696.
    EndedDeliberately,
    /// Nothing to continue from — a clean exit, and every deliberate stop,
    /// discards its resume point.
    NoResumePoint,
    /// The workspace is gone; a resumed turn must run where its work is.
    WorkspaceGone,
    /// The run is waiting on an approval nobody is present to give. Resuming
    /// it at boot would park it again and stall the whole sweep behind it.
    NeedsInput,
    /// `MAX_BOOT_ATTEMPTS` boot-time resumes have already been spent.
    AttemptsExhausted,
}

impl SkipReason {
    /// One line an operator can act on, for the console.
    pub(super) fn explain(self) -> String {
        match self {
            Self::NotSupervised => "not a supervised run".to_string(),
            Self::StillRunning => "still running".to_string(),
            Self::EndedDeliberately => {
                "ended deliberately — only an interrupted or crashed run is resumed at boot"
                    .to_string()
            }
            Self::NoResumePoint => "no resume point".to_string(),
            Self::WorkspaceGone => "workspace no longer exists".to_string(),
            Self::NeedsInput => "waiting on an approval — answer it with \
                 `stella daemon attach <id>`, then `stella daemon resume <id>`"
                .to_string(),
            Self::AttemptsExhausted => format!(
                "retired after {MAX_BOOT_ATTEMPTS} boot-time resumes — \
                 `stella daemon resume <id>` tries again by hand"
            ),
        }
    }
}

/// What the sweep will do about one candidate.
///
/// There is deliberately no variant that *starts a run over*: restarting
/// repeats work, re-spends budget and re-applies side effects, which is the
/// failure #1627 exists to prevent. The only action this module can take is
/// to continue an existing turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BootDecision {
    /// Continue the interrupted turn from its last committed step boundary,
    /// via [`crate::daemon::resume_supervised`].
    Continue,
    Skip(SkipReason),
}

/// The selection rule, pure over one candidate.
///
/// Ordered so the reason an operator is told is the most specific true one: a
/// stopped run that also has no resume point is reported as stopped, because
/// that is the fact they would act on.
pub(super) fn decide(candidate: &BootCandidate) -> BootDecision {
    if !candidate.supervised {
        return BootDecision::Skip(SkipReason::NotSupervised);
    }
    if candidate.lock_held {
        return BootDecision::Skip(SkipReason::StillRunning);
    }
    // `Error` deliberately falls through to the resume-point check rather
    // than being skipped here: since #1653 it means only "the run fell over",
    // and a crash with a resume point is the case this whole module exists
    // for (#1696). Every *other* terminal status is a run that ended on
    // purpose.
    if !candidate.stored_status.is_live() && candidate.stored_status != SessionStatus::Error {
        return BootDecision::Skip(SkipReason::EndedDeliberately);
    }
    if !candidate.has_resume_point {
        return BootDecision::Skip(SkipReason::NoResumePoint);
    }
    if !candidate.workspace_exists {
        return BootDecision::Skip(SkipReason::WorkspaceGone);
    }
    // Before the attempts brake, deliberately: a parked run has not failed at
    // anything, so spending a boot attempt on it — and retiring it after
    // `MAX_BOOT_ATTEMPTS` reboots — would punish it for waiting. It resumes
    // the moment somebody answers.
    if candidate.parked {
        return BootDecision::Skip(SkipReason::NeedsInput);
    }
    if candidate.attempts >= MAX_BOOT_ATTEMPTS {
        return BootDecision::Skip(SkipReason::AttemptsExhausted);
    }
    BootDecision::Continue
}

/// The whole sweep's decisions, in registry order — computed before anything
/// is spawned, so the console shows the plan and `--dry-run` shows exactly it.
pub(super) fn plan(candidates: &[BootCandidate]) -> Vec<(BootCandidate, BootDecision)> {
    candidates.iter().map(|c| (c.clone(), decide(c))).collect()
}

/// The durable count of boot-time resumes per run — the brake described in
/// the module docs.
///
/// A `BTreeMap` so the serialized form is byte-stable across writes, and so a
/// human reading the file finds the ids in a fixed order.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct AttemptLedger {
    #[serde(default)]
    attempts: BTreeMap<String, u32>,
}

impl AttemptLedger {
    /// Boot-time resumes already spent on `id`.
    pub(super) fn attempts(&self, id: &str) -> u32 {
        self.attempts.get(id).copied().unwrap_or(0)
    }

    /// Count one more, returning the new total.
    ///
    /// Called *before* the resume is spawned: a resume that takes the machine
    /// down before it can report anything must still be paid for, or the
    /// bound bounds nothing.
    pub(super) fn record_attempt(&mut self, id: &str) -> u32 {
        let count = self.attempts.entry(id.to_string()).or_insert(0);
        *count += 1;
        *count
    }

    /// Drop counts for runs the registry no longer knows about, so the ledger
    /// cannot grow without bound as sessions are pruned.
    pub(super) fn retain_known(&mut self, known: &[String]) {
        self.attempts.retain(|id, _| known.iter().any(|k| k == id));
    }

    /// Forget `id`'s count because the run has reached a terminal status.
    ///
    /// The bound counts *one interruption episode*, not a session's lifetime:
    /// a session that was resumed once and then ended must not carry that
    /// attempt into an unrelated interruption weeks later, or a long-lived
    /// session would silently run out of resumes it never spent.
    pub(super) fn forget(&mut self, id: &str) {
        self.attempts.remove(id);
    }

    /// Read the ledger at `path`, treating an absent or unparseable file as an
    /// empty one.
    ///
    /// Unparseable-is-empty is safe only because of what the sweep does next:
    /// it rewrites the ledger on the same pass, so a run that resumes on a
    /// lost ledger is bounded again from that point rather than forever.
    pub(super) fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Persist the ledger, owner-only — it names session ids.
    pub(super) fn store(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| format!("cannot serialize the resume-at-boot ledger: {e}"))?;
        stella_store::write_sensitive_file_atomic(path, &json)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}

/// Where the ledger lives — beside the service consoles, in the same
/// owner-only directory.
fn ledger_path() -> Result<PathBuf, String> {
    Ok(super::service::ensure_service_log_dir()?.join(LEDGER_FILE))
}

/// Read one registry record into the facts `decide` needs.
fn candidate(
    registry: &SessionRegistry,
    record: &SessionRecord,
    ledger: &AttemptLedger,
) -> BootCandidate {
    BootCandidate {
        id: record.id.clone(),
        title: record.title.clone(),
        supervised: record.supervisor.is_some(),
        // `registry.get` rather than the `list` row we already hold: `list`
        // presents a live status whose pid is gone as `Error`, which is
        // precisely the record this sweep exists to continue.
        stored_status: registry
            .get(&record.id)
            .map_or(record.status, |stored| stored.status),
        lock_held: super::lock_is_held(&registry.sidecar_dir(&record.id)) == Some(true),
        has_resume_point: super::has_resume_point(record),
        workspace_exists: Path::new(&record.workspace).is_dir(),
        // The request file, not the answer: an answered request is removed by
        // the child that consumed it, so a leftover request is exactly the
        // "still waiting" state. Probing the filesystem here rather than in
        // `decide` keeps that function pure over already-observed facts, like
        // every other field on this struct.
        parked: registry
            .sidecar_dir(&record.id)
            .join(stella_store::supervised::APPROVAL_REQUEST)
            .exists(),
        attempts: ledger.attempts(&record.id),
    }
}

/// A ceiling for a console line: `--ceiling` speaks minutes, so the console
/// does too whenever the value is whole minutes, and seconds otherwise (a
/// sub-minute ceiling exists only in tests, but a line that printed `0m`
/// there would be a lie).
pub(super) fn describe_ceiling(ceiling: std::time::Duration) -> String {
    let secs = ceiling.as_secs();
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{}-minute", secs / 60)
    } else if secs > 0 {
        format!("{secs}-second")
    } else {
        format!("{}-millisecond", ceiling.as_millis())
    }
}

/// The one line the console gets when a resumed run outlived `ceiling` —
/// pure, so the console contract is testable without a wedged run (#1921).
///
/// It states the honest ambiguity: from out here the sweep cannot tell a
/// stop the child honoured (ended deliberately, nothing left to resume) from
/// a kill it forced (resume point kept, swept again next boot), so the line
/// names the one command that answers either way.
pub(super) fn ceiling_report(ceiling: std::time::Duration) -> String {
    format!(
        "did not finish within the {} ceiling and was stopped at a safe boundary; \
         the sweep continues with the runs behind it — `stella daemon list` shows \
         where this one ended up",
        describe_ceiling(ceiling)
    )
}

/// `stella daemon resume-all` — the verb a registered service runs at boot.
///
/// Sequential on purpose: N turns resumed at once is N models spending at once
/// on a machine nobody is watching, and the runs were not concurrent when they
/// were killed either. `ceiling` bounds each of those turns by wall clock so
/// one that never ends cannot stall the ids behind it — see the module docs.
pub(super) fn resume_all<F>(
    dry_run: bool,
    ceiling: std::time::Duration,
    mut runtime: F,
) -> Result<(), String>
where
    F: FnMut() -> Result<tokio::runtime::Runtime, String>,
{
    let registry = SessionRegistry::open_default();
    let records = registry.list();
    let path = ledger_path()?;
    let mut ledger = AttemptLedger::load(&path);
    let known: Vec<String> = records.iter().map(|r| r.id.clone()).collect();
    ledger.retain_known(&known);

    let candidates: Vec<BootCandidate> = records
        .iter()
        .map(|record| candidate(&registry, record, &ledger))
        .collect();

    let mut to_continue = Vec::new();
    for (candidate, decision) in plan(&candidates) {
        match decision {
            BootDecision::Continue => {
                println!(
                    "{} {} — {}",
                    "↩ continuing".green().bold(),
                    candidate.id,
                    candidate.title
                );
                to_continue.push(candidate.id);
            }
            BootDecision::Skip(reason) => {
                if reason == SkipReason::EndedDeliberately {
                    // The interruption episode this run's count belonged to is
                    // over; the next one starts from zero.
                    ledger.forget(&candidate.id);
                }
                println!(
                    "{} {} — {}",
                    "▸ skipping".dimmed(),
                    candidate.id.dimmed(),
                    reason.explain().dimmed()
                );
            }
        }
    }

    if dry_run {
        println!(
            "{} — {} would be continued from their last step boundary; nothing was spawned, \
             nothing was spent, and nothing was written",
            "dry run".yellow(),
            to_continue.len()
        );
        return Ok(());
    }
    if to_continue.is_empty() {
        println!(
            "Nothing to resume: no supervised run was interrupted with work left to continue."
        );
        // Still written: the pass above may have pruned entries for sessions
        // that ended, or that the registry no longer knows about.
        return ledger.store(&path);
    }

    for id in &to_continue {
        let spent = ledger.record_attempt(id);
        // Persisted before the spawn, so a resume that never returns is still
        // counted against the bound — and, for a run the ceiling below has to
        // stop, deliberately never refunded (see the module docs).
        ledger.store(&path)?;
        println!("  boot-time resume {spent} of {MAX_BOOT_ATTEMPTS} for {id}");
        match super::resume_supervised(runtime()?, Some(id), Some(ceiling)) {
            Ok(super::Watched::Finished) => {}
            Ok(super::Watched::CeilingReached) => {
                println!("{} {} — {}", "⚠".yellow(), id, ceiling_report(ceiling));
            }
            // One failed resume must not strand the rest: the sweep's whole
            // job is the runs it can still continue.
            Err(e) => eprintln!("{} could not resume {id}: {e}", "⚠".yellow()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
