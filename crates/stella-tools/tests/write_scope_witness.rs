// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The write-confinement witness: what the file tools may and may not touch.
//!
//! Driven through the tools' public API rather than through
//! `stella_core::workspace_scope` directly — the pure decision has its own
//! unit and property tests, and what needs proving *here* is that the tools
//! actually consult it before opening anything.

use stella_tools::ctx::ToolCtx;
use stella_tools::registry::Tool;
use stella_tools::{delete::DeleteFile, edit::EditFile, read::ReadFile, write::WriteFile};

use stella_core::workspace_scope::WriteScope;

/// A session root with a worktree directory already in it, as
/// `.stella/worktrees/<name>` — the shape every worktree Stella creates takes.
fn workspace_with_a_worktree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let worktree = dir.path().join(".stella/worktrees/sibling-task");
    std::fs::create_dir_all(&worktree).expect("mkdir worktree");
    std::fs::write(worktree.join("secret.rs"), "pub fn other_branch() {}\n")
        .expect("write worktree file");
    std::fs::write(dir.path().join("mine.rs"), "pub fn mine() {}\n").expect("write own file");
    dir
}

fn message(output: &stella_protocol::tool::ToolOutput) -> String {
    match output {
        stella_protocol::tool::ToolOutput::Error { message, .. } => message.clone(),
        stella_protocol::tool::ToolOutput::Ok { content, .. } => {
            panic!("expected a refusal, got success: {content}")
        }
    }
}

/// The session's own tree stays fully writable — the property everything else
/// here must not break.
#[tokio::test]
async fn the_session_tree_is_writable_as_before() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ToolCtx::bare(dir.path().to_path_buf());

    let out = WriteFile::default()
        .execute(
            &serde_json::json!({ "path": "src/deep/new.rs", "content": "fn main() {}\n" }),
            &ctx,
        )
        .await;
    assert!(!out.is_error(), "an in-tree write must succeed: {out:?}");
    assert!(dir.path().join("src/deep/new.rs").exists());
}

/// Writing into another session's worktree is refused, and the refusal says
/// why rather than reading as a generic path error.
#[tokio::test]
async fn a_write_into_a_sibling_worktree_is_refused() {
    let dir = workspace_with_a_worktree();
    let ctx = ToolCtx::bare(dir.path().to_path_buf());

    let out = WriteFile::default()
        .execute(
            &serde_json::json!({
                "path": ".stella/worktrees/sibling-task/planted.rs",
                "content": "fn planted() {}\n"
            }),
            &ctx,
        )
        .await;
    let message = message(&out);
    assert!(message.contains("worktrees"), "{message}");
    assert!(
        !dir.path()
            .join(".stella/worktrees/sibling-task/planted.rs")
            .exists(),
        "the refusal must land before anything is written"
    );
}

/// Deleting inside a sibling worktree is refused — the irreversible case.
#[tokio::test]
async fn a_delete_inside_a_sibling_worktree_is_refused() {
    let dir = workspace_with_a_worktree();
    let ctx = ToolCtx::bare(dir.path().to_path_buf());
    let victim = dir.path().join(".stella/worktrees/sibling-task/secret.rs");

    let out = DeleteFile
        .execute(
            &serde_json::json!({ "path": ".stella/worktrees/sibling-task/secret.rs" }),
            &ctx,
        )
        .await;
    assert!(out.is_error(), "{out:?}");
    assert!(victim.exists(), "the other session's file must survive");
}

/// **Reading** a sibling worktree is refused too, which is the half that is
/// about correctness rather than damage: it is a second checkout of the same
/// repository at another revision, so a read there answers the question about
/// the wrong tree.
#[tokio::test]
async fn a_read_of_a_sibling_worktree_is_refused() {
    let dir = workspace_with_a_worktree();
    let ctx = ToolCtx::bare(dir.path().to_path_buf());

    let out = ReadFile::default()
        .execute(
            &serde_json::json!({ "path": ".stella/worktrees/sibling-task/secret.rs" }),
            &ctx,
        )
        .await;
    let message = message(&out);
    assert!(message.contains("worktrees"), "{message}");
}

/// Reading anything else is untouched — the asymmetry is the whole design.
#[tokio::test]
async fn reading_the_session_tree_is_unaffected() {
    let dir = workspace_with_a_worktree();
    let ctx = ToolCtx::bare(dir.path().to_path_buf());

    let out = ReadFile::default()
        .execute(&serde_json::json!({ "path": "mine.rs" }), &ctx)
        .await;
    assert!(!out.is_error(), "{out:?}");
}

/// A session that has ENTERED a worktree treats it as the workspace: nothing
/// about its own tree is denied. This is the case the denial must not break,
/// because it is how fleet workers and pipeline candidates run.
#[tokio::test]
async fn a_session_rooted_at_a_worktree_may_write_its_own_tree() {
    let dir = workspace_with_a_worktree();
    let worktree = dir.path().join(".stella/worktrees/sibling-task");
    let ctx = ToolCtx::bare(worktree.clone());

    let out = WriteFile::default()
        .execute(
            &serde_json::json!({ "path": "inside.rs", "content": "fn inside() {}\n" }),
            &ctx,
        )
        .await;
    assert!(
        !out.is_error(),
        "entering a worktree makes it the workspace: {out:?}"
    );
    assert!(worktree.join("inside.rs").exists());

    let read = ReadFile::default()
        .execute(&serde_json::json!({ "path": "secret.rs" }), &ctx)
        .await;
    assert!(!read.is_error(), "its own files are readable: {read:?}");
}

/// An operator-added directory becomes writable — the `--allow-dir` /
/// `stella.toml` / `/add-dir` capability, at the layer that enforces it.
#[tokio::test]
async fn an_operator_added_directory_is_writable_and_others_are_not() {
    let session = tempfile::tempdir().expect("session tempdir");
    let shared = tempfile::tempdir().expect("shared tempdir");
    let forbidden = tempfile::tempdir().expect("forbidden tempdir");

    let scope = WriteScope::new(session.path().canonicalize().expect("canonicalize"))
        .with_additional([shared.path().canonicalize().expect("canonicalize")]);
    let ctx = ToolCtx::bare(session.path().to_path_buf()).with_scope(scope);

    let allowed = shared.path().join("generated.rs");
    let out = WriteFile::default()
        .execute(
            &serde_json::json!({
                "path": allowed.to_string_lossy(),
                "content": "fn generated() {}\n"
            }),
            &ctx,
        )
        .await;
    assert!(
        !out.is_error(),
        "an added directory must be writable: {out:?}"
    );
    assert!(allowed.exists());

    let refused = forbidden.path().join("nope.rs");
    let out = WriteFile::default()
        .execute(
            &serde_json::json!({
                "path": refused.to_string_lossy(),
                "content": "fn nope() {}\n"
            }),
            &ctx,
        )
        .await;
    assert!(out.is_error(), "an unlisted directory stays refused");
    assert!(!refused.exists());
}

/// The scope check must not resolve the path's final component.
///
/// Regression test for a bug this change introduced and this suite caught:
/// canonicalizing the whole path to compare it against the scope followed the
/// leaf symlink, so `delete_file` — whose entire contract is that it unlinks
/// the LINK and spares the target — would have deleted the real file instead.
/// Silent data loss on any repository carrying in-tree symlinks.
#[cfg(unix)]
#[tokio::test]
async fn the_scope_check_does_not_resolve_the_final_component() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("real.toml"), "keep = true\n").expect("write target");
    std::os::unix::fs::symlink(dir.path().join("real.toml"), dir.path().join("link.toml"))
        .expect("symlink");

    let ctx = ToolCtx::bare(dir.path().to_path_buf());
    let out = DeleteFile
        .execute(&serde_json::json!({ "path": "link.toml" }), &ctx)
        .await;

    assert!(!out.is_error(), "deleting the link must succeed: {out:?}");
    assert!(
        dir.path().join("real.toml").exists(),
        "the symlink's target must survive — the scope check must not have \
         resolved the leaf"
    );
    assert!(!dir.path().join("link.toml").exists());
}

/// The half that matters most for benchmarking: the shell audit must not
/// refuse ordinary work.
///
/// A false refusal breaks a build for a reason the agent cannot diagnose, and
/// one of those costs more than the rare escape the refusal would have caught.
/// Every command here is something a real task runs constantly.
#[tokio::test]
async fn the_shell_audit_permits_ordinary_build_and_test_commands() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
    let ctx = ToolCtx::bare(dir.path().to_path_buf());

    for command in [
        "echo hello",
        "ls -la",
        "cat src/lib.rs",
        "grep -rn TODO .",
        "mkdir -p build/out",
        "touch src/new.rs",
        "rm -f build/stale.o",
        "cp src/a.rs src/b.rs",
        "mv src/b.rs src/c.rs",
        "echo done > build/log.txt",
        "cargo build --release 2>&1 | tail -20",
        "sed -i.bak s/a/b/ src/c.rs",
        // Unresolvable by design: a variable, a glob, a home path. Skipped
        // rather than guessed at.
        "rm -rf $BUILD_DIR",
        "rm -f build/*.o",
        "cp config ~/backup",
    ] {
        let out = stella_tools::bash::Bash::new(None)
            .execute(&serde_json::json!({ "command": command }), &ctx)
            .await;
        if let stella_protocol::tool::ToolOutput::Error { message, .. } = &out {
            assert!(
                !message.contains("outside this session's writable directories")
                    && !message.contains("worktrees"),
                "the audit must not refuse `{command}`: {message}"
            );
        }
    }
}

/// What the audit is for: a clearly-resolvable write outside the session's
/// directories is refused before the shell runs.
#[tokio::test]
async fn the_shell_audit_refuses_a_resolvable_write_outside_the_scope() {
    let session = tempfile::tempdir().expect("session");
    let outside = tempfile::tempdir().expect("outside");
    let victim = outside.path().join("precious.txt");
    std::fs::write(&victim, "do not delete\n").expect("write victim");

    let ctx = ToolCtx::bare(session.path().to_path_buf());
    let out = stella_tools::bash::Bash::new(None)
        .execute(
            &serde_json::json!({ "command": format!("rm -f {}", victim.display()) }),
            &ctx,
        )
        .await;

    let message = message(&out);
    assert!(
        message.contains("outside this session's writable directories"),
        "{message}"
    );
    assert!(
        victim.exists(),
        "the refusal must land before the shell runs"
    );
}

/// A redirect creates a file whatever the command around it is, so the audit
/// reads it as a write.
#[tokio::test]
async fn the_shell_audit_refuses_a_redirect_outside_the_scope() {
    let session = tempfile::tempdir().expect("session");
    let outside = tempfile::tempdir().expect("outside");
    let target = outside.path().join("planted.txt");

    let ctx = ToolCtx::bare(session.path().to_path_buf());
    let out = stella_tools::bash::Bash::new(None)
        .execute(
            &serde_json::json!({ "command": format!("echo pwned > {}", target.display()) }),
            &ctx,
        )
        .await;

    assert!(out.is_error(), "{out:?}");
    assert!(!target.exists(), "nothing may be created outside the scope");
}

/// The shell may not write into a sibling worktree either — the same boundary
/// the file tools enforce, so the two cannot disagree.
#[tokio::test]
async fn the_shell_audit_refuses_writing_into_a_sibling_worktree() {
    let dir = workspace_with_a_worktree();
    let ctx = ToolCtx::bare(dir.path().to_path_buf());

    let out = stella_tools::bash::Bash::new(None)
        .execute(
            &serde_json::json!({
                "command": "rm -f .stella/worktrees/sibling-task/secret.rs"
            }),
            &ctx,
        )
        .await;

    let message = message(&out);
    assert!(message.contains("worktrees"), "{message}");
    assert!(
        dir.path()
            .join(".stella/worktrees/sibling-task/secret.rs")
            .exists()
    );
}

/// The edit tool consults the same boundary as the write tool — one session,
/// one answer.
#[tokio::test]
async fn edit_refuses_the_same_paths_write_does() {
    let dir = workspace_with_a_worktree();
    let ctx = ToolCtx::bare(dir.path().to_path_buf());

    let out = EditFile::default()
        .execute(
            &serde_json::json!({
                "path": ".stella/worktrees/sibling-task/secret.rs",
                "old_string": "other_branch",
                "new_string": "hijacked"
            }),
            &ctx,
        )
        .await;
    let message = message(&out);
    assert!(message.contains("worktrees"), "{message}");
    let untouched =
        std::fs::read_to_string(dir.path().join(".stella/worktrees/sibling-task/secret.rs"))
            .expect("read");
    assert!(untouched.contains("other_branch"));
}
