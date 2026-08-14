use super::*;

use stella_pipeline::{DiagnosticInvocation, DiagnosticRunner};

#[test]
fn witness_fingerprint_hashes_complete_bytes_not_size_and_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("witness.rs");
    std::fs::write(&path, b"aaaa").unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let before = fs_fingerprint(&path).unwrap();

    std::fs::write(&path, b"bbbb").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let after = fs_fingerprint(&path).unwrap();

    assert_ne!(
        before, after,
        "same-length, same-mtime edits must be detected"
    );
}

#[cfg(unix)]
#[test]
fn witness_identity_rejects_symlinks_hardlinks_and_hashes_mode() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("witness.rs");
    let hardlink = dir.path().join("hardlink.rs");
    let symlink = dir.path().join("symlink.rs");
    std::fs::write(&file, "test bytes\n").unwrap();

    let before = fs_artifact_identity(dir.path(), "witness.rs").unwrap();
    assert_eq!(before.kind, ArtifactKind::Regular);
    assert!(before.is_regular_single_link());
    assert_eq!(
        before.path, "witness.rs",
        "the identity attests the location it was observed at"
    );

    std::fs::hard_link(&file, &hardlink).unwrap();
    assert!(
        fs_artifact_identity(dir.path(), "witness.rs").is_none(),
        "multi-link files fail closed at the identity boundary"
    );

    std::os::unix::fs::symlink(&file, &symlink).unwrap();
    assert!(
        fs_artifact_identity(dir.path(), "symlink.rs").is_none(),
        "no-follow identity must never open a symlink target"
    );

    std::fs::remove_file(&hardlink).unwrap();
    let mut permissions = std::fs::metadata(&file).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&file, permissions).unwrap();
    let executable = fs_artifact_identity(dir.path(), "witness.rs").unwrap();
    assert_ne!(before.fingerprint, executable.fingerprint);
}

#[cfg(unix)]
#[test]
fn opened_witness_identity_rejects_path_retarget_before_fingerprinting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("witness.rs");
    let moved = dir.path().join("original.rs");
    std::fs::write(&path, "original bytes\n").unwrap();
    let opened = OpenedWitnessArtifact::open(&path).expect("regular file opens no-follow");

    std::fs::rename(&path, &moved).unwrap();
    std::fs::write(&path, "replacement bytes\n").unwrap();

    assert!(
        opened.identity_for_path(&path).is_none(),
        "the opened handle must not be credited after its path is retargeted"
    );
}

#[cfg(unix)]
#[test]
fn witness_identity_attests_the_observed_location_through_an_aliased_lookup() {
    // A renamed witness can stay reachable at its pinned path when the
    // lookup is aliased — here a symlinked parent directory, the same
    // shape a case-folding filesystem produces. The identity must report
    // where the bytes actually live, so the pipeline's pinned-path
    // equality rejects the move as tampering.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("moved")).unwrap();
    std::fs::write(dir.path().join("moved/witness.rs"), "test bytes\n").unwrap();
    std::os::unix::fs::symlink(dir.path().join("moved"), dir.path().join("tests")).unwrap();

    let identity = fs_artifact_identity(dir.path(), "tests/witness.rs")
        .expect("the aliased lookup still opens a regular file");
    assert_eq!(
        identity.path, "moved/witness.rs",
        "the attested path is the canonical location, not the asked-for one"
    );
    assert!(
        !stella_pipeline::witness_identity_matches(
            &stella_pipeline::ArtifactIdentity {
                path: "tests/witness.rs".into(),
                ..identity.clone()
            },
            Some(&identity)
        ),
        "an identity pinned at the asked-for path must reject the moved artifact"
    );
}

#[test]
fn witness_identity_requires_established_platform_link_count() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("witness.rs"), "test bytes\n").unwrap();
    let identity = fs_artifact_identity(dir.path(), "witness.rs");
    #[cfg(unix)]
    assert!(
        identity.is_some(),
        "Unix exposes link count from the handle"
    );
    #[cfg(not(unix))]
    assert!(
        identity.is_none(),
        "platforms without a stable handle link count fail closed"
    );
}

#[tokio::test]
async fn repo_status_hashes_tracked_working_tree_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let mut command = std::process::Command::new("git");
        stella_tools::exec::scrub_sensitive_std_env(&mut command);
        let output = command.args(args).current_dir(dir.path()).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    std::fs::write(dir.path().join("src.rs"), "before\n").unwrap();
    git(&["add", "src.rs"]);
    git(&[
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-q",
        "-m",
        "base",
    ]);
    std::fs::write(dir.path().join("src.rs"), "after\n").unwrap();

    let files = GitRepoStatus {
        root: dir.path().to_path_buf(),
    }
    .tracked_fingerprints()
    .await;
    assert_eq!(
        files.get("src.rs"),
        fs_fingerprint(&dir.path().join("src.rs")).as_ref()
    );
}

#[tokio::test]
async fn typed_test_runner_never_interprets_redirection_in_an_argument() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("must-not-exist");
    let runner = TypedTestRunner {
        root: dir.path().to_path_buf(),
    };
    let outcome = runner
        .run_test(&stella_pipeline::TestInvocation {
            program: "printf".into(),
            args: vec![format!("owned > {}", target.display())],
        })
        .await;

    assert!(outcome.passed());
    assert!(
        !target.exists(),
        "argv content must never become shell syntax"
    );
}

#[tokio::test]
async fn typed_test_runner_binds_candidate_pwd_and_scrubs_git_repo_pointers() {
    let dir = tempfile::tempdir().unwrap();
    let invocation = TestInvocation {
        program: "sh".into(),
        args: vec!["-c".into(), "printf '%s' \"$PWD\"".into()],
    };
    let command = test_process(&invocation, dir.path());
    let configured_env: std::collections::HashMap<_, _> = command.as_std().get_envs().collect();
    for var in stella_tools::exec::GIT_REPO_ENV_VARS {
        assert_eq!(configured_env.get(std::ffi::OsStr::new(var)), Some(&None));
    }
    let runner = TypedTestRunner {
        root: dir.path().to_path_buf(),
    };
    let outcome = runner.run_test(&invocation).await;

    assert!(outcome.passed(), "{}", outcome.stderr_tail);
    let expected = dir.path().canonicalize().unwrap();
    assert_eq!(
        std::path::Path::new(&outcome.stdout_tail)
            .canonicalize()
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn diagnostic_runner_passes_untracked_paths_as_literal_git_argv() {
    let dir = tempfile::tempdir().unwrap();
    let odd = "odd;touch owned.txt";
    std::fs::write(dir.path().join(odd), "one\ntwo\n").unwrap();
    let runner = GitDiagnosticRunner::new(dir.path().to_path_buf());

    let outcome = runner
        .run_diagnostic(&DiagnosticInvocation::UntrackedNumstat {
            path: odd.to_string(),
        })
        .await;

    assert!(outcome.stdout_tail.contains(odd), "{}", outcome.stdout_tail);
    assert!(!dir.path().join("owned.txt").exists());
}
