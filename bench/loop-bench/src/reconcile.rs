//! Requested tasks vs reported trials — the denominator check (#1299).
//!
//! Harbor takes `-i <name>` filters and errors only when *nothing* matched, so
//! a run that asked for eight tasks and matched six produces six healthy rows
//! and a clean exit. Six of eight solved reads as 75% when it is 60%, and
//! nothing on the page says which number you are looking at. A wrong figure in
//! the flattering direction with no marker on it is worse than no figure.
//!
//! #611 closed half of this: a requested task with no trial directory at all
//! becomes a `NOT-RUN` row that counts in the denominator and gates red. Three
//! ways past that check remained, and this module closes them:
//!
//! * **Missing *trials*, not tasks.** With `--trials 5`, a task that produced
//!   two trial directories is present, healthy, and quietly measured over two
//!   fifths of the sample it claimed.
//! * **Surplus trials.** Harbor writes into `<jobs-dir>/<job-name>/`
//!   verbatim. Running twice under the same job name leaves both runs' trial
//!   directories side by side, and the second run scores itself over the union
//!   — yesterday's binary folded into today's gate, invisibly.
//! * **A name the directory cannot hold.** Harbor names a trial directory
//!   `<task_name[:32] with trailing _- stripped>__<shortuuid>`
//!   (`TrialConfig.generate_trial_name`). A requested task longer than 32
//!   characters therefore never equals its own directory prefix: every one of
//!   its trials was skipped as belonging to some other run, and the task it
//!   really ran was reported `NOT-RUN`. Two failures from one truncation, both
//!   pointing away from the cause.
//!
//! The rule throughout: reconciliation is *reported*, never *applied*. Nothing
//! here adjusts a count to make it consistent — it says what was asked for,
//! what came back, and where the two disagree.

use std::collections::BTreeMap;

/// Harbor truncates a task name to this many characters when naming a trial
/// directory (`TrialConfig.generate_trial_name`).
const TRIAL_DIR_TASK_LIMIT: usize = 32;

/// What this run asked harbor for — the denominator every figure is taken
/// over, carried separately from the results so the two can be compared.
#[derive(Debug, Clone, Copy)]
pub struct Requested<'a> {
    /// The resolved task list, exactly as passed to harbor's `-i`.
    pub tasks: &'a [String],
    /// Trial directories expected per task (harbor's `-k`).
    pub trials_per_task: usize,
}

impl<'a> Requested<'a> {
    /// Which requested task a trial directory belongs to, or `None` when it
    /// belongs to none of them.
    ///
    /// `task_name` is harbor's own record off `result.json` and is preferred
    /// whenever it is present: the directory name is a *truncation* of it, and
    /// only the untruncated form can tell two 40-character task names with the
    /// same first 32 apart. The prefix comparison is the fallback for a trial
    /// whose result file never landed — which is precisely the crashed trial,
    /// the one it would be worst to lose.
    ///
    /// Returns the match plus whether the prefix was ambiguous, so the caller
    /// can record a guess as a guess.
    #[must_use]
    pub fn match_dir(&self, dir_prefix: &str, task_name: Option<&str>) -> (Option<&'a str>, bool) {
        if let Some(name) = task_name
            && let Some(task) = self.tasks.iter().find(|t| t.as_str() == name)
        {
            return (Some(task.as_str()), false);
        }
        let matches: Vec<&str> = self
            .tasks
            .iter()
            .filter(|t| trial_dir_prefix(t) == dir_prefix)
            .map(String::as_str)
            .collect();
        (matches.first().copied(), matches.len() > 1)
    }

    /// A fresh, all-zero reconciliation over this request — every requested
    /// task present with a count of nothing, so a task that never appears is
    /// still in the table rather than absent from it.
    #[must_use]
    pub fn reconciliation(&self) -> Reconciliation {
        Reconciliation {
            trials_per_task: self.trials_per_task,
            observed: self.tasks.iter().map(|t| (t.clone(), 0)).collect(),
            unrequested: BTreeMap::new(),
            ambiguous: Vec::new(),
        }
    }
}

/// The directory-name prefix harbor will give a task's trials.
#[must_use]
pub fn trial_dir_prefix(task: &str) -> String {
    task.chars()
        .take(TRIAL_DIR_TASK_LIMIT)
        .collect::<String>()
        .trim_end_matches(['_', '-'])
        .to_string()
}

/// Requested vs reported, per task.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Reconciliation {
    /// Trial directories expected per requested task.
    pub trials_per_task: usize,
    /// Requested task → trial directories found for it. Every requested task
    /// is a key, including the ones with a count of zero.
    pub observed: BTreeMap<String, usize>,
    /// Trial-directory prefix → directories skipped because no requested task
    /// claims them. Skipping is right — a previous run's tasks must not
    /// contaminate this run's gate — but doing it silently means a mistyped
    /// `--tasks` looks like a run where harbor launched nothing.
    pub unrequested: BTreeMap<String, usize>,
    /// Directory prefixes that matched more than one requested task and could
    /// not be resolved by harbor's own record. The attribution below is a
    /// guess, and is labelled as one.
    pub ambiguous: Vec<String>,
}

impl Reconciliation {
    /// Record one trial directory against the task it was attributed to.
    pub fn saw(&mut self, task: &str, ambiguous_prefix: Option<&str>) {
        *self.observed.entry(task.to_string()).or_default() += 1;
        if let Some(prefix) = ambiguous_prefix
            && !self.ambiguous.iter().any(|p| p == prefix)
        {
            self.ambiguous.push(prefix.to_string());
        }
    }

    /// Record one trial directory that no requested task claims.
    pub fn skipped(&mut self, dir_prefix: &str) {
        *self.unrequested.entry(dir_prefix.to_string()).or_default() += 1;
    }

    /// Requested trials that never produced a directory, per task. These
    /// become `NOT-RUN` rows: the denominator stays the size that was asked
    /// for, and the gate goes red.
    #[must_use]
    pub fn missing(&self) -> Vec<(&str, usize)> {
        self.observed
            .iter()
            .filter_map(|(task, seen)| {
                self.trials_per_task
                    .checked_sub(*seen)
                    .filter(|short| *short > 0)
                    .map(|short| (task.as_str(), short))
            })
            .collect()
    }

    /// Trial directories beyond what this run asked for, per task — a second
    /// run's results sitting in the first run's job directory.
    #[must_use]
    pub fn surplus(&self) -> Vec<(&str, usize)> {
        self.observed
            .iter()
            .filter_map(|(task, seen)| {
                seen.checked_sub(self.trials_per_task)
                    .filter(|extra| *extra > 0)
                    .map(|extra| (task.as_str(), extra))
            })
            .collect()
    }

    /// The report covers exactly the trials that were asked for, and each is
    /// attributed to a task with certainty.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.missing().is_empty() && self.surplus().is_empty() && self.ambiguous.is_empty()
    }

    /// The denominator cannot be trusted: results this run did not ask for are
    /// folded into it, or a directory could only be attributed by guessing.
    ///
    /// Deliberately *not* the same predicate as [`is_clean`](Self::is_clean).
    /// A missing trial is already reported — it becomes a `NOT-RUN` row that
    /// gates red and is visible on the table. Surplus has no row to become:
    /// the extra trials look exactly like legitimate results, so this is the
    /// only place a reader would ever learn of them.
    #[must_use]
    pub fn contaminated(&self) -> bool {
        !self.surplus().is_empty() || !self.ambiguous.is_empty()
    }

    /// Every disagreement between what was asked for and what came back, as
    /// lines for an operator. Empty when the two agree exactly.
    #[must_use]
    pub fn findings(&self) -> Vec<String> {
        let mut findings = Vec::new();
        let expected = self.trials_per_task;
        for (task, short) in self.missing() {
            let seen = self.observed.get(task).copied().unwrap_or(0);
            findings.push(format!(
                "{task}: {seen}/{expected} trial(s) reported — {short} missing, \
                 counted as NOT-RUN so the denominator stays {expected}"
            ));
        }
        for (task, extra) in self.surplus() {
            let seen = self.observed.get(task).copied().unwrap_or(0);
            findings.push(format!(
                "{task}: {seen} trial dir(s) for {expected} requested — {extra} extra. \
                 An earlier run's results are in this job directory; use a fresh \
                 --job-name or delete it, or this run is scored over both"
            ));
        }
        for prefix in &self.ambiguous {
            findings.push(format!(
                "`{prefix}…`: more than one requested task truncates to this directory \
                 name and no result.json disambiguated it — its trials are attributed \
                 to the first match, which may be the wrong task"
            ));
        }
        for (prefix, count) in &self.unrequested {
            findings.push(format!(
                "`{prefix}`: {count} trial dir(s) excluded — no requested task matches. \
                 They contribute to nothing here; check --tasks if that is a surprise"
            ));
        }
        findings
    }
}

/// The task name to show for a trial directory the requested set does not
/// claim: harbor's own record when it landed, else the truncated prefix.
#[must_use]
pub fn reported_task_name(dir_prefix: &str, task_name: Option<&str>) -> String {
    task_name.unwrap_or(dir_prefix).to_string()
}
