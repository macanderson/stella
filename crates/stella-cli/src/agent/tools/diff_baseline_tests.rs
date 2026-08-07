use super::*;
use stella_pipeline::{DiagnosticInvocation, DiagnosticRunner};

fn git(root: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git runs")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// Committing is not the same as doing nothing. A bare `git diff` reports
/// only unstaged changes, so an agent that commits its work — with
/// `repo_commit`, which this registry ships — left a clean tree and was
/// told "no changes were made to the repository" while its files sat on
/// disk. Measured on Terminal-Bench `kv-store-grpc`: the task verifier
/// passed 5 of 7 sub-tests while the verifier insisted the diff was empty.
#[tokio::test]
async fn committed_work_is_still_visible_to_the_diff() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "baseline"]);

    // Construct at session setup and then never touch it again — the
    // production order. Nothing here forces an early capture; if the
    // baseline resolved lazily on the first diff it would land AFTER the
    // commit below and this test would fail, which is the point.
    let runner = GitDiagnosticRunner::new(root.to_path_buf());

    // The agent writes AND commits — the tree ends clean.
    std::fs::write(root.join("server.py"), "def serve():\n    return 1\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "the agent's work"]);

    let out = runner.run_diagnostic(&DiagnosticInvocation::GitDiff).await;
    assert!(
        out.stdout_tail.contains("server.py"),
        "committed work must appear in the diff, got: {:?}",
        out.stdout_tail
    );
}

/// No resolvable HEAD (an empty or non-git tree) leaves the plain
/// working-tree diff, which is already the whole truth there.
#[tokio::test]
async fn a_tree_without_a_commit_falls_back_to_the_working_diff() {
    let dir = tempfile::tempdir().unwrap();
    let runner = GitDiagnosticRunner::new(dir.path().to_path_buf());
    assert!(runner.baseline_commit().is_none());
}

/// #613: this crate's `pre_exec(setsid)` spawn site had no
/// `GroupKillGuard`, so dropping the future mid-run orphaned the whole
/// process group — the exact leak #582 fixed in stella-tools and skipped
/// here. The grandchild's pid is recorded because an orphaned direct
/// child is reaped by init anyway; a surviving grandchild is the leak.
#[cfg(unix)]
#[tokio::test]
async fn a_dropped_run_command_kills_the_whole_process_group() {
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let script = format!("sleep 30 & echo $! > {} && wait", pidfile.display());
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c").arg(script);

    let handle = tokio::spawn(async move { run_command(cmd).await });
    let mut pid = None;
    for _ in 0..250 {
        if let Some(p) = std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
        {
            pid = Some(p);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid = pid.expect("the child never started");

    handle.abort();
    let _ = handle.await;

    let mut dead = false;
    for _ in 0..250 {
        if unsafe { libc::kill(pid, 0) } == -1 {
            dead = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(dead, "cancelled run_command left subprocess {pid} running");
}
