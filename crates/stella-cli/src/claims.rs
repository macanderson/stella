//! Claim-on-first-write — coordinator-free write coordination over one
//! shared tree.
//!
//! Every writer in a workspace (the deck's lead, its sub-session workers,
//! shared-tree fleet workers — across processes and across fleets) wraps its
//! tool stack in a [`ClaimTap`]. The moment a tool would mutate a path, the
//! tap acquires that path's cooperative lock in the workspace store
//! (`file_locks` — a single atomic SQLite upsert, sub-millisecond). Two
//! rules make it invalidation-free:
//!
//! 1. **Lock on first write.** Nothing is declared up front; the claim set
//!    is discovered as the work unfolds, so emergent edits (the file the
//!    model realizes mid-task it must touch) are covered exactly like
//!    planned ones. A conflict fails FAST and NAMES the holder — the model
//!    reads the refusal and adapts (different file, or retry) instead of
//!    silently clobbering a sibling.
//! 2. **Hold to turn end, release in one statement.** Claims release when
//!    the holder's turn/worker settles (`release_all`, one DELETE by
//!    holder), never mid-edit — a file a worker is iterating on stays its
//!    own for the whole attempt. Crash hygiene is age-based: a claim older
//!    than [`STALE_CLAIM_MAX_AGE_SECS`] is swept at session start (a dead
//!    process cannot release its own).
//!
//! Two **transient** lanes sit beside the per-path claims — see
//! [`transient_lane`]. They serialize a tool for the duration of its CALL
//! (a bounded wait, not a hard refusal — lane contention is routine, edit
//! contention is signal), not to the end of the turn:
//!
//! - [`BUILD_CLAIM`] — build/test runners, so one worker's test run never
//!   observes a sibling's half-written edit.
//! - [`COMMIT_CLAIM`] — commit creation. One shared tree means one `HEAD`,
//!   and the fleet ledger has to name WHICH task moved it; the lane is what
//!   makes the answer decidable (#1216).
//!
//! A missing/failed store degrades to no coordination rather than no work —
//! the same observability-loss-not-work-stoppage contract as every other
//! store write.
//!
//! ## What a claim is taken on
//!
//! Two routes, because a tool's writes are knowable at two different moments.
//!
//! **Declared**, before the call: [`mutating_path`] reads the path out of the
//! call's own arguments, and the tap claims it or refuses. It keys on the tool
//! NAME, so it covers the shipped file tools `write_file`, `edit_file`,
//! `delete_file` and the conventional `apply_edits`, plus any workspace custom
//! tool adopting one of those names. An MCP tool can never match — its wire
//! name carries the `mcp__<server>__` prefix.
//!
//! **Observed**, after the call: a shell command's writes cannot be read out
//! of its arguments, and guessing them from a command line would refuse work
//! that was safe. So [`ShellWatch`] reads the work tree either side of a
//! `bash` call (`git status --porcelain -z`) and claims every path that
//! changed, exactly as the file tools would have. It NEVER refuses — by the
//! time anything is known the command has already run — so what it buys is the
//! warning the NEXT writer gets. A path a live rival already holds stays that
//! rival's.
//!
//! The observed route is armed only where writes land in the tree the
//! coordination store guards: the deck's lead and worker lanes, and a fleet
//! worker under [`Isolation::SharedTree`]. A worker in its own isolated
//! worktree has one writer and its own paths, so claiming them in the shared
//! table would refuse a sibling an edit to a file it cannot even see.
//!
//! Three gaps the observed route has, stated so nobody reads more into it:
//!
//! - **A workspace that is not a git repository takes no shell claim.** So
//!   does one where `git` is absent or its index is locked by a concurrent
//!   command. The snapshot fails, nothing is claimed, and the command runs.
//! - **Git's own view is the resolution.** An ignored path is invisible, and a
//!   write that changes neither a file's size nor its modification time reads
//!   as no write. A command that only moves a path in or out of the dirty set
//!   — a commit — claims it too; over-claiming costs a rival a wait, and
//!   under-claiming costs it an overwritten edit.
//! - **Two `git status` runs are spent per shell call**, tens of milliseconds
//!   on a repository of a few thousand files.
//!
//! [`Isolation::SharedTree`]: stella_fleet::Isolation::SharedTree

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::ToolExecutor;
use stella_fleet::{GitCli, Isolation};
use stella_protocol::{ToolOutput, ToolSchema};
use stella_store::Store;

/// The pseudo-path serializing build/test runs across every writer in the
/// workspace. Not a real file — `//` is unrepresentable as a workspace-
/// relative path, so it can never collide with a genuine claim.
pub(crate) const BUILD_CLAIM: &str = "//build";

/// The pseudo-path serializing commit CREATION across every writer in the
/// workspace — the same unrepresentable-path trick as [`BUILD_CLAIM`], for a
/// different reason.
///
/// Under a shared tree every worker commits onto one `HEAD`, and the fleet
/// ledger claims to be "the authoritative record" of which task made which
/// commit. Re-deriving that after the fact from `git log <start>..HEAD` reads
/// a sibling's interleaved commits as this worker's (#1216). The only
/// race-free answer is "the `HEAD` advance observed while this lane was
/// held", which is what [`crate::fleet_commits::CommitObserver`] reads —
/// sitting *inside* this tap so its window is exactly the lane's.
///
/// The two halves are joined by [`transient_lane`]: the observer watches the
/// tools this function routes here and no others, so a tool added to the arm
/// below is observed by construction rather than by memory.
pub(crate) const COMMIT_CLAIM: &str = "//commit";

/// How long a call waits for its transient lane before giving up.
const LANE_WAIT_MS: u64 = 60_000;
/// Poll cadence while waiting for a lane.
const LANE_POLL_MS: u64 = 500;
/// How many dead lane holders one acquire may reap before it stops
/// retrying immediately and falls back to the polled wait. Bounds the only
/// arm of the acquire loop that does not sleep.
const MAX_LANE_REAPS: u32 = 8;

/// The transient lane `name` serializes under for the duration of one call,
/// or `None` for a tool that needs no lane.
pub(crate) fn transient_lane(name: &str) -> Option<&'static str> {
    match name {
        "run_tests" | "build_project" | "diagnostics" | "run_lint" | "format_code"
        | "run_script" => Some(BUILD_CLAIM),
        "repo_commit" => Some(COMMIT_CLAIM),
        _ => None,
    }
}

/// The lane's name in a refusal a model reads. `//build` is a path, not
/// prose.
fn lane_label(lane: &str) -> &'static str {
    if lane == COMMIT_CLAIM {
        "commit"
    } else {
        "build/test"
    }
}

/// Claims older than this are swept at session start (crash hygiene).
pub(crate) const STALE_CLAIM_MAX_AGE_SECS: u64 = 6 * 3600;

/// The pid embedded in a holder identity's owner prefix — every writer's
/// identity is `<owner>/<lane-or-task>` where the owner ends in the minting
/// process's pid (`ses-<ms>-<pid>`, `fleet-<ms>-<pid>`). `None` when the
/// identity doesn't parse; the caller must then assume the holder is alive.
fn holder_pid(holder: &str) -> Option<u32> {
    holder.split('/').next()?.rsplit('-').next()?.parse().ok()
}

/// Whether `pid` is a live process — the same probe the session registry
/// uses for its dead-pid downgrade: `kill(pid, 0)`, with EPERM still
/// meaning alive. Elsewhere: assume alive (a stale refusal beats reaping a
/// live rival's claims).
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // `pid_t` is signed: a pid that doesn't fit must read as dead — an
        // `as` cast would wrap it negative, and `kill(-N, 0)` probes
        // process GROUP N, which can spuriously report alive.
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        if pid == 0 {
            return false;
        }
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// The conventional tool names whose successful call mutates the path in
/// their `path` input. No built-in carries these names; a workspace custom
/// tool that adopts one is claim-gated by construction (an MCP tool cannot —
/// its wire name is `mcp__`-prefixed), and anything else writes outside
/// claim tracking (the witness/verify ladder covers it).
fn mutating_path<'i>(name: &str, input: &'i Value) -> Option<&'i str> {
    match name {
        "write_file" | "edit_file" | "delete_file" => input.get("path").and_then(Value::as_str),
        // apply_edits carries its paths in a batch; the first edit's path
        // stands in for claim attribution — a multi-file batch is still one
        // claim-worthy mutation, and attributing it to one path beats leaving
        // it entirely unclaimed.
        "apply_edits" => input
            .get("edits")
            .and_then(Value::as_array)
            .and_then(|edits| edits.first())
            .and_then(|e| e.get("path"))
            .and_then(Value::as_str),
        _ => None,
    }
}

/// The tool whose writes are observed after the call rather than declared in
/// its arguments — the one shell on the built-in surface.
const SHELL_TOOL: &str = "bash";

/// Watches the work tree across one shell call so what the command turns out
/// to have written is claimed for its caller.
pub(crate) struct ShellWatch {
    git: Box<dyn GitCli>,
    /// The tree the snapshots are taken in, and the root a claim key is
    /// relative to.
    root: PathBuf,
}

impl ShellWatch {
    pub(crate) fn new(git: impl GitCli + 'static, root: impl Into<PathBuf>) -> Self {
        Self {
            git: Box::new(git),
            root: root.into(),
        }
    }

    /// The watch a fleet attempt gets. A shared-tree worker writes into the
    /// very tree the coordination store guards, so what its shell turns out
    /// to have written is claimed for it; an isolated worktree has one writer
    /// and paths of its own, and claiming those in the shared table would
    /// refuse a sibling an edit to a file it cannot see.
    pub(crate) fn for_attempt(
        isolation: Isolation,
        git: impl GitCli + 'static,
        root: impl Into<PathBuf>,
    ) -> Option<Self> {
        match isolation {
            Isolation::SharedTree => Some(Self::new(git, root)),
            Isolation::Isolated => None,
        }
    }

    /// Every path git calls dirty, with the mark that dates it. `None` when
    /// git could not answer — no `git` on `PATH`, a workspace that is not a
    /// repository, an index another command holds — and the caller then
    /// claims nothing rather than guessing.
    async fn snapshot(&self) -> Option<BTreeMap<String, Mark>> {
        let out = self
            .git
            .run(&self.root, &["status", "--porcelain", "-z"])
            .await
            .ok()?;
        if !out.success {
            return None;
        }
        Some(
            dirty_paths(&out.stdout)
                .into_iter()
                .map(|(status, path)| {
                    let stamp = stamp(&self.root.join(&path));
                    (path, Mark { status, stamp })
                })
                .collect(),
        )
    }
}

/// One dirty path as of a snapshot: git's two status letters, and the file's
/// own size and modification time.
///
/// The letters alone cannot see the SECOND write to a path — a file already
/// modified reads ` M` before and after the call — so the stamp is read
/// beside them. A write that leaves both size and modification time untouched
/// is still invisible; seeing it would mean hashing the tree on every shell
/// call, which costs more than the guarantee is worth.
#[derive(PartialEq, Eq)]
struct Mark {
    status: String,
    stamp: Option<(u64, SystemTime)>,
}

/// A path's size and modification time, or `None` where it cannot be read — a
/// deletion, or a root that is not the repository root. The status letters
/// carry a deletion on their own.
fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// The paths in `git status --porcelain -z` output, each with the two status
/// letters git gave it.
///
/// The `-z` form is `XY <path>` terminated by NUL, and a rename or copy
/// carries the path it came from as a second NUL-terminated field — `-z`
/// reverses the arrow form's order, so the entry reads `R  <new>\0<old>\0`.
/// Both ends are returned: the command wrote one and unwrote the other. An
/// entry too short to hold `XY ` is skipped rather than guessed at.
fn dirty_paths(status: &str) -> Vec<(String, String)> {
    let mut fields = status.split('\0').filter(|field| !field.is_empty());
    let mut out = Vec::new();
    while let Some(entry) = fields.next() {
        let Some((marks, path)) = entry
            .split_at_checked(2)
            .and_then(|(marks, rest)| Some((marks, rest.strip_prefix(' ')?)))
        else {
            continue;
        };
        out.push((marks.to_string(), path.to_string()));
        if marks.contains(['R', 'C'])
            && let Some(origin) = fields.next()
        {
            out.push((marks.to_string(), origin.to_string()));
        }
    }
    out
}

/// The paths whose dirty state moved across the call — written, created,
/// deleted, renamed, or committed. A path in one snapshot and not the other
/// counts, and so does one whose [`Mark`] changed.
fn changed(before: &BTreeMap<String, Mark>, after: &BTreeMap<String, Mark>) -> BTreeSet<String> {
    before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect()
}

/// What one acquire settled.
enum Claimed {
    /// The path is this tap's.
    Mine,
    /// A live rival holds it.
    Rival,
    /// The store could not say — coordination degrades, the work continues.
    Unknown,
}

/// Wraps a tool executor with claim-on-first-write (see the module doc).
pub(crate) struct ClaimTap<'a> {
    pub(crate) inner: &'a dyn ToolExecutor,
    /// The workspace store carrying `file_locks`. `None` = coordination
    /// disabled (no store), tools pass straight through.
    store: Option<Arc<Store>>,
    /// This writer's lock-table identity (`<session>/<lane>` or
    /// `<run>/<task>`) — what a rival's conflict error names.
    holder: String,
    /// Paths this tap already claimed (skip the store round-trip on the
    /// second write to the same file).
    held: std::sync::Mutex<HashSet<String>>,
    /// Set where this writer's shell writes land in the tree the store
    /// guards; `None` leaves the shell outside claim tracking.
    shell_watch: Option<ShellWatch>,
}

impl<'a> ClaimTap<'a> {
    pub(crate) fn new(
        inner: &'a dyn ToolExecutor,
        store: Option<Arc<Store>>,
        holder: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            store,
            holder: holder.into(),
            held: std::sync::Mutex::new(HashSet::new()),
            shell_watch: None,
        }
    }

    /// Claim what a shell call turns out to have written (see the module
    /// doc). `None` leaves `bash` outside claim tracking.
    #[must_use]
    pub(crate) fn with_shell_watch(mut self, watch: Option<ShellWatch>) -> Self {
        self.shell_watch = watch;
        self
    }

    /// The key a path is claimed under: relative to the watched root, so a
    /// tool naming `<root>/src/lib.rs` and a shell whose write git reports as
    /// `src/lib.rs` claim one file rather than two.
    ///
    /// Git reports a work-tree path relative to the REPOSITORY root, which is
    /// the workspace root in every layout Stella normally runs in. A workspace
    /// root nested inside a larger repository keys the two sides differently,
    /// and only asking git for its top level would settle that.
    fn claim_key(&self, path: &str) -> String {
        let trimmed = path.strip_prefix("./").unwrap_or(path);
        let Some(watch) = &self.shell_watch else {
            return trimmed.to_string();
        };
        match Path::new(trimmed).strip_prefix(&watch.root) {
            Ok(relative) => relative.to_string_lossy().into_owned(),
            Err(_) => trimmed.to_string(),
        }
    }

    /// Acquire `path` for this tap, reaping a dead holder once and retrying.
    /// Records the path as held on success, so a second write to the same
    /// file skips the store entirely.
    fn claim(&self, store: &Store, path: &str) -> Claimed {
        if self.already_held(path) {
            return Claimed::Mine;
        }
        let mut acquired = store.acquire_file_lock(path, &self.holder);
        if matches!(acquired, Ok(false)) && self.reap_dead_holder(store, path) {
            acquired = store.acquire_file_lock(path, &self.holder);
        }
        match acquired {
            Ok(true) => {
                self.mark_held(path);
                Claimed::Mine
            }
            Ok(false) => Claimed::Rival,
            Err(_) => Claimed::Unknown,
        }
    }

    /// Release every claim this tap acquired — called when the turn/worker
    /// settles (including cancellation; the drop-at-await future never gets
    /// to release, so the owner must).
    pub(crate) fn release_all(&self) {
        if let Some(store) = &self.store {
            let _ = store.release_file_locks_for_holder(&self.holder);
        }
        self.held.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    fn already_held(&self, path: &str) -> bool {
        self.held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(path)
    }

    fn mark_held(&self, path: &str) {
        self.held
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(path.to_string());
    }

    /// Free `path`'s claim set when its holder's process is dead — the gap
    /// the age-based sweep leaves: a crashed/killed writer's claims would
    /// otherwise refuse rivals for up to [`STALE_CLAIM_MAX_AGE_SECS`]. All
    /// of the dead holder's claims go at once (a dead process cannot be
    /// mid-edit on any of them). `true` means something was released and
    /// the caller should retry its acquire; an unparsable holder identity
    /// is assumed alive.
    fn reap_dead_holder(&self, store: &Store, path: &str) -> bool {
        let Some(rival) = store.file_lock_holder(path).ok().flatten() else {
            return false;
        };
        let Some(pid) = holder_pid(&rival) else {
            return false;
        };
        if pid_alive(pid) {
            return false;
        }
        store.release_file_locks_for_holder(&rival).is_ok()
    }
}

#[async_trait]
impl ToolExecutor for ClaimTap<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner.schemas()
    }

    /// Forwarded unfiltered, like `schemas()` (#3287): the tap coordinates
    /// writes, it does not change what exists.
    fn contracts(&self) -> Vec<stella_protocol::ToolContract> {
        self.inner.contracts()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        let Some(store) = &self.store else {
            return self.inner.execute(name, input).await;
        };

        // Lock on first write: refuse (naming the holder) instead of
        // clobbering a sibling's in-flight work. A conflict is only honored
        // while its holder's process is alive — a dead process's leftover
        // claim is reaped and the acquire retried once. Store trouble is
        // observability loss, never a work stoppage: it proceeds
        // uncoordinated.
        if let Some(path) = mutating_path(name, input) {
            let path = self.claim_key(path);
            if matches!(self.claim(store, &path), Claimed::Rival) {
                let rival = store
                    .file_lock_holder(&path)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "(released meanwhile)".to_string());
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::RefusedByPolicy,
                    format!(
                        "`{path}` is currently claimed by `{rival}` — another agent is \
                         editing it right now. Work on a different file, or retry in a \
                         moment; the claim releases when that agent's turn ends."
                    ),
                );
            }
        }

        // Claim on observed write: a shell command's writes are read off the
        // tree either side of the call, because they cannot be read out of
        // its arguments. Never a refusal — the command has already run by the
        // time anything is known — so a path a live rival holds stays that
        // rival's, and this call's own writes are what the NEXT writer is
        // warned about.
        if name == SHELL_TOOL
            && let Some(watch) = &self.shell_watch
        {
            let before = watch.snapshot().await;
            let output = self.inner.execute(name, input).await;
            if let Some(before) = before
                && let Some(after) = watch.snapshot().await
            {
                for path in changed(&before, &after) {
                    let _ = self.claim(store, &path);
                }
            }
            return output;
        }

        // A transient lane: bounded-wait serialization so a test run never
        // observes a sibling's half-written tree — and a sibling never
        // observes a formatter/linter/script mid-rewrite, or a commit racing
        // a commit.
        if let Some(lane) = transient_lane(name) {
            let mut waited = 0u64;
            let mut reaps = 0u32;
            let acquired = loop {
                match store.acquire_file_lock(lane, &self.holder) {
                    Ok(true) => break true,
                    // A dead process cannot release the lane; reap it and
                    // retry immediately instead of waiting out the bound.
                    // Bounded, because this arm neither sleeps nor yields:
                    // `reap_dead_holder` reports success on a DELETE that
                    // matched no row, so an acquire that keeps failing for
                    // any other reason would spin here forever — pegging a
                    // runtime worker and hanging the turn with no timeout to
                    // rescue it. One dead holder needs one reap; the bound
                    // covers a genuine pile-up after a multi-worker crash and
                    // otherwise falls through to the waited path below.
                    Ok(false) if reaps < MAX_LANE_REAPS && self.reap_dead_holder(store, lane) => {
                        reaps += 1;
                    }
                    Ok(false) if waited < LANE_WAIT_MS => {
                        tokio::time::sleep(std::time::Duration::from_millis(LANE_POLL_MS)).await;
                        waited += LANE_POLL_MS;
                    }
                    Ok(false) => {
                        let rival = store
                            .file_lock_holder(lane)
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| "(released meanwhile)".to_string());
                        return ToolOutput::classified_error(
                            stella_protocol::ErrorClass::RefusedByPolicy,
                            format!(
                                "the {} lane has been held by `{rival}` for over {}s — retry \
                                 shortly",
                                lane_label(lane),
                                LANE_WAIT_MS / 1000
                            ),
                        );
                    }
                    // Degrade to an unserialized run rather than blocking
                    // real work on a broken store.
                    Err(_) => break false,
                }
            };
            let output = self.inner.execute(name, input).await;
            if acquired {
                let _ = store.release_file_lock(lane, &self.holder);
            }
            return output;
        }

        self.inner.execute(name, input).await
    }

    /// Forwarded: this is a decorator, and a decorator that let the default
    /// `0.0` stand would silently drop sub-agent spend out of the parent's
    /// budget (see the port's contract).
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }

    /// Forwarded for the same reason as the spend drain above: a swallowed
    /// wait request silently turns parked waits (#1471) back into
    /// model-step polling.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        self.inner.drain_wait_request()
    }

    /// Forwarded: a decorator that let the empty default stand would silently
    /// turn the end-of-turn service assertion (#2764) off for every surface
    /// composed through it — the agent goes back to declaring a service done
    /// without ever being asked whether it is still listening.
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        self.inner.live_services()
    }

    /// Forwarded: letting the empty default stand would silently serialize the
    /// inner executor's sibling spawns (see the port's contract). The spawn
    /// tool names no mutating path and no transient lane, so concurrent
    /// siblings pass through `execute` above without touching a claim.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.inner.parallel_safe_names()
    }

    /// Forwarded: this tap claims files, it dispatches no name of its own,
    /// and it sits BETWEEN the custom-tool set and the base in the deck's
    /// lead turn — so a `None` here would silently un-gate every custom tool
    /// in every deck session (#2793).
    fn dispatch_gate(&self) -> Option<&dyn stella_core::ports::DispatchGate> {
        self.inner.dispatch_gate()
    }
}

#[cfg(test)]
mod tests {
    use stella_fleet::{GitError, GitOutput};

    use super::*;

    /// A recording fake: succeeds every call and remembers what ran.
    struct Passthrough(std::sync::Mutex<Vec<String>>);

    #[async_trait]
    impl ToolExecutor for Passthrough {
        fn schemas(&self) -> Vec<ToolSchema> {
            Vec::new()
        }
        async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
            self.0
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(name.to_string());
            ToolOutput::Ok {
                content: "ok".into(),
                data: None,
            }
        }
        fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
            std::collections::HashSet::from(["delegate".to_string()])
        }
    }

    fn store() -> Arc<Store> {
        Arc::new(Store::in_memory().unwrap())
    }

    /// The tap sits directly under the engine in every deck lane, so a tap
    /// that swallowed the claim (the default is empty) would serialize
    /// sibling spawns no matter what the registry advertised.
    #[test]
    fn the_claim_tap_forwards_parallel_safe_names() {
        let inner = Passthrough(Default::default());
        let tap = ClaimTap::new(&inner, None, "ses-1/lead");
        assert!(
            tap.parallel_safe_names().contains("delegate"),
            "the inner executor's concurrency claim must survive the tap"
        );
    }

    #[tokio::test]
    async fn first_write_claims_and_a_rival_is_refused_with_the_holder_named() {
        let store = store();
        let inner_a = Passthrough(std::sync::Mutex::new(Vec::new()));
        let inner_b = Passthrough(std::sync::Mutex::new(Vec::new()));
        let a = ClaimTap::new(&inner_a, Some(store.clone()), "ses-1/lead");
        let b = ClaimTap::new(&inner_b, Some(store.clone()), "ses-1/req:1");
        let input = serde_json::json!({ "path": "src/lib.rs", "content": "x" });

        assert!(!a.execute("write_file", &input).await.is_error());
        let refusal = b.execute("edit_file", &input).await;
        match refusal {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("ses-1/lead"), "{message}");
                assert!(message.contains("src/lib.rs"), "{message}");
            }
            other => panic!("rival write must be refused, got {other:?}"),
        }
        // The refused call never reached the inner executor.
        assert!(inner_b.0.lock().unwrap().is_empty());

        // Release frees the path for the rival.
        a.release_all();
        assert!(!b.execute("edit_file", &input).await.is_error());
    }

    #[tokio::test]
    async fn repeat_writes_by_the_holder_pass_and_reads_are_never_gated() {
        let store = store();
        let inner = Passthrough(std::sync::Mutex::new(Vec::new()));
        let tap = ClaimTap::new(&inner, Some(store.clone()), "ses-1/lead");
        let input = serde_json::json!({ "path": "src/lib.rs" });
        assert!(!tap.execute("edit_file", &input).await.is_error());
        assert!(!tap.execute("edit_file", &input).await.is_error());
        // A read-only tool with a path input is not a mutation.
        assert!(!tap.execute("read_file", &input).await.is_error());
        assert_eq!(inner.0.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn build_claim_is_transient_and_released_after_the_call() {
        let store = store();
        let inner = Passthrough(std::sync::Mutex::new(Vec::new()));
        let tap = ClaimTap::new(&inner, Some(store.clone()), "ses-1/lead");
        let input = serde_json::json!({});
        // Every tree-rewriting executor rides the lane, not just tests —
        // and `diagnostics`, which rewrites nothing but must never observe
        // a sibling's half-written tree (phantom errors).
        for tool in [
            "run_tests",
            "build_project",
            "diagnostics",
            "run_lint",
            "format_code",
            "run_script",
        ] {
            assert!(!tap.execute(tool, &input).await.is_error());
            // Released immediately — a rival takes the lane without waiting.
            assert!(
                store.acquire_file_lock(BUILD_CLAIM, "ses-1/req:2").unwrap(),
                "build lane must be free after `{tool}`"
            );
            store.release_file_lock(BUILD_CLAIM, "ses-1/req:2").unwrap();
        }
    }

    /// The commit lane's decisive property, and the reason
    /// [`crate::fleet_commits`] can attribute a shared tree's `HEAD` advance
    /// at all: the lane is held for the whole inner call — so a rival cannot
    /// commit inside the window the observer nested there — and released the
    /// moment it returns, so the next commit is not made to wait.
    #[tokio::test]
    async fn the_commit_lane_is_held_across_the_call_and_freed_after_it() {
        /// Asks, from inside the call, who holds the commit lane.
        struct HolderDuringCall(Arc<Store>, std::sync::Mutex<Option<String>>);

        #[async_trait]
        impl ToolExecutor for HolderDuringCall {
            fn schemas(&self) -> Vec<ToolSchema> {
                Vec::new()
            }
            async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
                *self.1.lock().unwrap() = self.0.file_lock_holder(COMMIT_CLAIM).unwrap();
                ToolOutput::Ok {
                    content: "committed".into(),
                    data: None,
                }
            }
        }

        let store = store();
        let inner = HolderDuringCall(store.clone(), std::sync::Mutex::new(None));
        let tap = ClaimTap::new(&inner, Some(store.clone()), "fleet-1/t1");

        assert!(
            !tap.execute("repo_commit", &serde_json::json!({}))
                .await
                .is_error()
        );

        assert_eq!(
            inner.1.lock().unwrap().as_deref(),
            Some("fleet-1/t1"),
            "the commit ran while this worker held the lane"
        );
        assert!(
            store.acquire_file_lock(COMMIT_CLAIM, "fleet-1/t2").unwrap(),
            "and the lane is free again for the next commit"
        );
    }

    #[tokio::test]
    async fn no_store_means_no_gating() {
        let inner = Passthrough(std::sync::Mutex::new(Vec::new()));
        let tap = ClaimTap::new(&inner, None, "ses-1/lead");
        let input = serde_json::json!({ "path": "src/lib.rs" });
        assert!(!tap.execute("write_file", &input).await.is_error());
    }

    #[test]
    fn holder_pids_parse_from_deck_and_fleet_identities_only() {
        assert_eq!(holder_pid("ses-1753-4242/lead"), Some(4242));
        assert_eq!(holder_pid("ses-1753-4242/req:1"), Some(4242));
        assert_eq!(holder_pid("fleet-1753-77/t1"), Some(77));
        // Anything else must read as "unknown → assume alive".
        assert_eq!(holder_pid("lead"), None);
        assert_eq!(holder_pid("agent-one/x"), None);
        assert_eq!(holder_pid(""), None);
    }

    // A pid that cannot belong to a live process: `pid_t::try_from` rejects
    // it outright, so the liveness probe is deterministic (unix only — the
    // non-unix fallback assumes every pid alive).
    #[cfg(unix)]
    const DEAD_HOLDER: &str = "ses-1753-4294967294/lead";

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dead_holders_claims_are_reaped_at_conflict_time() {
        // The age-based sweep would honor this leftover for up to 6h; a
        // conflict must instead notice the holder's process is gone, free
        // its whole claim set, and let the rival proceed.
        let store = store();
        store.acquire_file_lock("src/lib.rs", DEAD_HOLDER).unwrap();
        store
            .acquire_file_lock("src/other.rs", DEAD_HOLDER)
            .unwrap();
        let inner = Passthrough(std::sync::Mutex::new(Vec::new()));
        let tap = ClaimTap::new(&inner, Some(store.clone()), "ses-2/req:1");
        let input = serde_json::json!({ "path": "src/lib.rs", "content": "x" });

        assert!(!tap.execute("write_file", &input).await.is_error());
        assert_eq!(
            store.file_lock_holder("src/lib.rs").unwrap(),
            Some("ses-2/req:1".to_string()),
            "the rival now holds the reaped path"
        );
        assert_eq!(
            store.file_lock_holder("src/other.rs").unwrap(),
            None,
            "the dead holder's whole claim set went at once"
        );
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn build_lane_reaps_a_dead_holder_instead_of_waiting() {
        // (Paused clock: on a regression this auto-advances through the
        // bounded wait instead of stalling the suite, and the refusal below
        // fails the assert.)
        let store = store();
        store.acquire_file_lock(BUILD_CLAIM, DEAD_HOLDER).unwrap();
        let inner = Passthrough(std::sync::Mutex::new(Vec::new()));
        let tap = ClaimTap::new(&inner, Some(store.clone()), "ses-2/lead");

        assert!(
            !tap.execute("run_tests", &serde_json::json!({}))
                .await
                .is_error(),
            "a dead process must not hold the build lane"
        );
        // Transient as ever: released again after the call.
        assert!(store.acquire_file_lock(BUILD_CLAIM, "ses-3/lead").unwrap());
    }

    /// A `git status --porcelain -z` transcript: one scripted answer per
    /// call, the last repeating once the script runs out — so a two-entry
    /// script says "the tree before the command, then after it", and a
    /// one-entry script says "the command changed nothing".
    struct ScriptedGit(std::sync::Mutex<Vec<GitOutput>>);

    impl ScriptedGit {
        fn statuses(script: &[&str]) -> Self {
            Self(std::sync::Mutex::new(
                script.iter().map(|out| GitOutput::ok(*out)).collect(),
            ))
        }

        fn refusing(stderr: &str) -> Self {
            Self(std::sync::Mutex::new(vec![GitOutput::failed(128, stderr)]))
        }
    }

    #[async_trait]
    impl GitCli for ScriptedGit {
        async fn run(&self, _repo: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
            assert_eq!(
                args,
                ["status", "--porcelain", "-z"].as_slice(),
                "the watch spends one git command per snapshot"
            );
            let mut script = self.0.lock().unwrap_or_else(|p| p.into_inner());
            Ok(if script.len() > 1 {
                script.remove(0)
            } else {
                script
                    .first()
                    .cloned()
                    .unwrap_or_else(|| GitOutput::ok(String::new()))
            })
        }
    }

    /// A shell stand-in that rewrites a file, the way a redirect would.
    struct RewritesAFile(PathBuf);

    #[async_trait]
    impl ToolExecutor for RewritesAFile {
        fn schemas(&self) -> Vec<ToolSchema> {
            Vec::new()
        }
        async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
            std::fs::write(&self.0, "one line and then another").unwrap();
            ToolOutput::Ok {
                content: "ok".into(),
                data: None,
            }
        }
    }

    /// The witness for claim-on-observed-write. Session A edits a path through
    /// the shell — the call's arguments name no path, so nothing could be
    /// claimed up front — and the tree says what happened afterwards. Session
    /// B's `edit_file` on that path is then refused, naming A. Without the
    /// observed route the shell takes no claim at all: `file_locks` stays
    /// empty and B writes straight over A.
    #[tokio::test]
    async fn a_shell_write_is_claimed_and_the_next_writer_is_refused_by_name() {
        let store = store();
        let inner_a = Passthrough(Default::default());
        let inner_b = Passthrough(Default::default());
        let a = ClaimTap::new(&inner_a, Some(store.clone()), "ses-1/lead").with_shell_watch(Some(
            ShellWatch::new(ScriptedGit::statuses(&["", " M src/x.rs\0"]), "/repo"),
        ));
        let b = ClaimTap::new(&inner_b, Some(store.clone()), "ses-2/lead");

        let shell = a
            .execute(
                "bash",
                &serde_json::json!({ "command": "echo x >> src/x.rs" }),
            )
            .await;
        assert!(!shell.is_error(), "the shell is never refused by the tap");
        assert_eq!(
            store.file_lock_holder("src/x.rs").unwrap(),
            Some("ses-1/lead".to_string()),
            "the path the command turned out to write is claimed for its caller"
        );

        match b
            .execute("edit_file", &serde_json::json!({ "path": "src/x.rs" }))
            .await
        {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("ses-1/lead"), "{message}");
                assert!(message.contains("src/x.rs"), "{message}");
            }
            other => panic!("the next writer must be refused, got {other:?}"),
        }
    }

    /// A read-only command claims nothing — including a path that was already
    /// dirty when it ran.
    #[tokio::test]
    async fn a_shell_command_that_writes_nothing_claims_nothing() {
        let store = store();
        let inner = Passthrough(Default::default());
        let tap = ClaimTap::new(&inner, Some(store.clone()), "ses-1/lead").with_shell_watch(Some(
            ShellWatch::new(ScriptedGit::statuses(&[" M src/x.rs\0"]), "/repo"),
        ));

        assert!(
            !tap.execute("bash", &serde_json::json!({ "command": "ls" }))
                .await
                .is_error()
        );
        assert_eq!(store.file_lock_holder("src/x.rs").unwrap(), None);
    }

    /// A command that takes a path back OUT of the dirty set — `git checkout
    /// -- x` — wrote it too, and is claimed on the way out.
    #[tokio::test]
    async fn a_path_leaving_the_dirty_set_is_claimed_as_a_write() {
        let store = store();
        let inner = Passthrough(Default::default());
        let tap = ClaimTap::new(&inner, Some(store.clone()), "ses-1/lead").with_shell_watch(Some(
            ShellWatch::new(ScriptedGit::statuses(&[" M src/x.rs\0", ""]), "/repo"),
        ));

        assert!(
            !tap.execute(
                "bash",
                &serde_json::json!({ "command": "git checkout -- src/x.rs" })
            )
            .await
            .is_error()
        );
        assert_eq!(
            store.file_lock_holder("src/x.rs").unwrap(),
            Some("ses-1/lead".to_string())
        );
    }

    /// The false-refusal direction, which the design makes structurally
    /// impossible: the command has already run, so a rival's claim can be
    /// reported but never enforced. The shell is not refused, and the rival
    /// keeps the path.
    #[tokio::test]
    async fn a_shell_write_onto_a_rivals_claim_is_never_refused() {
        let store = store();
        let live_rival = format!("ses-1753-{}/lead", std::process::id());
        store.acquire_file_lock("src/x.rs", &live_rival).unwrap();
        let inner = Passthrough(Default::default());
        let tap = ClaimTap::new(&inner, Some(store.clone()), "ses-2/lead").with_shell_watch(Some(
            ShellWatch::new(ScriptedGit::statuses(&["", " M src/x.rs\0"]), "/repo"),
        ));

        assert!(
            !tap.execute(
                "bash",
                &serde_json::json!({ "command": "sed -i s/a/b/ src/x.rs" })
            )
            .await
            .is_error()
        );
        assert_eq!(
            inner.0.lock().unwrap().join(","),
            "bash",
            "the command reached the shell"
        );
        assert_eq!(
            store.file_lock_holder("src/x.rs").unwrap(),
            Some(live_rival),
            "first claim wins: the rival keeps the path it holds"
        );
    }

    /// A workspace that is not a git repository — or a git that will not
    /// answer — claims nothing and loses no work.
    #[tokio::test]
    async fn a_workspace_git_cannot_read_claims_nothing_and_still_runs_the_command() {
        let store = store();
        let inner = Passthrough(Default::default());
        let tap = ClaimTap::new(&inner, Some(store.clone()), "ses-1/lead").with_shell_watch(Some(
            ShellWatch::new(
                ScriptedGit::refusing("fatal: not a git repository"),
                "/tmp/plain",
            ),
        ));

        assert!(
            !tap.execute("bash", &serde_json::json!({ "command": "echo x > x.rs" }))
                .await
                .is_error()
        );
        assert_eq!(inner.0.lock().unwrap().join(","), "bash");
        assert_eq!(store.file_lock_holder("x.rs").unwrap(), None);
    }

    /// An unwatched tap — a fleet worker in its own isolated worktree — leaves
    /// the shell outside claim tracking, as every tap did before this route.
    #[tokio::test]
    async fn an_unwatched_shell_claims_nothing() {
        let store = store();
        let inner = Passthrough(Default::default());
        let tap = ClaimTap::new(&inner, Some(store.clone()), "ses-1/lead");

        assert!(
            !tap.execute(
                "bash",
                &serde_json::json!({ "command": "echo x > src/x.rs" })
            )
            .await
            .is_error()
        );
        assert_eq!(store.file_lock_holder("src/x.rs").unwrap(), None);
    }

    /// The second write to a path that was ALREADY modified: git's status
    /// letters read ` M` on both sides of the call, so the file's own size
    /// and modification time are what carry the change.
    #[tokio::test]
    async fn a_second_write_to_an_already_dirty_path_is_still_claimed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.rs"), "one line").unwrap();
        let store = store();
        let inner = RewritesAFile(dir.path().join("x.rs"));
        let tap = ClaimTap::new(&inner, Some(store.clone()), "ses-1/lead").with_shell_watch(Some(
            ShellWatch::new(ScriptedGit::statuses(&[" M x.rs\0"]), dir.path()),
        ));

        assert!(
            !tap.execute(
                "bash",
                &serde_json::json!({ "command": "sed -i s/a/b/ x.rs" })
            )
            .await
            .is_error()
        );
        assert_eq!(
            store.file_lock_holder("x.rs").unwrap(),
            Some("ses-1/lead".to_string())
        );
    }

    /// Both routes key one file the same way: git reports a work-tree path
    /// relative to the repository root, and a tool naming the same file
    /// absolutely must meet that claim rather than open a second one.
    #[tokio::test]
    async fn an_absolute_tool_path_meets_the_shells_repo_relative_claim() {
        let store = store();
        let inner_a = Passthrough(Default::default());
        let inner_b = Passthrough(Default::default());
        let a = ClaimTap::new(&inner_a, Some(store.clone()), "ses-1/lead").with_shell_watch(Some(
            ShellWatch::new(ScriptedGit::statuses(&["", " M src/x.rs\0"]), "/repo"),
        ));
        let b = ClaimTap::new(&inner_b, Some(store.clone()), "ses-2/lead")
            .with_shell_watch(Some(ShellWatch::new(ScriptedGit::statuses(&[""]), "/repo")));

        assert!(
            !a.execute(
                "bash",
                &serde_json::json!({ "command": "echo x > /repo/src/x.rs" })
            )
            .await
            .is_error()
        );
        match b
            .execute(
                "edit_file",
                &serde_json::json!({ "path": "/repo/src/x.rs" }),
            )
            .await
        {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("ses-1/lead"), "{message}")
            }
            other => panic!("the absolute spelling must meet the claim, got {other:?}"),
        }
    }

    /// The exact shape git writes, captured from `git status --porcelain -z`
    /// on this repository: two status letters, a space, the path, a NUL — and
    /// a rename carrying the path it came from as a second field, new before
    /// old.
    #[test]
    fn the_z_status_form_is_parsed_including_both_ends_of_a_rename() {
        let raw = " M crates/stella-cli/src/claims.rs\0?? new.txt\0R  README2.md\0README.md\0";
        assert_eq!(
            dirty_paths(raw),
            vec![
                (
                    " M".to_string(),
                    "crates/stella-cli/src/claims.rs".to_string()
                ),
                ("??".to_string(), "new.txt".to_string()),
                ("R ".to_string(), "README2.md".to_string()),
                ("R ".to_string(), "README.md".to_string()),
            ]
        );
    }

    #[test]
    fn an_entry_too_short_to_carry_a_path_is_skipped_not_guessed() {
        assert!(dirty_paths("M\0").is_empty());
        assert!(dirty_paths(" M\0").is_empty());
        assert!(dirty_paths("").is_empty());
    }

    #[tokio::test]
    async fn a_live_holders_claim_still_refuses_rivals() {
        // This process's own pid in the holder identity: alive, so the reap
        // must not fire and the refusal must stand.
        let store = store();
        let live_holder = format!("ses-1753-{}/lead", std::process::id());
        store.acquire_file_lock("src/lib.rs", &live_holder).unwrap();
        let inner = Passthrough(std::sync::Mutex::new(Vec::new()));
        let tap = ClaimTap::new(&inner, Some(store.clone()), "ses-2/req:1");
        let input = serde_json::json!({ "path": "src/lib.rs", "content": "x" });

        match tap.execute("write_file", &input).await {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains(&live_holder), "{message}")
            }
            other => panic!("a live holder's claim must refuse, got {other:?}"),
        }
    }
}
