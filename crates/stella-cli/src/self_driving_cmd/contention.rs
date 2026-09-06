//! Who else looks like they are already on this issue (#4002, #4300).
//!
//! [`stella_autonomy::contention_verdict`] weighs four signals and the
//! operator's [`ContentionPolicy`] decides what they mean. Gathering them is
//! this module's job, and it does it for **two callers with different
//! questions**:
//!
//! - [`for_base_fix`] asks *before adopting a broken base*. There, another
//!   self-driving process's worktree is the whole point of the worktree
//!   signal: two loops against one clone would otherwise both adopt the same
//!   breakage and race to fix it.
//! - [`for_issue`] asks *before claiming an issue off the ranked queue*. There,
//!   a worktree inside this verb's own root may be **this loop's own crashed
//!   run**, and deferring on it retires the issue permanently.
//!
//! # Why the difference is an argument and not a second copy
//!
//! The two probes read the same `git ls-remote` and `git worktree list`
//! output, so the gathering lives here once — [`gather`] — and the policy
//! difference is its `own_root` argument, visible at both call sites rather
//! than buried in a filter one of them cannot see. #4300 asks for exactly this
//! shape: the claim site must not defer on the loop's own leftovers, and the
//! base-fix site must keep seeing every worktree.
//!
//! The parsing is split out into pure functions ([`branches_naming`],
//! [`worktrees_naming`], [`prs_naming`], [`claims_naming`]) for the ordinary
//! reason — the reads need a subprocess and the decisions do not, so only the
//! decisions can be tested.
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

/// Every contention signal for one issue key, **at claim time**.
///
/// Drops worktrees under this verb's own root, because there a leftover may be
/// the loop's own crashed run and `work::start`'s `discard_undelivered_attempt`
/// is what repairs it — one step further on than a deferral ever reaches
/// (#4300).
#[must_use]
pub(super) fn for_issue(root: &Path, key: &str) -> Contention {
    gather(root, key, Some(&super::work::worktrees_root(root)))
}

/// Every contention signal for one issue key, **before adopting a broken
/// base**.
///
/// Keeps every worktree. A peer self-driving process is precisely the case
/// this signal exists for: two loops against one clone would otherwise both
/// adopt the same breakage and race to fix it, and unlike the claim site there
/// is no recovery downstream that repairs a leftover of the loop's own.
#[must_use]
pub(super) fn for_base_fix(root: &Path, key: &str) -> Contention {
    gather(root, key, None)
}

/// The four reads, with the one policy difference as an argument.
///
/// Each is cheap, and asked only for the one issue in question — not for the
/// whole ranked queue. Every one of them fails **open**: a probe that cannot
/// answer contributes no evidence rather than a deferral, because "the forge
/// is unreachable" is not a peer and a loop meant to run for days cannot treat
/// a network blip as somebody else's work.
#[must_use]
fn gather(root: &Path, key: &str, own_root: Option<&Path>) -> Contention {
    let mut contention = Contention::default();

    if let Some(out) = git(root, &["ls-remote", "--heads", "origin"]) {
        contention.remote_branches = branches_naming(&out, key);
    }

    if let Ok(raw) = super::deliver::prs_matching(key) {
        contention.open_prs = raw;
    }

    if let Some(out) = git(root, &["worktree", "list", "--porcelain"]) {
        contention.local_worktrees = worktrees_naming(&out, key, own_root);
    }

    contention.ledger_claims = ledger_claims(root, key);

    contention
}

/// Whether `text` mentions `key` as an issue number, rather than as a run of
/// digits sitting inside a longer one.
///
/// `contains` is the wrong test for a number. Issue 43 is not issue 4300, and a
/// bare substring cannot tell them apart — so a loop asking about #43 read
/// every branch, worktree and claim naming #430, #4300 and #1436 as somebody
/// else working its issue, and deferred. The lower the issue number, the more
/// of the backlog matched it.
///
/// A match counts only where neither neighbouring character is a digit, which
/// is what makes `i-43` a mention and `i-4300` not one.
///
/// The rule holds only over text somebody wrote. It cannot tell that part of a
/// name is a hash. There the neighbours are hex letters, so every number is
/// spelled sooner or later by accident. Callers reading a slug cut that suffix
/// off with [`stella_fleet::strip_slug_hash`] first.
#[must_use]
fn names_issue(text: &str, key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    text.match_indices(key).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + key.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_digit()) && !after.is_some_and(|c| c.is_ascii_digit())
    })
}

/// Remote branch names carrying `key`, from `git ls-remote --heads` output.
///
/// Remote only at both call sites: a *local* branch of the loop's own is not
/// another actor, and the branch-collision guard in `work::start` is what
/// speaks for those.
///
/// It is also the only cheap signal that reaches another machine. So the deck
/// pushes the branch its start-work opens, and its test reads this function.
#[must_use]
pub(crate) fn branches_naming(ls_remote: &str, key: &str) -> Vec<String> {
    ls_remote
        .lines()
        // The ref first, then the test. `git ls-remote` prints
        // `<sha>\trefs/heads/<name>`, so testing the whole line tests the
        // commit sha as well — and a sha is forty hex characters, which for a
        // one-digit key contains it about nine times in ten. The loop then
        // deferred on a branch whose name does not carry the key at all, and
        // reported that name as its evidence.
        .filter_map(|line| line.split("refs/heads/").nth(1))
        .map(str::trim)
        // The stem, not the hash on the end of it. That suffix is sixteen hex
        // characters of FNV-1a, and hex is mostly letters, so `names_issue`'s
        // digit-neighbour rule lets a number inside one through: issue 18
        // matched the `18` in issue 11's branch `stella/11-8fdade18d3a4de74`
        // and was deferred on every pass for hours, because a deferral writes
        // no `spent` entry. The evidence string keeps the whole name — an
        // operator looking for the branch needs the one git will show them.
        .filter(|name| names_issue(stella_fleet::strip_slug_hash(name), key))
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
        // Still the whole path, not its last component: a peer laying its
        // checkouts out differently must still be seen, and a false negative
        // here is two loops on one issue where a false positive is only a
        // deferral. [`names_issue`] removes the part that was simply wrong —
        // #43 matching a worktree for #4300 — and `strip_slug_hash` the part
        // that was wrong for the same reason one directory further down: a
        // worktree's leaf is a slug, so the hash sits on the end of the path
        // and answers for any number that happens to fall inside it.
        .filter(|path| names_issue(stella_fleet::strip_slug_hash(path), key))
        .filter(|path| own_root.is_none_or(|own| !Path::new(path).starts_with(own)))
        .map(str::to_owned)
        .collect()
}

/// Pull request numbers from a `gh pr list --json number,title,body` payload,
/// keeping the ones whose text names `key` as an issue.
///
/// The search that produced this payload is a full-text one on the bare
/// number, so the forge answers with every open pull request that mentions it
/// for any reason — "43 files changed", "cuts latency by 43%". Each of those
/// deferred issue #43, and a deferral writes no `spent` entry, so it deferred
/// again on the next pass. `deliver::open_prs_for_issue`, in the same file,
/// already searched the precise form; this one did not.
///
/// A mention is `#<key>` with no digit after it, so `#43` names issue 43 and
/// `#4300` does not. The loop writes `Closes #<key>` into every pull request it
/// opens, so its own always match.
///
/// Two tolerances, both in the direction that keeps a signal rather than losing
/// one: a row missing `number` is skipped rather than failing the whole read,
/// and a row carrying **neither** `title` nor `body` counts as contention. This
/// build cannot tell whether such a row names the issue, and silently dropping
/// a contention signal it could not read is what ends with two loops on one
/// issue.
#[must_use]
pub(super) fn prs_naming(raw: &str, key: &str) -> Vec<String> {
    let Ok(rows) = serde_json::from_str::<Vec<serde_json::Value>>(raw) else {
        return Vec::new();
    };
    let reference = format!("#{key}");
    rows.iter()
        .filter(|row| {
            let title = row.get("title").and_then(serde_json::Value::as_str);
            let body = row.get("body").and_then(serde_json::Value::as_str);
            match (title, body) {
                (None, None) => true,
                _ => [title, body]
                    .into_iter()
                    .flatten()
                    .any(|text| names_issue(text, &reference)),
            }
        })
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
    claims_naming(&claims, key, &super::claim::owner())
}

/// The live claims that name this issue and are somebody else's, as evidence
/// strings.
///
/// The key comes from [`issue_claim_key`], the same function
/// [`super::claim::acquire`] takes the lease under, so the reader and the
/// producer cannot drift into two namespaces that never meet. Matched
/// **exactly** rather than by substring, which is the one thing worth pinning
/// here: `issue:41` answering for `issue:410` would defer a real issue on
/// somebody else's unrelated work.
///
/// `own_owner` is the mirror of [`worktrees_naming`]'s `own_root`, and exists
/// for the same reason: **contention is other people.** Now that this loop
/// mints the claims it also reads, a probe that counted its own would be the
/// #4300 shape again through the authoritative signal — the loop deferring
/// forever on evidence only it produces. Nothing reaches that today: the
/// candidate filter in `drive` keeps a claimed or spent key out of the queue.
/// It is here so "the loop never defers on its own lease" is a property of
/// this function, not a lucky side effect of a filter two modules away that a
/// later edit could take back.
///
/// It is the *owner string* that is compared, not liveness: a crashed run of
/// this loop had a different pid, so its lapsing lease is somebody else's
/// claim as far as this is concerned, and it stops mattering by expiring.
/// A recycled pid can make a dead run's live claim read as this process's
/// own — which is the recovery that case wants anyway.
///
/// [`issue_claim_key`]: stella_fleet::issue_claim_key
#[must_use]
pub(super) fn claims_naming(
    claims: &[stella_fleet::DispatchClaim],
    key: &str,
    own_owner: &str,
) -> Vec<String> {
    let wanted = stella_fleet::issue_claim_key(key);
    claims
        .iter()
        .filter(|claim| claim.claim_key == wanted)
        .filter(|claim| claim.owner != own_owner)
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
        let leftover = own.join("stella-4300-8f6b50dc").display().to_string();
        let seen = porcelain(&[leftover.as_str()]);

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
        // it is holding. Minted under another owner, because that is what
        // makes it a peer rather than this process.
        let peer = super::super::claim::acquire_as(root.path(), "4300", "self-driving:99999");
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

        let _peer = super::super::claim::acquire_as(root.path(), "4300", "self-driving:99999");
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

    /// A commit sha is not a branch name, and must not be searched as one.
    ///
    /// `git ls-remote` prints `<sha>\trefs/heads/<name>`. Testing the whole
    /// line for the key tested forty hex characters of sha as well: for a
    /// one-digit key that is a match roughly nine times in ten, so nearly every
    /// remote branch counted as contention and the loop stopped taking
    /// low-numbered issues. The evidence it recorded was a branch name that did
    /// not carry the key at all.
    #[test]
    fn a_sha_that_happens_to_contain_the_key_is_not_a_branch_naming_it() {
        // Two real-shaped shas, both full of digits; neither branch names #7.
        let out = "4a7b39c17d2e4f5061829304a5b6c7d8e9f00112\trefs/heads/main\n\
                   77770000111122223333444455556666777788ab\trefs/heads/docs/typo\n";

        assert!(
            branches_naming(out, "7").is_empty(),
            "no branch here names issue 7"
        );
    }

    /// The hash on a slug is not a name, and must not be searched as one.
    ///
    /// The live one. `stella/11-8fdade18d3a4de74` is issue 11's branch on
    /// `macanderson/rainforest`, left behind when its pull request merged. The
    /// `18` inside its hash has a letter on each side, so the digit-neighbour
    /// rule passed it and issue 18 — the only claimable issue in that repo —
    /// read as somebody else's work. A deferral writes no `spent` entry, so
    /// the next pass asked the same question and got the same answer, for
    /// hours.
    #[test]
    fn a_hash_that_happens_to_contain_the_key_is_not_a_branch_naming_it() {
        let out = "5e998e0dacbae5481cea564051a52acbbd0570ff\t\
                   refs/heads/stella/11-8fdade18d3a4de74\n";

        assert!(
            branches_naming(out, "18").is_empty(),
            "this branch is issue 11's; the 18 is inside its hash"
        );
        // The control: the branch still answers for the issue it is named for.
        assert_eq!(
            branches_naming(out, "11"),
            vec!["stella/11-8fdade18d3a4de74"],
            "and the evidence keeps the whole name, hash included"
        );
    }

    /// The same collision one directory down, where a worktree path ends in a
    /// slug and every leftover of a run carries nearly the same digits.
    #[test]
    fn a_hash_in_a_worktree_path_is_not_that_path_naming_the_key() {
        let porcelain = "worktree /repo/.stella/private/self-driving/11-8fdade18d3a4de74\n";

        assert!(
            worktrees_naming(porcelain, "18", None).is_empty(),
            "this worktree is issue 11's"
        );
        assert_eq!(worktrees_naming(porcelain, "11", None).len(), 1);
    }

    /// A name nobody generated keeps matching exactly as it did.
    ///
    /// The fix is a narrower scan, not a rule about what a branch may be
    /// called: a human's checkout and a preserved branch mint no slug, so
    /// stripping a hash must not stop either of them counting.
    #[test]
    fn stripping_the_hash_does_not_stop_a_human_or_preserved_branch_matching() {
        let out = "abc\trefs/heads/wip-43-preserved\n\
                   def\trefs/heads/fix-43\n\
                   012\trefs/heads/stella/43-8fdade18d3a4de74\n";

        assert_eq!(
            branches_naming(out, "43"),
            vec!["wip-43-preserved", "fix-43", "stella/43-8fdade18d3a4de74"]
        );
        assert!(
            worktrees_naming("worktree /home/dev/checkouts/issue-43\n", "43", None).len() == 1,
            "a person's checkout is still a peer"
        );
    }

    /// Issue 43 is not issue 4300.
    #[test]
    fn a_longer_issue_number_is_not_a_mention_of_its_prefix() {
        let out = "abc\trefs/heads/self-driving/i-4300\n\
                   def\trefs/heads/self-driving/i-43\n\
                   fed\trefs/heads/self-driving/i-143\n";

        assert_eq!(
            branches_naming(out, "43"),
            vec!["self-driving/i-43"],
            "only the branch naming 43 itself"
        );
    }

    /// The same rule for worktree paths.
    #[test]
    fn a_worktree_for_a_longer_issue_is_not_contention_for_its_prefix() {
        let seen = porcelain(&[
            "/w/.stella/private/self-driving/i-4300",
            "/w/.stella/private/self-driving/i-43",
        ]);

        assert_eq!(
            worktrees_naming(&seen, "43", None),
            vec!["/w/.stella/private/self-driving/i-43".to_string()]
        );
    }

    /// A pull request that merely contains the number is not about the issue.
    ///
    /// The search behind this payload is full-text on the bare number, so the
    /// forge returns everything mentioning it. Counting all of them deferred
    /// the issue, and a deferral writes no `spent` entry, so it deferred again
    /// every pass.
    #[test]
    fn a_pull_request_that_only_mentions_the_number_is_not_contention() {
        let raw = r#"[
            {"number":11,"title":"perf: cuts latency by 43%","body":"no issue here"},
            {"number":12,"title":"43 files changed","body":"a sweep"},
            {"number":13,"title":"fix: the thing","body":"Closes #43"}
        ]"#;

        assert_eq!(
            prs_naming(raw, "43"),
            vec!["13".to_string()],
            "only the one that names the issue"
        );
    }

    /// `#43` is not `#4300`.
    #[test]
    fn a_longer_issue_reference_is_not_a_mention_of_its_prefix() {
        let raw = r#"[
            {"number":11,"title":"fix","body":"Closes #4300"},
            {"number":12,"title":"fix","body":"Refs #43, and more"}
        ]"#;

        assert_eq!(prs_naming(raw, "43"), vec!["12".to_string()]);
    }

    /// A row this build cannot read counts as contention rather than vanishing.
    ///
    /// The two errors are not symmetric: keeping a row the build cannot
    /// classify costs a deferral, and dropping it costs two loops working one
    /// issue. A forge that stops returning `title` and `body` must not silently
    /// switch this signal off.
    #[test]
    fn a_row_with_no_text_at_all_is_kept() {
        assert_eq!(
            prs_naming(r#"[{"number":7}]"#, "43"),
            vec!["7".to_string()],
            "neither field present: this build cannot tell, so it does not drop it"
        );
        // A row with text that simply does not name the issue is still dropped.
        assert!(prs_naming(r#"[{"number":7,"title":"unrelated"}]"#, "43").is_empty());
        // And a row with no number is skipped rather than failing the read.
        assert_eq!(
            prs_naming(
                r#"[{"title":"Closes #43"},{"number":9,"body":"Closes #43"}]"#,
                "43"
            ),
            vec!["9".to_string()]
        );
        assert!(prs_naming("not json at all", "43").is_empty());
    }

    /// One claim, as the ledger would hand it back.
    fn claim(claim_key: &str, owner: &str) -> stella_fleet::DispatchClaim {
        stella_fleet::DispatchClaim {
            claim_key: claim_key.to_string(),
            owner: owner.to_string(),
            fence: 1,
            acquired_at_ms: 0,
            renewed_at_ms: 0,
            expires_at_ms: 900_000,
        }
    }

    /// A ledger claim is matched exactly: `issue:41` must not answer for
    /// `issue:410`, which a substring match would have it do.
    #[test]
    fn a_ledger_claim_matches_its_issue_and_no_longer_one() {
        let claims = [
            claim("issue:410", "run-b"),
            claim("issue:41", "run-b"),
            claim("task:41", "run-b"),
        ];

        assert_eq!(
            claims_naming(&claims, "41", "run-a"),
            vec!["issue:41 held by run-b"]
        );
    }

    /// The owner-side mirror of the own-root worktree exclusion (#4309):
    /// a claim this process holds is not another actor, and a peer's is —
    /// on the same key, in the same read.
    #[test]
    fn a_claim_of_our_own_is_not_contention_but_a_peers_is() {
        let claims = [claim("issue:41", "run-a"), claim("issue:41", "run-b")];

        assert_eq!(
            claims_naming(&claims, "41", "run-a"),
            vec!["issue:41 held by run-b"]
        );
        assert!(claims_naming(&[claim("issue:41", "run-a")], "41", "run-a").is_empty());
    }
}
