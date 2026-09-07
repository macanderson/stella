// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One driver session after another.
//!
//! A driver that answers `sleep` asks to be woken again. Nothing woke it
//! before. `stella plugin drive` opened one session and returned. So a loop
//! that could claim an issue and work it to a diff did that once and stopped.
//!
//! # Who decides, and who waits
//!
//! `stella_autonomy::drive::next_session` decides. This module opens the
//! session, adds what it cost to the run, and does what that function says.
//! The policy is written down once. Two hosts of this loop cannot disagree
//! about it.
//!
//! # The port
//!
//! [`DriveHost`] is the seam. Starting a process, writing the ledger, waiting
//! and printing all sit behind it. So a test drives many sessions with no
//! subprocess and no clock. [`PluginDriveHost`] is the shipping side.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use stella_autonomy::drive::{DriveLimits, DriveStep, RunSoFar, SessionEnding, StopReason};
use stella_plugin::DriveNext;

use super::session_log::{DriverSessionOutcome, DriverSessionRecord};
use crate::self_driving_cmd::budget::RunBudget;
use crate::self_driving_cmd::config::LoopConfig;

/// One session's ending, as the host saw it.
pub(crate) struct SessionEnd {
    /// What the driver said to do next, or why the session never said.
    pub(crate) next: Result<DriveNext, String>,
    /// Every ask this session could not serve, in order.
    pub(crate) refusals: Vec<String>,
}

impl SessionEnd {
    /// The ending in the vocabulary the decision function reads.
    fn ending(&self) -> SessionEnding {
        match &self.next {
            Ok(DriveNext::Sleep { secs }) => SessionEnding::Sleep { secs: *secs },
            Ok(DriveNext::Halt { reason }) => SessionEnding::Halt {
                reason: reason.clone(),
            },
            Err(message) => SessionEnding::Failed {
                message: message.clone(),
            },
        }
    }
}

/// What a run of sessions needs from the host it runs in.
pub(crate) trait DriveHost {
    /// A session identifier the next session is opened under.
    fn mint(&mut self) -> String;

    /// Open one session and read back what the driver said.
    fn open(&mut self, session: &str) -> SessionEnd;

    /// Write the session down and say on screen how it ended.
    fn settle(&mut self, session: &str, end: &SessionEnd);

    /// What every session of this run has spent between them, in USD.
    fn spent(&self) -> f64;

    /// Wait before opening the next session.
    fn wait(&mut self, secs: u32);
}

/// How a run of sessions ended, and how many it took.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunEnd {
    /// Why it stopped. A [`StopReason::Failed`] is the caller's error to
    /// report; every other reason is an ordinary ending.
    pub(crate) reason: StopReason,
    /// How many sessions this invocation opened.
    pub(crate) sessions: u32,
}

/// Open sessions until the driver stops asking for another one.
pub(crate) fn run(host: &mut dyn DriveHost, limits: &DriveLimits) -> RunEnd {
    let mut so_far = RunSoFar::default();
    loop {
        let session = host.mint();
        let end = host.open(&session);
        host.settle(&session, &end);

        so_far.sessions = so_far.sessions.saturating_add(1);
        so_far.spent_usd = host.spent();

        match stella_autonomy::drive::next_session(&end.ending(), &so_far, limits) {
            DriveStep::Stop { reason } => {
                return RunEnd {
                    reason,
                    sessions: so_far.sessions,
                };
            }
            DriveStep::Open { after_secs } => host.wait(after_secs),
        }
    }
}

/// The shipping host: a real plugin process per session, the real ledger, and
/// a real wait.
pub(crate) struct PluginDriveHost<'a> {
    /// Where the ledger is written and where a work turn is cut.
    workspace_root: &'a Path,
    /// The installed plugin's manifest name.
    plugin: &'a str,
    /// The bound driver, re-served per session so each one gets a gate of its
    /// own.
    resolved: super::ResolvedDriver,
    /// This workspace's own loop settings, for the work verbs.
    config: LoopConfig,
    /// The ceiling every session of this run shares.
    budget: Arc<Mutex<RunBudget>>,
    /// Whether the operator has already been told what this build serves. Said
    /// once per run rather than once per session: it is a fact about the
    /// build, and repeating it every fifteen minutes would bury the sessions.
    announced: bool,
}

impl<'a> PluginDriveHost<'a> {
    /// A host over one workspace and one installed driver.
    pub(crate) fn new(
        workspace_root: &'a Path,
        plugin: &'a str,
        resolved: super::ResolvedDriver,
        config: LoopConfig,
        budget: Arc<Mutex<RunBudget>>,
    ) -> Self {
        Self {
            workspace_root,
            plugin,
            resolved,
            config,
            budget,
            announced: false,
        }
    }

    /// The program a record names.
    fn program(&self) -> String {
        self.resolved.program().to_string()
    }

    /// The capabilities one session is served through.
    ///
    /// Built per session because [`super::work::WorkSlot`] holds the one unit
    /// a session may carry and [`super::deliver::DeliverDesk`] the one pull
    /// request opened for it, and either shared between sessions would hand
    /// the second one the first one's checkout. The budget behind them is
    /// shared, which is the opposite choice for the opposite reason: a ceiling
    /// that reset per session would cap nothing.
    fn capabilities(&self) -> Box<dyn stella_runtime::wrapper::DriverCapabilities> {
        Box::new(super::capabilities::HostDriverCapabilities::new(
            self.plugin,
            self.resolved.gates().cloned(),
            Box::new(crate::issue_provider::GhIssueProvider::for_workspace(
                self.workspace_root,
            )),
            self.config.clone(),
            self.workspace_root.to_path_buf(),
            super::work::WorkSlot::new(Box::new(super::work::SpawnedWorkRunner::new(
                self.workspace_root.to_path_buf(),
                self.config.clone(),
                Arc::clone(&self.budget),
            ))),
            super::deliver::DeliverDesk::new(Box::new(super::deliver::GhDeliverForge::new(
                self.workspace_root.to_path_buf(),
                self.config.clone(),
            ))),
        ))
    }
}

impl DriveHost for PluginDriveHost<'_> {
    fn mint(&mut self) -> String {
        crate::plugin_cmd::session_id()
    }

    fn open(&mut self, session: &str) -> SessionEnd {
        let bound = self.resolved.clone().serving(self.capabilities());
        if !self.announced && bound.offers_calls() {
            self.announced = true;
            println!(
                "  this build serves `backlog_next`, `backlog_claim`, `work_start`, \
                 `work_status`, `work_abandon`, `deliver_open`, `deliver_observe`, \
                 `deliver_next`, `deliver_ready` and `deliver_merge`; every other \
                 capability this run asks for will be refused as unsupported"
            );
            if self
                .budget
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cap()
                .is_none()
            {
                println!(
                    "  no --spend-limit was given, so the turns this run asks for spend until \
                     they finish"
                );
            }
        }
        let next = bound.open(session);
        let refusals = bound.refusals();
        SessionEnd { next, refusals }
    }

    fn settle(&mut self, session: &str, end: &SessionEnd) {
        // Printed whichever way the session ended. A driver that asked for a
        // capability and then failed is the case where the refusals matter
        // most: they are usually why it failed.
        for refusal in &end.refusals {
            eprintln!("  ! {refusal}");
        }
        match &end.next {
            Ok(DriveNext::Sleep { secs }) => {
                println!("session {session} ended: sleep {secs}s before the next one");
            }
            Ok(DriveNext::Halt { reason }) => {
                println!("session {session} ended: halt — {reason}");
            }
            Err(error) => eprintln!("session {session} ended: {error}"),
        }
        if let Err(error) = record(
            self.workspace_root,
            self.plugin,
            session,
            &self.program(),
            end,
        ) {
            eprintln!("  ! {error}");
        }
    }

    fn spent(&self) -> f64 {
        self.budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .spent()
    }

    fn wait(&mut self, secs: u32) {
        if secs == 0 {
            return;
        }
        println!("  waiting {secs}s before the next session");
        std::thread::sleep(Duration::from_secs(u64::from(secs)));
    }
}

/// Append one session to the workspace's driver ledger.
///
/// A free function so a test host writes through the same call the shipping
/// one does. A test then reads the ledger back off disk instead of asking a
/// mock what it was told.
pub(crate) fn record(
    workspace_root: &Path,
    plugin: &str,
    session: &str,
    program: &str,
    end: &SessionEnd,
) -> Result<(), String> {
    super::session_log::record(
        workspace_root,
        &DriverSessionRecord {
            at: crate::timefmt::rfc3339_utc_now(),
            plugin: plugin.to_string(),
            session_id: session.to_string(),
            program: program.to_string(),
            refusals: end.refusals.clone(),
            outcome: DriverSessionOutcome::from_result(&end.next),
        },
    )
}

/// What a caller prints when the run is over.
#[must_use]
pub(crate) fn ending_line(reason: &StopReason, sessions: u32) -> String {
    let plural = if sessions == 1 { "session" } else { "sessions" };
    let opener = format!("run ended after {sessions} {plural}");
    match reason {
        StopReason::Halted { reason } => format!("{opener}: halt — {reason}"),
        StopReason::Failed { message } => format!("{opener}: {message}"),
        StopReason::BudgetSpent { cap, spent } => {
            format!("{opener}: the run's ${cap:.2} spend limit is reached (${spent:.2} spent)")
        }
        StopReason::SessionCeiling { ceiling } => {
            format!("{opener}: --max-sessions {ceiling} reached")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A host that answers from a script and writes the real ledger.
    ///
    /// Everything a sequence can do is here: it opens sessions, records them
    /// through `session_log`, charges what the script says each one spent, and
    /// counts its waits instead of taking them.
    struct ScriptedHost {
        /// Where the ledger goes.
        root: PathBuf,
        /// What each session answers, in order. A run that asks for one more
        /// than the script holds gets a halt saying so, which fails a test by
        /// ending the run rather than by hanging it.
        script: Vec<(SessionEnd, f64)>,
        /// How many sessions have been opened.
        opened: u32,
        /// What the run has spent, folded in as each session ends.
        spent: f64,
        /// Every wait the sequence asked for, in seconds.
        waits: Vec<u32>,
    }

    impl ScriptedHost {
        fn new(root: PathBuf, script: Vec<(SessionEnd, f64)>) -> Self {
            Self {
                root,
                script,
                opened: 0,
                spent: 0.0,
                waits: Vec::new(),
            }
        }
    }

    impl DriveHost for ScriptedHost {
        fn mint(&mut self) -> String {
            format!("drive-test-{}", self.opened + 1)
        }

        fn open(&mut self, _session: &str) -> SessionEnd {
            let index = self.opened as usize;
            self.opened += 1;
            if index >= self.script.len() {
                return SessionEnd {
                    next: Ok(DriveNext::Halt {
                        reason: "the script ran out".into(),
                    }),
                    refusals: Vec::new(),
                };
            }
            let (end, cost) = &self.script[index];
            self.spent += cost;
            SessionEnd {
                next: end.next.clone(),
                refusals: end.refusals.clone(),
            }
        }

        fn settle(&mut self, session: &str, end: &SessionEnd) {
            record(
                &self.root,
                "watcher",
                session,
                "/opt/pkgs/watcher/drive.sh",
                end,
            )
            .expect("the ledger must be writable");
        }

        fn spent(&self) -> f64 {
            self.spent
        }

        fn wait(&mut self, secs: u32) {
            self.waits.push(secs);
        }
    }

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "stella-driver-sequence-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        root
    }

    fn sleeping(secs: u32) -> SessionEnd {
        SessionEnd {
            next: Ok(DriveNext::Sleep { secs }),
            refusals: Vec::new(),
        }
    }

    fn halting(reason: &str) -> SessionEnd {
        SessionEnd {
            next: Ok(DriveNext::Halt {
                reason: reason.into(),
            }),
            refusals: Vec::new(),
        }
    }

    fn limits() -> DriveLimits {
        DriveLimits {
            spend_cap: None,
            max_sleep_secs: 24 * 60 * 60,
            session_ceiling: None,
        }
    }

    /// **The witness.** Before this module one invocation opened one session
    /// and returned, so a driver that asked to sleep was never re-opened.
    ///
    /// Three sessions from one call: sleep, wake, sleep, wake, halt. The waits
    /// are the ones the driver asked for, and the halt is what ends it.
    #[test]
    fn one_invocation_sleeps_reopens_and_stops_on_a_halt() {
        let root = temp_root("sequence");
        let mut host = ScriptedHost::new(
            root.clone(),
            vec![
                (sleeping(900), 0.0),
                (sleeping(30), 0.0),
                (halting("backlog empty"), 0.0),
            ],
        );

        let end = run(&mut host, &limits());

        assert_eq!(host.opened, 3, "one invocation opened three sessions");
        assert_eq!(
            host.waits,
            vec![900, 30],
            "each sleep was honoured as asked"
        );
        assert_eq!(
            end,
            RunEnd {
                reason: StopReason::Halted {
                    reason: "backlog empty".into()
                },
                sessions: 3,
            }
        );
    }

    /// A halt reaches the ledger carrying its reason, and it is the last line
    /// there: the sequence ended.
    #[test]
    fn a_halt_reaches_the_ledger_with_its_reason_and_ends_the_sequence() {
        let root = temp_root("ledger");
        let mut host = ScriptedHost::new(
            root.clone(),
            vec![(sleeping(5), 0.0), (halting("operator stop"), 0.0)],
        );

        run(&mut host, &limits());

        let sessions = super::super::session_log::read_sessions(&root);
        assert_eq!(sessions.len(), 2, "every session is written down");
        assert_eq!(sessions[0].outcome, DriverSessionOutcome::Sleep { secs: 5 });
        assert_eq!(
            sessions[1].outcome,
            DriverSessionOutcome::Halt {
                reason: "operator stop".into()
            },
            "the halt's own reason is what the ledger holds"
        );
    }

    /// The ceiling spans the run. Three sessions each spending four dollars
    /// under a ten dollar cap stop at the third, not after each one is
    /// separately under the cap.
    #[test]
    fn a_run_of_sessions_cannot_spend_past_the_cap() {
        let root = temp_root("cap");
        let mut host = ScriptedHost::new(
            root,
            vec![
                (sleeping(1), 4.0),
                (sleeping(1), 4.0),
                (sleeping(1), 4.0),
                (sleeping(1), 4.0),
            ],
        );

        let end = run(
            &mut host,
            &DriveLimits {
                spend_cap: Some(10.0),
                ..limits()
            },
        );

        assert_eq!(
            host.opened, 3,
            "the run stopped at the session that crossed the cap"
        );
        assert_eq!(
            end.reason,
            StopReason::BudgetSpent {
                cap: 10.0,
                spent: 12.0
            }
        );
    }

    /// A session that failed ends the run rather than being re-opened, and the
    /// failure is still written down.
    #[test]
    fn a_failed_session_ends_the_run_and_is_still_recorded() {
        let root = temp_root("failed");
        let mut host = ScriptedHost::new(
            root.clone(),
            vec![(
                SessionEnd {
                    next: Err("driver \"watcher\" timed out".into()),
                    refusals: vec!["plugin \"watcher\": backlog_file: unsupported".into()],
                },
                0.0,
            )],
        );

        let end = run(&mut host, &limits());

        assert_eq!(host.opened, 1);
        assert!(matches!(end.reason, StopReason::Failed { .. }));
        let sessions = super::super::session_log::read_sessions(&root);
        assert_eq!(
            sessions[0].outcome,
            DriverSessionOutcome::Error {
                message: "driver \"watcher\" timed out".into()
            }
        );
        assert_eq!(
            sessions[0].refusals,
            vec!["plugin \"watcher\": backlog_file: unsupported".to_string()],
            "a refused ask survives the session that made it"
        );
    }

    /// `--max-sessions` bounds an invocation whose driver would sleep for ever.
    #[test]
    fn the_session_ceiling_stops_a_driver_that_never_halts() {
        let root = temp_root("ceiling");
        let mut host = ScriptedHost::new(
            root,
            vec![
                (sleeping(1), 0.0),
                (sleeping(1), 0.0),
                (sleeping(1), 0.0),
                (sleeping(1), 0.0),
            ],
        );

        let end = run(
            &mut host,
            &DriveLimits {
                session_ceiling: Some(2),
                ..limits()
            },
        );

        assert_eq!(host.opened, 2);
        assert_eq!(end.reason, StopReason::SessionCeiling { ceiling: 2 });
    }

    /// Every ending says how many sessions it took and what stopped it.
    #[test]
    fn each_ending_names_itself_and_counts_the_sessions() {
        assert_eq!(
            ending_line(
                &StopReason::Halted {
                    reason: "done".into()
                },
                1
            ),
            "run ended after 1 session: halt — done"
        );
        assert_eq!(
            ending_line(&StopReason::SessionCeiling { ceiling: 3 }, 3),
            "run ended after 3 sessions: --max-sessions 3 reached"
        );
        assert_eq!(
            ending_line(
                &StopReason::BudgetSpent {
                    cap: 10.0,
                    spent: 12.0
                },
                2
            ),
            "run ended after 2 sessions: the run's $10.00 spend limit is reached ($12.00 spent)"
        );
        assert_eq!(
            ending_line(
                &StopReason::Failed {
                    message: "timed out".into()
                },
                2
            ),
            "run ended after 2 sessions: timed out"
        );
    }
}
