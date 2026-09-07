//! Whether to open another driver session, and when.
//!
//! A driver plugin ends each session with one word. `sleep` asks to be woken
//! again. `halt` says it is done. Something has to hear that and act on it, or
//! a driver that asks for fifteen minutes waits for ever. This module is the
//! part that decides; the host it runs in does the waiting.
//!
//! # A halt has to stop the run
//!
//! [`SessionEnding::Halt`] ends the sequence. It is read before any other
//! rule. Money left and sessions left cannot outvote it.
//!
//! # The ceilings span the run
//!
//! A spend cap that reset each session would cap nothing. Ten sessions under
//! a ten dollar cap would buy a hundred dollars. So [`RunSoFar`] carries what
//! the whole run has spent, and [`DriveLimits::spend_cap`] is read against
//! that total. [`DriveLimits::session_ceiling`] does the same for how many
//! sessions a run may open.
//!
//! # No clock, no sleeping
//!
//! [`next_session`] returns the wait in seconds and never takes one. That is
//! what lets a test drive a sequence of sessions in no time at all, and it is
//! this crate's own rule: the decisions are pure and the host does the I/O.
//! The wait it returns is already clamped to
//! [`DriveLimits::max_sleep_secs`], so a driver cannot buy an unbounded
//! absence by asking for one.

use serde::{Deserialize, Serialize};

/// How the session that just ended ended.
///
/// A local type rather than `stella_plugin::DriveNext`, for the reason the
/// loop machine keeps its own [`IssueRef`](crate::IssueRef): this crate
/// depends on no workspace crate, so the host maps. It also holds one case
/// the wire type cannot — a session that failed before it said anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "ending")]
pub enum SessionEnding {
    /// The driver asked to be woken again after this many seconds.
    Sleep {
        /// What it asked for, before this module's ceiling is applied.
        secs: u32,
    },
    /// The driver stopped, and said why.
    Halt {
        /// The sentence a human reads in the ledger.
        reason: String,
    },
    /// The session never reached an answer: the process would not start, timed
    /// out, died, or ended without saying what to do next.
    Failed {
        /// The host's own message, which already names the program.
        message: String,
    },
}

/// What bounds a run of sessions.
///
/// Every field is a ceiling. None of them is a floor: a driver that asks for
/// less gets what it asked for.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DriveLimits {
    /// The whole run's spend ceiling in USD, or `None` to spend unbounded.
    ///
    /// `None` is the observed mode `--spend-limit` already describes: spend is
    /// still summed so a run can report what it cost, and nothing is refused.
    pub spend_cap: Option<f64>,
    /// The longest wait this host will honour between sessions.
    ///
    /// The host clamps a driver's request at the socket already
    /// (`stella_runtime::wrapper::MAX_SLEEP_SECS`). Naming it here as well is
    /// what makes the ceiling a fact this function can be tested against
    /// rather than one it has to trust its caller for.
    pub max_sleep_secs: u32,
    /// How many sessions one invocation may open, or `None` for as many as the
    /// driver asks for.
    pub session_ceiling: Option<u32>,
}

/// What the run has done so far.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct RunSoFar {
    /// How many sessions this invocation has already opened, the one that just
    /// ended included.
    pub sessions: u32,
    /// What those sessions have spent between them, in USD.
    pub spent_usd: f64,
}

/// Why a run of sessions ended.
///
/// Typed because the reader of a finished run has three different next moves.
/// The driver decided, the host failed, or a ceiling was met.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "stop")]
pub enum StopReason {
    /// The driver halted, and this is what it said.
    Halted {
        /// The driver's own reason, carried through to the ledger.
        reason: String,
    },
    /// A session failed before it could say what to do next.
    Failed {
        /// The host's message.
        message: String,
    },
    /// The run met its spend ceiling.
    BudgetSpent {
        /// The ceiling.
        cap: f64,
        /// What the run had spent when it was met.
        spent: f64,
    },
    /// The run opened every session it was allowed.
    SessionCeiling {
        /// How many that was.
        ceiling: u32,
    },
}

/// The run's next move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "drive")]
pub enum DriveStep {
    /// Open another session, after waiting this long.
    Open {
        /// Seconds to wait first, already clamped to the host's ceiling. Zero
        /// means open the next one now.
        after_secs: u32,
    },
    /// Stop the run, and say why.
    Stop {
        /// What ended it.
        reason: StopReason,
    },
}

/// Decide what to do after a session ends.
///
/// Total over its inputs, with no clock, no randomness and no I/O, so the same
/// ending under the same run always yields the same move.
///
/// # Precedence
///
/// The order is the policy, so it is written down rather than read out of the
/// body:
///
/// 1. **A halt ends the run.** Checked first, so nothing outvotes it.
/// 2. **A failed session ends the run.** A driver that could not answer cannot
///    be asked to answer again by the same host in the same way.
/// 3. **The spend ceiling ends the run**, measured against everything the run
///    has spent rather than against one session.
/// 4. **The session ceiling ends the run.**
/// 5. **A sleep re-opens**, after the wait it asked for or the host's ceiling,
///    whichever is shorter.
#[must_use]
pub fn next_session(ending: &SessionEnding, run: &RunSoFar, limits: &DriveLimits) -> DriveStep {
    // 1 and 2. What the session itself decided outranks every ceiling. A
    // ceiling says the run may not continue; these say it is already over.
    match ending {
        SessionEnding::Halt { reason } => {
            return DriveStep::Stop {
                reason: StopReason::Halted {
                    reason: reason.clone(),
                },
            };
        }
        SessionEnding::Failed { message } => {
            return DriveStep::Stop {
                reason: StopReason::Failed {
                    message: message.clone(),
                },
            };
        }
        SessionEnding::Sleep { .. } => {}
    }

    // 3. The cap is against the run's total. Anything else caps nothing.
    if let Some(cap) = limits.spend_cap
        && run.spent_usd >= cap
    {
        return DriveStep::Stop {
            reason: StopReason::BudgetSpent {
                cap,
                spent: run.spent_usd,
            },
        };
    }

    // 4. How many sessions one invocation is allowed to open.
    if let Some(ceiling) = limits.session_ceiling
        && run.sessions >= ceiling
    {
        return DriveStep::Stop {
            reason: StopReason::SessionCeiling { ceiling },
        };
    }

    // 5. The wait the driver asked for, under the host's ceiling.
    let SessionEnding::Sleep { secs } = ending else {
        unreachable!("the two other endings returned above");
    };
    DriveStep::Open {
        after_secs: (*secs).min(limits.max_sleep_secs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A day, the ceiling the driver socket already applies.
    const DAY: u32 = 24 * 60 * 60;

    fn limits() -> DriveLimits {
        DriveLimits {
            spend_cap: None,
            max_sleep_secs: DAY,
            session_ceiling: None,
        }
    }

    fn after(sessions: u32, spent_usd: f64) -> RunSoFar {
        RunSoFar {
            sessions,
            spent_usd,
        }
    }

    /// **The witness.** Before this module nothing answered "open another
    /// one?", so a driver that asked to sleep was never woken.
    #[test]
    fn a_sleep_asks_for_another_session_after_the_wait_it_named() {
        assert_eq!(
            next_session(
                &SessionEnding::Sleep { secs: 900 },
                &after(1, 0.0),
                &limits()
            ),
            DriveStep::Open { after_secs: 900 }
        );
    }

    /// A halt ends the sequence, and the driver's own words are what the run
    /// reports — that is what reaches the ledger.
    #[test]
    fn a_halt_ends_the_run_and_keeps_its_reason() {
        assert_eq!(
            next_session(
                &SessionEnding::Halt {
                    reason: "backlog empty".into()
                },
                &after(1, 0.0),
                &limits()
            ),
            DriveStep::Stop {
                reason: StopReason::Halted {
                    reason: "backlog empty".into()
                }
            }
        );
    }

    /// Nothing outvotes a halt: money left and sessions left are both no
    /// reason to open another one.
    #[test]
    fn a_halt_outranks_every_ceiling_that_would_allow_another_session() {
        let generous = DriveLimits {
            spend_cap: Some(100.0),
            max_sleep_secs: DAY,
            session_ceiling: Some(50),
        };
        assert!(matches!(
            next_session(
                &SessionEnding::Halt {
                    reason: "done".into()
                },
                &after(1, 0.0),
                &generous
            ),
            DriveStep::Stop {
                reason: StopReason::Halted { .. }
            }
        ));
    }

    /// A session that never answered ends the run rather than being retried.
    #[test]
    fn a_failed_session_ends_the_run() {
        assert_eq!(
            next_session(
                &SessionEnding::Failed {
                    message: "driver \"watcher\" timed out".into()
                },
                &after(1, 0.0),
                &limits()
            ),
            DriveStep::Stop {
                reason: StopReason::Failed {
                    message: "driver \"watcher\" timed out".into()
                }
            }
        );
    }

    /// The cap is read against the run's total, so a sequence cannot spend
    /// past it by spending a little at a time.
    #[test]
    fn the_spend_cap_is_measured_against_the_whole_run() {
        let capped = DriveLimits {
            spend_cap: Some(10.0),
            ..limits()
        };
        let asleep = SessionEnding::Sleep { secs: 5 };

        assert_eq!(
            next_session(&asleep, &after(3, 9.5), &capped),
            DriveStep::Open { after_secs: 5 },
            "under the cap, the run continues"
        );
        assert_eq!(
            next_session(&asleep, &after(4, 10.0), &capped),
            DriveStep::Stop {
                reason: StopReason::BudgetSpent {
                    cap: 10.0,
                    spent: 10.0
                }
            },
            "at the cap, the run stops"
        );
    }

    /// No cap is the observed mode: spend is summed and nothing is refused.
    #[test]
    fn an_uncapped_run_is_never_stopped_for_spending() {
        assert_eq!(
            next_session(
                &SessionEnding::Sleep { secs: 1 },
                &after(99, 1_000.0),
                &limits()
            ),
            DriveStep::Open { after_secs: 1 }
        );
    }

    /// An invocation opens as many sessions as it was allowed and no more.
    #[test]
    fn the_session_ceiling_ends_the_run() {
        let two = DriveLimits {
            session_ceiling: Some(2),
            ..limits()
        };
        let asleep = SessionEnding::Sleep { secs: 30 };

        assert_eq!(
            next_session(&asleep, &after(1, 0.0), &two),
            DriveStep::Open { after_secs: 30 }
        );
        assert_eq!(
            next_session(&asleep, &after(2, 0.0), &two),
            DriveStep::Stop {
                reason: StopReason::SessionCeiling { ceiling: 2 }
            }
        );
    }

    /// The wait is a ceiling, never a floor. A decade is cut to the host's
    /// bound; a minute is honoured as asked.
    #[test]
    fn a_long_wait_is_cut_to_the_hosts_ceiling_and_a_short_one_is_not() {
        let decade = 10 * 365 * DAY;
        assert_eq!(
            next_session(
                &SessionEnding::Sleep { secs: decade },
                &after(1, 0.0),
                &limits()
            ),
            DriveStep::Open { after_secs: DAY }
        );
        assert_eq!(
            next_session(
                &SessionEnding::Sleep { secs: 60 },
                &after(1, 0.0),
                &limits()
            ),
            DriveStep::Open { after_secs: 60 }
        );
    }

    /// Serde-first: every type here crosses into `stella-cli`, so each one
    /// round-trips byte for byte (AGENTS.md's rule 4).
    #[test]
    fn the_types_round_trip_through_json() {
        let ending = SessionEnding::Halt {
            reason: "budget spent".into(),
        };
        let json = serde_json::to_string(&ending).expect("serialize");
        assert_eq!(
            serde_json::from_str::<SessionEnding>(&json).expect("deserialize"),
            ending
        );

        let step = DriveStep::Stop {
            reason: StopReason::BudgetSpent {
                cap: 30.0,
                spent: 31.5,
            },
        };
        let json = serde_json::to_string(&step).expect("serialize");
        assert_eq!(
            serde_json::from_str::<DriveStep>(&json).expect("deserialize"),
            step
        );

        let run = after(4, 2.5);
        let json = serde_json::to_string(&run).expect("serialize");
        assert_eq!(
            serde_json::from_str::<RunSoFar>(&json).expect("deserialize"),
            run
        );

        let bounds = DriveLimits {
            spend_cap: Some(30.0),
            max_sleep_secs: DAY,
            session_ceiling: Some(4),
        };
        let json = serde_json::to_string(&bounds).expect("serialize");
        assert_eq!(
            serde_json::from_str::<DriveLimits>(&json).expect("deserialize"),
            bounds
        );
    }
}
