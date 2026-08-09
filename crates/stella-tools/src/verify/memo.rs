//! The turn-scoped memo over `verify_done`'s previous-code run.
//!
//! # What is memoised, and why only that
//!
//! `verify_done` runs two halves. The first runs the witness test against the
//! working tree, and is **never** memoised: it is the answer to "does my
//! change work *now*", and the tree it reads changes under the agent's own
//! hands. The second builds a detached shadow worktree at the witness
//! baseline, copies the test files in, and runs the suite there — a fresh
//! checkout with a cold build, which is where the wall clock goes. On
//! `extract-elf` in match `cc00894779ff` a worker re-ran it with identical
//! arguments inside one turn and paid 250s, 24% of the trial's budget, for an
//! observation the run already held (#2208).
//!
//! That second half is memoisable because its inputs are *closed*. The only
//! things that reach the shadow are the baseline commit, the test files copied
//! into it, the command, and the timeout — nothing else in the working tree is
//! visible from inside a fresh checkout. So the key below is not a heuristic
//! summary of the call: it is the complete determinant of the run, which is
//! what makes a hit sound rather than merely likely. A worker that edits the
//! *implementation* between two calls still gets a fresh shadow verdict for
//! free, because the shadow never saw the implementation and its verdict
//! cannot have moved; the half that *does* see the edit re-runs every time.
//!
//! # Turn scoping, and why the key alone is not the whole answer
//!
//! A wrong memo is worse than no memo: this tool is the flip oracle's evidence
//! source, so a false hit is a false proof — the failure class #2067 fixed.
//! Two guards, and both must hold before an entry is served:
//!
//! 1. the key covers every input the shadow reads, so a changed test file, a
//!    moved baseline, or a different command all miss;
//! 2. entries live for exactly one turn, which bounds what the key cannot
//!    cover — a flaky suite, a clock, a network resource, an environment the
//!    host changed between turns. Without that bound one unlucky observation
//!    would stick for the life of the session.
//!
//! Anything short of certainty fails open (no memo, run the shadow): an
//! unreadable test file, a non-UTF-8 path, or a turn that was never bracketed
//! all yield no key and no entry.
//!
//! # Where the turn boundary comes from
//!
//! The registry's workspace-probe bracket, which is the only turn boundary a
//! `ToolRegistry` is told about: `begin_workspace_probe` arms
//! ([`ShadowMemo::arm`]) and `settle_workspace_probe` clears
//! ([`ShadowMemo::invalidate`]). A host that never brackets never arms, and an
//! unarmed memo stores and serves nothing — exactly the behaviour that shipped
//! before this existed.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

/// The shared handle: one per [`crate::registry::ToolRegistry`], held by the
/// registry (which brackets the turn) and by the `verify_done` instance it
/// registered (which reads and writes it).
pub type ShadowMemoHandle = Arc<ShadowMemo>;

/// How many distinct shadow observations one turn may hold.
///
/// A turn that witnesses more than this many *different* things is not the
/// shape this exists for, and an unbounded map would let a pathological turn
/// hold every test output it produced. Past the cap the memo simply stops
/// recording — the run happens, exactly as it did before.
const MAX_ENTRIES: usize = 16;

/// Domain separator, so a key can never be confused with any other digest in
/// the tree, and a version so a change to the encoding cannot silently be read
/// as the old one.
const KEY_DOMAIN: &str = "stella/verify_done/shadow/v1";

/// What one shadow run observed: the pair the verdict is read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ShadowObservation {
    pub(super) exit: i32,
    pub(super) output: String,
}

/// The complete determinant of a shadow run, hashed.
///
/// Deliberately built from the *resolved* values rather than the raw
/// model-supplied argument strings: `verify_done` derives the shadow copy
/// destination from the canonical root-relative path, so two calls naming one
/// file differently are the same run, and two different files that happen to
/// share a spelling are not.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ShadowKey(String);

impl ShadowKey {
    /// Hash everything the shadow run reads, or `None` when any part of it
    /// cannot be read exactly — an unreadable test file (it may vanish
    /// mid-turn), or a path that is not UTF-8. `None` means "no memo", never
    /// "no match".
    ///
    /// `resolved` is `verify_done`'s own `(display name, canonical source,
    /// toplevel-relative destination)` triple; the display name is ignored
    /// because it is the model's spelling, not an input to the run.
    pub(super) async fn compute(
        toplevel: &Path,
        root_rel: &Path,
        baseline_sha: &str,
        test_cmd: &str,
        timeout_secs: u64,
        resolved: &[(String, std::path::PathBuf, std::path::PathBuf)],
    ) -> Option<Self> {
        // Sorted and deduplicated by destination: the copy order cannot change
        // what the shadow contains (the destinations are distinct paths), so
        // two calls that list the same files in a different order describe one
        // run and must hash to one key.
        let mut files: BTreeMap<&str, String> = BTreeMap::new();
        for (_, src, dst) in resolved {
            let dst = dst.to_str()?;
            let bytes = tokio::fs::read(src).await.ok()?;
            files.insert(dst, crate::staleness::hex_sha256(&bytes));
        }

        let mut buf = String::new();
        for field in [
            KEY_DOMAIN,
            toplevel.to_str()?,
            root_rel.to_str()?,
            baseline_sha,
            test_cmd,
            &timeout_secs.to_string(),
        ] {
            push_field(&mut buf, field);
        }
        for (dst, digest) in &files {
            push_field(&mut buf, dst);
            push_field(&mut buf, digest);
        }
        Some(Self(crate::staleness::hex_sha256(buf.as_bytes())))
    }
}

/// Append one length-prefixed field. Length-prefixing rather than a separator
/// byte because a separator has to be a byte the fields cannot contain, and a
/// `test_cmd` is an arbitrary shell string — the one field where that
/// assumption is not ours to make.
fn push_field(buf: &mut String, field: &str) {
    use std::fmt::Write as _;

    let _ = write!(buf, "{}:{field}", field.len());
}

/// One turn's shadow observations.
#[derive(Default)]
pub struct ShadowMemo {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Whether a host has ever bracketed a turn on this registry. Latched by
    /// [`ShadowMemo::arm`] and never cleared, because it answers "does this
    /// host mark turn boundaries?" — not "is a turn open?". The pipeline arms
    /// once per candidate and settles once per round, so disarming on settle
    /// would leave every turn after the first without a memo.
    armed: bool,
    entries: HashMap<ShadowKey, ShadowObservation>,
}

impl ShadowMemo {
    /// Open the memo for a bracketed session, discarding anything a previous
    /// bracket left.
    pub fn arm(&self) {
        let mut state = self.lock();
        state.armed = true;
        state.entries.clear();
    }

    /// End the turn: every observation was true of *that* turn only.
    pub fn invalidate(&self) {
        self.lock().entries.clear();
    }

    pub(super) fn get(&self, key: &ShadowKey) -> Option<ShadowObservation> {
        let state = self.lock();
        if !state.armed {
            return None;
        }
        state.entries.get(key).cloned()
    }

    pub(super) fn insert(&self, key: ShadowKey, observation: ShadowObservation) {
        let mut state = self.lock();
        if !state.armed || state.entries.len() >= MAX_ENTRIES {
            return;
        }
        state.entries.insert(key, observation);
    }

    /// A poisoned memo is recovered rather than propagated: the worst a panic
    /// mid-update can leave behind is a missing entry, and refusing to serve
    /// the tool for the rest of the session over that would be a far larger
    /// failure than re-running one shadow build.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use stella_protocol::tool::ToolOutput;

    use super::*;
    use crate::registry::Tool;
    use crate::verify::VerifyDone;
    // The git-repo fixture the tool's own tests already build. Shared rather
    // than re-scaffolded here so a change to what a witness workspace looks
    // like cannot leave these two suites describing different worlds.
    use crate::verify::tests::scaffold;

    /// Write a witness script that is a flip AND a shadow-run counter: it
    /// appends one line to `marker` on every run that happens inside a linked
    /// worktree (where `.git` is a file, not a directory), which is exactly
    /// the shadow. `salt` changes the file's bytes without changing what it
    /// asserts — the memo keys on content, so this is how a caller says "same
    /// proof, different test file".
    fn counting_witness(root: &Path, marker: &Path, salt: &str) {
        std::fs::write(
            root.join("witness.sh"),
            format!(
                "# {salt}\nif [ ! -d .git ]; then printf 'r\\n' >> '{}'; fi\ngrep -q 'new \
                 behavior' impl.txt\n",
                marker.display()
            ),
        )
        .unwrap();
    }

    /// How many shadow builds have happened.
    fn shadow_runs(marker: &Path) -> usize {
        std::fs::read_to_string(marker)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    fn witness_input() -> Value {
        serde_json::json!({
            "test_cmd": "bash witness.sh",
            "test_files": ["witness.sh"],
            "timeout_secs": 60
        })
    }

    /// #2208 witness. An identical `verify_done` inside one turn pays for the
    /// shadow worktree once: its verdict is a function of the baseline, the
    /// test files and the command, and none of those moved. Changing the test
    /// file moves one of them, and the second build happens.
    ///
    /// Fails on `main`, where every call built a shadow — 250s, 24% of the
    /// trial budget, on `extract-elf` in match `cc00894779ff`.
    #[tokio::test]
    async fn an_identical_call_in_one_turn_builds_one_shadow() {
        let root = scaffold("memo_hit").await;
        let marker = root.join("shadow-runs");
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        counting_witness(&root, &marker, "first");

        let memo: ShadowMemoHandle = Arc::new(ShadowMemo::default());
        memo.arm();
        let tool = VerifyDone::with_memo(memo.clone());

        let first = tool.execute(&witness_input(), &root).await;
        assert!(
            matches!(&first, ToolOutput::Ok { content } if content.contains("WITNESS CONFIRMED")),
            "{first:?}"
        );
        assert_eq!(
            shadow_runs(&marker),
            1,
            "the first call must build a shadow"
        );

        let second = tool.execute(&witness_input(), &root).await;
        match &second {
            ToolOutput::Ok { content } => {
                assert!(content.contains("WITNESS CONFIRMED"), "{content}");
                assert!(
                    content.contains("REUSED"),
                    "a reused observation must say so: {content}"
                );
            }
            other => panic!("expected the same verdict, got {other:?}"),
        }
        assert_eq!(
            shadow_runs(&marker),
            1,
            "an identical call re-derived nothing and must not rebuild"
        );

        // A different test file is a different proof, whatever else matches.
        counting_witness(&root, &marker, "second");
        let third = tool.execute(&witness_input(), &root).await;
        assert!(
            matches!(&third, ToolOutput::Ok { content } if !content.contains("REUSED")),
            "{third:?}"
        );
        assert_eq!(
            shadow_runs(&marker),
            2,
            "a changed test file must be re-measured"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The two fail-open halves of the memo, which are what keep a stale hit
    /// from ever manufacturing a proof: a host that marks no turn boundaries
    /// gets no memo at all, and an observation never outlives the turn that
    /// made it.
    #[tokio::test]
    async fn nothing_is_reused_across_a_turn_or_without_one() {
        let root = scaffold("memo_scope").await;
        let marker = root.join("shadow-runs");
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();
        counting_witness(&root, &marker, "only");

        // No bracket: exactly the behaviour that shipped before the memo.
        let unbracketed = VerifyDone::default();
        assert!(
            !unbracketed
                .execute(&witness_input(), &root)
                .await
                .is_error()
        );
        assert!(
            !unbracketed
                .execute(&witness_input(), &root)
                .await
                .is_error()
        );
        assert_eq!(
            shadow_runs(&marker),
            2,
            "an unarmed memo must serve nothing"
        );

        let memo: ShadowMemoHandle = Arc::new(ShadowMemo::default());
        memo.arm();
        let tool = VerifyDone::with_memo(memo.clone());
        assert!(!tool.execute(&witness_input(), &root).await.is_error());
        assert_eq!(shadow_runs(&marker), 3);

        // The turn ends — the tree may move under any observation it made.
        memo.invalidate();
        assert!(!tool.execute(&witness_input(), &root).await.is_error());
        assert_eq!(
            shadow_runs(&marker),
            4,
            "an observation must not outlive its turn"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    fn observation(exit: i32) -> ShadowObservation {
        ShadowObservation {
            exit,
            output: format!("exit {exit}"),
        }
    }

    fn key(text: &str) -> ShadowKey {
        ShadowKey(text.to_string())
    }

    /// An unarmed memo is inert in both directions — the fail-open default for
    /// a host that marks no turn boundaries.
    #[test]
    fn an_unarmed_memo_stores_and_serves_nothing() {
        let memo = ShadowMemo::default();
        memo.insert(key("k"), observation(1));
        assert_eq!(memo.get(&key("k")), None);
    }

    #[test]
    fn an_armed_memo_serves_what_it_stored_until_the_turn_ends() {
        let memo = ShadowMemo::default();
        memo.arm();
        memo.insert(key("k"), observation(1));
        assert_eq!(memo.get(&key("k")), Some(observation(1)));
        assert_eq!(memo.get(&key("other")), None);

        memo.invalidate();
        assert_eq!(
            memo.get(&key("k")),
            None,
            "an entry must not outlive its turn"
        );

        // Still armed: the pipeline arms once and settles once per round, so
        // the turn after a settle must be able to memoise again.
        memo.insert(key("k"), observation(2));
        assert_eq!(memo.get(&key("k")), Some(observation(2)));
    }

    #[test]
    fn a_pathological_turn_stops_recording_rather_than_growing() {
        let memo = ShadowMemo::default();
        memo.arm();
        for i in 0..MAX_ENTRIES + 4 {
            memo.insert(key(&i.to_string()), observation(i as i32));
        }
        assert_eq!(memo.get(&key("0")), Some(observation(0)));
        assert_eq!(
            memo.get(&key(&MAX_ENTRIES.to_string())),
            None,
            "past the cap the run happens instead"
        );
    }

    /// Length-prefixing, not separators: a `test_cmd` is an arbitrary shell
    /// string, so no byte can be reserved as a field boundary. Two calls whose
    /// concatenated fields are identical must still key differently.
    #[test]
    fn field_boundaries_survive_a_command_that_contains_them() {
        let mut a = String::new();
        push_field(&mut a, "ab");
        push_field(&mut a, "c");
        let mut b = String::new();
        push_field(&mut b, "a");
        push_field(&mut b, "bc");
        assert_ne!(a, b);
    }

    /// The key is over the resolved destinations and the bytes behind them, so
    /// argument order cannot split one run into two, and a changed test file
    /// cannot be served a stale observation.
    #[tokio::test]
    async fn the_key_is_order_free_but_content_sensitive() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.sh"), "a\n").unwrap();
        std::fs::write(root.join("b.sh"), "b\n").unwrap();
        let entry = |name: &str| {
            (
                name.to_string(),
                root.join(name),
                std::path::PathBuf::from(name),
            )
        };
        async fn compute(
            root: &Path,
            resolved: Vec<(String, std::path::PathBuf, std::path::PathBuf)>,
        ) -> Option<ShadowKey> {
            ShadowKey::compute(
                root,
                std::path::Path::new(""),
                "deadbeef",
                "bash a.sh",
                60,
                &resolved,
            )
            .await
        }

        let forward = compute(root, vec![entry("a.sh"), entry("b.sh")]).await;
        let backward = compute(root, vec![entry("b.sh"), entry("a.sh")]).await;
        assert!(forward.is_some());
        assert_eq!(
            forward, backward,
            "argument order is not an input to the run"
        );

        std::fs::write(root.join("b.sh"), "b changed\n").unwrap();
        assert_ne!(
            forward,
            compute(root, vec![entry("a.sh"), entry("b.sh")]).await,
            "a changed test file must change the key"
        );

        // A file that vanishes mid-turn yields no key at all, never a key that
        // silently omits it.
        std::fs::remove_file(root.join("b.sh")).unwrap();
        assert_eq!(
            compute(root, vec![entry("a.sh"), entry("b.sh")]).await,
            None
        );
    }
}
