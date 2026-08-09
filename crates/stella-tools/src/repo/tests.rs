//! Tests for [`super`] — split out to keep `repo.rs` under the file-size
//! ratchet. A child module, so the private helpers stay reachable via
//! `super::*`.

use super::*;

/// Whether a real `git` is on PATH; these integration tests skip
/// cleanly (with a printed note) when it isn't — mirroring the
/// real-git tests in stella-fleet.
async fn git_available() -> bool {
    tokio::process::Command::new("git")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A `git` invocation scoped to `dir` and nothing else.
///
/// `current_dir` alone is not enough: `GIT_DIR` overrides it, and git
/// *exports* `GIT_DIR` to every hook it runs. So these tests pass when run
/// directly and fail from inside the pre-push gate, where every fixture
/// command silently retargets the real repository instead of its tempdir.
/// That reads as a flaky test; it is a deterministic one being handed a
/// different repo.
fn git_in(dir: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(dir);
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_PREFIX",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

/// Fixture plumbing: run git expecting success (unwrap is fine in tests).
async fn sh_git(dir: &Path, args: &[&str]) {
    let out = git_in(dir).args(args).output().await.unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Give a repo a deterministic identity and no signing, so commits work
/// in any environment.
async fn config_identity(dir: &Path) {
    for (k, v) in [
        ("user.email", "test@stella.local"),
        ("user.name", "Stella Test"),
        ("commit.gpgsign", "false"),
    ] {
        sh_git(dir, &["config", k, v]).await;
    }
}

/// Build: a seed repo on `main` with one commit → a bare `origin.git`
/// → a working clone `ws` (whose `origin/HEAD` resolves to `main`).
/// Returns the working clone path.
async fn fixture(dir: &Path) -> std::path::PathBuf {
    let seed = dir.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    sh_git(&seed, &["init", "--quiet"]).await;
    // Version-portable "main" (predates `init -b`).
    sh_git(&seed, &["symbolic-ref", "HEAD", "refs/heads/main"]).await;
    config_identity(&seed).await;
    std::fs::write(seed.join("README.md"), "seed\n").unwrap();
    sh_git(&seed, &["add", "README.md"]).await;
    sh_git(&seed, &["commit", "-q", "-m", "seed"]).await;

    let origin = dir.join("origin.git");
    sh_git(
        dir,
        &[
            "clone",
            "--quiet",
            "--bare",
            seed.to_str().unwrap(),
            origin.to_str().unwrap(),
        ],
    )
    .await;
    let ws = dir.join("ws");
    sh_git(
        dir,
        &[
            "clone",
            "--quiet",
            origin.to_str().unwrap(),
            ws.to_str().unwrap(),
        ],
    )
    .await;
    config_identity(&ws).await;
    ws
}

fn backend() -> Arc<dyn RepoBackend> {
    Arc::new(GitCli)
}

#[tokio::test]
async fn repo_status_reports_branch_upstream_and_changed_rows() {
    if !git_available().await {
        eprintln!("skipping repo_status test: `git` not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = fixture(dir.path()).await;
    std::fs::write(ws.join("README.md"), "changed\n").unwrap();
    std::fs::write(ws.join("new.txt"), "new\n").unwrap();

    let out = RepoStatusTool(backend()).execute(&Value::Null, &ws).await;
    let ToolOutput::Ok { content } = out else {
        panic!("repo_status must succeed: {out:?}");
    };
    let status: Value = serde_json::from_str(&content).expect("typed JSON payload");
    assert_eq!(status["branch"], "main");
    assert_eq!(status["ahead"], 0);
    assert_eq!(status["behind"], 0);
    let changed: Vec<String> = status["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["path"].as_str().unwrap().to_string())
        .collect();
    assert!(changed.contains(&"README.md".to_string()), "{content}");
    assert!(changed.contains(&"new.txt".to_string()), "{content}");
    assert_eq!(status["truncated"], false);
}

/// The Terminal-Bench `fix-git` witness, at the grain that caused it.
///
/// A commit orphaned by a checkout is exactly the state that task hides,
/// and every field `repo_status` used to carry is structurally blind to it:
/// `branch`, `ahead`, `behind` and `changed` describe the WORKING TREE, and
/// an orphaned commit leaves the working tree clean. The tool answered
/// truthfully — `branch: null` (detached), `changed: []` (clean) — and told
/// the caller nothing it could act on, so the agent fell back to raw
/// `git fsck` / `git reflog` from step 1.
///
/// It must now name where HEAD is, and list the commit nothing points at.
#[tokio::test]
async fn repo_status_names_head_and_the_commits_no_ref_reaches() {
    if !git_available().await {
        eprintln!("skipping repo_status orphan test: `git` not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = fixture(dir.path()).await;

    // Orphan a commit the way a checkout does: commit on a detached HEAD,
    // then leave it. Nothing references it afterwards; only the reflog
    // remembers.
    sh_git(&ws, &["checkout", "--quiet", "--detach"]).await;
    std::fs::write(ws.join("lost.txt"), "the user's lost work\n").unwrap();
    sh_git(&ws, &["add", "lost.txt"]).await;
    sh_git(&ws, &["commit", "--quiet", "-m", "Move to Stanford"]).await;
    sh_git(&ws, &["checkout", "--quiet", "main"]).await;
    // Detach again so the reported state matches a candidate workspace.
    sh_git(&ws, &["checkout", "--quiet", "--detach"]).await;

    let out = RepoStatusTool(backend()).execute(&Value::Null, &ws).await;
    let ToolOutput::Ok { content } = out else {
        panic!("repo_status must succeed: {out:?}");
    };
    let status: Value = serde_json::from_str(&content).expect("typed JSON payload");

    // The state that used to be reported as `branch: null` and nothing else.
    assert!(status["branch"].is_null(), "detached: {content}");
    assert!(
        status["head"]["commit"]
            .as_str()
            .is_some_and(|c| !c.is_empty()),
        "a detached checkout must still say where HEAD is: {content}"
    );

    let subjects: Vec<String> = status["unreachable"]
        .as_array()
        .expect("unreachable is an array")
        .iter()
        .map(|c| c["subject"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        subjects.iter().any(|s| s == "Move to Stanford"),
        "the orphaned commit must be named: {content}"
    );
    assert_eq!(status["stashes"], 0, "zero rules the stash out: {content}");
}

/// The pipeline's own baseline snapshots are unreachable by construction —
/// a candidate workspace is a detached-HEAD shadow — so listing them would
/// present Stella's bookkeeping to the agent as the user's lost work. That
/// is precisely the confusion that made an authored witness unsatisfiable
/// on `fix-git`; `repo_status` must not reproduce it one layer down.
#[tokio::test]
async fn repo_status_never_lists_the_pipelines_own_snapshot_commits() {
    if !git_available().await {
        eprintln!("skipping snapshot-exclusion test: `git` not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = fixture(dir.path()).await;

    sh_git(&ws, &["checkout", "--quiet", "--detach"]).await;
    std::fs::write(ws.join("snap.txt"), "machinery\n").unwrap();
    sh_git(&ws, &["add", "snap.txt"]).await;
    let email_cfg = format!("user.email={}", crate::verify::CANDIDATE_SNAPSHOT_EMAIL);
    sh_git(
        &ws,
        &[
            "-c",
            "user.name=stella-pipeline",
            "-c",
            &email_cfg,
            "commit",
            "--quiet",
            "-m",
            "stella: candidate baseline snapshot",
        ],
    )
    .await;
    sh_git(&ws, &["checkout", "--quiet", "main"]).await;

    let out = RepoStatusTool(backend()).execute(&Value::Null, &ws).await;
    let ToolOutput::Ok { content } = out else {
        panic!("repo_status must succeed: {out:?}");
    };
    assert!(
        !content.contains("candidate baseline snapshot"),
        "the pipeline's own commits must never read as the user's lost work: {content}"
    );
}

/// The issue-#332 witness: after modifying a file, `repo_diff` returns
/// a hunk containing the changed line — not just names and states.
#[tokio::test]
async fn repo_diff_returns_hunks_containing_the_changed_line() {
    if !git_available().await {
        eprintln!("skipping repo_diff test: `git` not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = fixture(dir.path()).await;
    std::fs::write(ws.join("README.md"), "reviewed line\n").unwrap();

    let out = RepoDiffTool(backend()).execute(&Value::Null, &ws).await;
    let ToolOutput::Ok { content } = out else {
        panic!("repo_diff must succeed: {out:?}");
    };
    // The actual hunk — old line out, new line in — not a narration.
    assert!(content.contains("+reviewed line"), "{content}");
    assert!(content.contains("-seed"), "{content}");
    // And the compact per-file line summary.
    assert!(content.contains("README.md +1 -1"), "{content}");
}

#[tokio::test]
async fn repo_diff_separates_staged_changes_and_scopes_to_paths() {
    if !git_available().await {
        eprintln!("skipping repo_diff test: `git` not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = fixture(dir.path()).await;

    // A clean tree diffs to nothing, loudly named.
    let clean = RepoDiffTool(backend()).execute(&Value::Null, &ws).await;
    let ToolOutput::Ok { content } = clean else {
        panic!("clean diff must succeed: {clean:?}");
    };
    assert!(content.contains("no unstaged changes"), "{content}");

    // A second tracked file, so path scoping has two candidates.
    std::fs::write(ws.join("b.txt"), "b\n").unwrap();
    sh_git(&ws, &["add", "b.txt"]).await;
    sh_git(&ws, &["commit", "-q", "-m", "add b"]).await;
    std::fs::write(ws.join("README.md"), "readme edit\n").unwrap();
    std::fs::write(ws.join("b.txt"), "b edit\n").unwrap();

    // Unscoped: both files' hunks.
    let both = RepoDiffTool(backend()).execute(&Value::Null, &ws).await;
    let ToolOutput::Ok { content } = both else {
        panic!("unscoped diff must succeed: {both:?}");
    };
    assert!(content.contains("+readme edit"), "{content}");
    assert!(content.contains("+b edit"), "{content}");

    // Scoped to b.txt: README's hunk is gone.
    let scoped = RepoDiffTool(backend())
        .execute(&serde_json::json!({"paths": ["b.txt"]}), &ws)
        .await;
    let ToolOutput::Ok { content } = scoped else {
        panic!("scoped diff must succeed: {scoped:?}");
    };
    assert!(content.contains("+b edit"), "{content}");
    assert!(!content.contains("readme edit"), "{content}");

    // Stage README: it moves from the unstaged view to the staged one.
    sh_git(&ws, &["add", "README.md"]).await;
    let unstaged = RepoDiffTool(backend()).execute(&Value::Null, &ws).await;
    let ToolOutput::Ok { content } = unstaged else {
        panic!("unstaged diff must succeed: {unstaged:?}");
    };
    assert!(content.contains("+b edit"), "{content}");
    assert!(!content.contains("readme edit"), "{content}");
    let staged = RepoDiffTool(backend())
        .execute(&serde_json::json!({"staged": true}), &ws)
        .await;
    let ToolOutput::Ok { content } = staged else {
        panic!("staged diff must succeed: {staged:?}");
    };
    assert!(content.contains("+readme edit"), "{content}");
    assert!(!content.contains("b edit"), "{content}");
}

#[test]
fn repo_diff_render_caps_the_patch_with_loud_elision() {
    let patch = "+padding line\n".repeat(3_000);
    let total = patch.trim_end().len();
    let diff = RepoDiff {
        files: vec![
            DiffFileStat {
                path: "big.txt".into(),
                added: Some(3_000),
                removed: Some(0),
            },
            DiffFileStat {
                path: "logo.png".into(),
                added: None,
                removed: None,
            },
        ],
        patch,
    };
    let rendered = render_diff(&diff, false);
    assert!(rendered.len() < total, "the cap must actually cut");
    assert!(rendered.contains("diff truncated"), "{rendered}");
    assert!(
        rendered.contains(&format!("of {total} bytes shown")),
        "{rendered}"
    );
    // The cut lands on a whole line — no half-shown hunk line.
    let marker = rendered.find("[… diff truncated").unwrap();
    assert_eq!(&rendered[marker - 1..marker], "\n");
    // Binary files render as such, never as +0 -0.
    assert!(rendered.contains("logo.png (binary)"), "{rendered}");
}

#[tokio::test]
async fn repo_commit_stages_exactly_the_named_paths_and_refuses_empty() {
    if !git_available().await {
        eprintln!("skipping repo_commit test: `git` not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = fixture(dir.path()).await;
    std::fs::write(ws.join("a.txt"), "a\n").unwrap();
    std::fs::write(ws.join("b.txt"), "b\n").unwrap();

    // Empty list: the structural refusal, before anything runs.
    let refused = RepoCommit(backend())
        .execute(&serde_json::json!({"message": "nope", "paths": []}), &ws)
        .await;
    match refused {
        ToolOutput::Error { message } => {
            assert!(message.contains("non-empty"), "{message}")
        }
        other => panic!("empty paths must be refused: {other:?}"),
    }

    let out = RepoCommit(backend())
        .execute(
            &serde_json::json!({"message": "add a only", "paths": ["a.txt"]}),
            &ws,
        )
        .await;
    let ToolOutput::Ok { content } = out else {
        panic!("commit must succeed: {out:?}");
    };
    assert!(content.contains("add a only"), "{content}");

    // b.txt was NOT swept into the commit.
    let status = RepoStatusTool(backend()).execute(&Value::Null, &ws).await;
    let ToolOutput::Ok { content } = status else {
        panic!("status after commit");
    };
    assert!(content.contains("b.txt"), "b.txt must remain uncommitted");
    assert!(
        !content.contains("a.txt"),
        "a.txt must be committed: {content}"
    );
}

#[tokio::test]
async fn repo_push_refuses_the_default_branch_but_pushes_a_feature_branch() {
    if !git_available().await {
        eprintln!("skipping repo_push test: `git` not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = fixture(dir.path()).await;

    // On the default branch: the structural refusal names the rule.
    let refused = RepoPush(backend())
        .execute(&serde_json::json!({}), &ws)
        .await;
    match refused {
        ToolOutput::Error { message } => {
            assert!(message.contains("default branch"), "{message}");
            assert!(message.contains("`main`"), "{message}");
        }
        other => panic!("default-branch push must be refused: {other:?}"),
    }

    // An option-shaped branch name is rejected before any spawn.
    let injected = RepoPush(backend())
        .execute(&serde_json::json!({"branch": "--force"}), &ws)
        .await;
    match injected {
        ToolOutput::Error { message } => {
            assert!(message.contains("not a valid branch name"), "{message}")
        }
        other => panic!("option-shaped branch must be refused: {other:?}"),
    }

    // A feature branch pushes fine and lands on the remote.
    sh_git(&ws, &["checkout", "-q", "-b", "feature/x"]).await;
    std::fs::write(ws.join("f.txt"), "f\n").unwrap();
    RepoCommit(backend())
        .execute(
            &serde_json::json!({"message": "feature", "paths": ["f.txt"]}),
            &ws,
        )
        .await;
    let pushed = RepoPush(backend())
        .execute(&serde_json::json!({}), &ws)
        .await;
    assert!(!pushed.is_error(), "{pushed:?}");
    let origin = dir.path().join("origin.git");
    let out = git_in(&origin)
        .args(["rev-parse", "--verify", "refs/heads/feature/x"])
        .output()
        .await
        .unwrap();
    assert!(out.status.success(), "feature/x must exist on the remote");
}

/// git chatter on stderr must not corrupt any value parsed from stdout.
///
/// The shared runner merges the two streams, so before `git_ok_stdout` the
/// branch read in `current_branch` returned the branch name *plus* whatever
/// git had written to stderr. `push` then built a refspec from that and git
/// rejected the whole thing:
///
/// ```text
/// fatal: invalid refspec 'refs/heads/feature/x
///        trace: built-in: git rev-parse --abbrev-ref HEAD'
/// ```
///
/// `GIT_TRACE` is the reproducible stand-in here; in the wild the same
/// corruption comes from advice hints and warnings, which need no
/// environment variable at all. This also fixes a real flake — the env var
/// is process-global, so any test setting it leaked trace output into every
/// other test spawning git concurrently in the same binary.
#[tokio::test]
async fn git_stderr_chatter_does_not_corrupt_the_pushed_branch() {
    if !git_available().await {
        eprintln!("skipping repo_push trace test: `git` not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = fixture(dir.path()).await;

    sh_git(&ws, &["checkout", "-q", "-b", "feature/traced"]).await;
    std::fs::write(ws.join("t.txt"), "t\n").unwrap();
    RepoCommit(backend())
        .execute(
            &serde_json::json!({"message": "traced", "paths": ["t.txt"]}),
            &ws,
        )
        .await;

    let _trace = crate::subprocess_env::test_support::ScopedEnvVar::set("GIT_TRACE", "1");
    let status = RepoStatusTool(backend()).execute(&Value::Null, &ws).await;
    let ToolOutput::Ok { content } = status else {
        panic!("status must survive stderr chatter: {status:?}");
    };
    assert!(
        content.contains("feature/traced"),
        "the branch name must be read from stdout alone: {content}"
    );

    let pushed = RepoPush(backend())
        .execute(&serde_json::json!({}), &ws)
        .await;
    assert!(
        !pushed.is_error(),
        "a traced push must still resolve a clean refspec: {pushed:?}"
    );

    let origin = dir.path().join("origin.git");
    let out = git_in(&origin)
        .args(["rev-parse", "--verify", "refs/heads/feature/traced"])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "feature/traced must exist on the remote"
    );
}

/// The flake #801's chatter test exposed in CI: with `GIT_TRACE` set
/// (process-global, so ANY concurrent test leaking it counts), the diff
/// patch was read from the MERGED stream — a clean tree's empty diff
/// came back as trace chatter, `render_diff` skipped its "no changes"
/// branch, and the report read "0 files changed" plus noise. Both the
/// clean and dirty answers must be chatter-proof.
#[tokio::test]
async fn stderr_chatter_never_pollutes_the_diff_patch() {
    if !git_available().await {
        eprintln!("skipping repo_diff trace test: `git` not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = fixture(dir.path()).await;
    let _trace = crate::subprocess_env::test_support::ScopedEnvVar::set("GIT_TRACE", "1");

    let clean = RepoDiffTool(backend()).execute(&Value::Null, &ws).await;
    let ToolOutput::Ok { content } = clean else {
        panic!("clean diff must succeed: {clean:?}");
    };
    assert!(
        content.contains("no unstaged changes"),
        "a clean tree stays clean under chatter: {content}"
    );

    std::fs::write(ws.join("README.md"), "traced edit\n").unwrap();
    let dirty = RepoDiffTool(backend()).execute(&Value::Null, &ws).await;
    let ToolOutput::Ok { content } = dirty else {
        panic!("dirty diff must succeed: {dirty:?}");
    };
    assert!(content.contains("+traced edit"), "{content}");
    assert!(
        !content.contains("trace:"),
        "the shown patch must be stdout alone: {content}"
    );
}

#[tokio::test]
async fn repo_pull_fast_forwards_and_types_divergence() {
    if !git_available().await {
        eprintln!("skipping repo_pull test: `git` not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = fixture(dir.path()).await;
    // A second clone advances origin/main (fixture plumbing, raw git —
    // repo_push structurally refuses main, which is the point of it).
    let ws2 = dir.path().join("ws2");
    sh_git(
        dir.path(),
        &[
            "clone",
            "--quiet",
            dir.path().join("origin.git").to_str().unwrap(),
            ws2.to_str().unwrap(),
        ],
    )
    .await;
    config_identity(&ws2).await;
    std::fs::write(ws2.join("upstream.txt"), "up\n").unwrap();
    sh_git(&ws2, &["add", "upstream.txt"]).await;
    sh_git(&ws2, &["commit", "-q", "-m", "upstream change"]).await;
    sh_git(&ws2, &["push", "-q", "origin", "main"]).await;

    // Clean fast-forward.
    let pulled = RepoPull(backend()).execute(&Value::Null, &ws).await;
    assert!(!pulled.is_error(), "{pulled:?}");
    assert!(ws.join("upstream.txt").exists(), "ff must land the commit");

    // Diverge: upstream advances again AND ws commits locally.
    std::fs::write(ws2.join("upstream.txt"), "up2\n").unwrap();
    sh_git(&ws2, &["add", "upstream.txt"]).await;
    sh_git(&ws2, &["commit", "-q", "-m", "upstream 2"]).await;
    sh_git(&ws2, &["push", "-q", "origin", "main"]).await;
    std::fs::write(ws.join("local.txt"), "local\n").unwrap();
    sh_git(&ws, &["add", "local.txt"]).await;
    sh_git(&ws, &["commit", "-q", "-m", "local change"]).await;

    let diverged = RepoPull(backend()).execute(&Value::Null, &ws).await;
    match diverged {
        ToolOutput::Error { message } => {
            assert!(message.contains("fast-forward"), "{message}")
        }
        other => panic!("divergence must be a typed error: {other:?}"),
    }
}

#[tokio::test]
async fn repo_rollback_restores_named_paths_and_refuses_an_empty_list() {
    if !git_available().await {
        eprintln!("skipping repo_rollback test: `git` not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = fixture(dir.path()).await;
    std::fs::write(ws.join("README.md"), "mangled\n").unwrap();

    let refused = RepoRollback(backend())
        .execute(&serde_json::json!({"paths": []}), &ws)
        .await;
    match refused {
        ToolOutput::Error { message } => {
            assert!(message.contains("non-empty"), "{message}");
        }
        other => panic!("empty rollback must be refused: {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(ws.join("README.md")).unwrap(),
        "mangled\n",
        "a refused rollback must not touch the tree"
    );

    let rolled = RepoRollback(backend())
        .execute(&serde_json::json!({"paths": ["README.md"]}), &ws)
        .await;
    assert!(!rolled.is_error(), "{rolled:?}");
    assert_eq!(
        std::fs::read_to_string(ws.join("README.md")).unwrap(),
        "seed\n",
        "the named path returns to its committed content"
    );
}
