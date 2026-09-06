//! Where the loop looks for work once the ranked queue is empty.
//!
//! The queue drains. Three other supplies do not, and this module holds the
//! rules that say which of them is open:
//!
//! - `rearm` re-opens the lens ladder once the base branch has moved.
//! - `regress` re-checks the fixes the loop has already claimed.
//! - `meta` reads the loop's own ledger for habits.
//!
//! Each one has its own switch, and each switch starts off. A loop that
//! upgrades keeps drawing from the queue alone. An endless supply of work is
//! not an endless supply of *useful* work, so opening one is a choice a
//! person makes.
//!
//! Re-passing a lens is cheap because of [`novel`]. The dedup set is keyed by
//! digest across the whole repository, so a finding the loop has filed before
//! is dropped before it reaches the tracker. A re-pass yields the new code and
//! nothing else.
//!
//! [`WorkSupply`] names the three. The crate root's `Supply` is a different
//! thing: what the machine has to spend, in cores and disk and memory.
//!
//! `doc:backlog-self-driving` §4 is the design.

use serde::{Deserialize, Serialize};

/// One thing to file, before it becomes a tracker draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// One line. The dedup key is taken from this.
    pub title: String,
    /// The handoff. Assume the reader was not there.
    pub body: String,
    /// The labels the draft carries.
    pub labels: Vec<String>,
}

/// A supply the loop draws from when the queue is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkSupply {
    /// Re-open the lens ladder against a base that has moved.
    Rearm,
    /// Re-check the fixes this loop has claimed.
    Regress,
    /// Read the loop's own ledger.
    Meta,
}

impl WorkSupply {
    /// Every supply, in the order a sweep draws from them.
    pub const ALL: &[Self] = &[Self::Rearm, Self::Regress, Self::Meta];

    /// The word an operator writes in `stella.toml`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rearm => "rearm",
            Self::Regress => "regress",
            Self::Meta => "meta",
        }
    }
}

/// How many commits the base must gain before a dry ladder re-opens.
///
/// Fifty is about a week of merges on a busy tree. It is a choice and not a
/// measurement: small enough that a busy tree gets a fresh look, large enough
/// that a quiet one is left alone.
pub const DEFAULT_REARM_COMMITS: u64 = 50;

/// How many days may pass instead, when the commit count stays low.
///
/// A tree that gains ten commits a month still drifts. Thirty days is the
/// other way in.
pub const DEFAULT_REARM_DAYS: u64 = 30;

/// Which supplies are open, and how far the base must move to re-arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupplyPolicy {
    /// Whether a dry ladder may re-open.
    pub rearm: bool,
    /// Whether closed work is re-checked.
    pub regress: bool,
    /// Whether the ledger is read for habits.
    pub meta: bool,
    /// Commits the base must gain first.
    pub rearm_commits: u64,
    /// Days that may pass instead.
    pub rearm_days: u64,
}

impl Default for SupplyPolicy {
    /// Draw from the queue and nothing else.
    fn default() -> Self {
        Self {
            rearm: false,
            regress: false,
            meta: false,
            rearm_commits: DEFAULT_REARM_COMMITS,
            rearm_days: DEFAULT_REARM_DAYS,
        }
    }
}

impl SupplyPolicy {
    /// Whether this policy opens any supply at all.
    #[must_use]
    pub fn any_open(&self) -> bool {
        self.rearm || self.regress || self.meta
    }

    /// Whether one named supply is open.
    #[must_use]
    pub fn opens(&self, supply: WorkSupply) -> bool {
        match supply {
            WorkSupply::Rearm => self.rearm,
            WorkSupply::Regress => self.regress,
            WorkSupply::Meta => self.meta,
        }
    }
}

/// What the loop can see about the base since the ladder went dry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Baseline {
    /// Commits the base has gained since the clean sweep. `None` when git
    /// could not be asked.
    pub commits: Option<u64>,
    /// Days since that sweep. `None` when nothing wrote the date down.
    pub days: Option<u64>,
    /// Whether the loop is filing far more than it finds.
    pub noisy: bool,
}

/// Whether a dry ladder re-opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rearm {
    /// Re-open the ladder at this rung.
    Reopen {
        /// The first lens.
        lens: &'static str,
    },
    /// Leave it shut.
    Hold {
        /// What holds it shut.
        reason: Hold,
    },
}

/// Why a dry ladder stays shut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// The switch is off. This is the default.
    SwitchOff,
    /// A lens is still open, so there is nothing to re-arm.
    LadderOpen,
    /// The base has not moved far enough yet.
    Unmoved,
    /// Nothing says where the last clean sweep was.
    NoBaseline,
    /// The loop files far more than it finds. Widening would add noise.
    Noisy,
}

/// Whether the ladder re-opens against a base that has moved.
///
/// `aperture` is the rung the loop has open. Anything but [`crate::WATCH`]
/// means the ladder still has a question to ask, so there is nothing to
/// re-arm.
///
/// A `NOISY` loop is held shut. That signal fires when the loop files far more
/// than it finds, and the answer to that is to narrow rather than to open one
/// more lens.
///
/// # Examples
///
/// ```
/// # use stella_autonomy::supply::{Baseline, Hold, Rearm, SupplyPolicy, rearm};
/// let mut policy = SupplyPolicy::default();
/// let moved = Baseline { commits: Some(500), days: None, noisy: false };
///
/// // Off by default, whatever the base did.
/// assert_eq!(rearm("watch", &moved, &policy), Rearm::Hold { reason: Hold::SwitchOff });
///
/// policy.rearm = true;
/// assert_eq!(rearm("watch", &moved, &policy), Rearm::Reopen { lens: "rubric" });
/// ```
#[must_use]
pub fn rearm(aperture: &str, base: &Baseline, policy: &SupplyPolicy) -> Rearm {
    let hold = |reason| Rearm::Hold { reason };
    if !policy.rearm {
        return hold(Hold::SwitchOff);
    }
    if aperture.trim() != crate::WATCH {
        return hold(Hold::LadderOpen);
    }
    if base.noisy {
        return hold(Hold::Noisy);
    }
    if base.commits.is_none() && base.days.is_none() {
        return hold(Hold::NoBaseline);
    }
    let moved = base.commits.is_some_and(|n| n >= policy.rearm_commits)
        || base.days.is_some_and(|n| n >= policy.rearm_days);
    if moved {
        Rearm::Reopen {
            lens: crate::LENSES.first().map_or(crate::WATCH, |l| l.name),
        }
    } else {
        hold(Hold::Unmoved)
    }
}

/// The label a lens finding carries.
///
/// The same word `regress` and `meta` file under. A defect a lens found is a
/// defect, and the type axis of a backlog convention spells that `bug`. A
/// convention with no such member refuses the draft, and the refusal stands.
pub const DEFECT_LABEL: &str = "bug";

/// How many findings one lens pass offers at most.
///
/// A tool that prints a hundred errors would otherwise become a hundred
/// issues in one pass. Ten is a bound and not a measurement: enough that a
/// real run is not cut short, small enough that a broken tool cannot flood a
/// tracker. What is over the cap is offered again on the next pass, because
/// nothing wrote it down as seen.
pub const MAX_LENS_FINDINGS: usize = 10;

/// How the driver reads one lens command's output, with no model.
///
/// A lens declares one only when its output can be read without judgement.
/// Most `interpret` lines ask for a judgement — "each listed file that holds
/// pure decision logic" — and a mechanical read of those would file noise.
/// That is how `doc:backlog-self-driving` §4.4 says this supply turns into
/// make-work. A lens that asks for judgement declares no reading, and the
/// model-driven audit phase still reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    /// Every line the tool prints as an error is one defect.
    ///
    /// `error: text` and `error[code]: text` are both read, and the text is
    /// the finding. That is the shape `cargo deny` writes, which is what the
    /// `supply-chain` lens declares. A reading that matches nothing yields
    /// nothing, so a tool that changes its output costs the loop a supply and
    /// never a wrong filing.
    ErrorLines,
}

/// Turn one lens command's output into findings.
///
/// Empty for a lens that declares no reading, and for output the reading does
/// not match. Both mean one thing to the caller: this lens offered nothing.
///
/// The order is the order the tool printed. A repeat of one line is dropped,
/// so a tool that says the same thing twice is one finding.
#[must_use]
pub fn read(lens: &crate::Lens, output: &str) -> Vec<Finding> {
    let crate::Tooling::Command {
        run,
        reading: Some(reading),
        ..
    } = lens.tooling
    else {
        return Vec::new();
    };

    let mut taken: Vec<&str> = Vec::new();
    let mut found = Vec::new();
    for line in output.lines() {
        let Some(message) = (match reading {
            Reading::ErrorLines => error_message(line),
        }) else {
            continue;
        };
        if taken.contains(&message) {
            continue;
        }
        taken.push(message);
        found.push(lens_finding(lens.name, run, message));
        if found.len() == MAX_LENS_FINDINGS {
            break;
        }
    }
    found
}

/// What a tool said on a line it printed as an error, if it did.
///
/// `error: text` and `error[code]: text` both yield `text`. A line with the
/// word somewhere else in it — a path, a sentence — is not one, so the prefix
/// is what is matched.
fn error_message(line: &str) -> Option<&str> {
    let rest = line.trim().strip_prefix("error")?;
    let rest = match rest.strip_prefix('[') {
        Some(tail) => tail.split_once(']')?.1,
        None => rest,
    };
    let message = rest.strip_prefix(':')?.trim();
    (!message.is_empty()).then_some(message)
}

/// One defect a lens read out of its own tool's output.
fn lens_finding(lens: &str, run: &str, message: &str) -> Finding {
    Finding {
        title: format!("the `{lens}` lens reports: {message}"),
        body: format!(
            "The `{lens}` rung of the aperture ladder ran `{run}`. It printed this \
             line as an error:\n\n\
             ```\n{message}\n```\n\n\
             The loop read the line and did not judge it. That lens says every error \
             its tool prints is a defect, so the line is filed rather than weighed.\n\n\
             ## How to check\n\n\
             1. Run `{run}` at the root of this repository.\n\
             2. Find the error above in what it prints.\n\
             3. Fix the cause, or say on this issue why the tool is wrong here.\n\n\
             ## Definition of done\n\n\
             - [ ] `{run}` no longer prints this error, or this issue records why the \
             error is not a defect.\n"
        ),
        labels: vec![DEFECT_LABEL.to_owned()],
    }
}

/// The findings whose digest is absent from the seen set.
///
/// The key is [`crate::finding_digest`] over the title, which is the key the
/// filing path uses. This is what makes a re-pass safe: a repeat finding is
/// dropped before anything is sent.
#[must_use]
pub fn novel<'a>(findings: &'a [Finding], seen: &[String]) -> Vec<&'a Finding> {
    findings
        .iter()
        .filter(|finding| {
            let digest = crate::finding_digest(&finding.title);
            !seen.iter().any(|line| line.trim() == digest)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(title: &str) -> Finding {
        Finding {
            title: title.to_owned(),
            body: "body".to_owned(),
            labels: vec!["bug".to_owned()],
        }
    }

    /// **The witness.** Every supply is shut on a fresh policy, so a build
    /// that gains them changes no running loop.
    #[test]
    fn every_supply_is_shut_by_default() {
        let policy = SupplyPolicy::default();
        assert!(!policy.any_open());
        for supply in WorkSupply::ALL {
            assert!(
                !policy.opens(*supply),
                "{} is open by default",
                supply.as_str()
            );
        }
    }

    /// **The witness for the re-arm rule.** A ladder that went dry at one
    /// commit re-opens once the base has gained enough of them, and not
    /// before.
    #[test]
    fn a_dry_ladder_reopens_once_the_base_has_moved_far_enough() {
        let policy = SupplyPolicy {
            rearm: true,
            ..SupplyPolicy::default()
        };

        let just_short = Baseline {
            commits: Some(DEFAULT_REARM_COMMITS - 1),
            days: Some(0),
            noisy: false,
        };
        assert_eq!(
            rearm(crate::WATCH, &just_short, &policy),
            Rearm::Hold {
                reason: Hold::Unmoved
            }
        );

        let far_enough = Baseline {
            commits: Some(DEFAULT_REARM_COMMITS),
            days: Some(0),
            noisy: false,
        };
        assert_eq!(
            rearm(crate::WATCH, &far_enough, &policy),
            Rearm::Reopen { lens: "rubric" }
        );
    }

    /// Days are the other way in, for a tree that merges rarely.
    #[test]
    fn enough_days_reopen_the_ladder_on_their_own() {
        let policy = SupplyPolicy {
            rearm: true,
            ..SupplyPolicy::default()
        };
        let slow = Baseline {
            commits: Some(1),
            days: Some(DEFAULT_REARM_DAYS),
            noisy: false,
        };
        assert_eq!(
            rearm(crate::WATCH, &slow, &policy),
            Rearm::Reopen { lens: "rubric" }
        );
    }

    /// An open lens has a question left, so there is nothing to re-arm.
    #[test]
    fn an_open_lens_is_not_rearmed() {
        let policy = SupplyPolicy {
            rearm: true,
            ..SupplyPolicy::default()
        };
        let moved = Baseline {
            commits: Some(10_000),
            days: Some(10_000),
            noisy: false,
        };
        assert_eq!(
            rearm("rubric", &moved, &policy),
            Rearm::Hold {
                reason: Hold::LadderOpen
            }
        );
    }

    /// A loop that files far more than it finds is narrowed, not widened.
    #[test]
    fn a_noisy_loop_does_not_widen() {
        let policy = SupplyPolicy {
            rearm: true,
            ..SupplyPolicy::default()
        };
        let moved = Baseline {
            commits: Some(10_000),
            days: Some(10_000),
            noisy: true,
        };
        assert_eq!(
            rearm(crate::WATCH, &moved, &policy),
            Rearm::Hold {
                reason: Hold::Noisy
            }
        );
    }

    /// With nothing recorded, the loop cannot say the base moved, so it does
    /// not act as though it had.
    #[test]
    fn an_unknown_baseline_holds_the_ladder_shut() {
        let policy = SupplyPolicy {
            rearm: true,
            ..SupplyPolicy::default()
        };
        assert_eq!(
            rearm(crate::WATCH, &Baseline::default(), &policy),
            Rearm::Hold {
                reason: Hold::NoBaseline
            }
        );
    }

    /// **The witness for the sweep's yield.** A re-opened lens offers only
    /// what the seen set does not already hold.
    #[test]
    fn a_reopened_sweep_yields_only_digests_absent_from_the_seen_set() {
        let old = finding("the parser drops a trailing comma");
        let fresh = finding("the writer forgets the closing brace");
        let seen = vec![crate::finding_digest(&old.title)];

        // Bound rather than passed inline: `novel` borrows from the slice, so
        // a temporary array would be dropped before the assertion reads it.
        let offered = [old.clone(), fresh.clone()];
        let out = novel(&offered, &seen);

        assert_eq!(out, vec![&fresh]);
    }

    /// The same defect with a shifted line number is one finding, because the
    /// digest normalizes it. That is what keeps a re-pass cheap.
    #[test]
    fn a_shifted_line_number_is_the_same_finding() {
        let first = finding("parser.rs:120 drops a trailing comma");
        let again = finding("parser.rs:481 drops a trailing comma");
        let seen = vec![crate::finding_digest(&first.title)];

        assert!(novel(&[again], &seen).is_empty());
    }

    /// The lens the shipped table gives a reading to.
    fn mechanical() -> &'static crate::Lens {
        crate::LENSES
            .iter()
            .find(|lens| match lens.tooling {
                crate::Tooling::Command { reading, .. } => reading.is_some(),
                crate::Tooling::ModelOnly { .. } => false,
            })
            .expect("one lens declares a reading")
    }

    /// **The witness for the mechanical read.** A tool's error lines become
    /// findings, and its other lines do not.
    #[test]
    fn a_reading_turns_error_lines_into_findings() {
        let out = read(
            mechanical(),
            "checking 412 crates\n\
             error[vulnerability]: a crate has a known advisory\n\
             warning[unmaintained]: a crate is unmaintained\n\
             error: a crate carries a license this workspace refuses\n\
             advisories FAILED, bans ok\n",
        );

        assert_eq!(out.len(), 2, "two error lines, two findings");
        assert!(out[0].title.ends_with("a crate has a known advisory"));
        assert!(
            out[1]
                .title
                .ends_with("a crate carries a license this workspace refuses")
        );
        assert_eq!(out[0].labels, vec![DEFECT_LABEL.to_owned()]);
    }

    /// One line said twice is one defect, so a tool that repeats itself does
    /// not open two issues.
    #[test]
    fn a_repeated_line_is_one_finding() {
        let out = read(
            mechanical(),
            "error: the same thing\nerror: the same thing\n",
        );

        assert_eq!(out.len(), 1);
    }

    /// A broken tool cannot flood the tracker in one pass.
    #[test]
    fn a_pass_offers_no_more_than_the_cap() {
        let flood: String = (0..MAX_LENS_FINDINGS * 3)
            .map(|n| format!("error: defect number {n}\n"))
            .collect();

        assert_eq!(read(mechanical(), &flood).len(), MAX_LENS_FINDINGS);
    }

    /// A lens with no reading offers nothing, so the audit phase keeps it.
    #[test]
    fn a_lens_with_no_reading_offers_nothing() {
        let model_read = crate::lens("rubric").expect("the first rung");

        assert!(read(model_read, "error: something went wrong").is_empty());
    }

    /// The word has to be the prefix. A path or a sentence that holds it is
    /// not a tool reporting one.
    #[test]
    fn the_word_error_inside_a_line_is_not_a_finding() {
        let out = read(
            mechanical(),
            "compiling crates/stella-core/src/error.rs\n\
             an error is not what this line reports\n\
             error:\n",
        );

        assert!(out.is_empty(), "got {out:?}");
    }

    /// **The witness.** Every `Command` lens must name a command a shell can
    /// run, not just the ones that declare a [`Reading`]. Three `run` strings
    /// used to be prose instead — `rubric`'s "/ultraudit (deep) or /reaudit
    /// (fast)", `performance`'s "…, plus the prompt-cache golden fixtures
    /// (cargo test -p stella-model)", and `soak`'s "…(the long task list;
    /// rehearse with --rig --dry-run first)" — and were safe only because
    /// none of the three declared a reading, so
    /// `crates/stella-cli/src/self_driving_cmd/supply.rs`'s `Declared::sweep`
    /// never reached them. That was a tripwire on the declared half of the
    /// hazard, not a fix for it: nothing stopped a future reading, or a
    /// future caller that runs every `Command` lens regardless. This rule
    /// covers the whole `Tooling::Command` set, so it fails on the old three
    /// prose strings whether or not they ever gain a reading.
    #[test]
    fn every_command_lens_names_a_runnable_command() {
        for lens in crate::LENSES {
            let crate::Tooling::Command { run, .. } = lens.tooling else {
                continue;
            };
            for word in ["(", ")", " or "] {
                assert!(
                    !run.contains(word),
                    "the `{}` lens's `run` is `{run}`, and the driver runs it as \
                     written under `bash -lc` — `{word}` says that string is prose, \
                     not a command",
                    lens.name
                );
            }
        }
    }
}
