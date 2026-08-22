//! `deliver` — branch, pull request, CI, merge.
//!
//! `doc:backlog-self-driving` §3.3 (#3599 B3). The pure state machine is
//! [`stella_autonomy::deliver_next`]; this is the I/O half that produces the
//! observation it decides over, and performs the action it returns.
//!
//! The split is the whole design. **Deciding buys no model call** — it is
//! arithmetic over observed facts — so everything judgemental lives in the pure
//! machine where a test can pin it, and everything here is reading a forge and
//! running `git`.
//!
//! # `base_ci` is a second read, and it is the point
//!
//! The machine separates [`CiRed`](stella_autonomy::PrState::CiRed) from
//! [`BaseBroken`](stella_autonomy::PrState::BaseBroken)
//! because a failure that reproduces on the base branch is not this pull
//! request's failure, and treating it as one burns every cycle on somebody
//! else's breakage. That distinction is only as good as the observation feeding
//! it, so this module does not ask a blanket "is `main` red".
//!
//! **It compares the same checks by name.** For every check failing on the pull
//! request, it asks what the check *of that name* concluded on the base branch's
//! head. The base counts as broken only when the failures actually overlap —
//! `base_conclusion` is the witness. A blanket read would call a PR's genuine
//! regression "inherited" whenever anything unrelated was red on `main`, which
//! is the expensive direction to be wrong in: the loop would wait forever for
//! someone else to fix a break that was its own.
//!
//! # Closing the issue takes both spellings
//!
//! `deliver open` writes `Closes #N` into the pull request body **and** as a
//! commit trailer. Both are required and AGENTS.md § *Closing the issue on
//! merge* says why: a squash merge composes the commit message from
//! `COMMIT_MESSAGES` and never from the PR body, so a `Closes` that exists only
//! in the description never reaches the commit — and a rebase merge replays the
//! commits verbatim, so the PR body is likewise never consulted. Either alone is
//! a silent single point of failure whose failure mode is invisible until
//! someone audits the backlog.

use stella_autonomy::{CiConclusion, Contention, Mergeability, Observation, ReviewState};

use super::state::git;

/// One check as the forge reports it, reduced to what the mapping reads.
///
/// # There are two kinds of check and they do not share a spelling
///
/// A pull request's rollup mixes **check runs** (GitHub Actions: `name`,
/// `status`, `conclusion`) with **commit statuses** (the older API that
/// third-party services still post to: `context`, `state`, and no `status`
/// field at all). Deserializing only the first shape does not fail — every
/// field is `#[serde(default)]` — it silently produces a check with no name
/// and no conclusion.
///
/// Which reads as *pending forever*: `status` is not `COMPLETED`, so
/// [`Self::pending`] is true on every poll, for a check that concluded before
/// the loop ever looked. That is what it did. This repository carries a Vercel
/// commit status that has been failing on every pull request, and the loop sat
/// on #4022 re-reading `ci=Pending` for twenty-five minutes after all three
/// required checks had gone green.
///
/// The empty `name` is the same bug's other half: [`base_conclusion`] joins by
/// name, and every commit status would have joined against every other.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(super) struct Check {
    /// A check run's name.
    #[serde(default)]
    pub name: String,
    /// A commit status's name. GitHub calls the same thing `context` here.
    #[serde(default)]
    pub context: String,
    /// A check run's outcome: `SUCCESS`, `FAILURE`, `SKIPPED`, `""` while
    /// running.
    #[serde(default)]
    pub conclusion: String,
    /// A commit status's outcome: `SUCCESS`, `FAILURE`, `ERROR`, `PENDING`,
    /// `EXPECTED`.
    #[serde(default)]
    pub state: String,
    /// A check run's progress: `COMPLETED`, `IN_PROGRESS`, `QUEUED`. **Absent
    /// on a commit status**, which is how the two are told apart.
    #[serde(default)]
    pub status: String,
}

impl Check {
    /// The join key, whichever shape reported it.
    pub(super) fn name(&self) -> &str {
        if self.name.is_empty() {
            &self.context
        } else {
            &self.name
        }
    }

    /// The outcome, whichever shape reported it.
    fn outcome(&self) -> String {
        let raw = if self.conclusion.is_empty() {
            &self.state
        } else {
            &self.conclusion
        };
        raw.trim().to_ascii_uppercase()
    }

    /// Whether this is a commit status rather than a check run.
    ///
    /// The absence of `status` is the discriminator, because it is the one
    /// field the older API has no equivalent for.
    fn is_commit_status(&self) -> bool {
        self.status.trim().is_empty()
    }

    /// Whether this check has concluded and concluded badly.
    ///
    /// `ERROR` is a commit status's way of saying the service itself broke,
    /// which is a failure for every purpose this loop has.
    fn failed(&self) -> bool {
        matches!(
            self.outcome().as_str(),
            "FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED"
        )
    }

    /// Whether it has not finished.
    fn pending(&self) -> bool {
        if self.is_commit_status() {
            // No progress field exists, so the outcome is the whole story.
            return matches!(self.outcome().as_str(), "PENDING" | "EXPECTED" | "");
        }
        !self.status.eq_ignore_ascii_case("COMPLETED")
            // A completed check with no conclusion is a forge quirk, not a
            // pass: treated as still-unknown rather than green, on the same
            // reasoning as `Mergeability::Unknown`.
            || self.outcome().is_empty()
    }

    /// Whether it should be ignored entirely.
    ///
    /// A skipped or neutral check is not a failure and not a pass — it did not
    /// run. Counting it as pending would stall the loop forever on a job that
    /// is skipped by design (this repository skips several on a docs-only
    /// diff).
    fn inert(&self) -> bool {
        matches!(self.outcome().as_str(), "SKIPPED" | "NEUTRAL" | "CANCELLED")
    }
}

/// Reduce a rollup to the one conclusion the machine branches on.
///
/// Red wins over pending: a build that has already failed does not become
/// undecided because something else is still running, and waiting for the rest
/// before saying so only delays the fix.
#[must_use]
pub(super) fn ci_from(checks: &[Check]) -> CiConclusion {
    let live: Vec<&Check> = checks.iter().filter(|c| !c.inert()).collect();
    if live.is_empty() {
        return CiConclusion::Pending;
    }
    if live.iter().any(|c| c.failed()) {
        return CiConclusion::Red;
    }
    if live.iter().any(|c| c.pending()) {
        return CiConclusion::Pending;
    }
    CiConclusion::Green
}

/// What the checks failing on this pull request concluded on the base branch.
///
/// The join is **by name**, and the answer is [`CiConclusion::Red`] only when a
/// check failing here is also failing there. Anything else is
/// [`CiConclusion::Green`] — meaning "the base does not excuse this failure" —
/// because that is the reading the machine needs: it is asking *is this my
/// fault*, not *is the base perfect*.
#[must_use]
pub(super) fn base_conclusion(pr: &[Check], base: &[Check]) -> CiConclusion {
    let failing_here: Vec<&str> = pr.iter().filter(|c| c.failed()).map(Check::name).collect();

    if failing_here.is_empty() {
        // Nothing failed here, so there is nothing for the base to excuse.
        return CiConclusion::Green;
    }

    let failing_there = |name: &str| base.iter().any(|c| c.name() == name && c.failed());

    if failing_here.iter().all(|name| failing_there(name)) {
        CiConclusion::Red
    } else {
        CiConclusion::Green
    }
}

/// Map the forge's mergeability string.
///
/// `UNKNOWN` and anything unrecognised become [`Mergeability::Unknown`] rather
/// than `Clean`: GitHub reports the field before it has computed it, and
/// reading that as clean is how a merge is attempted into a conflict.
#[must_use]
pub(super) fn mergeable_from(raw: &str) -> Mergeability {
    match raw.to_ascii_uppercase().as_str() {
        "MERGEABLE" => Mergeability::Clean,
        "CONFLICTING" => Mergeability::Conflicted,
        _ => Mergeability::Unknown,
    }
}

/// Map the forge's review decision.
///
/// An empty string is "nobody has reviewed", which is what GitHub returns when
/// a repository does not require review at all.
#[must_use]
pub(super) fn review_from(raw: &str) -> ReviewState {
    match raw.to_ascii_uppercase().as_str() {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        _ => ReviewState::None,
    }
}

/// The payload `gh pr view` returns, in the shape this module reads.
///
/// The forge's camelCase spellings are confined here by `rename`, the same way
/// `issue_provider.rs` confines GitHub's issue field names: this struct is the
/// only place in the tree that has to know how one forge spells things.
#[derive(Debug, Clone, serde::Deserialize)]
pub(super) struct PrView {
    #[serde(default, rename = "isDraft")]
    pub is_draft: bool,
    #[serde(default)]
    pub mergeable: String,
    #[serde(default, rename = "reviewDecision")]
    pub review_decision: String,
    #[serde(default, rename = "statusCheckRollup")]
    pub status_check_rollup: Vec<Check>,
    /// The branch this pull request targets, so the base is read from the
    /// branch it actually merges into rather than an assumed `main`.
    #[serde(default, rename = "baseRefName")]
    pub base_ref_name: String,
    /// The pull request's labels, which is where local verification is
    /// recorded so it survives a restart.
    #[serde(default)]
    pub labels: Vec<stella_protocol::IssueLabel>,
    /// `OPEN`, `MERGED`, `CLOSED`.
    ///
    /// Read because a pull request that has already reached its destination
    /// needs no further transition, and the observation the pure machine
    /// decides over cannot express that: a merged pull request reports
    /// `mergeable: UNKNOWN`, which is `Wait` — forever.
    #[serde(default)]
    pub state: String,
}

/// Assemble the observation the pure machine decides over.
#[must_use]
pub(super) fn observation_from(view: &PrView, base_checks: &[Check]) -> Observation {
    Observation {
        ci: ci_from(&view.status_check_rollup),
        base_ci: base_conclusion(&view.status_check_rollup, base_checks),
        mergeable: mergeable_from(&view.mergeable),
        review: review_from(&view.review_decision),
        draft: view.is_draft,
    }
}

/// Compose a pull request body that closes its issue, signed.
///
/// See the module docs on why the trailer is not enough on its own, and
/// `stella_autonomy::sign` on why the signature sits exactly one line break
/// after the last character.
#[must_use]
pub(super) fn pr_body(issue_key: &str, summary: &str, signature: &str) -> String {
    stella_autonomy::sign(&format!("{summary}\n\nCloses #{issue_key}"), signature)
}

/// The commit trailer that closes the issue on the *other* merge path.
///
/// Lives here beside [`pr_body`] because the two are one decision — a squash
/// merge reads only the commit, a rebase merge reads only the commits, and the
/// PR body closes an issue through neither. But it is **applied** by
/// [`super::work`]: the commit is authored by the turn, in the worktree, and
/// asking `deliver` to amend somebody else's commit afterwards would be a
/// rewrite in a place that has no business rewriting.
#[must_use]
pub(super) fn commit_trailer(issue_key: &str) -> String {
    format!("Closes #{issue_key}")
}

/// Re-decide a red verdict against the base as it stands now.
///
/// The one remedy the loop has for a red it did not cause and cannot see. A
/// check that ran against a base which has since been repaired keeps its
/// failing verdict forever, and `base_conclusion` compares what is failing
/// *now* — so once the base goes green the pull request is left holding a
/// failure that reproduces nowhere.
///
/// # Re-running alone does not do it, and that is the whole subtlety
///
/// A `pull_request` run is anchored to the merge commit computed when the run
/// was **created**. `gh run rerun` replays that same commit, stale base and
/// all. Measured here rather than assumed: #4022's failed job was re-run at
/// 23:28, twelve minutes after #4015 repaired `main`, and reproduced the
/// identical `cargo fmt` diff at `stella-autonomy/src/lib.rs:824` that only
/// ever existed on the old base.
///
/// So the base is merged in first — a new head commit, and a fresh run that
/// sees the repair. Re-running is kept as the fallback for the case
/// `update-branch` declines, which is a branch that is *already* current: then
/// there is no staleness to clear and a red that re-runs green was a flake.
///
/// Either way the caller bounds this to once per pull request, so a genuine
/// failure reaches the fix path on the next poll.
pub(super) fn refresh_against_base(pr: &str) -> Result<(), String> {
    // Ahead of the fallback because it is the one that can actually change the
    // answer. A branch already level with the base refuses here, which is
    // exactly when re-running is the right move instead.
    if gh(&["pr", "update-branch", pr]).is_ok() {
        return Ok(());
    }
    rerun_failed(pr)
}

/// Ask the forge to run this pull request's failed checks again.
///
/// The fallback half of [`refresh_against_base`] — correct for a flake, and
/// unable to clear a stale base. See that function for why the distinction
/// matters.
fn rerun_failed(pr: &str) -> Result<(), String> {
    // `gh pr checks --json` names the run each check belongs to only through
    // its URL, so the run id is recovered from there. A pull request whose
    // checks all pass yields nothing to re-run, which is not an error.
    //
    // `gh pr checks` exits non-zero whenever the checks are not all green —
    // exit 1 on a failure, exit 8 while any are still pending — and does so
    // *while still writing the requested JSON to stdout*. The plain `gh`
    // helper reads that as an error and throws the payload away, so it cannot
    // be used here: this fallback only ever runs on a red pull request, i.e.
    // exactly when the exit code is non-zero. Read stdout regardless of exit
    // status and let the JSON parse below be the arbiter instead.
    let raw = gh_stdout(&["pr", "checks", pr, "--json", "state,link"])?;

    #[derive(serde::Deserialize)]
    struct Row {
        #[serde(default)]
        state: String,
        #[serde(default)]
        link: String,
    }

    let rows: Vec<Row> = serde_json::from_str(&raw).map_err(|error| {
        format!("`gh pr checks` returned a payload this build cannot read: {error}")
    })?;

    let mut runs: Vec<String> = rows
        .iter()
        .filter(|row| row.state.eq_ignore_ascii_case("FAILURE"))
        .filter_map(|row| run_id_from_link(&row.link))
        .collect();
    runs.sort();
    runs.dedup();

    if runs.is_empty() {
        return Err("no failed check named a workflow run to re-run".to_owned());
    }

    for run in &runs {
        gh(&["run", "rerun", run, "--failed"])?;
    }
    Ok(())
}

/// The workflow-run id inside a check's link, if it has one.
///
/// A check link looks like `…/actions/runs/<run>/job/<job>`. Anything else —
/// a third-party check pointing at its own dashboard, an empty link — yields
/// `None` rather than a guess, because re-running the wrong id is worse than
/// re-running nothing.
#[must_use]
fn run_id_from_link(link: &str) -> Option<String> {
    let after = link.split("/actions/runs/").nth(1)?;
    let id = after.split('/').next()?;
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
        Some(id.to_owned())
    } else {
        None
    }
}

/// The checks this repository actually requires to merge into `branch`.
///
/// # Why "CI is red" cannot mean "any check is red"
///
/// A repository's rollup contains checks it requires and checks it merely
/// runs. This one carries a Vercel commit status that has been failing on
/// every pull request for as long as the account has been blocked — it is
/// advisory, the forge merges past it, and a human ignores it. A loop that
/// treats it as blocking never merges anything again, and cannot be argued
/// out of it: the failure is real, it is red, and it will never go green.
///
/// GitHub agrees, and says so in its own vocabulary: #4022 reported
/// `mergeStateStatus: UNSTABLE, mergeable: MERGEABLE` — advisory checks
/// failing, merge permitted. Reading branch protection is how the loop learns
/// the same thing without hardcoding which service to ignore.
///
/// An unreadable protection document (no permission, no protection
/// configured) yields an empty list, and an empty list means **no filtering**:
/// every check counts, which is what the loop did before it could ask. That is
/// the conservative direction — it can refuse to merge something mergeable,
/// but it can never merge something the repository would have blocked.
fn required_contexts(branch: &str) -> Vec<String> {
    let out = std::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{{owner}}/{{repo}}/branches/{branch}/protection"),
            "--jq",
            ".required_status_checks.contexts[]?",
        ])
        .env("NO_COLOR", "1")
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Keep only the checks that can block a merge.
///
/// An empty `required` list leaves the rollup untouched — see
/// [`required_contexts`] for why that is the safe reading rather than "nothing
/// is required".
#[must_use]
fn only_required(checks: Vec<Check>, required: &[String]) -> Vec<Check> {
    if required.is_empty() {
        return checks;
    }
    checks
        .into_iter()
        .filter(|check| required.iter().any(|name| name == check.name()))
        .collect()
}

/// The forge endpoint carrying a ref's check runs.
///
/// Split out so the one thing that can be wrong here — *which commit gets
/// asked about* — is visible to a test. See [`checks_for_branch`].
#[must_use]
fn check_runs_endpoint(branch: &str) -> String {
    format!("repos/{{owner}}/{{repo}}/commits/{branch}/check-runs")
}

/// Read a branch's checks from the forge.
///
/// # The branch is named to the forge, never resolved locally first
///
/// This used to run `git rev-parse origin/<branch>` and ask about the commit
/// that came back. A remote-tracking ref is only as fresh as the last fetch,
/// and this loop does not fetch between polls — so once the process had been
/// up for a while, `origin/main` still pointed at whatever main was when it
/// started.
///
/// That is not a stale-data annoyance; it is a loss of work, and in the one
/// direction that costs. [`base_conclusion`] excuses a pull request only when
/// **every** check failing on it also fails on the base. Reading a
/// pre-breakage base makes the base look green, so an inherited failure is
/// scored as the pull request's own and a change that was never wrong gets a
/// fix attempt or an escalation. It happened on the first pull request this
/// loop ever opened: #4014 failed on a `cargo fmt` diff in a file its own diff
/// never touched, and was escalated for it.
///
/// Naming the branch to the forge has no local state left to be stale. It is
/// the same rule [`open_prs_for_prefix`] follows: for a question about the
/// forge, ask the forge.
pub(super) fn checks_for_branch(branch: &str) -> Vec<Check> {
    // `gh pr view` is not available for a branch with no pull request, so the
    // base is read through the ref's check-runs instead. A base with no checks
    // at all yields an empty list, which `base_conclusion` reads as "does not
    // excuse anything" — which blames the pull request, so it is only correct
    // while it means "the base really has no checks".
    if branch.is_empty() {
        return Vec::new();
    }
    let mut checks = read_jq(
        &check_runs_endpoint(branch),
        ".check_runs[] | {name: .name, conclusion: (.conclusion // \"\"), status: .status}",
    );

    // Commit statuses live behind a different endpoint and speak a different
    // dialect. Read separately and appended, because a pull request's rollup
    // contains both — and a base showing only half of it would fail to excuse
    // exactly the checks most likely to be broken repository-wide, since a
    // third-party service is what posts a commit status.
    checks.extend(read_jq(
        &statuses_endpoint(branch),
        ".statuses[] | {context: .context, state: .state}",
    ));
    checks
}

/// The forge endpoint carrying a ref's commit statuses.
///
/// A second endpoint rather than a second field: GitHub never merged the two
/// APIs, and `check-runs` genuinely does not contain a commit status.
#[must_use]
fn statuses_endpoint(branch: &str) -> String {
    format!("repos/{{owner}}/{{repo}}/commits/{branch}/status")
}

/// Run one `gh api` read and parse a [`Check`] per line.
///
/// An unreachable forge yields no checks, which `base_conclusion` reads as
/// "excuses nothing" — the direction that blames the pull request, and so the
/// one that must never be reached by accident. It is reached only when `gh`
/// itself cannot run.
fn read_jq(endpoint: &str, jq: &str) -> Vec<Check> {
    let out = std::process::Command::new("gh")
        .args(["api", endpoint, "--paginate", "--jq", jq])
        .env("NO_COLOR", "1")
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Check>(line).ok())
        .collect()
}

/// Run `gh` and return stdout, with colour forced off.
fn gh(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("gh")
        .args(args)
        .env("NO_COLOR", "1")
        .env("CLICOLOR_FORCE", "0")
        .output()
        .map_err(|error| format!("could not run `gh`: {error} — is the GitHub CLI installed?"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Run `gh` and return stdout even when the command exits non-zero.
///
/// Some `gh` subcommands report their result through the exit code while still
/// emitting the requested payload on stdout — `gh pr checks` exits 1 on a
/// failing check and 8 on a pending one, yet writes its `--json` output either
/// way. For those, a non-zero exit is a verdict about the checks, not a
/// failure of the command, so the payload must survive it. stderr is only
/// surfaced when stdout came back empty, which is the one case that signals
/// the command itself could not run.
fn gh_stdout(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("gh")
        .args(args)
        .env("NO_COLOR", "1")
        .env("CLICOLOR_FORCE", "0")
        .output()
        .map_err(|error| format!("could not run `gh`: {error} — is the GitHub CLI installed?"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if stdout.is_empty() && !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(stdout)
}

/// Push `branch` and open a draft pull request that closes `issue_key`.
///
/// Draft on purpose: the machine takes it out of draft itself once CI is green
/// ([`stella_autonomy::Action::MarkReady`]), so a pull request that never goes
/// green never asks a human to look at it.
pub(super) fn open(
    root: &std::path::Path,
    branch: &str,
    issue_key: &str,
    title: &str,
    signature: &str,
) -> Result<String, String> {
    git(root, &["push", "-u", "origin", branch])
        .ok_or_else(|| format!("could not push `{branch}` — is the remote reachable?"))?;

    let body = pr_body(
        issue_key,
        &format!("Autonomous fix for #{issue_key}."),
        signature,
    );
    let url = gh(&[
        "pr", "create", "--head", branch, "--title", title, "--body", &body, "--draft",
    ])?;

    url.rsplit('/')
        .next()
        .filter(|s| s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("`gh pr create` printed no pull request number: {url:?}"))
}

/// What one read of the forge said about a pull request.
///
/// `settled` is carried beside the observation rather than inside it because
/// it is not something the pure machine decides over — it is the question of
/// whether there is anything left to decide. A merged pull request reports
/// `mergeable: UNKNOWN`, which the machine correctly reads as `Wait`; without
/// this flag the loop waits on it for the rest of the run. It did, on #4022,
/// after merging it successfully.
#[derive(Debug, Clone)]
pub(super) struct Reading {
    /// What the pure machine decides over.
    pub observation: Observation,
    /// Whether the pull request has already reached a terminal state.
    pub settled: bool,
    /// Required checks this policy declined to enforce, each with its grounds.
    ///
    /// Carried so the caller can say it out loud. A loop that merges past a
    /// required check without naming which one, and why, is indistinguishable
    /// from one that is simply broken.
    pub waived: Vec<String>,
    /// Whether this change was proved on this machine.
    pub verified_locally: bool,
}

/// One read of the forge, plus the second read of the base its
/// [`Observation::base_ci`] needs.
pub(super) fn observe(
    pr: &str,
    policy: &stella_autonomy::BlockingPolicy,
) -> Result<Reading, String> {
    let raw = gh(&[
        "pr",
        "view",
        pr,
        "--json",
        "isDraft,mergeable,reviewDecision,statusCheckRollup,baseRefName,state,labels",
    ])?;
    let view: PrView = serde_json::from_str(&raw).map_err(|error| {
        format!("`gh pr view` returned a payload this build cannot read: {error}")
    })?;

    // The forge's own name for the base, passed through untouched. Prefixing
    // it with `origin/` would name a remote-tracking ref this process no
    // longer resolves — see `checks_for_branch`.
    let required = required_contexts(&view.base_ref_name);

    // Required is necessary and not sufficient. A check that has been red on
    // the base for every recent commit is not something this pull request can
    // fix — see `stella_autonomy::gate`.
    let stuck = if required.is_empty() {
        std::collections::BTreeSet::new()
    } else {
        stella_autonomy::stuck_on_base(
            &base_failure_history(&view.base_ref_name, policy.stuck_after.max(1)),
            policy.stuck_after,
        )
    };
    let blocking = stella_autonomy::blocking(&required, &stuck, policy);
    let waived = stella_autonomy::waived(&required, &stuck, policy);

    let base_checks = only_required(checks_for_branch(&view.base_ref_name), &blocking);

    // Both sides filtered by the same list, so the base can still excuse a
    // blocking check — and a waived one can no longer condemn.
    let mut view = view;
    let verified_locally = view
        .labels
        .iter()
        .any(|label| label.name == VERIFIED_LOCALLY_LABEL);
    view.status_check_rollup = only_required(view.status_check_rollup, &blocking);

    let settled = matches!(
        view.state.to_ascii_uppercase().as_str(),
        "MERGED" | "CLOSED"
    );
    let mut observation = observation_from(&view, &base_checks);

    // Nothing is left that can gate this pull request remotely, for one of two
    // reasons, and the loop treats them the same because the forge does:
    //
    // - every check the repository *requires* has been waived as unwinnable; or
    // - the repository declares no required checks at all. A private repository
    //   on a free plan cannot even expose branch protection — the API answers
    //   403 — and `gh pr merge` will merge such a pull request whatever its
    //   checks say. A gate the forge does not enforce is not a gate.
    //
    // Either way the forge has no opinion to offer, so `ci_from` sees an empty
    // rollup and says `Pending`, parking the loop forever on a verdict that is
    // never coming.
    //
    // Such a repository still has a test suite. The label says whether this
    // change survived it on this machine, and that becomes the verdict —
    // weaker evidence than a clean CI run, and incomparably stronger than
    // waiting for a suspended account to be reinstated.
    if blocking.is_empty() {
        observation.ci = if verified_locally {
            CiConclusion::Green
        } else {
            CiConclusion::Red
        };
        observation.base_ci = CiConclusion::Green;
    }

    Ok(Reading {
        observation,
        settled,
        waived,
        verified_locally,
    })
}

/// The label that records a change proved on this machine.
///
/// On the pull request rather than in the process, so it survives a restart
/// and so a human can see exactly which pull requests were merged on local
/// evidence rather than on a clean CI run.
pub(super) const VERIFIED_LOCALLY_LABEL: &str = "stella-verified-locally";

/// Record that a pull request was proved on this machine.
pub(super) fn mark_verified_locally(pr: &str) -> Result<(), String> {
    gh(&["pr", "edit", pr, "--add-label", VERIFIED_LOCALLY_LABEL]).map(|_| ())
}

/// Which checks failed on each of the last `depth` commits of `branch`.
///
/// Newest first, one entry per commit. Feeds
/// [`stella_autonomy::stuck_on_base`], whose whole question is whether a check
/// has *ever* been green recently — so a commit that could not be read yields
/// an empty entry, which reads as "nothing failed here" and therefore breaks
/// a stuck streak. That is the conservative direction: an unreadable history
/// must not manufacture evidence that a check is unwinnable.
#[must_use]
pub(super) fn base_failure_history(branch: &str, depth: usize) -> Vec<Vec<String>> {
    let raw = gh(&[
        "api",
        &format!("repos/{{owner}}/{{repo}}/commits?sha={branch}&per_page={depth}"),
        "--jq",
        ".[].sha",
    ])
    .unwrap_or_default();

    raw.lines()
        .map(str::trim)
        .filter(|sha| !sha.is_empty())
        .take(depth)
        .map(|sha| {
            checks_for_branch(sha)
                .into_iter()
                .filter(Check::failed)
                .map(|check| check.name().to_owned())
                .collect()
        })
        .collect()
}

/// Is the base branch itself broken right now?
///
/// The loop's own pull requests are judged against the base by
/// [`base_conclusion`], which asks a narrower question: *does the base excuse
/// **this** failure*. This asks the blunt one — is `main` red at all — because
/// a broken base is not a property of any one pull request. It blocks
/// everybody, and the loop is supposed to go and fix it rather than open work
/// that will fail for a reason nobody's diff caused.
///
/// Only **required** checks count, on exactly the reasoning `only_required`
/// documents: an advisory check that fails forever would otherwise convince
/// the loop the repository is permanently on fire.
///
/// A base that cannot be read is reported healthy. That is the conservative
/// direction here and the opposite of the one `base_conclusion` takes, and
/// deliberately: an unreadable forge would otherwise make the loop file a
/// breakage report about nothing and adopt an imaginary emergency.
#[must_use]
pub(super) fn base_is_broken(branch: &str) -> bool {
    let required = required_contexts(branch);
    let checks = only_required(checks_for_branch(branch), &required);
    if checks.is_empty() {
        return false;
    }
    ci_from(&checks) == CiConclusion::Red
}

/// What else on this machine or this forge looks like a fix already in flight.
///
/// Four signals, and the doctrine decides what they mean — this only gathers
/// them. Cheap reads, all of them, because this runs on every poll while the
/// base is red.
///
/// `local_worktrees` is the one nobody checks, and the one most likely to
/// matter: two self-driving processes against one clone each see the other's
/// worktree here, and without it they would both adopt the same breakage and
/// race to fix it.
///
/// **Passes `None` for the own-root exclusion deliberately.** The claim-time
/// probe drops worktrees inside this verb's own root, because there a leftover
/// is usually the loop's own crashed run (#4300); here it is the peer the
/// signal exists to catch. The parsing is shared with
/// [`super::contention`] so the two policies stay one argument apart rather
/// than two copies.
#[must_use]
pub(super) fn base_fix_contention(root: &std::path::Path, key: &str) -> Contention {
    let mut contention = Contention::default();

    // A branch whose name carries the issue key. Remote only: a local branch
    // of the loop's own is not another actor.
    if let Some(out) = git(root, &["ls-remote", "--heads", "origin"]) {
        contention.remote_branches = super::contention::branches_naming(&out, key);
    }

    // An open pull request that says it closes the issue, or names it.
    if let Ok(raw) = prs_matching(key) {
        contention.open_prs = raw;
    }

    // Worktrees on this machine holding a branch for the same key.
    if let Some(out) = git(root, &["worktree", "list", "--porcelain"]) {
        contention.local_worktrees = super::contention::worktrees_naming(&out, key, None);
    }

    contention
}

/// Open pull request numbers the forge's own search associates with `key`.
///
/// Broader than [`open_prs_for_issue`] on purpose, and the two are not
/// interchangeable: that one asks "did *this loop* deliver an attempt", so it
/// searches the `Closes #key` trailer it writes itself, and a false positive
/// there would refuse to clean up a dead branch. This one asks "is *anybody*
/// on this", where a pull request that merely names the issue is exactly the
/// actor worth deferring to.
pub(super) fn prs_matching(key: &str) -> Result<Vec<String>, String> {
    let raw = gh(&[
        "pr", "list", "--state", "open", "--search", key, "--json", "number",
    ])?;
    Ok(super::contention::pr_numbers(&raw))
}

/// Open pull requests that say they close `key`.
///
/// Asked before a leftover branch is discarded: an attempt that delivered has
/// a pull request, and one that does not has nothing worth keeping. Searched
/// rather than listed-and-filtered because the forge already indexes the body
/// text, and the loop writes `Closes #key` into every one it opens.
pub(super) fn open_prs_for_issue(key: &str) -> Result<Vec<String>, String> {
    let raw = gh(&[
        "pr",
        "list",
        "--state",
        "open",
        "--search",
        &format!("Closes #{key}"),
        "--json",
        "number",
    ])?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&raw).map_err(|error| {
        format!("`gh pr list` returned a payload this build cannot read: {error}")
    })?;
    Ok(rows
        .iter()
        .filter_map(|row| row.get("number").and_then(serde_json::Value::as_u64))
        .map(|n| n.to_string())
        .collect())
}

/// Every open pull request on a branch with this workspace's prefix.
///
/// How a restarted loop finds what it was carrying. It **asks the forge**
/// rather than reading a list it wrote down, because the forge is the source
/// of truth about a pull request and a remembered list can only be wrong —
/// stale after a human merges one by hand, or after a run died between opening
/// a pull request and recording it.
pub(super) fn open_prs_for_prefix(prefix: &str) -> Result<Vec<String>, String> {
    #[derive(serde::Deserialize)]
    struct Row {
        number: u64,
        #[serde(rename = "headRefName", default)]
        head_ref_name: String,
    }

    let raw = gh(&[
        "pr",
        "list",
        "--state",
        "open",
        "--json",
        "number,headRefName",
    ])?;
    let rows: Vec<Row> = serde_json::from_str(&raw).map_err(|error| {
        format!("`gh pr list` returned a payload this build cannot read: {error}")
    })?;

    Ok(rows
        .into_iter()
        .filter(|row| row.head_ref_name.starts_with(prefix))
        .map(|row| row.number.to_string())
        .collect())
}

/// Take a pull request out of draft.
///
/// Called only when the machine returned `MarkReady`, which it does once CI is
/// green — so a pull request that never goes green never asks a human to look
/// at it.
pub(super) fn mark_ready(pr: &str) -> Result<(), String> {
    gh(&["pr", "ready", pr]).map(|_| ())
}

/// Merge the pull request.
///
/// Called **only** when [`stella_autonomy::deliver_next`] returned
/// [`stella_autonomy::Action::Merge`]; the caller enforces that, and the
/// machine emits it from exactly one state.
pub(super) fn merge(pr: &str) -> Result<(), String> {
    // No `--delete-branch`. It deletes the *local* branch too, and this loop
    // works inside git worktrees that hold exactly those branches — so the
    // delete fails, `gh` exits non-zero, and a merge that already succeeded is
    // reported as a failure. That is what it did: #4022 merged at 00:32:54 and
    // the loop went on re-observing a merged pull request because the branch
    // cleanup after it had failed.
    //
    // Cleanup is a separate concern from delivery, and the forge's own
    // "automatically delete head branches" setting does it without a local
    // side effect. Losing a branch is recoverable; losing the record of a
    // merge strands the pull request forever.
    let outcome = gh(&["pr", "merge", pr, "--squash"]).map(|_| ());

    // A pull request that is already merged is the state this asked for, so it
    // is a success. Racing a human who merged it by hand, or re-observing
    // after a partial failure, must not read as an error.
    match outcome {
        Err(error) if error.to_ascii_lowercase().contains("already merged") => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The base is asked about by branch name, so no local ref can go stale.
    ///
    /// The endpoint used to carry a commit resolved by `git rev-parse
    /// origin/<branch>`, which is only as fresh as the last fetch — and this
    /// loop never fetches. A base that broke while the process was up still
    /// resolved to the pre-breakage commit, so [`base_conclusion`] saw a green
    /// base and scored an inherited failure as the pull request's own. #4014
    /// was escalated for a `cargo fmt` diff in a file it never touched.
    ///
    /// Asserting the *absence* of `origin/` is the half that matters: a
    /// remote-tracking spelling would resolve locally again and re-introduce
    /// exactly the staleness this replaced.
    #[test]
    fn the_base_is_named_to_the_forge_not_a_remote_tracking_ref() {
        let endpoint = check_runs_endpoint("main");
        assert_eq!(endpoint, "repos/{owner}/{repo}/commits/main/check-runs");
        assert!(
            !endpoint.contains("origin/"),
            "a remote-tracking ref resolves against the last fetch, not the forge: {endpoint}"
        );
    }

    /// An advisory check that fails forever must not block forever.
    ///
    /// **The witness for the rule that "CI is red" means the *required*
    /// checks are red.** This repository runs a Vercel commit status that has
    /// failed on every pull request for as long as the account has been
    /// blocked. It is not a required context, the forge merges past it
    /// (#4022 reported `UNSTABLE / MERGEABLE`), and a human ignores it.
    ///
    /// Counting it made the loop escalate two pull requests whose every
    /// required check was green — and no amount of waiting could have helped,
    /// because that failure is never going to clear.
    #[test]
    fn an_advisory_check_does_not_block_a_merge() {
        let required = vec!["fmt + clippy + test".to_owned()];
        let rollup = vec![
            check("fmt + clippy + test", "SUCCESS"),
            Check {
                context: "Vercel".into(),
                state: "FAILURE".into(),
                ..Check::default()
            },
        ];

        assert_eq!(
            ci_from(&rollup),
            CiConclusion::Red,
            "unfiltered, the advisory failure condemns the pull request"
        );
        assert_eq!(
            ci_from(&only_required(rollup, &required)),
            CiConclusion::Green,
            "filtered to what the repository requires, it is green"
        );
    }

    /// A repository with no declared requirements keeps every check.
    ///
    /// The conservative direction: unable to read protection, the loop can
    /// still refuse to merge something mergeable, but can never merge
    /// something the repository would have blocked.
    #[test]
    fn no_declared_requirements_filters_nothing() {
        let rollup = vec![check("a", "FAILURE"), check("b", "SUCCESS")];
        assert_eq!(only_required(rollup, &[]).len(), 2);
    }

    /// Filtering applies to the base too, so it can still excuse a required
    /// check that is broken on both sides.
    #[test]
    fn the_base_is_filtered_by_the_same_list() {
        let required = vec!["fmt + clippy + test".to_owned()];
        let pr = only_required(
            vec![
                check("fmt + clippy + test", "FAILURE"),
                check("noise", "FAILURE"),
            ],
            &required,
        );
        let base = only_required(
            vec![
                check("fmt + clippy + test", "FAILURE"),
                check("other noise", "SUCCESS"),
            ],
            &required,
        );
        assert_eq!(
            base_conclusion(&pr, &base),
            CiConclusion::Red,
            "the required check is broken on the base, so this is not ours"
        );
    }

    /// A commit status is a concluded check, not a pending one.
    ///
    /// **The witness for the bug that stalled the loop.** GitHub's rollup
    /// mixes two dialects: a check run carries `name`/`status`/`conclusion`,
    /// a commit status carries `context`/`state` and no `status` at all.
    /// Reading only the first shape does not fail — every field defaults — it
    /// yields a nameless check with no conclusion, which `pending()` reports
    /// as unfinished on every poll forever.
    ///
    /// That is what happened: this repository's Vercel commit status had
    /// already concluded `FAILURE`, and the loop re-read `ci=Pending` on #4022
    /// for twenty-five minutes after all three required checks went green.
    #[test]
    fn a_commit_status_is_read_as_concluded_not_as_pending() {
        let vercel = Check {
            context: "Vercel".into(),
            state: "FAILURE".into(),
            ..Check::default()
        };

        assert!(
            !vercel.pending(),
            "it concluded — before the loop ever looked"
        );
        assert!(vercel.failed());
        assert_eq!(vercel.name(), "Vercel", "and it must join by its context");
        assert_eq!(ci_from(&[vercel]), CiConclusion::Red);
    }

    /// A commit status that really is pending still reads as pending.
    ///
    /// The other direction, so the fix above cannot be "call every commit
    /// status finished".
    #[test]
    fn a_pending_commit_status_still_reads_as_pending() {
        let waiting = Check {
            context: "Vercel".into(),
            state: "PENDING".into(),
            ..Check::default()
        };
        assert!(waiting.pending());
        assert!(!waiting.failed());
        assert_eq!(ci_from(&[waiting]), CiConclusion::Pending);
    }

    /// A commit status failing on both sides is the base's fault, not ours.
    ///
    /// This only works because the base is read from *two* endpoints. A base
    /// read that saw check runs alone would show no `Vercel` row, the join
    /// would find nothing, and a service broken account-wide would be charged
    /// to every pull request the loop opened.
    #[test]
    fn a_commit_status_broken_on_the_base_excuses_the_pull_request() {
        let failing = |context: &str| Check {
            context: context.into(),
            state: "FAILURE".into(),
            ..Check::default()
        };

        assert_eq!(
            base_conclusion(&[failing("Vercel")], &[failing("Vercel")]),
            CiConclusion::Red,
            "the base is broken in the same way, so this is not ours to fix"
        );
        assert_eq!(
            base_conclusion(&[failing("Vercel")], &[]),
            CiConclusion::Green,
            "and a base that does not share the failure excuses nothing"
        );
    }

    /// A check run and a commit status of the same name are one check.
    ///
    /// Neither dialect is privileged: the join key is whichever field carried
    /// the name.
    #[test]
    fn the_two_dialects_join_on_one_name() {
        let run = Check {
            name: "build".into(),
            status: "COMPLETED".into(),
            conclusion: "FAILURE".into(),
            ..Check::default()
        };
        let status = Check {
            context: "build".into(),
            state: "FAILURE".into(),
            ..Check::default()
        };
        assert_eq!(base_conclusion(&[run], &[status]), CiConclusion::Red);
    }

    /// A check link yields a run id, or nothing — never a guess.
    ///
    /// The re-run is the loop's one remedy for a red the base already
    /// explains, and it is aimed by parsing a URL. Re-running the wrong id is
    /// worse than re-running nothing, so every shape that is not a GitHub
    /// Actions run must decline: this repository's checks include third-party
    /// ones (Vercel) whose links point somewhere else entirely, and one of
    /// them is failing on every pull request right now.
    #[test]
    fn only_an_actions_run_link_names_a_run_to_rerun() {
        assert_eq!(
            run_id_from_link(
                "https://github.com/macanderson/stella/actions/runs/32311670564/job/96255819957"
            )
            .as_deref(),
            Some("32311670564")
        );
        assert_eq!(run_id_from_link("https://vercel.com/github"), None);
        assert_eq!(run_id_from_link(""), None);
        assert_eq!(
            run_id_from_link("https://github.com/o/r/actions/runs/not-a-number/job/1"),
            None
        );
    }

    /// A base with no name is not a base with no failures.
    ///
    /// Reading an empty list here means "the base excuses nothing", which
    /// blames the pull request. That is only sound when the base genuinely has
    /// no checks — so the empty-name path returns early rather than asking the
    /// forge about a ref spelled `""` and reading its 404 as the same thing.
    #[test]
    fn an_unnamed_base_asks_the_forge_nothing() {
        assert!(checks_for_branch("").is_empty());
    }

    fn check(name: &str, conclusion: &str) -> Check {
        Check {
            name: name.into(),
            conclusion: conclusion.into(),
            status: "COMPLETED".into(),
            ..Check::default()
        }
    }

    fn running(name: &str) -> Check {
        Check {
            name: name.into(),
            conclusion: String::new(),
            status: "IN_PROGRESS".into(),
            ..Check::default()
        }
    }

    /// **The witness for §3.3's first commitment**, at the observation layer
    /// this time. A blanket "is the base red" read would call this PR's own
    /// regression inherited, because something unrelated is failing on `main`.
    #[test]
    fn a_base_failing_a_different_check_does_not_excuse_this_one() {
        let pr = vec![
            check("fmt + clippy + test", "FAILURE"),
            check("docs", "SUCCESS"),
        ];
        let base = vec![
            check("deck-fit", "FAILURE"),
            check("fmt + clippy + test", "SUCCESS"),
        ];

        assert_eq!(
            base_conclusion(&pr, &base),
            CiConclusion::Green,
            "the base failing something else is not an excuse — this is our regression"
        );
    }

    /// The other half: the *same* check failing on the base is exactly what
    /// `BaseBroken` is for, and the loop must not spend a fix on it.
    #[test]
    fn the_same_check_failing_on_the_base_is_inherited() {
        let pr = vec![check("fmt + clippy + test", "FAILURE")];
        let base = vec![check("fmt + clippy + test", "FAILURE")];

        assert_eq!(base_conclusion(&pr, &base), CiConclusion::Red);
    }

    /// A PR failing two checks where the base only shares one is still this
    /// PR's problem: it introduced the second.
    #[test]
    fn a_partial_overlap_is_still_this_prs_fault() {
        let pr = vec![check("a", "FAILURE"), check("b", "FAILURE")];
        let base = vec![check("a", "FAILURE"), check("b", "SUCCESS")];

        assert_eq!(base_conclusion(&pr, &base), CiConclusion::Green);
    }

    /// Nothing failing here means the base has nothing to excuse, whatever
    /// state it is in.
    #[test]
    fn a_green_pr_is_never_inherited_from_a_red_base() {
        let pr = vec![check("a", "SUCCESS")];
        let base = vec![check("a", "FAILURE")];

        assert_eq!(base_conclusion(&pr, &base), CiConclusion::Green);
    }

    /// Red wins over pending: a build that has already failed does not become
    /// undecided because something else is still running.
    #[test]
    fn a_failure_beats_a_check_still_running() {
        assert_eq!(
            ci_from(&[check("a", "FAILURE"), running("b")]),
            CiConclusion::Red
        );
    }

    /// A skipped check did not run. Counting it as pending would stall the loop
    /// forever on a job this repository skips by design on a docs-only diff.
    #[test]
    fn a_skipped_check_neither_blocks_nor_fails() {
        assert_eq!(
            ci_from(&[check("a", "SUCCESS"), check("cla", "SKIPPED")]),
            CiConclusion::Green
        );
    }

    /// A completed check with an empty conclusion is a forge quirk, not a pass.
    #[test]
    fn a_completed_check_with_no_conclusion_is_not_green() {
        let odd = Check {
            name: "a".into(),
            conclusion: String::new(),
            status: "COMPLETED".into(),
            ..Check::default()
        };
        assert_eq!(ci_from(&[odd]), CiConclusion::Pending);
    }

    #[test]
    fn no_checks_at_all_is_pending_not_green() {
        assert_eq!(ci_from(&[]), CiConclusion::Pending);
    }

    /// Reading an uncomputed mergeability as clean is how a merge is attempted
    /// into a conflict.
    #[test]
    fn an_unrecognised_mergeability_is_unknown_never_clean() {
        assert_eq!(mergeable_from("MERGEABLE"), Mergeability::Clean);
        assert_eq!(mergeable_from("CONFLICTING"), Mergeability::Conflicted);
        assert_eq!(mergeable_from("UNKNOWN"), Mergeability::Unknown);
        assert_eq!(mergeable_from(""), Mergeability::Unknown);
        assert_eq!(mergeable_from("something new"), Mergeability::Unknown);
    }

    #[test]
    fn review_maps_the_three_states_and_defaults_to_none() {
        assert_eq!(review_from("APPROVED"), ReviewState::Approved);
        assert_eq!(
            review_from("CHANGES_REQUESTED"),
            ReviewState::ChangesRequested
        );
        assert_eq!(review_from(""), ReviewState::None);
    }

    /// **The closing witness.** Both spellings, because the two merge paths read
    /// different text and either alone is a silent single point of failure.
    #[test]
    fn closing_an_issue_takes_both_the_body_and_the_trailer() {
        let body = pr_body("3939", "summary", stella_autonomy::SIGNATURE);
        assert!(body.contains("Closes #3939"));
        assert_eq!(commit_trailer("3939"), "Closes #3939");
        // And the footer is a horizontal rule below the closing keyword, so
        // the `Closes` line is never swallowed into the signature.
        assert!(
            body.ends_with("Closes #3939\n\n---\ncreated by stella*"),
            "{body:?}"
        );
    }

    /// The real payload shape, parsed from what `gh pr view` actually returned
    /// for PR #3985 — abridged, but with its field names and its empty
    /// `reviewDecision` untouched.
    #[test]
    fn the_real_gh_payload_maps_onto_an_observation() {
        let raw = r#"{
            "isDraft": false,
            "mergeable": "MERGEABLE",
            "reviewDecision": "",
            "statusCheckRollup": [
                {"name": "typecheck + build", "conclusion": "", "status": "IN_PROGRESS"},
                {"name": "docs guards", "conclusion": "SUCCESS", "status": "COMPLETED"}
            ]
        }"#;
        let view: PrView = serde_json::from_str(raw).expect("the recorded payload parses");
        let obs = observation_from(&view, &[]);

        assert_eq!(obs.ci, CiConclusion::Pending);
        assert_eq!(obs.mergeable, Mergeability::Clean);
        assert_eq!(obs.review, ReviewState::None);
        assert!(!obs.draft);
        // Nothing failed, so the base excuses nothing.
        assert_eq!(obs.base_ci, CiConclusion::Green);
    }
}
