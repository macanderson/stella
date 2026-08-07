use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use stella_pipeline::{DiagnosticInvocation, DiagnosticRunner};

use stella_media::{
    CostDecision, ImageRequest, MediaArtifact, MediaCapabilities, MediaError, MediaJob,
    MediaJobStatus, MediaKind, MediaProvider, MediaSpendGate, MediaSpendRequest, VideoRequest,
};

#[test]
fn structure_summary_composition_is_stable_without_an_index() {
    // No orientation (no code-graph index yet): the tree is
    // byte-identical to the pre-#342 output. With one: appended after a
    // blank line, and a tree-less workspace still gets the orientation.
    let tree = "src/\n  main.rs\n".to_string();
    assert_eq!(compose_structure_summary(tree.clone(), None), tree);
    assert_eq!(
        compose_structure_summary(tree.clone(), Some("Languages: rust".into())),
        format!("{tree}\n\nLanguages: rust")
    );
    assert_eq!(
        compose_structure_summary(String::new(), Some("Languages: rust".into())),
        "Languages: rust"
    );
}

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

struct FixedOperationId(&'static str);

impl stella_tools::media::MediaOperationIdSource for FixedOperationId {
    fn operation_id(&self) -> stella_tools::media::HostMediaOperation {
        stella_tools::media::HostMediaOperation {
            opaque_id: self.0.to_string(),
            expires_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 3600,
        }
    }
}

struct CountingGate(AtomicUsize);

#[async_trait::async_trait]
impl MediaSpendGate for CountingGate {
    async fn authorize(&self, _request: &MediaSpendRequest) -> CostDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        CostDecision::Approve
    }
}

#[test]
fn host_journal_rejects_workspace_paths_symlinks_and_dot_fallback() {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&[
        "STELLA_DATA_DIR",
        "HOME",
        "XDG_DATA_HOME",
        "APPDATA",
    ]);
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let original_dir = std::env::current_dir().unwrap();

    unsafe { std::env::set_var("STELLA_DATA_DIR", workspace.join(".stella")) };
    assert!(host_media_operation_journal(&workspace).is_none());

    #[cfg(unix)]
    {
        let link = outside.join("linked-data");
        std::os::unix::fs::symlink(&workspace, &link).unwrap();
        unsafe { std::env::set_var("STELLA_DATA_DIR", link) };
        assert!(host_media_operation_journal(&workspace).is_none());

        let ancestor_link = outside.join("linked-parent");
        std::os::unix::fs::symlink(&workspace, &ancestor_link).unwrap();
        let nested = ancestor_link.join("must-not-be-created");
        unsafe { std::env::set_var("STELLA_DATA_DIR", nested) };
        assert!(host_media_operation_journal(&workspace).is_none());
        assert!(
            !workspace.join("must-not-be-created").exists(),
            "workspace must not be mutated before the resolved containment check"
        );
    }

    std::env::set_current_dir(&workspace).unwrap();
    unsafe {
        for name in ["STELLA_DATA_DIR", "HOME", "XDG_DATA_HOME", "APPDATA"] {
            std::env::remove_var(name);
        }
    }
    assert!(host_media_operation_journal(&workspace).is_none());

    std::env::set_current_dir(original_dir).unwrap();
}

struct CountingImageProvider(AtomicUsize);

#[async_trait::async_trait]
impl MediaProvider for CountingImageProvider {
    fn id(&self) -> &str {
        "managed-test"
    }

    fn capabilities(&self) -> MediaCapabilities {
        MediaCapabilities {
            provider_id: self.id().into(),
            image: true,
            image_usd_each: Some(0.01),
            ..Default::default()
        }
    }

    async fn generate_image(&self, request: ImageRequest) -> Result<MediaArtifact, MediaError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(MediaArtifact {
            kind: MediaKind::Image,
            bytes: b"image".to_vec(),
            extension: "png".into(),
            label: request.label,
            model: "managed-test".into(),
            cost_usd: 0.01,
        })
    }

    async fn generate_video(&self, _request: VideoRequest) -> Result<MediaJob, MediaError> {
        Err(MediaError::Transport("not under test".into()))
    }

    async fn poll_video(&self, _job: &MediaJob) -> Result<MediaJobStatus, MediaError> {
        Err(MediaError::Transport("not under test".into()))
    }
}

fn load_managed_config(
    workspace: &std::path::Path,
    home: &std::path::Path,
    managed: &std::path::Path,
) -> (
    crate::settings::Settings,
    Config,
    stella_tools::RegistryOptions,
) {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&[
        "HOME",
        "STELLA_MANAGED_SETTINGS",
        "STELLA_DATA_DIR",
    ]);
    let original_dir = std::env::current_dir().unwrap();
    let data_dir = home.join(format!(
        "data-{}",
        workspace.file_name().unwrap().to_string_lossy()
    ));
    unsafe {
        std::env::set_var("HOME", home);
        std::env::set_var("STELLA_MANAGED_SETTINGS", managed);
        std::env::set_var("STELLA_DATA_DIR", &data_dir);
    }
    std::env::set_current_dir(workspace).unwrap();
    let settings = crate::settings::Settings::load(workspace);
    let cfg = Config::load(
        Some("local/managed-test"),
        Some("test-key"),
        Some("http://localhost:11434/v1"),
    );
    let options = registry_options(cfg.as_ref().unwrap());
    std::env::set_current_dir(original_dir).unwrap();
    assert!(data_dir.join("media-operations.db").exists());
    assert!(!data_dir.starts_with(workspace));
    (settings.unwrap(), cfg.unwrap(), options)
}

#[tokio::test]
async fn managed_media_ceiling_flows_through_config_into_registry_enforcement() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut results = Vec::new();
    for (name, authority_json, expected_allowed) in [
        (
            "off",
            r#"{"authority":{"media_requires_host_approval":"off"}}"#,
            false,
        ),
        (
            "on",
            r#"{"authority":{"media_requires_host_approval":"on"}}"#,
            true,
        ),
        ("absent", r#"{}"#, true),
    ] {
        let workspace = dir.path().join(name);
        let managed = dir.path().join(format!("managed-{name}.json"));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(&managed, authority_json).unwrap();
        let (settings, cfg, mut options) = load_managed_config(&workspace, &home, &managed);
        let gate = Arc::new(CountingGate(AtomicUsize::new(0)));
        let provider = Arc::new(CountingImageProvider(AtomicUsize::new(0)));
        assert!(options.media_operation_journal.is_some());
        options.media_spend_gate = Some(gate.clone());
        options.media_operation_ids = Some(Arc::new(FixedOperationId("host-managed-test")));
        options.media_host_data_isolation =
            Some(stella_tools::media::HostDataIsolation::ProcessFree);
        let registry = stella_tools::ToolRegistry::with_backends_and_options(
            workspace,
            None,
            Some(stella_tools::media::MediaBackend {
                image: provider.clone(),
                video: None,
            }),
            options,
        );
        let output = registry
            .execute("generate_image", &serde_json::json!({"prompt": "test"}))
            .await;
        results.push((
            name,
            expected_allowed,
            settings.authority_policy.media_requires_host_approval,
            cfg.authority.media_requires_host_approval,
            gate.0.load(Ordering::SeqCst),
            provider.0.load(Ordering::SeqCst),
            output.is_error(),
        ));
    }

    for (name, allowed, settings_value, config_value, gate, provider, is_error) in results {
        assert_eq!(settings_value, allowed, "settings row {name}");
        assert_eq!(config_value, allowed, "config row {name}");
        let expected_calls = usize::from(allowed);
        assert_eq!(gate, expected_calls, "gate row {name}");
        assert_eq!(provider, expected_calls, "provider row {name}");
        assert_eq!(is_error, !allowed, "output row {name}");
    }
}
