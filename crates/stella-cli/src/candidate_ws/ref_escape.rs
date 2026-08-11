//! Shared-ref escape detection (#2541) — did this candidate write into the
//! ref namespace the GRADED tree depends on? A child module of
//! `candidate_ws`, like [`super::escape`], whose file-level sibling it is:
//! same seam (immediately around the seal), same best-effort contract, same
//! insistence on a signature a mid-run *user* action cannot forge.
//!
//! [`super::escape`] answers the question for file bytes. This one answers it
//! for refs, because candidate isolation isolates the working **tree** and not
//! the **ref namespace**: `git worktree add` gives the candidate its own
//! checkout but the *same* `.git`, so `refs/heads/*`, `refs/tags/*` and
//! `refs/stash` are one shared object the graded tree also reads. Git's own
//! interlock refuses to check out a branch another worktree holds, which is
//! why the observed failure had to defeat it explicitly
//! (`git checkout --ignore-other-worktrees master`) — after which every
//! subsequent commit moved the graded tree's `master` out from under its
//! index, and the graded tree's `git status` read as a staged reversal of the
//! entire task.
//!
//! # The signature
//!
//! A shared ref that simply *moved* proves nothing: a user committing in
//! their own checkout while a run is in flight moves refs too, and blaming a
//! candidate for that would be the false positive [`super::escape`] goes to
//! the same lengths to avoid. A move is this candidate's iff **the ref's new
//! value is work only this candidate has**, which is exactly two shapes:
//!
//! * the new value is an ancestor-or-equal of the candidate's live `HEAD` —
//!   the candidate committed *onto* the shared branch (the observed shape:
//!   after `--ignore-other-worktrees`, candidate `HEAD` and `refs/heads/master`
//!   are the same commit); or
//! * the candidate's private baseline commit is an ancestor of the new value —
//!   the candidate's snapshot history was *merged or grafted into* the shared
//!   ref, which is also how a banned `git stash` shows up (`refs/stash`'s
//!   first parent is the candidate's `HEAD`).
//!
//! Both are qualified by a third probe that removes the one collision either
//! would otherwise admit: a ref moved **backwards into history the graded
//! tree already had** (`git reset --hard HEAD~1` in the user's own checkout)
//! lands on a commit that is trivially an ancestor of the candidate's `HEAD`,
//! and is a user action, not an escape. So a divergence counts only when the
//! new value was *not* already reachable from the value recorded at creation.
//!
//! # Detection, then repair (#2641)
//!
//! An attributed escape is **put back**, not merely named: the refusal used
//! to quote a `git update-ref` and leave the graded tree exactly as corrupted
//! as it found it, which cost a whole measured run its score.
//! [`GitCandidateWorkspace::restore`] performs the restore and [`refusal`]
//! reports what it did.
//!
//! Three properties make that a repair rather than a second helping of
//! damage:
//!
//! * **Compare-and-swap, never force.** Every write names the value this
//!   candidate recorded at creation *and* the value the audit just read. A
//!   user who moved the same ref between those two moments loses the CAS, and
//!   the refusal says the ref is still moved — the one case where refusing is
//!   the correct answer. All non-stash refs ride in one
//!   `git update-ref --stdin` transaction, so a multi-ref escape restores
//!   all-or-nothing.
//! * **Nothing is destroyed.** A restore is one reflog entry, so the
//!   candidate's value stays reachable (`git reflog <ref>`) and the refusal
//!   names the command that reinstates it.
//! * **`refs/stash` is repaired with the stash's own primitive.**
//!   `git stash list` *is* the reflog of `refs/stash`, so an `update-ref`
//!   back to the recorded value leaves the candidate's entry AND a phantom
//!   entry in the user's stash list. [`GitCandidateWorkspace::restore_stash`]
//!   walks `git reflog delete --updateref --rewrite refs/stash@{0}` — what
//!   `git stash drop` itself runs — and re-attributes every entry before
//!   dropping it, so a stash the *user* pushed mid-run stops the walk instead
//!   of being destroyed by the repair.
//!
//! # Boundaries, accepted and documented
//!
//! Best-effort detection, like its sibling: every failed probe reports "no
//! escape" rather than failing a candidate on broken forensics, so this check
//! can only ever *add* a detection. A **deleted** ref carries no new value to
//! sign and is deliberately out of scope, as is a rewind to an unrelated
//! commit — for those the graded tree's own history is the record (#2812).
//! The structural fix — a candidate substrate that cannot reach the graded
//! tree's refs at all — is #1383.
//!
//! Restoring is not free of collateral and does not pretend to be: a commit
//! the *user* made mid-run on the escaped branch is unwound too when the
//! candidate built on top of it, and the attribution probes cannot tell the
//! two apart (the worker commits with the repository's own identity). The
//! graded tree's integrity is what is being protected, so the restore happens
//! anyway — and the refusal enumerates what it took out of that ref's history
//! rather than leaving the user to discover it.
//!
//! One coupling to know before changing either: the attribution probes run
//! against the REAL repository yet name the candidate's private `baseline` and
//! `HEAD`, which is only resolvable because the two share an object store —
//! the very property this module exists to police. A substrate that separates
//! the object stores (#1383) makes this module unnecessary *and* breaks its
//! probes in the same stroke; it must be retired with that change, not ported
//! across it.

use std::collections::BTreeMap;
use std::path::Path;

use tokio::io::AsyncWriteExt;

use super::{GitCandidateWorkspace, git};

/// Ceiling on refs *named* in one refusal. A candidate that moved more refs
/// than this has already made the point, and the message stays readable; the
/// tail is counted, never silently dropped.
const MAX_NAMED: usize = 8;

/// Ceiling on refs *attributed* — and therefore repairable — in one audit.
///
/// Deliberately larger than [`MAX_NAMED`], which used to do both jobs: under
/// a detection-only contract a shared cap only shortened a message, but under
/// a repair contract it silently meant "at most eight refs get put back"
/// (#2641). Anything past this ceiling is reported as unexamined by
/// [`refusal`], so the budget is stated rather than assumed. Each attributed
/// ref costs at most three probes here plus two forensic reads in
/// [`GitCandidateWorkspace::restore`], all on an aborting path.
const MAX_PROBED: usize = 64;

/// Commits enumerated per restored ref before the rest are counted.
const MAX_UNWOUND: usize = 5;

/// The shared ref whose reflog *is* a user-facing list, so it cannot be
/// repaired with `update-ref` — see the module doc and
/// [`GitCandidateWorkspace::restore_stash`].
const STASH_REF: &str = "refs/stash";

/// One shared ref whose new value is provably this candidate's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RefEscape {
    /// Full ref name, e.g. `refs/heads/master`.
    pub(super) name: String,
    /// The value recorded when this candidate was created — `None` when the
    /// candidate created the ref outright.
    pub(super) before: Option<String>,
    /// What the ref points at now.
    pub(super) after: String,
}

/// What one audit of the shared ref namespace found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RefAudit {
    /// Refs whose current value is provably this candidate's work.
    pub(super) escaped: Vec<RefEscape>,
    /// Refs that moved but sat beyond [`MAX_PROBED`], so they were never
    /// attributed and therefore never restored. Zero in every ordinary run;
    /// non-zero is a fact the refusal has to state rather than imply.
    pub(super) unexamined: usize,
}

/// What the repair actually did to one escaped ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Repaired {
    /// The ref is back at the value recorded at this candidate's creation.
    Restored,
    /// The ref this candidate created outright is gone.
    Deleted,
    /// The write was not performed and the ref is still moved — git lost the
    /// compare-and-swap because someone else moved it while we probed, or the
    /// ref store rejected the transaction. Never forced past: a repair that
    /// overwrites a value it did not predict is worse than the damage.
    Refused(String),
}

/// One escaped ref, what the repair did about it, and what that repair took
/// out of the ref's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Restored {
    pub(super) escape: RefEscape,
    pub(super) outcome: Repaired,
    /// What the ref no longer reaches. For an ordinary ref these are
    /// `git log --oneline <before>..<after>` lines, capped at
    /// [`MAX_UNWOUND`]; for [`STASH_REF`] they are the object ids of the
    /// stash entries the walk dropped, because a stash list is not a line of
    /// history to log.
    pub(super) unwound: Vec<String>,
    /// Commits past the ones [`Restored::unwound`] names.
    pub(super) unwound_more: usize,
    /// The recorded value is an ancestor of the escaped one: the restore
    /// rewinds along a single line of history rather than displacing a
    /// divergent one.
    pub(super) rewind: bool,
}

/// Forensics for one ref, read *before* the repair writes anything.
struct Unwound {
    commits: Vec<String>,
    more: usize,
    rewind: bool,
}

/// Every ref of `toplevel` and its object id, keyed by full ref name.
///
/// Read from the real repository, so it sees the shared namespace plus the
/// *main* worktree's own per-worktree refs — never a candidate's
/// `refs/worktree/*`, which is what keeps the pipeline's own
/// [`stella_tools::verify::WITNESS_BASELINE_WORKTREE_REF`] pin out of this
/// comparison by construction.
///
/// Best-effort: an unreadable ref store yields an empty map, which can only
/// make the later comparison quieter.
pub(super) async fn shared_refs(toplevel: &Path) -> BTreeMap<String, String> {
    let Ok(listing) = git(
        toplevel,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
    )
    .await
    else {
        return BTreeMap::new();
    };
    listing
        .lines()
        .filter_map(|line| {
            // Ref names cannot contain a space, so the first one splits the
            // record exactly.
            let (oid, name) = line.trim_end().split_once(' ')?;
            Some((name.to_string(), oid.to_string()))
        })
        .collect()
}

/// `git merge-base --is-ancestor <ancestor> <descendant>` in `repo`.
///
/// Exit 0 is "yes"; git spells "no" as exit 1 and a broken argument as exit
/// 128, and [`git`] flattens both into `Err` — which is the conservative
/// answer for every caller here, since a failed probe must never manufacture
/// an escape.
async fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    git(repo, &["merge-base", "--is-ancestor", ancestor, descendant])
        .await
        .is_ok()
}

/// One git command against `repo` with `input` fed to its stdin.
///
/// [`git`] runs through the fleet's `SystemGitCli`, whose `run` takes no
/// stdin — and `update-ref --stdin` is the only primitive that makes a
/// multi-ref restore ONE transaction (a batch with a single failed
/// compare-and-swap rolls back entirely). A repair that half-lands is not a
/// shape the graded tree can survive, so this repeats that helper's process
/// discipline instead: explicit `-C` targeting, hook-exported `GIT_*`
/// scrubbed, sensitive env scrubbed, no terminal prompt.
async fn git_stdin(repo: &Path, args: &[&str], input: &str) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    for var in stella_tools::exec::GIT_REPO_ENV_VARS {
        cmd.env_remove(var);
    }
    stella_tools::subprocess_env::scrub_sensitive_env(&mut cmd);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn `git {}`: {e}", args.join(" ")))?;
    // Taken (not borrowed) so the pipe is closed by the drop at the end of
    // this block: `update-ref --stdin` reads to EOF before it commits the
    // transaction, so holding the handle open would deadlock the wait below.
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("`git {}` gave no stdin pipe", args.join(" ")))?;
        stdin
            .write_all(input.as_bytes())
            .await
            .map_err(|e| format!("could not write to `git {}`: {e}", args.join(" ")))?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("`git {}` did not complete: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Detach `HEAD` in `dir` if the worker attached it to a branch.
///
/// The seal is machinery: it must land in the candidate's private history and
/// nowhere else. When a worker has pointed the candidate's `HEAD` at a shared
/// branch, an ordinary `git commit` moves that branch instead — which is how
/// witness scaffolding reached a branch the user keeps (#2541). Rewriting
/// `HEAD` itself (`--no-deref`) to the commit it already resolves to detaches
/// it without touching the index or a single file in the worktree, so the
/// candidate's work is exactly as the worker left it.
///
/// A `HEAD` that is symbolic but unresolvable (a worker's `git checkout
/// --orphan`) is an error, not a silent pass: committing from there would
/// mint a new shared branch, which is the very thing this prevents.
pub(super) async fn detach_head(dir: &Path) -> Result<(), String> {
    // `symbolic-ref -q HEAD` exits non-zero exactly when HEAD is detached.
    if git(dir, &["symbolic-ref", "-q", "HEAD"]).await.is_err() {
        return Ok(());
    }
    let head = git(dir, &["rev-parse", "HEAD"])
        .await
        .map_err(|e| format!("candidate HEAD is attached to a branch with no commit on it: {e}"))?;
    git(dir, &["update-ref", "--no-deref", "HEAD", head.trim()])
        .await
        .map(|_| ())
}

impl GitCandidateWorkspace {
    /// Shared refs of the real repository whose current value is provably
    /// this candidate's work — see the module docs for the signature.
    ///
    /// Cost on a well-behaved candidate: exactly one `git` invocation (the
    /// ref enumeration against the real repo), because nothing has moved and
    /// no attribution probe is ever paid.
    pub(super) async fn escaped_refs_inner(&self) -> RefAudit {
        let now = shared_refs(&self.toplevel).await;
        let moved: Vec<(&String, &String)> = now
            .iter()
            .filter(|(name, after)| self.refs_at_create.get(*name) != Some(*after))
            .collect();
        if moved.is_empty() {
            return RefAudit::default();
        }
        let unexamined = moved.len().saturating_sub(MAX_PROBED);
        // The candidate's live HEAD, read once: the first signature compares
        // every moved ref against it.
        let Ok(head) = git(&self.dir, &["rev-parse", "HEAD"]).await else {
            // Broken forensics report no finding at all — including no
            // unexamined tail, which would otherwise assert a fact this
            // audit never established.
            return RefAudit::default();
        };
        let head = head.trim().to_string();

        let mut escaped = Vec::new();
        for (name, after) in moved.into_iter().take(MAX_PROBED) {
            let before = self.refs_at_create.get(name);
            // A ref rewound into history the graded tree already had is the
            // user's own doing — screen it out before either signature can
            // read it as candidate work.
            if let Some(before) = before
                && is_ancestor(&self.toplevel, after, before).await
            {
                continue;
            }
            let committed_onto = is_ancestor(&self.toplevel, after, &head).await;
            let grafted_in =
                !committed_onto && is_ancestor(&self.toplevel, &self.baseline, after).await;
            if committed_onto || grafted_in {
                escaped.push(RefEscape {
                    name: name.clone(),
                    before: before.cloned(),
                    after: after.clone(),
                });
            }
        }
        RefAudit {
            escaped,
            unexamined,
        }
    }

    /// Put the graded tree's refs back where this candidate found them.
    ///
    /// Forensics are read first, while the escaped values are still the ones
    /// on disk; then every ref but [`STASH_REF`] rides in one
    /// `update-ref --stdin` transaction, and the stash gets the `git stash
    /// drop` primitive it requires. Returns one [`Restored`] per input, in
    /// input order, for [`refusal`] to report.
    pub(super) async fn restore(&self, escaped: &[RefEscape]) -> Vec<Restored> {
        let mut forensics = Vec::with_capacity(escaped.len());
        for esc in escaped {
            forensics.push(self.unwound_by(esc).await);
        }

        // One transaction for every ordinary ref. `-m` stamps the reflog, so
        // the value this candidate wrote stays reachable after the restore.
        let reflog_msg = format!(
            "stella: restore ref moved by candidate {}",
            self.dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "(unnamed)".to_string())
        );
        let mut batch = String::new();
        for esc in escaped.iter().filter(|e| e.name != STASH_REF) {
            match &esc.before {
                Some(before) => {
                    batch.push_str(&format!("update {} {} {}\n", esc.name, before, esc.after));
                }
                None => batch.push_str(&format!("delete {} {}\n", esc.name, esc.after)),
            }
        }
        let batch_outcome = if batch.is_empty() {
            None
        } else {
            Some(
                git_stdin(
                    &self.toplevel,
                    &["update-ref", "-m", &reflog_msg, "--stdin"],
                    &batch,
                )
                .await,
            )
        };

        let mut out = Vec::with_capacity(escaped.len());
        for (esc, mut unwound) in escaped.iter().zip(forensics) {
            let outcome = if esc.name == STASH_REF {
                let (outcome, dropped) = self.restore_stash(esc).await;
                unwound.more = 0;
                unwound.commits = dropped;
                outcome
            } else {
                match &batch_outcome {
                    // The transaction is all-or-nothing, so one answer covers
                    // every ref in it.
                    Some(Ok(_)) if esc.before.is_some() => Repaired::Restored,
                    Some(Ok(_)) => Repaired::Deleted,
                    Some(Err(reason)) => Repaired::Refused(reason.clone()),
                    None => Repaired::Refused("no restore was attempted".to_string()),
                }
            };
            out.push(Restored {
                escape: esc.clone(),
                outcome,
                unwound: unwound.commits,
                unwound_more: unwound.more,
                rewind: unwound.rewind,
            });
        }
        out
    }

    /// What restoring `esc` takes out of that ref's history — read before any
    /// write, so the enumeration describes the tree the user actually has.
    ///
    /// Two probes for an ordinary moved ref, none for a ref the candidate
    /// created (its whole history is the single value the refusal already
    /// names) or for [`STASH_REF`] (whose entries the drop walk reports).
    async fn unwound_by(&self, esc: &RefEscape) -> Unwound {
        let empty = Unwound {
            commits: Vec::new(),
            more: 0,
            rewind: false,
        };
        if esc.name == STASH_REF {
            return empty;
        }
        let Some(before) = &esc.before else {
            return empty;
        };
        let rewind = is_ancestor(&self.toplevel, before, &esc.after).await;
        let range = format!("{before}..{}", esc.after);
        let commits: Vec<String> = git(
            &self.toplevel,
            &[
                "log",
                "--oneline",
                "--no-decorate",
                &format!("-n{MAX_UNWOUND}"),
                &range,
            ],
        )
        .await
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim_end().to_string())
        .filter(|line| !line.is_empty())
        .collect();
        let total = git(&self.toplevel, &["rev-list", "--count", &range])
            .await
            .ok()
            .and_then(|out| out.trim().parse::<usize>().ok())
            .unwrap_or(commits.len());
        Unwound {
            more: total.saturating_sub(commits.len()),
            commits,
            rewind,
        }
    }

    /// Undo this candidate's stash pushes the way `git stash drop` does.
    ///
    /// `git stash list` IS the reflog of [`STASH_REF`], so restoring it with
    /// `update-ref` leaves the candidate's entry *and* a phantom entry in the
    /// user's stash list — a repair that introduces new damage. The primitive
    /// `git stash drop` runs is `git reflog delete --updateref --rewrite
    /// refs/stash@{0}`, and that is what this walks.
    ///
    /// Every entry is re-attributed with the module's own signature before it
    /// is dropped. A stash the *user* pushed mid-run is not this candidate's
    /// to destroy, so the walk stops at the first foreign entry and reports a
    /// partial repair rather than clearing the list to reach the recorded
    /// value. The reflog snapshot bounds the loop: `reflog delete` on an
    /// already-empty reflog exits zero, so a "drop until it matches" loop
    /// would not terminate on its own.
    ///
    /// Returns the outcome plus the object ids actually dropped.
    async fn restore_stash(&self, esc: &RefEscape) -> (Repaired, Vec<String>) {
        let entries: Vec<String> = git(
            &self.toplevel,
            &["reflog", "show", "--format=%H", STASH_REF],
        )
        .await
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

        let mut dropped: Vec<String> = Vec::new();
        for entry in &entries {
            // Reached the value recorded at this candidate's creation: every
            // entry below it is the user's and the ref is already back.
            if esc.before.as_deref() == Some(entry.as_str()) {
                break;
            }
            if !is_ancestor(&self.toplevel, &self.baseline, entry).await {
                return (
                    Repaired::Refused(format!(
                        "`{STASH_REF}@{{0}}` ({entry}) is not this candidate's work, so the walk \
                         stopped rather than destroying a stash entry the user pushed; {} \
                         entr{} of this candidate's were dropped first",
                        dropped.len(),
                        if dropped.len() == 1 { "y" } else { "ies" }
                    )),
                    dropped,
                );
            }
            if let Err(reason) = git(
                &self.toplevel,
                &[
                    "reflog",
                    "delete",
                    "--updateref",
                    "--rewrite",
                    "refs/stash@{0}",
                ],
            )
            .await
            {
                return (Repaired::Refused(reason), dropped);
            }
            dropped.push(entry.clone());
        }

        match &esc.before {
            Some(before) => match git(&self.toplevel, &["rev-parse", STASH_REF]).await {
                Ok(now) if now.trim() == before => (Repaired::Restored, dropped),
                Ok(now) => (
                    Repaired::Refused(format!(
                        "`{STASH_REF}` is at {} after the drop walk, not the recorded {before}",
                        now.trim()
                    )),
                    dropped,
                ),
                Err(reason) => (Repaired::Refused(reason), dropped),
            },
            // The candidate created the stash outright. The walk emptied the
            // reflog — so `git stash list` is already empty — but the last
            // `reflog delete` leaves the ref itself behind, dangling. Real
            // `git stash drop` removes it at exactly this point, and so must
            // this, or the next `git stash push` would stack onto a list the
            // user cannot see.
            None => match git(&self.toplevel, &["rev-parse", STASH_REF]).await {
                Ok(now) => {
                    match git(&self.toplevel, &["update-ref", "-d", STASH_REF, now.trim()]).await {
                        Ok(_) => (Repaired::Deleted, dropped),
                        Err(reason) => (Repaired::Refused(reason), dropped),
                    }
                }
                // Already gone — nothing left to delete.
                Err(_) => (Repaired::Deleted, dropped),
            },
        }
    }
}

/// What one repaired ref took out of that ref's history, rendered for the
/// refusal — empty when the restore dropped nothing (a fast-forward the user
/// never had, or a ref the candidate created).
fn unwound_clause(entry: &Restored) -> String {
    if entry.unwound.is_empty() && entry.unwound_more == 0 {
        return String::new();
    }
    let more = if entry.unwound_more > 0 {
        format!(" and {} more", entry.unwound_more)
    } else {
        String::new()
    };
    format!(", unwinding {}{more}", entry.unwound.join(", "))
}

/// The seal refusal for a candidate that moved refs the graded tree shares.
///
/// Reports what the repair actually **did** (#2641): which refs are back,
/// which the candidate created and are now gone, which git refused to move
/// because someone else had already moved them, and what each restore took
/// out of that ref's history. `unexamined` is the moved-ref tail
/// [`GitCandidateWorkspace::escaped_refs_inner`] never attributed, stated
/// rather than implied.
pub(super) fn refusal(restored: &[Restored], unexamined: usize) -> String {
    let mut named: Vec<String> = Vec::new();
    let mut reinstate: Vec<String> = Vec::new();
    let mut recovery: Vec<String> = Vec::new();
    for entry in restored.iter().take(MAX_NAMED) {
        let esc = &entry.escape;
        match &entry.outcome {
            Repaired::Restored => {
                let before = esc.before.as_deref().unwrap_or("its recorded value");
                let how = if entry.rewind {
                    "rewound from"
                } else {
                    "displaced from the divergent"
                };
                named.push(format!(
                    "{} restored to {before} ({how} {}{})",
                    esc.name,
                    esc.after,
                    unwound_clause(entry)
                ));
                reinstate.push(format!(
                    "git update-ref {} {} {before}",
                    esc.name, esc.after
                ));
            }
            Repaired::Deleted => {
                named.push(format!(
                    "{} deleted — this candidate created it at {}{}",
                    esc.name,
                    esc.after,
                    unwound_clause(entry)
                ));
                reinstate.push(format!("git update-ref {} {}", esc.name, esc.after));
            }
            Repaired::Refused(reason) => {
                let was = match &esc.before {
                    Some(before) => format!("was {before}, "),
                    None => "created, ".to_string(),
                };
                named.push(format!(
                    "{} NOT restored ({reason}) — {was}still {}",
                    esc.name, esc.after
                ));
                recovery.push(match &esc.before {
                    Some(before) => {
                        format!("git update-ref {} {before} {}", esc.name, esc.after)
                    }
                    None => format!("git update-ref -d {} {}", esc.name, esc.after),
                });
            }
        }
    }
    if restored.len() > MAX_NAMED {
        let tail = &restored[MAX_NAMED..];
        let refused = tail
            .iter()
            .filter(|e| matches!(e.outcome, Repaired::Refused(_)))
            .count();
        named.push(match refused {
            0 => format!("and {} more, all put back", tail.len()),
            n => format!("and {} more, {n} of them NOT restored", tail.len()),
        });
    }

    let mut out = format!(
        "this candidate moved refs the graded tree shares: {}. A candidate worktree isolates the \
         working tree but not the ref namespace — it shares `.git` with the real repository, so a \
         ref write escapes isolation even though every file write is contained.",
        named.join("; ")
    );
    if !reinstate.is_empty() {
        out.push_str(&format!(
            " Nothing was destroyed: each restore is one reflog entry, so the candidate's value is \
             still reachable and `{}` reinstates it.",
            reinstate.join(" && ")
        ));
    }
    if !recovery.is_empty() {
        out.push_str(&format!(
            " The refs above that are still moved need `{}`.",
            recovery.join(" && ")
        ));
    }
    if unexamined > 0 {
        out.push_str(&format!(
            " {unexamined} further moved ref(s) were past this audit's probe budget of \
             {MAX_PROBED} and were neither attributed nor restored."
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use stella_pipeline::ports::CandidateWorkspace;
    use stella_tools::RegistryOptions;

    use super::super::tests::{scaffold, scratch_git};
    use super::super::{GitCandidateWorkspaces, git};
    use super::{RefEscape, Repaired, Restored, refusal};

    /// One [`Restored`] with no unwound history, for the pure message tests.
    fn entry(name: &str, before: Option<&str>, after: &str, outcome: Repaired) -> Restored {
        Restored {
            escape: RefEscape {
                name: name.to_string(),
                before: before.map(str::to_string),
                after: after.to_string(),
            },
            outcome,
            unwound: Vec::new(),
            unwound_more: 0,
            rewind: true,
        }
    }

    fn port(root: std::path::PathBuf) -> GitCandidateWorkspaces {
        GitCandidateWorkspaces::new(
            root,
            RegistryOptions::default(),
            Default::default(),
            Vec::new(),
            crate::rules::ResolvedRules::default(),
        )
    }

    /// The branch the scaffold's `git init` produced — `master` or `main`
    /// depending on the host's `init.defaultBranch`.
    fn default_branch(root: &std::path::Path) -> String {
        scratch_git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
            .trim()
            .to_string()
    }

    /// The observed Terminal-Bench `fix-git` shape (#2541): the worker
    /// defeats git's worktree interlock, commits onto the branch the graded
    /// tree has checked out, and the graded tree's ref moves out from under
    /// its index. The seal must refuse and name the ref.
    #[tokio::test]
    async fn committing_onto_the_graded_trees_branch_refuses_the_seal() {
        let root = scaffold("refesc-hijack");
        let branch = default_branch(&root);
        let ws = port(root.clone()).create_workspace().await.unwrap();

        // Exactly the observed escape: git refuses a plain checkout of a
        // branch another worktree holds, so the worker overrides it.
        scratch_git(
            ws.dir(),
            &["checkout", "--ignore-other-worktrees", "-q", &branch],
        );
        std::fs::write(ws.dir().join("hijack.txt"), "worker\n").unwrap();
        scratch_git(ws.dir(), &["add", "hijack.txt"]);
        scratch_git(ws.dir(), &["commit", "-q", "-m", "worker commit"]);

        let err = ws.seal().await.unwrap_err().to_string();
        assert!(
            err.contains(&format!("refs/heads/{branch}")),
            "the refusal must name the moved ref: {err}"
        );
        assert!(
            err.contains("git update-ref"),
            "the refusal must spell out recovery: {err}"
        );

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// The other half of #2541: witness scaffolding reached a branch the user
    /// keeps because the seal committed wherever the worker had left `HEAD`.
    /// The seal detaches first, so the shared branch cannot receive it — and
    /// the candidate's own work is untouched by the detach.
    #[tokio::test]
    async fn the_seal_never_lands_on_a_branch_the_user_keeps() {
        let root = scaffold("refesc-detach");
        let branch = default_branch(&root);
        let ws = port(root.clone()).create_workspace().await.unwrap();
        let branch_ref = format!("refs/heads/{branch}");
        let before = scratch_git(&root, &["rev-parse", &branch_ref])
            .trim()
            .to_string();

        // The worker attaches HEAD but commits nothing: the only thing that
        // would move the shared branch is Stella's own seal.
        scratch_git(
            ws.dir(),
            &["checkout", "--ignore-other-worktrees", "-q", &branch],
        );
        std::fs::write(ws.dir().join("witness_lost_changes.sh"), "#!/bin/sh\n").unwrap();

        ws.seal().await.expect("an unescaped candidate still seals");

        assert_eq!(
            scratch_git(&root, &["rev-parse", &branch_ref]).trim(),
            before,
            "the seal must not move a branch the graded tree depends on"
        );
        assert!(
            git(ws.dir(), &["symbolic-ref", "-q", "HEAD"])
                .await
                .is_err(),
            "the seal must leave the candidate's HEAD detached"
        );
        // The detach is metadata only — the worker's file is still there and
        // is what the seal captured.
        assert!(ws.dir().join("witness_lost_changes.sh").exists());
        assert!(
            ws.sealed_is_unchanged().await.unwrap(),
            "the seal captured the candidate's tree as the worker left it"
        );

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// A user committing in their own checkout while a run is in flight moves
    /// a shared ref too. That is the false positive this signature exists to
    /// exclude — the candidate must still seal.
    #[tokio::test]
    async fn a_mid_run_user_commit_is_not_a_ref_escape() {
        let root = scaffold("refesc-user-commit");
        let branch = format!("refs/heads/{}", default_branch(&root));
        let ws = port(root.clone()).create_workspace().await.unwrap();

        std::fs::write(root.join("user.txt"), "user\n").unwrap();
        scratch_git(&root, &["add", "user.txt"]);
        scratch_git(&root, &["commit", "-q", "-m", "the user's own commit"]);
        let theirs = scratch_git(&root, &["rev-parse", &branch]);

        ws.seal()
            .await
            .expect("a user's own commit must never fail a candidate's seal");

        // #2641 gave the guard a write, so "not an escape" now has a second
        // half: the repair must not fire either. A restore here would silently
        // delete the user's commit from their own branch.
        assert_eq!(
            scratch_git(&root, &["rev-parse", &branch]),
            theirs,
            "the user's own commit must survive the seal"
        );
        ws.remove().await;
        assert_eq!(
            scratch_git(&root, &["rev-parse", &branch]),
            theirs,
            "and must survive teardown, which audits and repairs too"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A branch rewound into history the graded tree already had is trivially
    /// an ancestor of the candidate's HEAD — and is still the user's doing.
    #[tokio::test]
    async fn a_mid_run_user_rewind_is_not_a_ref_escape() {
        let root = scaffold("refesc-user-rewind");
        // Two commits so there is somewhere to rewind to.
        std::fs::write(root.join("second.txt"), "second\n").unwrap();
        scratch_git(&root, &["add", "second.txt"]);
        scratch_git(&root, &["commit", "-q", "-m", "second"]);
        let branch = format!("refs/heads/{}", default_branch(&root));
        let ws = port(root.clone()).create_workspace().await.unwrap();

        scratch_git(&root, &["reset", "--hard", "-q", "HEAD~1"]);
        let theirs = scratch_git(&root, &["rev-parse", &branch]);

        ws.seal()
            .await
            .expect("a user's own rewind must never fail a candidate's seal");

        // The repair must not "helpfully" undo the user's rewind: restoring
        // the value recorded at creation would put back the commit they just
        // deliberately dropped.
        assert_eq!(
            scratch_git(&root, &["rev-parse", &branch]),
            theirs,
            "the user's own rewind must survive the seal"
        );
        ws.remove().await;
        assert_eq!(
            scratch_git(&root, &["rev-parse", &branch]),
            theirs,
            "and must survive teardown, which audits and repairs too"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The module's other signature: candidate history reaching a shared ref
    /// without the candidate ever committing onto it. `git stash` is the
    /// everyday shape — `refs/stash`'s first parent is the candidate's HEAD —
    /// and the stash is shared machine state the module doc bans outright.
    #[tokio::test]
    async fn stashing_from_a_candidate_refuses_the_seal() {
        let root = scaffold("refesc-stash");
        let ws = port(root.clone()).create_workspace().await.unwrap();

        std::fs::write(ws.dir().join("tracked.txt"), "base\ndirty\ncandidate\n").unwrap();
        scratch_git(ws.dir(), &["stash", "push", "-q", "-m", "escape"]);

        let err = ws.seal().await.unwrap_err().to_string();
        assert!(
            err.contains("refs/stash"),
            "the refusal must name the stash ref: {err}"
        );

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// A candidate that touches no ref pays one enumeration and seals.
    #[tokio::test]
    async fn an_ordinary_candidate_seals_with_no_ref_findings() {
        let root = scaffold("refesc-clean");
        let ws = port(root.clone()).create_workspace().await.unwrap();

        std::fs::write(ws.dir().join("answer.js"), "const answer = 42;\n").unwrap();
        ws.seal().await.unwrap();

        assert_eq!(ws.escaped_refs_inner().await, Default::default());

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// #2641, the headline: a refused ref escape must leave the graded tree
    /// as it found it, not merely describe how. The observed #2541 damage is
    /// exactly a moved `refs/heads/<branch>` under an unchanged index, so
    /// `status --porcelain` reading back byte-identically is the proof that
    /// the graded tree is whole again — not just that one oid matched.
    #[tokio::test]
    async fn a_refused_ref_escape_restores_the_graded_trees_branch() {
        let root = scaffold("refesc-restore");
        let branch = default_branch(&root);
        let branch_ref = format!("refs/heads/{branch}");
        let ws = port(root.clone()).create_workspace().await.unwrap();

        let ref_before = scratch_git(&root, &["rev-parse", &branch_ref]);
        let status_before = scratch_git(&root, &["status", "--porcelain"]);

        scratch_git(
            ws.dir(),
            &["checkout", "--ignore-other-worktrees", "-q", &branch],
        );
        std::fs::write(ws.dir().join("hijack.txt"), "worker\n").unwrap();
        scratch_git(ws.dir(), &["add", "hijack.txt"]);
        scratch_git(ws.dir(), &["commit", "-q", "-m", "worker commit"]);
        assert_ne!(
            scratch_git(&root, &["rev-parse", &branch_ref]),
            ref_before,
            "the fixture must actually move the graded tree's branch"
        );

        let err = ws.seal().await.unwrap_err().to_string();
        assert!(err.contains(&format!("{branch_ref} restored to")), "{err}");

        assert_eq!(
            scratch_git(&root, &["rev-parse", &branch_ref]),
            ref_before,
            "the refusal must put the graded tree's branch back, not just name it"
        );
        assert_eq!(
            scratch_git(&root, &["status", "--porcelain"]),
            status_before,
            "restoring the ref must un-stage the phantom reversal the escape created"
        );
        // Nothing was destroyed: the restore is one reflog entry, so the
        // worker's commit is still reachable through it.
        assert!(
            scratch_git(&root, &["reflog", "show", &branch_ref]).contains("stella: restore ref"),
            "the restore must leave a reflog trail"
        );

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// The test that makes the repair correct rather than merely present.
    /// `git stash list` IS the reflog of `refs/stash`, so a naive
    /// `update-ref` restore leaves a phantom entry AND the candidate's escape
    /// entry in the user's list — the repair would introduce new damage.
    /// Character-identical is the only assertion that catches it.
    #[tokio::test]
    async fn a_refused_stash_escape_leaves_the_users_stash_list_untouched() {
        let root = scaffold("refesc-stash-restore");
        let ws = port(root.clone()).create_workspace().await.unwrap();

        // The scaffold seeds one pre-existing user stash; that listing is
        // what must survive byte-for-byte.
        let list_before = scratch_git(&root, &["stash", "list"]);
        assert!(
            list_before.contains("fixture"),
            "the scaffold must seed a user stash for this test to mean anything"
        );
        let stash_before = scratch_git(&root, &["rev-parse", "refs/stash"]);

        std::fs::write(ws.dir().join("tracked.txt"), "base\ndirty\ncandidate\n").unwrap();
        scratch_git(ws.dir(), &["stash", "push", "-q", "-m", "escape"]);
        assert!(
            scratch_git(&root, &["stash", "list"]).contains("escape"),
            "the fixture must actually reach the user's stash list"
        );

        let err = ws.seal().await.unwrap_err().to_string();
        assert!(err.contains("refs/stash restored to"), "{err}");

        assert_eq!(
            scratch_git(&root, &["stash", "list"]),
            list_before,
            "the stash repair must leave the user's list character-identical"
        );
        assert_eq!(
            scratch_git(&root, &["rev-parse", "refs/stash"]),
            stash_before
        );

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// The other stash shape: the candidate created `refs/stash` where the
    /// user had none. Emptying the reflog is not enough — the last
    /// `reflog delete` leaves the ref itself behind, dangling, and a repo with
    /// a `refs/stash` whose reflog is empty shows an empty `git stash list`
    /// while the next `git stash push` stacks onto it. Real `git stash drop`
    /// deletes the ref at that point and so must the repair.
    #[tokio::test]
    async fn a_created_stash_is_removed_ref_and_all() {
        let root = scaffold("refesc-stash-created");
        // Drop the scaffold's fixture entry so the candidate's push is the
        // one that brings `refs/stash` into existence.
        scratch_git(&root, &["stash", "drop", "-q"]);
        assert!(
            scratch_git(&root, &["stash", "list"]).is_empty(),
            "the fixture stash must be gone before the candidate runs"
        );
        let ws = port(root.clone()).create_workspace().await.unwrap();

        std::fs::write(ws.dir().join("tracked.txt"), "base\ndirty\ncandidate\n").unwrap();
        scratch_git(ws.dir(), &["stash", "push", "-q", "-m", "escape"]);

        let err = ws.seal().await.unwrap_err().to_string();
        assert!(err.contains("refs/stash deleted"), "{err}");

        assert!(
            scratch_git(&root, &["stash", "list"]).is_empty(),
            "the user's stash list must be empty again"
        );
        assert!(
            !root.join(".git/refs/stash").exists(),
            "the dangling `refs/stash` must be removed, not merely emptied"
        );

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// Option B (#2641): the seal-time repair only fires when a seal is
    /// reached, and the run that motivated this issue died on the turn budget
    /// without ever sealing. Teardown audits and repairs too, so a candidate
    /// dropped without a seal still leaves the graded tree whole.
    #[tokio::test]
    async fn a_candidate_torn_down_without_a_seal_still_restores_the_branch() {
        let root = scaffold("refesc-teardown");
        let branch = default_branch(&root);
        let branch_ref = format!("refs/heads/{branch}");
        let ws = port(root.clone()).create_workspace().await.unwrap();

        let ref_before = scratch_git(&root, &["rev-parse", &branch_ref]);
        let status_before = scratch_git(&root, &["status", "--porcelain"]);

        scratch_git(
            ws.dir(),
            &["checkout", "--ignore-other-worktrees", "-q", &branch],
        );
        std::fs::write(ws.dir().join("hijack.txt"), "worker\n").unwrap();
        scratch_git(ws.dir(), &["add", "hijack.txt"]);
        scratch_git(ws.dir(), &["commit", "-q", "-m", "worker commit"]);
        assert_ne!(scratch_git(&root, &["rev-parse", &branch_ref]), ref_before);

        // No `seal()` at all — the shape a turn-budget death takes.
        ws.remove().await;

        assert_eq!(
            scratch_git(&root, &["rev-parse", &branch_ref]),
            ref_before,
            "teardown must restore a ref the candidate moved even with no seal"
        );
        assert_eq!(
            scratch_git(&root, &["status", "--porcelain"]),
            status_before
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Hazard (a) of the repair, pinned: restoring a branch from the MAIN
    /// worktree drags an attached candidate `HEAD` back with it, orphaning
    /// the candidate's own commits from its own checkout. The seal detaches
    /// before it restores, so the candidate's work stays exactly where the
    /// worker left it — which is what makes the refusal recoverable rather
    /// than merely destructive.
    #[tokio::test]
    async fn a_refused_escape_leaves_the_candidates_own_work_reachable() {
        let root = scaffold("refesc-candidate-work");
        let branch = default_branch(&root);
        let ws = port(root.clone()).create_workspace().await.unwrap();

        scratch_git(
            ws.dir(),
            &["checkout", "--ignore-other-worktrees", "-q", &branch],
        );
        std::fs::write(ws.dir().join("hijack.txt"), "worker\n").unwrap();
        scratch_git(ws.dir(), &["add", "hijack.txt"]);
        scratch_git(ws.dir(), &["commit", "-q", "-m", "worker commit"]);
        let worker_commit = scratch_git(ws.dir(), &["rev-parse", "HEAD"])
            .trim()
            .to_string();

        ws.seal().await.unwrap_err();

        assert!(
            ws.dir().join("hijack.txt").exists(),
            "the repair must not touch the candidate's working tree"
        );
        assert_eq!(
            scratch_git(ws.dir(), &["rev-parse", "HEAD"]).trim(),
            worker_commit,
            "the candidate's HEAD must still be the worker's commit, not the restored value"
        );
        assert!(
            git(ws.dir(), &["symbolic-ref", "-q", "HEAD"])
                .await
                .is_err(),
            "the candidate's HEAD must be detached, which is what pinned its work"
        );

        ws.remove().await;
        std::fs::remove_dir_all(&root).ok();
    }

    /// #2641 changed what this sentence is: the ref the candidate created is
    /// GONE, so the message reports a deletion rather than prescribing one.
    #[test]
    fn the_refusal_names_a_created_ref_it_deleted() {
        let reason = refusal(
            &[entry(
                "refs/heads/scratch",
                None,
                "abc1234",
                Repaired::Deleted,
            )],
            0,
        );
        assert!(
            reason.contains("refs/heads/scratch deleted — this candidate created it at abc1234"),
            "{reason}"
        );
        // The candidate's value is still reachable, and the message says how
        // to put it back deliberately.
        assert!(
            reason.contains("git update-ref refs/heads/scratch abc1234"),
            "{reason}"
        );
        assert!(!reason.contains("NOT restored"), "{reason}");
    }

    /// The restored case: the value recorded at creation is where the ref is
    /// now, and the escaped value is named as the thing it was rewound from.
    #[test]
    fn the_refusal_reports_the_restore_it_performed() {
        let mut e = entry(
            "refs/heads/master",
            Some("1111111"),
            "2222222",
            Repaired::Restored,
        );
        e.unwound = vec!["2222222 worker commit".to_string()];
        e.unwound_more = 3;
        let reason = refusal(&[e], 0);
        assert!(
            reason.contains("refs/heads/master restored to 1111111 (rewound from 2222222"),
            "{reason}"
        );
        assert!(
            reason.contains("unwinding 2222222 worker commit and 3 more"),
            "{reason}"
        );
        // Reinstating is the *candidate's* value back over the restored one.
        assert!(
            reason.contains("git update-ref refs/heads/master 2222222 1111111"),
            "{reason}"
        );
    }

    /// A restore that lost the compare-and-swap must not claim a repair it did
    /// not perform, and must still hand over the command that would do it.
    #[test]
    fn the_refusal_admits_a_ref_it_could_not_restore() {
        let reason = refusal(
            &[entry(
                "refs/heads/master",
                Some("1111111"),
                "2222222",
                Repaired::Refused("someone else moved it".to_string()),
            )],
            0,
        );
        assert!(
            reason.contains(
                "refs/heads/master NOT restored (someone else moved it) — was 1111111, still \
                 2222222"
            ),
            "{reason}"
        );
        assert!(
            reason.contains("git update-ref refs/heads/master 1111111 2222222"),
            "{reason}"
        );
        assert!(!reason.contains("Nothing was destroyed"), "{reason}");
    }

    /// A divergent restore is not a rewind, and the message must not call it
    /// one — the user is losing a line of history, not replaying one.
    #[test]
    fn the_refusal_distinguishes_a_displacement_from_a_rewind() {
        let mut e = entry(
            "refs/heads/master",
            Some("1111111"),
            "2222222",
            Repaired::Restored,
        );
        e.rewind = false;
        let reason = refusal(&[e], 0);
        assert!(
            reason.contains("displaced from the divergent 2222222"),
            "{reason}"
        );
    }

    /// The probe budget is a fact about what was repaired, so it is stated —
    /// never left for the reader to infer from a short list (#2641).
    #[test]
    fn the_refusal_states_the_moved_refs_it_never_examined() {
        let reason = refusal(
            &[entry(
                "refs/heads/master",
                Some("1111111"),
                "2222222",
                Repaired::Restored,
            )],
            5,
        );
        assert!(
            reason.contains("5 further moved ref(s) were past this audit's probe budget of 64"),
            "{reason}"
        );
        assert!(
            reason.contains("neither attributed nor restored"),
            "{reason}"
        );
    }

    /// Past [`MAX_NAMED`] the message counts rather than lists — but a ref it
    /// could not restore must survive the truncation as a number.
    #[test]
    fn the_refusal_counts_the_refs_past_the_naming_cap() {
        let mut all: Vec<Restored> = (0..10)
            .map(|i| {
                entry(
                    &format!("refs/heads/b{i}"),
                    Some("1111111"),
                    "2222222",
                    Repaired::Restored,
                )
            })
            .collect();
        all[9].outcome = Repaired::Refused("raced".to_string());
        let reason = refusal(&all, 0);
        assert!(reason.contains("refs/heads/b7 restored"), "{reason}");
        assert!(!reason.contains("refs/heads/b8 restored"), "{reason}");
        assert!(
            reason.contains("and 2 more, 1 of them NOT restored"),
            "{reason}"
        );
    }
}
