//! Which checks are allowed to block a merge, and which have stopped earning
//! that right.
//!
//! `doc:backlog-self-driving` §3.3 (#3599). `crate::deliver` decides what to
//! do about a red build; this decides *which reds count*. They are separate
//! questions and were previously the same one, which is how a loop ends up
//! permanently blocked by something no pull request could ever fix.
//!
//! # A check can be required and still be unwinnable
//!
//! Filtering to a repository's required contexts is necessary and not
//! sufficient. A required check fails for two very different reasons:
//!
//! - **the code is wrong** — a test, a lint, a build. A pull request can fix
//!   this, and the loop should either fix it or wait for whoever will.
//! - **the infrastructure is unavailable** — a blocked billing account, an
//!   exhausted quota, a revoked token, a deleted runner. No diff fixes this.
//!   Waiting is not caution, it is a loop that has stopped working and has not
//!   noticed.
//!
//! Nothing in the check's *name* distinguishes them, and nothing in its
//! conclusion does either: both are `FAILURE`.
//!
//! # What does distinguish them is time
//!
//! A code breakage on the base branch gets fixed, usually within a commit or
//! two — and if this loop is adopting base breakage (`doctrine`), it fixes it
//! itself. An infrastructure failure stays red across every commit, because no
//! commit is what is wrong.
//!
//! So [`stuck_on_base`] asks a question with no text matching in it: **has this
//! check failed on every one of the last N base commits?** If it has, the loop
//! stops letting it block, says so out loud, and gets on with the work. That
//! is deliberately a statement about the base's history and not about this
//! pull request — a check red only here is this branch's problem and still
//! blocks.
//!
//! # And the operator gets the final say
//!
//! [`BlockingPolicy::ignore`] names checks that never block, whatever their
//! history. Automatic detection needs N commits of evidence before it acts;
//! an operator who already knows their Vercel account is suspended should not
//! have to wait for the loop to work it out.

use std::collections::BTreeSet;

/// The default number of consecutive base commits a check must fail on before
/// the loop stops treating it as fixable.
///
/// Three rather than one, because one failing commit is the ordinary case the
/// self-heal path exists to fix, and rather than ten because the whole point
/// is not to spend a day blocked. It is the smallest number that cannot be
/// produced by a single bad merge.
pub const DEFAULT_STUCK_AFTER: usize = 3;

/// Which checks may block a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockingPolicy {
    /// Checks that never block, whatever their history.
    ///
    /// The operator's escape hatch, and it outranks everything: a name here is
    /// someone stating a fact about their own infrastructure that the loop
    /// would otherwise need days of evidence to infer.
    pub ignore: Vec<String>,
    /// How many consecutive failing base commits make a check unwinnable.
    ///
    /// `0` disables the inference entirely, leaving only [`Self::ignore`] —
    /// for an operator who would rather block forever than merge past a red
    /// required check.
    pub stuck_after: usize,
}

impl Default for BlockingPolicy {
    fn default() -> Self {
        Self {
            ignore: Vec::new(),
            stuck_after: DEFAULT_STUCK_AFTER,
        }
    }
}

/// Checks that have failed on **every** one of the last `stuck_after` base
/// commits.
///
/// `history` is newest-first, one entry per base commit, each listing the
/// checks that failed on it. A check counts as stuck only if it appears in
/// every one of the first `stuck_after` entries — so a check that went green
/// even once in that window is still considered fixable, which is the
/// conservative reading and the one that keeps a flaky test blocking.
///
/// Too little history is **not** evidence of stuckness: a repository with two
/// commits cannot demonstrate that anything is chronic, and inferring it from
/// a short window is how the loop would merge past a genuine breakage on a
/// young branch.
#[must_use]
pub fn stuck_on_base(history: &[Vec<String>], stuck_after: usize) -> BTreeSet<String> {
    if stuck_after == 0 || history.len() < stuck_after {
        return BTreeSet::new();
    }

    let window = &history[..stuck_after];
    let Some((first, rest)) = window.split_first() else {
        return BTreeSet::new();
    };

    first
        .iter()
        .filter(|name| rest.iter().all(|commit| commit.contains(*name)))
        .cloned()
        .collect()
}

/// The checks that actually block, out of the ones the repository requires.
///
/// Order is preserved from `required` so the caller's audit line reads in the
/// repository's own order rather than an alphabetical one.
#[must_use]
pub fn blocking(
    required: &[String],
    stuck: &BTreeSet<String>,
    policy: &BlockingPolicy,
) -> Vec<String> {
    required
        .iter()
        .filter(|name| !policy.ignore.iter().any(|ignored| ignored == *name))
        .filter(|name| !stuck.contains(*name))
        .cloned()
        .collect()
}

/// The required checks this policy is choosing not to enforce, and why.
///
/// Returned for the audit log rather than derived at the call site: a loop
/// that quietly merges past a required check must say which one and on what
/// grounds, every time, or it is indistinguishable from one that is broken.
#[must_use]
pub fn waived(
    required: &[String],
    stuck: &BTreeSet<String>,
    policy: &BlockingPolicy,
) -> Vec<String> {
    required
        .iter()
        .filter_map(|name| {
            if policy.ignore.iter().any(|ignored| ignored == name) {
                Some(format!("{name} (named in `ignore`)"))
            } else if stuck.contains(name) {
                Some(format!(
                    "{name} (failed on the last {} base commits — not something a \
                     pull request can fix)",
                    policy.stuck_after
                ))
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    /// A check red on every recent base commit stops blocking.
    ///
    /// **The witness for "a billing failure is not a code failure."** A blocked
    /// Vercel account, an exhausted quota or a revoked token fails identically
    /// to a broken test — `FAILURE`, with nothing in the name to tell them
    /// apart. What tells them apart is that a code breakage gets fixed within a
    /// commit or two and an infrastructure one never does.
    #[test]
    fn a_check_red_on_every_recent_base_commit_stops_blocking() {
        let history = vec![
            names(&["Vercel"]),
            names(&["Vercel"]),
            names(&["Vercel"]),
            names(&[]),
        ];
        let policy = BlockingPolicy::default();
        let stuck = stuck_on_base(&history, policy.stuck_after);

        assert!(stuck.contains("Vercel"));
        assert_eq!(
            blocking(&names(&["fmt + clippy + test", "Vercel"]), &stuck, &policy),
            names(&["fmt + clippy + test"]),
            "the unwinnable check is waived and the real one still blocks"
        );
    }

    /// One green run in the window means it is still fixable.
    ///
    /// The conservative half: a flaky test that passed once recently is not
    /// infrastructure, and must keep blocking. Without this, the first
    /// intermittent failure to appear three times running would be waived
    /// forever.
    #[test]
    fn a_check_that_went_green_even_once_still_blocks() {
        let history = vec![
            names(&["flaky"]),
            names(&[]), // green here
            names(&["flaky"]),
            names(&["flaky"]),
        ];
        let stuck = stuck_on_base(&history, DEFAULT_STUCK_AFTER);
        assert!(
            stuck.is_empty(),
            "one green run means a diff can still fix it"
        );
    }

    /// A base breakage the loop is about to fix must not be waived.
    ///
    /// This is the interaction with the self-heal doctrine, and getting it
    /// wrong in this direction is the expensive one: a single bad merge makes
    /// one commit red, and waiving on that evidence would let the loop merge
    /// straight past the breakage it is supposed to go and repair.
    #[test]
    fn a_single_broken_commit_is_not_yet_unwinnable() {
        let history = vec![names(&["fmt + clippy + test"]), names(&[]), names(&[])];
        let stuck = stuck_on_base(&history, DEFAULT_STUCK_AFTER);
        assert!(stuck.is_empty());
    }

    /// Too little history proves nothing.
    #[test]
    fn a_short_history_infers_nothing() {
        let history = vec![names(&["Vercel"]), names(&["Vercel"])];
        assert!(stuck_on_base(&history, DEFAULT_STUCK_AFTER).is_empty());
    }

    /// The operator outranks the inference, and needs no evidence.
    ///
    /// Somebody who already knows their account is suspended should not have
    /// to wait three commits for the loop to work it out.
    #[test]
    fn an_operator_can_name_a_check_that_never_blocks() {
        let policy = BlockingPolicy {
            ignore: names(&["Vercel"]),
            ..BlockingPolicy::default()
        };
        let stuck = BTreeSet::new();
        assert_eq!(
            blocking(&names(&["Vercel", "fmt + clippy + test"]), &stuck, &policy),
            names(&["fmt + clippy + test"])
        );
    }

    /// Disabling the inference leaves only the operator's list.
    #[test]
    fn stuck_after_zero_disables_the_inference() {
        let history = vec![names(&["Vercel"]); 9];
        assert!(stuck_on_base(&history, 0).is_empty());
    }

    /// Every waiver names itself and its grounds.
    ///
    /// A loop that merges past a required check without saying which one, and
    /// why, is indistinguishable from one that is simply broken.
    #[test]
    fn a_waiver_says_which_check_and_on_what_grounds() {
        let policy = BlockingPolicy {
            ignore: names(&["Vercel"]),
            stuck_after: 3,
        };
        let mut stuck = BTreeSet::new();
        stuck.insert("deploy preview".to_owned());

        let reasons = waived(
            &names(&["Vercel", "deploy preview", "fmt + clippy + test"]),
            &stuck,
            &policy,
        );

        assert_eq!(reasons.len(), 2, "the healthy check is not waived");
        assert!(reasons[0].contains("Vercel") && reasons[0].contains("ignore"));
        assert!(reasons[1].contains("deploy preview") && reasons[1].contains("last 3"));
    }
}
