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

use stella_autonomy::{CiConclusion, Mergeability, Observation, ReviewState};

use super::state::git;

/// One check as the forge reports it, reduced to what the mapping reads.
#[derive(Debug, Clone, serde::Deserialize)]
pub(super) struct Check {
    /// The check's name — the join key against the base branch.
    #[serde(default)]
    pub name: String,
    /// `SUCCESS`, `FAILURE`, `""` while running, and the rest of the forge's
    /// vocabulary.
    #[serde(default)]
    pub conclusion: String,
    /// `COMPLETED`, `IN_PROGRESS`, `QUEUED`.
    #[serde(default)]
    pub status: String,
}

impl Check {
    /// Whether this check has concluded and concluded badly.
    fn failed(&self) -> bool {
        matches!(
            self.conclusion.to_ascii_uppercase().as_str(),
            "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED"
        )
    }

    /// Whether it has not finished.
    fn pending(&self) -> bool {
        !self.status.eq_ignore_ascii_case("COMPLETED")
            // A completed check with no conclusion is a forge quirk, not a
            // pass: treated as still-unknown rather than green, on the same
            // reasoning as `Mergeability::Unknown`.
            || self.conclusion.trim().is_empty()
    }

    /// Whether it should be ignored entirely.
    ///
    /// A skipped or neutral check is not a failure and not a pass — it did not
    /// run. Counting it as pending would stall the loop forever on a job that
    /// is skipped by design (this repository skips several on a docs-only
    /// diff).
    fn inert(&self) -> bool {
        matches!(
            self.conclusion.to_ascii_uppercase().as_str(),
            "SKIPPED" | "NEUTRAL" | "CANCELLED"
        )
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
    let failing_here: Vec<&str> = pr
        .iter()
        .filter(|c| c.failed())
        .map(|c| c.name.as_str())
        .collect();

    if failing_here.is_empty() {
        // Nothing failed here, so there is nothing for the base to excuse.
        return CiConclusion::Green;
    }

    let failing_there = |name: &str| base.iter().any(|c| c.name == name && c.failed());

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
    let raw = gh(&["pr", "checks", pr, "--json", "state,link"])?;

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
    let out = std::process::Command::new("gh")
        .args([
            "api",
            &check_runs_endpoint(branch),
            "--paginate",
            "--jq",
            ".check_runs[] | {name: .name, conclusion: (.conclusion // \"\"), status: .status}",
        ])
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

/// One read of the forge, plus the second read of the base its
/// [`Observation::base_ci`] needs.
pub(super) fn observe(pr: &str) -> Result<Observation, String> {
    let raw = gh(&[
        "pr",
        "view",
        pr,
        "--json",
        "isDraft,mergeable,reviewDecision,statusCheckRollup,baseRefName",
    ])?;
    let view: PrView = serde_json::from_str(&raw).map_err(|error| {
        format!("`gh pr view` returned a payload this build cannot read: {error}")
    })?;

    // The forge's own name for the base, passed through untouched. Prefixing
    // it with `origin/` would name a remote-tracking ref this process no
    // longer resolves — see `checks_for_branch`.
    let base_checks = checks_for_branch(&view.base_ref_name);

    Ok(observation_from(&view, &base_checks))
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
    gh(&["pr", "merge", pr, "--squash", "--delete-branch"]).map(|_| ())
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
        }
    }

    fn running(name: &str) -> Check {
        Check {
            name: name.into(),
            conclusion: String::new(),
            status: "IN_PROGRESS".into(),
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
