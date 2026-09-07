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
//! # The forge is a port
//!
//! Every function here takes a
//! [`PullRequestProvider`](stella_protocol::pull_request::PullRequestProvider).
//! Nothing in this module spells `gh`: the shipping adapter is
//! `crate::pull_request_provider`, GitHub's two check dialects are its
//! business, and a test hands over a fixture forge and runs the same code that
//! ships. It is the pull request half of what `crate::issue_provider` already
//! did for the backlog.
//!
//! What stays above the port is policy — which checks may block a merge,
//! whether a red base excuses a red pull request, and what to try when a
//! verdict is stale.
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
//! commit trailer. Both are required, and AGENTS.md § *Closing the issue on
//! merge* says why. A squash merge builds the commit message from
//! `COMMIT_MESSAGES`, never from the PR body. So a `Closes` that lives only in
//! the description never reaches the commit. A rebase merge replays the
//! commits as they are, so it never reads the PR body either. Either spelling
//! alone can fail on its own, and nothing shows it until someone audits the
//! backlog.

use stella_autonomy::{CiConclusion, Mergeability, Observation, ReviewState};
use stella_protocol::pull_request::{
    Check, CheckOutcome, MergeStatus, PullRequest, PullRequestDraft, PullRequestKey,
    PullRequestProvider, ReviewDecision,
};

#[cfg(test)]
pub(super) mod fixture;
#[cfg(test)]
mod witness;

use super::state::git;

/// Whether a check has concluded badly.
fn failed(check: &Check) -> bool {
    check.outcome == CheckOutcome::Failed
}

/// Reduce a rollup to the one conclusion the machine branches on.
///
/// Red wins over pending: a build that has already failed does not become
/// undecided because something else is still running, and waiting for the rest
/// before saying so only delays the fix.
///
/// A check the forge reported as [`Inert`](CheckOutcome::Inert) is dropped
/// first. It did not run, so it is neither a pass nor a failure, and counting
/// it as pending would stall the loop forever on a job that is skipped by
/// design — this repository skips several on a docs-only diff.
#[must_use]
pub(super) fn ci_from(checks: &[Check]) -> CiConclusion {
    let live: Vec<&Check> = checks
        .iter()
        .filter(|c| c.outcome != CheckOutcome::Inert)
        .collect();
    if live.is_empty() {
        return CiConclusion::Pending;
    }
    if live.iter().copied().any(failed) {
        return CiConclusion::Red;
    }
    if live.iter().any(|c| c.outcome == CheckOutcome::Running) {
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
///
/// `None` is the base nobody could read, and it is **not** a base with no
/// checks. Blaming the pull request needs evidence that the base is fine, and
/// an unread base is not that evidence — so an unread base excuses, which the
/// machine spells `Red` and answers with `BaseBroken` / `Wait`. Waiting costs a
/// poll; the alternative costs the `PushFix` turn that arm's own comment
/// forbids spending on somebody else's breakage.
#[must_use]
pub(super) fn base_conclusion(pr: &[Check], base: Option<&[Check]>) -> CiConclusion {
    let failing_here: Vec<&str> = pr
        .iter()
        .filter(|c| failed(c))
        .map(|c| c.name.as_str())
        .collect();

    if failing_here.is_empty() {
        // Nothing failed here, so there is nothing for the base to excuse, and
        // no need to have read it.
        return CiConclusion::Green;
    }

    let Some(base) = base else {
        return CiConclusion::Red;
    };

    let failing_there = |name: &str| base.iter().any(|c| c.name == name && failed(c));

    if failing_here.iter().all(|name| failing_there(name)) {
        CiConclusion::Red
    } else {
        CiConclusion::Green
    }
}

/// Map the forge's mergeability onto the machine's.
///
/// [`MergeStatus::Unknown`] becomes [`Mergeability::Unknown`] rather than
/// `Clean`: a forge reports the field before it has computed it, and reading
/// that as clean is how a merge is attempted into a conflict.
#[must_use]
pub(super) fn mergeable_from(raw: MergeStatus) -> Mergeability {
    match raw {
        MergeStatus::Clean => Mergeability::Clean,
        MergeStatus::Conflicted => Mergeability::Conflicted,
        MergeStatus::Unknown => Mergeability::Unknown,
    }
}

/// Map the forge's review decision onto the machine's.
#[must_use]
pub(super) fn review_from(raw: ReviewDecision) -> ReviewState {
    match raw {
        ReviewDecision::Approved => ReviewState::Approved,
        ReviewDecision::ChangesRequested => ReviewState::ChangesRequested,
        ReviewDecision::None => ReviewState::None,
    }
}

/// Assemble the observation the pure machine decides over.
#[must_use]
pub(super) fn observation_from(pr: &PullRequest, base_checks: Option<&[Check]>) -> Observation {
    Observation {
        ci: ci_from(&pr.checks),
        base_ci: base_conclusion(&pr.checks, base_checks),
        mergeable: mergeable_from(pr.merge_status),
        review: review_from(pr.review),
        draft: pr.draft,
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
/// check that ran against a base somebody has since repaired keeps its failing
/// verdict for ever. `base_conclusion` compares what is failing *now*. So once
/// the base goes green, the pull request holds a failure that happens
/// nowhere.
///
/// # Re-running alone does not do it, and that is the whole subtlety
///
/// A `pull_request` run is pinned to the merge commit worked out when the run
/// was **created**. Re-running replays that same commit, stale base and all.
/// This was measured, not assumed. One failed job was re-run twelve minutes
/// after a repair landed on `main`. It produced the same `cargo fmt` diff at
/// `stella-autonomy/src/lib.rs`'s `sign` that only ever existed on the old
/// base.
///
/// So the base is merged in first — a new head commit, and a fresh run that
/// sees the repair. Re-running is kept as the fallback for the case the forge
/// declines the sync, which is a branch that is *already* current: then there
/// is no staleness to clear and a red that re-runs green was a flake.
///
/// Either way the caller bounds this to once per pull request, so a genuine
/// failure reaches the fix path on the next poll.
pub(super) fn refresh_against_base(
    forge: &dyn PullRequestProvider,
    pr: &str,
) -> Result<(), String> {
    let key = PullRequestKey::from(pr);
    // Ahead of the fallback because it is the one that can actually change the
    // answer. A branch already level with the base refuses here, which is
    // exactly when re-running is the right move instead.
    if forge.sync_with_base(&key).is_ok() {
        return Ok(());
    }
    forge
        .rerun_failed_checks(&key)
        .map_err(|error| error.to_string())
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
        .filter(|check| required.iter().any(|name| name == &check.name))
        .collect()
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
fn required_contexts(forge: &dyn PullRequestProvider, branch: &str) -> Vec<String> {
    forge.required_checks(branch).unwrap_or_default()
}

/// Read a branch's checks from the forge.
///
/// # The branch is named to the forge, never resolved locally first
///
/// A remote-tracking ref is only as fresh as the last fetch, and this loop
/// does not fetch between polls. Ask about `origin/main` after the process has
/// been up a while and you get whatever main was when it started.
///
/// That is a loss of work, not a stale-data annoyance. [`base_conclusion`]
/// excuses a pull request only when **every** check failing on it fails on the
/// base too. An old base looks green, so a failure the base handed down gets
/// scored as the pull request's own. A change that was never wrong then draws
/// a fix attempt or an escalation. It happened on the first pull request this
/// loop ever opened, over a `cargo fmt` diff in a file it never touched.
///
/// Naming the branch to the forge leaves no local state to go stale.
/// [`open_prs_for_prefix`] follows the same rule. For a question about the
/// forge, ask the forge.
pub(super) fn checks_for_branch(
    forge: &dyn PullRequestProvider,
    branch: &str,
) -> Option<Vec<Check>> {
    // A pull request read is not available for a branch that has none, so the
    // base is read through the ref's checks instead. `Some(empty)` means "the
    // base really has no checks", which `base_conclusion` may read as "does
    // not excuse anything"; `None` means nobody could say, which it may not.
    // An unnamed base is the second of those — a payload that carried no base
    // name decodes to an empty string, which is a field that was absent rather
    // than a branch with no checks.
    if branch.is_empty() {
        return None;
    }
    forge.checks_for_ref(branch).ok()
}

/// Push `branch` and open a draft pull request that closes `issue_key`.
///
/// Draft on purpose: the machine takes it out of draft itself once CI is green
/// ([`stella_autonomy::Action::MarkReady`]), so a pull request that never goes
/// green never asks a human to look at it.
///
/// The push is `git`, on this machine, so it stays here rather than behind the
/// forge port: a provider is asked about a branch the forge can already see.
pub(crate) fn open(
    forge: &dyn PullRequestProvider,
    root: &std::path::Path,
    branch: &str,
    issue_key: &str,
    title: &str,
    signature: &str,
) -> Result<String, String> {
    git(root, &["push", "-u", "origin", branch])
        .ok_or_else(|| format!("could not push `{branch}` — is the remote reachable?"))?;

    let draft = PullRequestDraft {
        head_ref: branch.to_owned(),
        title: title.to_owned(),
        body: pr_body(
            issue_key,
            &format!("Autonomous fix for #{issue_key}."),
            signature,
        ),
        draft: true,
    };
    forge
        .open(&draft)
        .map(|key| key.0)
        .map_err(|error| error.to_string())
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
pub(crate) struct Reading {
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
pub(crate) fn observe(
    forge: &dyn PullRequestProvider,
    pr: &str,
    policy: &stella_autonomy::BlockingPolicy,
) -> Result<Reading, String> {
    let mut view = forge
        .get(&PullRequestKey::from(pr))
        .map_err(|error| error.to_string())?;

    // The forge's own name for the base, passed through untouched. Prefixing
    // it with `origin/` would name a remote-tracking ref this process no
    // longer resolves — see `checks_for_branch`.
    let required = required_contexts(forge, &view.base_ref);

    // Required is necessary and not sufficient. A check that has been red on
    // the base for every recent commit is not something this pull request can
    // fix — see `stella_autonomy::gate`.
    let stuck = if required.is_empty() {
        std::collections::BTreeSet::new()
    } else {
        stella_autonomy::stuck_on_base(
            &base_failure_history(forge, &view.base_ref, policy.stuck_after.max(1)),
            policy.stuck_after,
        )
    };
    let blocking = stella_autonomy::blocking(&required, &stuck, policy);
    let waived = stella_autonomy::waived(&required, &stuck, policy);

    let base_checks =
        checks_for_branch(forge, &view.base_ref).map(|checks| only_required(checks, &blocking));

    // Both sides filtered by the same list, so the base can still excuse a
    // blocking check — and a waived one can no longer condemn.
    let verified_locally = view
        .labels
        .iter()
        .any(|label| label == VERIFIED_LOCALLY_LABEL);
    view.checks = only_required(std::mem::take(&mut view.checks), &blocking);

    let settled = view.state.settled();
    let mut observation = observation_from(&view, base_checks.as_deref());

    // Nothing is left that can gate this pull request remotely, for one of two
    // reasons, and the loop treats them the same because the forge does:
    //
    // - every check the repository *requires* has been waived as unwinnable; or
    // - the repository declares no required checks at all. A private repository
    //   on a free plan cannot even expose branch protection — the API answers
    //   403 — and a merge will go through whatever its checks say. A gate the
    //   forge does not enforce is not a gate.
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
pub(super) fn mark_verified_locally(
    forge: &dyn PullRequestProvider,
    pr: &str,
) -> Result<(), String> {
    forge
        .add_label(&PullRequestKey::from(pr), VERIFIED_LOCALLY_LABEL)
        .map_err(|error| error.to_string())
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
pub(super) fn base_failure_history(
    forge: &dyn PullRequestProvider,
    branch: &str,
    depth: usize,
) -> Vec<Vec<String>> {
    forge
        .recent_commits(branch, depth)
        .unwrap_or_default()
        .into_iter()
        .take(depth)
        .map(|sha| {
            forge
                .checks_for_ref(&sha)
                .unwrap_or_default()
                .into_iter()
                .filter(failed)
                .map(|check| check.name)
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
/// Only **required** checks count, on exactly the reasoning [`only_required`]
/// documents: an advisory check that fails forever would otherwise convince
/// the loop the repository is permanently on fire.
///
/// A base that cannot be read is reported healthy. That is the conservative
/// direction here and the opposite of the one `base_conclusion` takes, and
/// deliberately: an unreadable forge would otherwise make the loop file a
/// breakage report about nothing and adopt an imaginary emergency.
#[must_use]
pub(super) fn base_is_broken(forge: &dyn PullRequestProvider, branch: &str) -> bool {
    let required = required_contexts(forge, branch);
    // `unwrap_or_default` here says the same thing the `is_empty` below
    // already said: an unreadable forge must not make the loop file a
    // breakage report about nothing.
    let checks = only_required(
        checks_for_branch(forge, branch).unwrap_or_default(),
        &required,
    );
    if checks.is_empty() {
        return false;
    }
    ci_from(&checks) == CiConclusion::Red
}

/// Open pull request numbers the forge's own search associates with `key`.
///
/// Broader than [`open_prs_for_issue`] on purpose, and the two do not swap.
/// That one asks whether *this loop* delivered an attempt, so it searches the
/// `Closes #key` trailer it writes itself. A false hit there would refuse to
/// clean up a dead branch. This one asks whether *anybody* is on it, and a
/// pull request that merely names the issue is the actor worth waiting for.
///
/// The gathering that reads it lives in [`super::contention`], with the other
/// three signals. It moved there once the claim site and the base-fix site
/// stopped sharing one probe. The claim site drops worktrees under the loop's
/// own root; the base-fix site keeps them. That difference is an argument to
/// one gatherer, not a second copy of four reads.
pub(super) fn prs_matching(
    forge: &dyn PullRequestProvider,
    key: &str,
) -> Result<Vec<String>, String> {
    // The search is a full-text one on the bare number, so the forge answers
    // with every open pull request that mentions it for any reason. The text
    // is what says whether the mention is this issue — see
    // `contention::prs_naming`.
    let rows = forge
        .list_open(Some(key))
        .map_err(|error| error.to_string())?;
    Ok(super::contention::prs_naming(&rows, key))
}

/// Open pull requests that say they close `key`.
///
/// Asked before a leftover branch is discarded: an attempt that delivered has
/// a pull request, and one that does not has nothing worth keeping. Searched
/// rather than listed-and-filtered because the forge already indexes the body
/// text, and the loop writes `Closes #key` into every one it opens.
pub(super) fn open_prs_for_issue(
    forge: &dyn PullRequestProvider,
    key: &str,
) -> Result<Vec<String>, String> {
    let rows = forge
        .list_open(Some(&format!("Closes #{key}")))
        .map_err(|error| error.to_string())?;
    Ok(rows.into_iter().map(|row| row.key.0).collect())
}

/// Every open pull request on a branch with this workspace's prefix.
///
/// How a restarted loop finds what it was carrying. It **asks the forge**
/// rather than reading a list it wrote down. The forge is the truth about a
/// pull request, and a remembered list can only be wrong. It goes stale when
/// a human merges one by hand, or when a run dies between opening a pull
/// request and writing it down.
pub(super) fn open_prs_for_prefix(
    forge: &dyn PullRequestProvider,
    prefix: &str,
) -> Result<Vec<String>, String> {
    let rows = forge.list_open(None).map_err(|error| error.to_string())?;
    Ok(rows
        .into_iter()
        .filter(|row| row.head_ref.starts_with(prefix))
        .map(|row| row.key.0)
        .collect())
}

/// Take a pull request out of draft.
///
/// Called only when the machine returned `MarkReady`, which it does once CI is
/// green — so a pull request that never goes green never asks a human to look
/// at it.
pub(crate) fn mark_ready(forge: &dyn PullRequestProvider, pr: &str) -> Result<(), String> {
    forge
        .mark_ready(&PullRequestKey::from(pr))
        .map_err(|error| error.to_string())
}

/// Merge the pull request.
///
/// Called **only** when [`stella_autonomy::deliver_next`] returned
/// [`stella_autonomy::Action::Merge`]; the caller enforces that, and the
/// machine emits it from exactly one state.
pub(crate) fn merge(forge: &dyn PullRequestProvider, pr: &str) -> Result<(), String> {
    forge
        .merge(&PullRequestKey::from(pr))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            check("fmt + clippy + test", CheckOutcome::Passed),
            check("Vercel", CheckOutcome::Failed),
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
        let rollup = vec![
            check("a", CheckOutcome::Failed),
            check("b", CheckOutcome::Passed),
        ];
        assert_eq!(only_required(rollup, &[]).len(), 2);
    }

    /// Filtering applies to the base too, so it can still excuse a required
    /// check that is broken on both sides.
    #[test]
    fn the_base_is_filtered_by_the_same_list() {
        let required = vec!["fmt + clippy + test".to_owned()];
        let pr = only_required(
            vec![
                check("fmt + clippy + test", CheckOutcome::Failed),
                check("noise", CheckOutcome::Failed),
            ],
            &required,
        );
        let base = only_required(
            vec![
                check("fmt + clippy + test", CheckOutcome::Failed),
                check("other noise", CheckOutcome::Passed),
            ],
            &required,
        );
        assert_eq!(
            base_conclusion(&pr, Some(&base)),
            CiConclusion::Red,
            "the required check is broken on the base, so this is not ours"
        );
    }

    /// A base with no name is not a base with no failures.
    ///
    /// Reading an empty list here means "the base excuses nothing", which
    /// blames the pull request. That is only sound when the base genuinely has
    /// no checks — so the empty-name path returns early rather than asking the
    /// forge about a ref spelled `""` and reading its 404 as the same thing.
    ///
    /// It returns `None` rather than an empty list, which is the same argument
    /// carried in the type: an unnamed base is one nobody read.
    #[test]
    fn an_unnamed_base_asks_the_forge_nothing() {
        let forge = super::fixture::FixtureForge::new();
        assert!(checks_for_branch(&forge, "").is_none());
        assert!(
            forge.journal().is_empty(),
            "an unnamed base is never asked about: {:?}",
            forge.journal()
        );
    }

    /// A base nobody could read does not become a base that excuses nothing.
    ///
    /// **The witness.** A forge read that runs and fails — a 404, an expired
    /// token, a rate limit — must not come back as a base with no checks.
    ///
    /// `base_conclusion` returning `Green` means "this failure is yours", and
    /// the machine answers it with `CiRed` / `PushFix` — a turn spent fixing a
    /// break the pull request may not have caused. That arm's own comment
    /// forbids exactly this: *"pushing a fix for it would be fixing somebody
    /// else's breakage on this PR's budget."* #4014 is the incident.
    #[test]
    fn an_unread_base_does_not_blame_the_pull_request() {
        let pr = [check("build", CheckOutcome::Failed)];

        assert_eq!(
            base_conclusion(&pr, None),
            CiConclusion::Red,
            "unread means the base is not ruled out, so the loop waits"
        );
        assert_eq!(
            base_conclusion(&pr, Some(&[])),
            CiConclusion::Green,
            "a base that genuinely has no checks still excuses nothing"
        );
    }

    /// Nothing failing here needs no base read at all.
    #[test]
    fn a_green_pull_request_needs_no_base() {
        assert_eq!(base_conclusion(&[], None), CiConclusion::Green);
    }

    fn check(name: &str, outcome: CheckOutcome) -> Check {
        Check {
            name: name.to_owned(),
            outcome,
        }
    }

    /// **The witness for §3.3's first commitment**, at the observation layer
    /// this time. A blanket "is the base red" read would call this PR's own
    /// regression inherited, because something unrelated is failing on `main`.
    #[test]
    fn a_base_failing_a_different_check_does_not_excuse_this_one() {
        let pr = vec![
            check("fmt + clippy + test", CheckOutcome::Failed),
            check("docs", CheckOutcome::Passed),
        ];
        let base = vec![
            check("deck-fit", CheckOutcome::Failed),
            check("fmt + clippy + test", CheckOutcome::Passed),
        ];

        assert_eq!(
            base_conclusion(&pr, Some(&base)),
            CiConclusion::Green,
            "the base failing something else is not an excuse — this is our regression"
        );
    }

    /// The other half: the *same* check failing on the base is exactly what
    /// `BaseBroken` is for, and the loop must not spend a fix on it.
    #[test]
    fn the_same_check_failing_on_the_base_is_inherited() {
        let pr = vec![check("fmt + clippy + test", CheckOutcome::Failed)];
        let base = vec![check("fmt + clippy + test", CheckOutcome::Failed)];

        assert_eq!(base_conclusion(&pr, Some(&base)), CiConclusion::Red);
    }

    /// A PR failing two checks where the base only shares one is still this
    /// PR's problem: it introduced the second.
    #[test]
    fn a_partial_overlap_is_still_this_prs_fault() {
        let pr = vec![
            check("a", CheckOutcome::Failed),
            check("b", CheckOutcome::Failed),
        ];
        let base = vec![
            check("a", CheckOutcome::Failed),
            check("b", CheckOutcome::Passed),
        ];

        assert_eq!(base_conclusion(&pr, Some(&base)), CiConclusion::Green);
    }

    /// Nothing failing here means the base has nothing to excuse, whatever
    /// state it is in.
    #[test]
    fn a_green_pr_is_never_inherited_from_a_red_base() {
        let pr = vec![check("a", CheckOutcome::Passed)];
        let base = vec![check("a", CheckOutcome::Failed)];

        assert_eq!(base_conclusion(&pr, Some(&base)), CiConclusion::Green);
    }

    /// Red wins over pending: a build that has already failed does not become
    /// undecided because something else is still running.
    #[test]
    fn a_failure_beats_a_check_still_running() {
        assert_eq!(
            ci_from(&[
                check("a", CheckOutcome::Failed),
                check("b", CheckOutcome::Running)
            ]),
            CiConclusion::Red
        );
    }

    /// A skipped check did not run. Counting it as pending would stall the loop
    /// forever on a job this repository skips by design on a docs-only diff.
    #[test]
    fn a_skipped_check_neither_blocks_nor_fails() {
        assert_eq!(
            ci_from(&[
                check("a", CheckOutcome::Passed),
                check("cla", CheckOutcome::Inert)
            ]),
            CiConclusion::Green
        );
    }

    #[test]
    fn no_checks_at_all_is_pending_not_green() {
        assert_eq!(ci_from(&[]), CiConclusion::Pending);
    }

    /// Reading an uncomputed mergeability as clean is how a merge is attempted
    /// into a conflict.
    #[test]
    fn an_unrecognised_mergeability_is_unknown_never_clean() {
        assert_eq!(mergeable_from(MergeStatus::Clean), Mergeability::Clean);
        assert_eq!(
            mergeable_from(MergeStatus::Conflicted),
            Mergeability::Conflicted
        );
        assert_eq!(mergeable_from(MergeStatus::Unknown), Mergeability::Unknown);
    }

    #[test]
    fn review_maps_the_three_states_and_defaults_to_none() {
        assert_eq!(review_from(ReviewDecision::Approved), ReviewState::Approved);
        assert_eq!(
            review_from(ReviewDecision::ChangesRequested),
            ReviewState::ChangesRequested
        );
        assert_eq!(review_from(ReviewDecision::None), ReviewState::None);
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

    /// A pull request still running one check is pending, whatever else passed.
    ///
    /// The shape `gh pr view` returned for #3985, in the port's vocabulary.
    #[test]
    fn a_running_check_leaves_the_observation_pending() {
        let pr = PullRequest {
            key: PullRequestKey::from("3985"),
            state: stella_protocol::pull_request::PullRequestState::Open,
            draft: false,
            base_ref: "main".to_owned(),
            head_ref: "fix/3985".to_owned(),
            title: String::new(),
            body: String::new(),
            merge_status: MergeStatus::Clean,
            review: ReviewDecision::None,
            checks: vec![
                check("typecheck + build", CheckOutcome::Running),
                check("docs guards", CheckOutcome::Passed),
            ],
            labels: Vec::new(),
        };
        let obs = observation_from(&pr, Some(&[]));

        assert_eq!(obs.ci, CiConclusion::Pending);
        assert_eq!(obs.mergeable, Mergeability::Clean);
        assert_eq!(obs.review, ReviewState::None);
        assert!(!obs.draft);
        // Nothing failed, so the base excuses nothing.
        assert_eq!(obs.base_ci, CiConclusion::Green);
    }
}
