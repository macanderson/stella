//! The wiring between the registry's turn bracket and `verify_done`'s
//! shadow-run memo (#2208).
//!
//! `verify.rs` proves the memo's behaviour against a directly-constructed
//! tool. This module proves the half that only the registry can: that the
//! `verify_done` a real session dispatches shares the memo the turn bracket
//! arms and clears. Without it the memo could be correct and reach no user —
//! which is the failure mode the "nothing left behind" rule names.

use super::*;

/// A git repo with a committed "previous version", an uncommitted new one,
/// and a witness script that appends a line to `shadow-runs` on every run
/// that happens inside a linked worktree — i.e. once per shadow build.
fn repo_with_counting_witness() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "t@t.t"],
        &["config", "user.name", "t"],
    ] {
        git(root, args);
    }
    std::fs::write(root.join("impl.txt"), "old behavior\n").expect("write impl");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "previous version"]);

    std::fs::write(root.join("impl.txt"), "new behavior\n").expect("rewrite impl");
    std::fs::write(
        root.join("witness.sh"),
        format!(
            "if [ ! -d .git ]; then printf 'r\\n' >> '{}'; fi\ngrep -q 'new behavior' impl.txt\n",
            root.join("shadow-runs").display()
        ),
    )
    .expect("write witness");
    dir
}

/// `git <args>` in `root`, with the hook-exported `GIT_*` vars scrubbed the
/// way every production path scrubs them — without it, running the suite from
/// inside the pre-push hook re-targets each command at the host repo.
fn git(root: &std::path::Path, args: &[&str]) {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args).current_dir(root);
    for var in crate::exec::GIT_REPO_ENV_VARS {
        cmd.env_remove(var);
    }
    let out = cmd.output().expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn shadow_runs(root: &std::path::Path) -> usize {
    std::fs::read_to_string(root.join("shadow-runs"))
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

/// #2208 witness, at the seam that ships: the registry's turn bracket is what
/// arms the memo, so two identical dispatches inside one bracket build one
/// shadow and the same pair across a settle builds two.
#[tokio::test]
async fn the_turn_bracket_scopes_the_dispatched_verify_dones_memo() {
    let dir = repo_with_counting_witness();
    let root = dir.path().to_path_buf();
    let reg = ToolRegistry::new(root.clone(), RegistryOptions::default());

    reg.begin_workspace_probe();
    let first = reg.execute("verify_done", &witness_input()).await;
    assert!(!first.is_error(), "{first:?}");
    let second = reg.execute("verify_done", &witness_input()).await;
    assert!(!second.is_error(), "{second:?}");
    assert_eq!(
        shadow_runs(&root),
        1,
        "the registered verify_done must share the bracket's memo"
    );

    // Settling ends the turn: the tree the next turn measures is not this one.
    reg.settle_workspace_probe();
    let after = reg.execute("verify_done", &witness_input()).await;
    assert!(!after.is_error(), "{after:?}");
    assert_eq!(
        shadow_runs(&root),
        2,
        "an observation must not survive the turn that made it"
    );
}

/// The fail-open default: a host that never brackets a turn has told the
/// registry nothing about when observations expire, so nothing is reused.
#[tokio::test]
async fn an_unbracketed_session_reuses_nothing() {
    let dir = repo_with_counting_witness();
    let root = dir.path().to_path_buf();
    let reg = ToolRegistry::new(root.clone(), RegistryOptions::default());

    assert!(
        !reg.execute("verify_done", &witness_input())
            .await
            .is_error()
    );
    assert!(
        !reg.execute("verify_done", &witness_input())
            .await
            .is_error()
    );
    assert_eq!(shadow_runs(&root), 2);
}
