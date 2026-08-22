//! Who else looks like they are already on this issue (#4002, #4300).
//!
//! [`stella_autonomy::contention_verdict`] weighs four signals and the
//! operator's [`ContentionPolicy`] decides what they mean. Gathering them is
//! this module's job, and it does it for **two callers with different
//! questions**:
//!
//! - `deliver::base_fix_contention` asks *before adopting a broken base*.
//!   There, another self-driving process's worktree is the whole point of the
//!   worktree signal: two loops against one clone would otherwise both adopt
//!   the same breakage and race to fix it.
//! - [`for_issue`] asks *before claiming an issue off the ranked queue*. There,
//!   a worktree inside this verb's own root is very often **this loop's own
//!   crashed run**, and deferring on it retires the issue permanently.
//!
//! # Why the difference is an argument and not a second copy
//!
//! The two probes read the same `git ls-remote` and `git worktree list`
//! output, so the parsing lives here once, pure, and the policy difference is
//! [`worktrees_naming`]'s `own_root` argument — visible at both call sites
//! rather than buried in a filter one of them cannot see. #4300 asks for
//! exactly this shape: the claim site must not defer on the loop's own
//! leftovers, and the base-fix site must keep seeing every worktree.
//!
//! # Why an own-root worktree is not evidence at claim time
//!
//! A crashed run leaves a registered worktree at
//! `.stella/private/self-driving/stella-<key>-<slug>`, whose path carries the
//! issue key. Nothing about that path distinguishes it from a live peer's —
//! at claim time the probe reads only names. What *does* distinguish them is
//! that the recovery already exists one step later:
//! `work::start`'s `discard_undelivered_attempt` asks the forge whether the
//! attempt delivered, and clears the branch and worktree only when it did not.
//! Deferring here means that recovery is never reached, and because a deferral
//! writes no `spent` entry it happens again on every pass — the issue is
//! unclaimable until a human sweeps up.
//!
//! So the claim site drops **only** worktrees under this verb's own root, and
//! nothing else. An actor that is genuinely somebody else still shows up: in a
//! worktree outside that root (a human's checkout, a fleet worktree, the
//! `wip-<key>-preserved` branch #4002 was filed from), in a remote branch, or
//! in an open pull request.
//!
//! # And why that exclusion needs [`super::claim`] to be sound
//!
//! A live peer self-driving process against the same clone puts its worktree
//! under **the same root**, so the exclusion drops that one too. Read as a
//! path question this is unanswerable, and dropping it is how #4300's fix
//! would take away the deferral #4300 explicitly asks to keep.
//!
//! It is answerable as a *liveness* question, and [`ledger_claims`] is where
//! the answer comes from: the loop holds a fenced lease on `issue:<key>` for
//! as long as a turn is in flight, and stops holding it the moment it stops
//! heartbeating. A crashed run's lease lapses on its own; a live peer's does
//! not. That is why this probe reads the ledger and why [`super::claim`]
//! writes it — a reader with no producer is always empty, which would make
//! this module report *no contention at all* for the one actor it most needs
//! to see.
//!
//! [`ContentionPolicy`]: stella_autonomy::ContentionPolicy

use std::path::Path;

use stella_autonomy::Contention;

use super::state::git;

/// Every contention signal for one issue key, at claim time.
///
/// Four reads, each cheap, and only for the one candidate about to be
/// claimed — not for the whole ranked queue. Every one of them fails **open**:
/// a probe that cannot answer contributes no evidence rather than a deferral,
/// because "the forge is unreachable" is not a peer and a loop meant to run
/// for days cannot treat a network blip as somebody else's work.
#[must_use]
pub(super) fn for_issue(root: &Path, key: &str) -> Contention {
    let mut contention = Contention::default();

    if let Some(out) = git(root, &["ls-remote", "--heads", "origin"]) {
        contention.remote_branches = branches_naming(&out, key);
    }

    if let Ok(raw) = super::deliver::prs_matching(key) {
        contention.open_prs = raw;
    }

    if let Some(out) = git(root, &["worktree", "list", "--porcelain"]) {
        contention.local_worktrees =
            worktrees_naming(&out, key, Some(&super::work::worktrees_root(root)));
    }

    contention.ledger_claims = ledger_claims(root, key);

    contention
}

/// Remote branch names carrying `key`, from `git ls-remote --heads` output.
///
/// Remote only at both call sites: a *local* branch of the loop's own is not
/// another actor, and the branch-collision guard in `work::start` is what
/// speaks for those.
#[must_use]
pub(super) fn branches_naming(ls_remote: &str, key: &str) -> Vec<String> {
    ls_remote
        .lines()
        .filter(|line| line.contains(key))
        .filter_map(|line| line.split("refs/heads/").nth(1))
        .map(str::to_owned)
        .collect()
}

/// Worktree paths on this machine carrying `key`, from
/// `git worktree list --porcelain` output.
///
/// `own_root` is the policy, and the reason both callers can share one parser:
///
/// - `None` keeps every match. The base-fix caller passes this, because seeing
///   another self-driving process's checkout is the signal's stated purpose.
/// - `Some(root)` drops the ones inside that directory. The claim caller passes
///   this verb's worktrees root, so a crashed run of its own stops standing in
///   for a peer (#4300).
#[must_use]
pub(super) fn worktrees_naming(porcelain: &str, key: &str, own_root: Option<&Path>) -> Vec<String> {
    porcelain
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(str::trim)
        .filter(|path| path.contains(key))
        .filter(|path| own_root.is_none_or(|own| !Path::new(path).starts_with(own)))
        .map(str::to_owned)
        .collect()
}

/// Pull request numbers from a `gh pr list --json number` payload.
///
/// Tolerant of a row missing the field rather than failing the whole read: a
/// payload this build only partly understands still carries the numbers it
/// does understand, and dropping all of them would turn a forge change into a
/// silent loss of the strongest cheap signal.
#[must_use]
pub(super) fn pr_numbers(raw: &str) -> Vec<String> {
    let Ok(rows) = serde_json::from_str::<Vec<serde_json::Value>>(raw) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| row.get("number").and_then(serde_json::Value::as_u64))
        .map(|n| n.to_string())
        .collect()
}

/// Live fleet dispatch claims naming this issue.
///
/// The only *authoritative* signal — a lease with an owner and an expiry,
/// rather than a name that resembles the work — and, since the own-root
/// exclusion above, the only thing that can tell a live peer self-driving
/// process from this loop's own crashed run. It is also the only signal
/// [`ContentionPolicy::ClaimsOnly`] consults, so without it that policy could
/// never defer at the claim site and an operator-facing setting would be
/// silently inert.
///
/// **Live** is the word doing the work: [`live_dispatch_claims`] filters on
/// `expires_at_ms > now`, so a claim nothing is renewing stops counting
/// without anything having to clean it up. That is the property #4300 needs
/// and the reason this is a lease rather than a file.
///
/// Offline and read-only. A workspace with no ledger has never dispatched, so
/// no claims is the correct answer rather than an error; so is a ledger this
/// build cannot open, on the fail-open rule above.
///
/// [`ContentionPolicy::ClaimsOnly`]: stella_autonomy::ContentionPolicy::ClaimsOnly
/// [`live_dispatch_claims`]: stella_fleet::Ledger::live_dispatch_claims
pub(super) fn ledger_claims(root: &Path, key: &str) -> Vec<String> {
    let Ok(path) = stella_store::workspace_private_sqlite_path(root, "fleet.db") else {
        return Vec::new();
    };
    if !path.exists() {
        return Vec::new();
    }
    let Ok(ledger) = stella_fleet::Ledger::open(&path) else {
        return Vec::new();
    };
    let Some(now_ms) = super::claim::now_ms() else {
        return Vec::new();
    };
    let Ok(claims) = ledger.live_dispatch_claims(now_ms) else {
        return Vec::new();
    };
    claims_naming(&claims, key)
}

/// The live claims that name this issue, as evidence strings.
///
/// The key comes from [`issue_claim_key`], the same function
/// [`super::claim::acquire`] takes the lease under, so the reader and the
/// producer cannot drift into two namespaces that never meet. Matched
/// **exactly** rather than by substring, which is the one thing worth pinning
/// here: `issue:41` answering for `issue:410` would defer a real issue on
/// somebody else's unrelated work.
///
/// [`issue_claim_key`]: stella_fleet::issue_claim_key
#[must_use]
pub(super) fn claims_naming(claims: &[stella_fleet::DispatchClaim], key: &str) -> Vec<String> {
    let wanted = stella_fleet::issue_claim_key(key);
    claims
        .iter()
        .filter(|claim| claim.claim_key == wanted)
        .map(|claim| format!("{} held by {}", claim.claim_key, claim.owner))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use stella_autonomy::{ContentionPolicy, ContentionVerdict, contention_verdict};

    use super::*;

    /// `git worktree list --porcelain` as git actually prints it: a `worktree`
    /// line, then `HEAD`/`branch` lines, blank-separated.
    fn porcelain(paths: &[&str]) -> String {
        paths
            .iter()
            .map(|p| format!("worktree {p}\nHEAD 0000000000000000000000000000000000000000\n\n"))
            .collect()
    }

    /// #4300's first half: the loop's own crashed run is not a peer.
    ///
    /// A worktree under this verb's own root carries the issue key in its
    /// path, so before the `own_root` argument existed it was reported as
    /// contention and — under the default `Defer` — retired the issue on every
    /// pass, unreachably ahead of `work::start`'s recovery.
    #[test]
    fn a_crashed_runs_own_worktree_is_not_claim_time_contention() {
        let own = PathBuf::from("/repo/.stella/private/self-driving");
        let seen = porcelain(&[
            "/repo",
            "/repo/.stella/private/self-driving/stella-4300-8f6b50dc",
        ]);

        assert!(worktrees_naming(&seen, "4300", Some(&own)).is_empty());
    }

    /// #4300's second half: everybody else still defers.
    ///
    /// A checkout outside the loop's own root is another actor by
    /// construction — the `wip-<key>-preserved` branch #4002 was filed from
    /// was exactly this — and the exclusion must not reach it.
    #[test]
    fn a_peers_worktree_outside_the_loops_root_still_defers() {
        let own = PathBuf::from("/repo/.stella/private/self-driving");
        let seen = porcelain(&[
            "/repo",
            "/repo/.stella/private/self-driving/stella-4300-8f6b50dc",
            "/repo/.stella/worktrees/wip-4300-preserved",
        ]);

        assert_eq!(
            worktrees_naming(&seen, "4300", Some(&own)),
            vec!["/repo/.stella/worktrees/wip-4300-preserved".to_string()]
        );
    }

    /// The base-fix caller keeps the signal whole.
    ///
    /// `None` is not a default anybody drifted into: `doctrine.rs`'s
    /// `local_worktrees` doc names a peer self-driving process as the case it
    /// exists for, and #4300 makes not weakening that an explicit constraint.
    #[test]
    fn the_base_fix_caller_sees_every_worktree() {
        let seen = porcelain(&[
            "/repo",
            "/repo/.stella/private/self-driving/stella-4300-8f6b50dc",
        ]);

        assert_eq!(
            worktrees_naming(&seen, "4300", None),
            vec!["/repo/.stella/private/self-driving/stella-4300-8f6b50dc".to_string()]
        );
    }

    /// A path merely *starting with the same characters* is not inside the
    /// root. `starts_with` on `Path` compares components, which is what makes
    /// `…/self-driving-old/` a different directory rather than a prefix match.
    #[test]
    fn a_sibling_directory_with_a_prefixed_name_is_not_the_loops_root() {
        let own = PathBuf::from("/repo/.stella/private/self-driving");
        let seen = porcelain(&["/repo/.stella/private/self-driving-old/stella-4300-8f6b50dc"]);

        assert_eq!(
            worktrees_naming(&seen, "4300", Some(&own)),
            vec!["/repo/.stella/private/self-driving-old/stella-4300-8f6b50dc".to_string()]
        );
    }

    /// The whole point, composed: gathering plus the verdict.
    ///
    /// The filter is only worth anything if it changes what the machine
    /// decides, and the two halves live in different crates — so this asserts
    /// the decision rather than the intermediate list.
    #[test]
    fn the_verdict_proceeds_on_a_crashed_run_and_defers_on_a_peer() {
        let own = PathBuf::from("/repo/.stella/private/self-driving");
        let mine = porcelain(&["/repo/.stella/private/self-driving/stella-4300-8f6b50dc"]);
        let theirs = porcelain(&["/repo/.stella/worktrees/wip-4300-preserved"]);

        let own_only = Contention {
            local_worktrees: worktrees_naming(&mine, "4300", Some(&own)),
            ..Contention::default()
        };
        assert_eq!(
            contention_verdict(ContentionPolicy::Defer, &own_only),
            ContentionVerdict::Proceed
        );

        let peer = Contention {
            local_worktrees: worktrees_naming(&theirs, "4300", Some(&own)),
            ..Contention::default()
        };
        assert!(matches!(
            contention_verdict(ContentionPolicy::Defer, &peer),
            ContentionVerdict::Defer { .. }
        ));
    }

    /// #4300's second half, in the shape that actually happens: **a live peer
    /// whose worktree is inside the loop's own root**.
    ///
    /// This is the case the own-root exclusion cannot see and the case the
    /// issue names as the constraint. A peer self-driving process runs the
    /// same verb against the same clone, so its worktree is
    /// `.stella/private/self-driving/stella-<key>-<slug>` — byte-identical in
    /// shape to the crashed run's, and dropped by the same filter. Testing
    /// the exclusion against a peer *outside* that root proves only that the
    /// filter has an edge; it does not prove a live peer still defers.
    ///
    /// What separates them is liveness, held in the ledger by
    /// [`super::super::claim`]. So the two halves are asserted here against
    /// one indistinguishable worktree path, differing only in whether a lease
    /// is live: crashed → `Proceed`, live peer → `Defer`.
    #[test]
    fn a_live_peer_in_the_same_root_defers_where_a_crashed_run_proceeds() {
        let root = tempfile::tempdir().expect("tempdir");
        let own = super::super::work::worktrees_root(root.path());
        // The one path. Both runs leave exactly this.
        let seen = porcelain(&[&own.join("stella-4300-8f6b50dc").to_string_lossy()]);

        let gathered = |root: &std::path::Path| Contention {
            local_worktrees: worktrees_naming(&seen, "4300", Some(&own)),
            ledger_claims: ledger_claims(root, "4300"),
            ..Contention::default()
        };

        // The crashed run: its worktree is still registered and its lease is
        // not, because nothing has renewed it since the process died.
        assert_eq!(
            contention_verdict(ContentionPolicy::Defer, &gathered(root.path())),
            ContentionVerdict::Proceed,
            "a crashed run's leftover must not defer its own issue (#4300)"
        );

        // The live peer: same worktree, same root, same string — and a lease
        // it is holding.
        let peer = super::super::claim::acquire(root.path(), "4300");
        assert!(matches!(peer, super::super::claim::Claim::Granted(_)));

        assert!(
            matches!(
                contention_verdict(ContentionPolicy::Defer, &gathered(root.path())),
                ContentionVerdict::Defer { .. }
            ),
            "a live peer must still defer, worktree exclusion or not (#4300)"
        );

        // And it is the peer's claim that carries it, not a leftover name:
        // dropping the lease returns the verdict to `Proceed`.
        drop(peer);
        assert_eq!(
            contention_verdict(ContentionPolicy::Defer, &gathered(root.path())),
            ContentionVerdict::Proceed
        );
    }

    /// The same peer, under the policy that trusts nothing else.
    ///
    /// `ClaimsOnly` weighs *only* ledger claims, so before a producer existed
    /// it could never defer at the claim site — an operator-facing setting
    /// that silently did nothing. This is what makes it real.
    #[test]
    fn claims_only_defers_on_a_live_peer_and_on_nothing_else() {
        let root = tempfile::tempdir().expect("tempdir");

        let names_only = Contention {
            remote_branches: vec!["stella/4300-fix".to_string()],
            local_worktrees: vec!["/elsewhere/4300".to_string()],
            ..Contention::default()
        };
        assert_eq!(
            contention_verdict(ContentionPolicy::ClaimsOnly, &names_only),
            ContentionVerdict::Proceed
        );

        let _peer = super::super::claim::acquire(root.path(), "4300");
        let claimed = Contention {
            ledger_claims: ledger_claims(root.path(), "4300"),
            ..Contention::default()
        };
        assert!(matches!(
            contention_verdict(ContentionPolicy::ClaimsOnly, &claimed),
            ContentionVerdict::Defer { .. }
        ));
    }

    /// Only branches naming the key, and only their short names.
    #[test]
    fn branches_naming_keeps_the_short_name_of_matching_heads() {
        let out = "abc\trefs/heads/stella/4300-fix\ndef\trefs/heads/stella/9999-other\n";

        assert_eq!(branches_naming(out, "4300"), vec!["stella/4300-fix"]);
    }

    /// A payload with a row this build cannot read still yields the rest.
    #[test]
    fn pr_numbers_survives_a_row_it_cannot_read() {
        assert_eq!(
            pr_numbers(r#"[{"number":7},{"title":"no number here"},{"number":9}]"#),
            vec!["7".to_string(), "9".to_string()]
        );
        assert!(pr_numbers("not json at all").is_empty());
    }

    /// A ledger claim is matched exactly: `issue:41` must not answer for
    /// `issue:410`, which a substring match would have it do.
    #[test]
    fn a_ledger_claim_matches_its_issue_and_no_longer_one() {
        let claim = |claim_key: &str| stella_fleet::DispatchClaim {
            claim_key: claim_key.to_string(),
            owner: "run-b".to_string(),
            fence: 1,
            acquired_at_ms: 0,
            renewed_at_ms: 0,
            expires_at_ms: 900_000,
        };
        let claims = [claim("issue:410"), claim("issue:41"), claim("task:41")];

        assert_eq!(claims_naming(&claims, "41"), vec!["issue:41 held by run-b"]);
    }
}
