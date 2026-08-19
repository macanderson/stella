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

/// Read a branch's checks from the forge.
pub(super) fn checks_for_branch(root: &std::path::Path, branch: &str) -> Vec<Check> {
    // `gh pr view` is not available for a branch with no pull request, so the
    // base is read through the commit's check-runs instead. A base with no
    // checks at all yields an empty list, which `base_conclusion` reads as
    // "does not excuse anything" — the safe direction.
    let raw = git(root, &["rev-parse", branch]).unwrap_or_default();
    if raw.is_empty() {
        return Vec::new();
    }
    let out = std::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{{owner}}/{{repo}}/commits/{}/check-runs", raw.trim()),
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
pub(super) fn observe(root: &std::path::Path, pr: &str) -> Result<Observation, String> {
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

    let base_branch = if view.base_ref_name.is_empty() {
        "origin/HEAD".to_owned()
    } else {
        format!("origin/{}", view.base_ref_name)
    };
    let base_checks = checks_for_branch(root, &base_branch);

    Ok(observation_from(&view, &base_checks))
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
        let body = pr_body("3939", "summary", "Created by stella.");
        assert!(body.contains("Closes #3939"));
        assert_eq!(commit_trailer("3939"), "Closes #3939");
        // And the signature sits exactly one line break after the last
        // character, not two.
        assert!(
            body.ends_with("Closes #3939\nCreated by stella."),
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
