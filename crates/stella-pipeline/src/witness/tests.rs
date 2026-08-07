use super::*;
use crate::ports::ArtifactKind;

fn fps(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(p, f)| (p.to_string(), f.to_string()))
        .collect()
}

// parse_witness_command

#[test]
fn parses_a_bare_marker_line() {
    assert_eq!(
        parse_witness_command("done.\nTEST_COMMAND: cargo test -p x witness_"),
        Some("cargo test -p x witness_".to_string())
    );
}

#[test]
fn last_marker_wins_and_backticks_are_stripped() {
    let text = "I will end with `TEST_COMMAND: placeholder`\n\
                ...work...\n\
                TEST_COMMAND: `pytest tests/test_witness.py -q`";
    assert_eq!(
        parse_witness_command(text),
        Some("pytest tests/test_witness.py -q".to_string())
    );
}

#[test]
fn marker_is_case_insensitive() {
    assert_eq!(
        parse_witness_command("test_command: go test ./pkg -run TestWitness"),
        Some("go test ./pkg -run TestWitness".to_string())
    );
}

#[test]
fn a_multibyte_reply_is_scanned_without_slicing_mid_character() {
    // The marker length is a byte count, so a line of multi-byte
    // characters can clear it in bytes while landing mid-character at that
    // index — the shape that panicked a whole run inside the parser.
    assert_eq!(parse_witness_command("日本語のテキスト"), None);
    assert_eq!(
        parse_witness_command("テストを書きました。\nTEST_COMMAND: pytest tests/test_x.py"),
        Some("pytest tests/test_x.py".to_string())
    );
}

#[test]
fn missing_or_empty_marker_is_none_not_a_guess() {
    assert_eq!(parse_witness_command("no marker here"), None);
    assert_eq!(parse_witness_command("TEST_COMMAND:"), None);
    assert_eq!(parse_witness_command("TEST_COMMAND:   ``  "), None);
}

#[test]
fn test_invocation_rejects_shell_operators_and_redirection() {
    for command in [
        "cargo test -p x && touch owned",
        "cargo test -p x || true",
        "cargo test -p x; touch owned",
        "cargo test -p x | tee results",
        "cargo test -p x > results",
        "cargo test -p x 2> results",
        "cargo test -p x < input",
        "cargo test -p $(touch owned)",
        "cargo test -p `touch owned`",
        "cargo test 'quoted;operator'",
        "cargo\u{00a0}test",
        "cargo test filter\u{ff1b}touch",
    ] {
        assert!(
            parse_test_invocation(command).is_err(),
            "shell syntax must be rejected: {command}"
        );
    }
}

#[test]
fn test_invocation_parses_only_known_test_programs_into_argv() {
    assert_eq!(
        parse_test_invocation("cargo test -p 'my crate' witness -- --exact").unwrap(),
        TestInvocation {
            program: "cargo".into(),
            args: vec![
                "test".into(),
                "-p".into(),
                "my crate".into(),
                "witness".into(),
                "--".into(),
                "--exact".into(),
            ],
        }
    );
    assert!(parse_test_invocation("sh -c 'cargo test'").is_err());
    assert!(parse_test_invocation("python helper.py").is_err());
    assert!(parse_test_invocation("cargo build").is_err());
}

#[test]
fn test_invocation_cannot_escape_or_retarget_the_candidate() {
    for command in [
        "env RUSTFLAGS=-Dwarnings cargo test",
        "/usr/bin/cargo test",
        "cargo test /tmp/outside.rs",
        "cargo test ../outside",
        "cargo test --manifest-path ../outside/Cargo.toml",
        "cargo test --config=../outside.toml",
        "cargo test -- --manifest-path ../outside/Cargo.toml",
        "pnpm test --dir ../outside",
        "npm test --prefix=/tmp/outside",
        "go test -exec /tmp/executor",
        "go test -- -exec ../executor",
        "pytest --rootdir ../outside",
        "dotnet test --test-adapter-path ../outside",
    ] {
        assert!(
            parse_test_invocation(command).is_err(),
            "candidate escape must be rejected: {command}"
        );
    }
}

#[test]
fn accepted_witness_is_exactly_one_new_test_artifact() {
    let accepted = validate_witness_artifact(
        &fps(&[("src/lib.rs", "prod-v1")]),
        &fps(&[("src/lib.rs", "prod-v1")]),
        &HashMap::new(),
        &fps(&[("tests/authority_witness.rs", "sha256:whole-file")]),
    )
    .unwrap();
    assert_eq!(
        accepted,
        fps(&[("tests/authority_witness.rs", "sha256:whole-file")])
    );
}

#[test]
fn witness_invocation_names_the_exact_authored_artifact_and_test() {
    for (path, command) in [
        (
            "tests/authority_witness.rs",
            "cargo test --test authority_witness authority_witness -- --exact",
        ),
        (
            "tests/authority_witness.rs",
            "cargo test -p stella-pipeline --test authority_witness authority_witness -- --exact",
        ),
        (
            "tests/test_authority.py",
            "pytest tests/test_authority.py::test_authority -q",
        ),
        (
            "src/authority.test.ts",
            "pnpm exec vitest run src/authority.test.ts",
        ),
        (
            "pkg/authority_test.go",
            "go test ./pkg -run ^TestAuthority$",
        ),
        (
            "tests/Authority.Tests/AuthorityWitnessTests.cs",
            "dotnet test tests/Authority.Tests/Authority.Tests.csproj --filter FullyQualifiedName=Authority.Tests.AuthorityWitness",
        ),
    ] {
        let invocation = parse_test_invocation(command).unwrap();
        assert!(
            validate_witness_invocation(path, &invocation).is_ok(),
            "exact witness invocation must be accepted: {command}"
        );
    }

    for (path, command) in [
        ("tests/authority_witness.rs", "cargo test"),
        ("tests/authority_witness.rs", "cargo test authority_witness"),
        (
            "tests/authority_witness.rs",
            "cargo test --no-run --test authority_witness authority_witness -- --exact",
        ),
        (
            "tests/authority_witness.rs",
            "cargo test --test authority_witness authority_witness -- --exact --skip authority_witness",
        ),
        (
            "tests/authority_witness.rs",
            "cargo test --workspace --test authority_witness authority_witness -- --exact",
        ),
        (
            "tests/authority_witness.rs",
            "cargo test --test authority_witness --test other authority_witness -- --exact",
        ),
        (
            "tests/authority_witness.rs",
            "cargo test stray --test authority_witness authority_witness -- --exact",
        ),
        (
            "tests/authority_witness.rs",
            "cargo test --test some_other_test authority_witness -- --exact",
        ),
        ("tests/test_authority.py", "pytest"),
        ("tests/test_authority.py", "pytest tests"),
        (
            "tests/test_authority.py",
            "pytest tests/test_authority.py --collect-only",
        ),
        (
            "tests/test_authority.py",
            "pytest --ignore tests/test_authority.py",
        ),
        ("src/authority.test.ts", "npm test"),
        ("src/authority.test.ts", "pnpm test"),
        (
            "src/authority.test.ts",
            "pnpm exec vitest run src/authority.test.ts --exclude src/authority.test.ts",
        ),
        ("pkg/authority_test.go", "go test ./..."),
        ("pkg/authority_test.go", "go test ./pkg"),
        (
            "pkg/authority_test.go",
            "go test ./pkg ./other -run ^TestAuthority$",
        ),
        (
            "pkg/authority_test.go",
            "go test ./pkg -run ^TestAuthority$ -count=0",
        ),
        (
            "tests/Authority.Tests/AuthorityWitnessTests.cs",
            "dotnet test tests/Authority.Tests/Authority.Tests.csproj",
        ),
        (
            "tests/Authority.Tests/AuthorityWitnessTests.cs",
            "dotnet test tests/Authority.Tests/Authority.Tests.csproj --filter FullyQualifiedName=Authority.Tests.AuthorityWitness --list-tests",
        ),
        (
            "tests/Authority.Tests/AuthorityWitnessTests.cs",
            "dotnet test tests/Authority.Tests/Authority.Tests.csproj --filter FullyQualifiedName~AuthorityWitness",
        ),
    ] {
        let invocation = parse_test_invocation(command).unwrap();
        assert!(
            validate_witness_invocation(path, &invocation).is_err(),
            "broad or mismatched witness invocation must be rejected: {command}"
        );
    }
}

/// The absent-artifact case is its own variant so the stage can degrade
/// past it: an author that emitted a `TEST_COMMAND` but never created a
/// file is a cannot-author condition, and mapping it to the same
/// fail-closed rejection as an integrity violation discarded a completed
/// worker change for want of scaffolding.
#[test]
fn an_absent_artifact_is_nothing_created_not_an_integrity_violation() {
    let error = validate_witness_artifact(
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
    )
    .unwrap_err();
    assert!(matches!(error, WitnessArtifactError::NothingCreated));
}

#[test]
fn witness_artifact_rejects_tracked_production_edits() {
    let error = validate_witness_artifact(
        &fps(&[("src/lib.rs", "prod-v1")]),
        &fps(&[("src/lib.rs", "prod-v2")]),
        &HashMap::new(),
        &fps(&[("tests/authority_witness.rs", "test")]),
    )
    .unwrap_err();
    assert!(error.to_string().contains("tracked"));
    assert!(error.to_string().contains("src/lib.rs"));
}

#[test]
fn witness_artifact_rejects_non_test_and_pre_existing_mutations() {
    let non_test = validate_witness_artifact(
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &fps(&[
            ("tests/authority_witness.rs", "test"),
            ("README.md", "note"),
        ]),
    )
    .unwrap_err();
    assert!(non_test.to_string().contains("README.md"));

    let existing = validate_witness_artifact(
        &HashMap::new(),
        &HashMap::new(),
        &fps(&[("tests/authority_witness.rs", "old")]),
        &fps(&[("tests/authority_witness.rs", "new")]),
    )
    .unwrap_err();
    assert!(existing.to_string().contains("new test file"));

    let backdoor = validate_witness_artifact(
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &fps(&[("src/witness_backdoor.rs", "payload")]),
    );
    assert!(
        backdoor.is_err(),
        "production files named witness are not tests"
    );
    let rust_prefix_backdoor = validate_witness_artifact(
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &fps(&[("src/test_backdoor.rs", "payload")]),
    );
    assert!(
        rust_prefix_backdoor.is_err(),
        "Rust test prefixes outside a recognized test directory are not integration tests"
    );
    let rust_suffix_backdoor = validate_witness_artifact(
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &fps(&[("src/backdoor_test.rs", "payload")]),
    );
    assert!(
        rust_suffix_backdoor.is_err(),
        "cargo runs integration tests only from tests/, so a `_test.rs` under src/ \
         is a production file the run would adopt, never a witness"
    );
}

#[test]
fn witness_test_path_shapes_per_language() {
    for accepted in [
        "tests/authority_witness.rs",
        "crates/api/tests/witness.rs",
        "tests/test_authority.py",
        "test_authority.py",
        "authority_test.py",
        "src/__tests__/authority.ts",
        "src/authority.test.ts",
        "src/authority.spec.tsx",
        "internal/authority_test.go",
        "tests/Authority.Tests/AuthorityWitnessTests.cs",
    ] {
        assert!(
            is_witness_test_path(accepted),
            "{accepted} is a legitimate witness artifact"
        );
    }
    for rejected in [
        // Rust: cargo cannot run an integration test outside tests/, so a
        // filename-only form is a production file, not a witness.
        "src/backdoor_test.rs",
        "src/test_backdoor.rs",
        "src/lib.rs",
        "README.md",
        "tests/fixture.json",
    ] {
        assert!(
            !is_witness_test_path(rejected),
            "{rejected} must not be accepted as a witness artifact"
        );
    }
}

// witness_identity_matches — the tamper check the pipeline actually runs.
//
// These cases were inherited from the deleted `tampered_paths`, which
// compared fingerprints only and had no caller. The live check is strictly
// stronger (bytes *plus* type, mode and link count, read without following
// symlinks), so the last three cases below are ones a fingerprint
// comparison could not express at all.

fn identity(fingerprint: &str) -> ArtifactIdentity {
    ArtifactIdentity {
        path: "tests/authority_witness.rs".to_string(),
        fingerprint: fingerprint.to_string(),
        kind: ArtifactKind::Regular,
        mode: 0o100_644,
        link_count: 1,
    }
}

#[test]
fn an_untouched_witness_artifact_is_not_tampered() {
    let expected = identity("w1");
    assert!(witness_identity_matches(&expected, Some(&expected.clone())));
}

#[test]
fn a_modified_witness_artifact_is_tampered() {
    let expected = identity("w1");
    assert!(!witness_identity_matches(&expected, Some(&identity("w2"))));
}

#[test]
fn a_deleted_witness_artifact_is_tampered() {
    let expected = identity("w1");
    assert!(
        !witness_identity_matches(&expected, None),
        "an artifact that no longer exists can never match"
    );
}

#[test]
fn a_witness_artifact_renamed_to_a_different_path_is_tampered() {
    // A rename keeps every content facet — bytes, mode, link count — and
    // an aliased lookup (a case-folding filesystem, a symlinked parent
    // directory) can still resolve the pinned path to the moved bytes.
    // The location the observation was made at is part of the identity,
    // so the moved artifact must not match.
    let expected = identity("w1");
    let renamed = ArtifactIdentity {
        path: "tests/renamed_witness.rs".to_string(),
        ..identity("w1")
    };
    assert!(
        !witness_identity_matches(&expected, Some(&renamed)),
        "the path is part of the identity — a content comparison misses a rename"
    );
}

#[test]
fn witness_acceptance_requires_the_identity_observed_at_the_accepted_path() {
    let observed_at_accepted_path = identity("w1");
    assert!(
        validate_witness_identity(
            "tests/authority_witness.rs",
            "w1",
            Some(observed_at_accepted_path)
        )
        .is_ok()
    );
    let observed_elsewhere = ArtifactIdentity {
        path: "tests/renamed_witness.rs".to_string(),
        ..identity("w1")
    };
    assert!(
        validate_witness_identity("tests/authority_witness.rs", "w1", Some(observed_elsewhere))
            .is_err(),
        "an identity the adapter attests was resolved elsewhere is not the accepted artifact"
    );
}

/// **Witness (#1789).** Acceptance hands back the identity it accepted.
///
/// The witness stage pinned the grafted artifact by calling this and then
/// unwrapping the same `Option` again. That unwrap was sound — this
/// function rejects `None` — but soundness proved one line away is the
/// shape that decays: any edit between the check and the unwrap makes it a
/// panic in the one stage whose entire contract is to degrade rather than
/// discard a finished change. Returning the value removes the second read.
///
/// Fails to compile on the old signature, which returned `()`.
#[test]
fn acceptance_yields_the_identity_it_pinned() {
    let observed = identity("w1");
    let pinned = validate_witness_identity("tests/authority_witness.rs", "w1", Some(observed))
        .expect("the identity was observed at the accepted path");
    assert_eq!(
        pinned,
        identity("w1"),
        "the caller must pin the identity acceptance proved, never a second observation"
    );
    assert!(
        validate_witness_identity("tests/authority_witness.rs", "w1", None).is_err(),
        "an absent observation is a rejection, which is what makes the caller's \
         re-unwrap removable rather than merely unlikely"
    );
}

#[test]
fn identical_bytes_with_a_changed_mode_are_tampered() {
    let expected = identity("w1");
    let chmodded = ArtifactIdentity {
        mode: 0o100_755,
        ..expected.clone()
    };
    assert!(
        !witness_identity_matches(&expected, Some(&chmodded)),
        "mode is part of the identity — a fingerprint comparison misses this"
    );
}

#[test]
fn a_witness_artifact_that_gained_a_hard_link_is_tampered() {
    let expected = identity("w1");
    let linked = ArtifactIdentity {
        link_count: 2,
        ..expected.clone()
    };
    assert!(!witness_identity_matches(&expected, Some(&linked)));
}

#[test]
fn a_non_regular_expected_identity_never_matches_even_itself() {
    // The accepted-at-authoring-time guard: a symlink or multi-link file
    // was never a valid witness artifact, so re-observing it unchanged
    // must still fail rather than credit the flip.
    for kind in [ArtifactKind::Symlink, ArtifactKind::Other] {
        let odd = ArtifactIdentity {
            kind,
            ..identity("w1")
        };
        assert!(
            !witness_identity_matches(&odd, Some(&odd.clone())),
            "{kind:?} is not an acceptable witness artifact"
        );
    }
    let multi = ArtifactIdentity {
        link_count: 2,
        ..identity("w1")
    };
    assert!(!witness_identity_matches(&multi, Some(&multi.clone())));
}

// prompts

#[test]
fn witness_prompt_carries_goal_structure_recall_and_marker() {
    let recall = vec![RecalledFrame {
        citation_label: "memory: retries".to_string(),
        provider: "workspace-memory".to_string(),
        source: "memory".to_string(),
        kind: "memory".to_string(),
        uri: None,
        method: None,
        content: "retry policy is deterministic".to_string(),
        token_cost: 4,
        id: None,
        content_digest: None,
    }];
    let p = witness_prompt("fix the retry bug", &recall, "src/\n  lib.rs", &[]);
    assert!(p.contains("fix the retry bug"));
    assert!(p.contains("src/"));
    assert!(p.contains("memory: retries"));
    // The fixed half rides the system message (#1786), never the
    // volatile user prompt — repeating it here would re-bill it.
    assert!(!p.contains("Hard requirements"), "{p}");
    assert!(
        p.trim_start().len() == p.len(),
        "no dangling blank opening: {p:?}"
    );
}

/// The fixed system block carries the whole enforced contract: the
/// marker line, the one-new-file rule, and the density-screen rule
/// (#863) — a refusal the author could not have anticipated costs the
/// same round trip the screen exists to save.
#[test]
fn the_system_prompt_carries_the_hard_requirements() {
    for required in [
        "TEST_COMMAND:",
        "ONE NEW test file",
        "assert_eq!(2, 2)",
        "REFUSED",
        "never modify production code",
    ] {
        assert!(
            WITNESS_SYSTEM_PROMPT.contains(required),
            "missing {required:?}"
        );
    }
}

/// The author has `read_file` and one create — no execution at all — so it
/// cannot discover that the runner it picked is not installed. A command
/// naming an absent toolchain yields `infra_failure`, which observes NO
/// assertion and discards the witness entirely, so the prompt has to say
/// that choosing a runner is a decision made from evidence, not a default.
#[test]
fn the_author_is_told_it_cannot_probe_and_must_pick_an_evidenced_runner() {
    assert!(
        WITNESS_SYSTEM_PROMPT.contains("CHOOSE A RUNNER THIS REPOSITORY ALREADY USES"),
        "{WITNESS_SYSTEM_PROMPT}"
    );
    assert!(
        WITNESS_SYSTEM_PROMPT.contains("cannot execute anything in this role"),
        "the author must know its own blindness, not just the rule"
    );
    assert!(
        WITNESS_SYSTEM_PROMPT.contains("NO observation at all"),
        "the cost has to be named — a missing runner is not a failing test"
    );
    assert!(
        WITNESS_SYSTEM_PROMPT.contains("emit no TEST_COMMAND line rather than guessing"),
        "with no evidenced runner, abstaining beats a fabricated command"
    );
}

#[test]
fn repair_prompt_names_the_passing_command() {
    let p = witness_repair_prompt("cargo test -p x");
    assert!(p.contains("cargo test -p x"));
    assert!(p.contains("TEST_COMMAND:"));
}

/// #1539: the probed availability set is the one toolchain fact the
/// blind author can be GIVEN — and its absence must leave no dangling
/// empty section.
#[test]
fn probed_runner_availability_reaches_the_author_as_a_constraint() {
    let available = vec!["cargo".to_string(), "pytest".to_string()];
    let p = witness_prompt("add a parser", &[], "Cargo.toml", &available);
    assert!(
        p.contains("Test runners available in this workspace"),
        "{p}"
    );
    assert!(p.contains("cargo, pytest"), "{p}");
    assert!(
        p.contains("must use one of them"),
        "a constraint, not a hint: {p}"
    );

    let bare = witness_prompt("add a parser", &[], "Cargo.toml", &[]);
    assert!(
        !bare.contains("Test runners available in this workspace"),
        "no probes, no section: {bare}"
    );
}

/// #1539: every vocabulary program has a probe, the probe never runs a
/// test, and the two interpreter hosts probe the pytest MODULE — a
/// spawnable python without pytest is still a missing toolchain.
#[test]
fn every_vocabulary_runner_has_a_version_style_probe() {
    for program in RUNNER_VOCABULARY {
        let probe = runner_probe(program)
            .unwrap_or_else(|| panic!("`{program}` is in the vocabulary but unprobeable"));
        assert_eq!(&probe.program, program);
        // `sh` has no portable `--version` (dash and busybox reject it); its
        // probe is the no-op `-c 'exit 0'`, which honors the same contract —
        // it discovers and runs no test (#2064).
        if *program == "sh" {
            assert_eq!(probe.args, vec!["-c", "exit 0"]);
            continue;
        }
        assert!(
            probe.args.iter().any(|a| a.contains("version")),
            "`{program}`'s probe must be version-style, never a test run: {:?}",
            probe.args
        );
    }
    assert_eq!(
        runner_probe("python").unwrap().args,
        vec!["-m", "pytest", "--version"],
        "python probes the pytest module, not the bare interpreter"
    );
    assert_eq!(runner_probe("bash"), None, "outside the vocabulary");
}

/// #2064: the shell arm accepts exactly one auditable form — a single
/// positional `.sh` script — and nothing that would turn the flip oracle
/// into a general shell executor.
#[test]
fn sh_commands_parse_narrowly() {
    let parsed = parse_test_invocation("sh ./witness_check.sh").unwrap();
    assert_eq!(parsed.program, "sh");
    assert_eq!(parsed.args, vec!["./witness_check.sh"]);
    assert!(parse_test_invocation("sh witness_check.sh").is_ok());

    // A bare shell is not a test.
    assert!(parse_test_invocation("sh").is_err());
    // An inline program is one opaque argv word the per-argument
    // confinement checks cannot see into — refused, script form only.
    assert!(parse_test_invocation("sh -c 'exit 0'").is_err());
    // Pipelines, redirects, and chains are shell programs, not assertions.
    assert!(parse_test_invocation("sh -c 'true | false'").is_err());
    assert!(parse_test_invocation("sh witness.sh > /tmp/out").is_err());
    // A flag-shaped or non-script positional is refused.
    assert!(parse_test_invocation("sh -x witness_check.sh").is_err());
    assert!(parse_test_invocation("sh notes.txt").is_err());
    // Escapes stay confined to the workspace, like every other runner.
    assert!(parse_test_invocation("sh /etc/init.d/nginx.sh").is_err());
    assert!(parse_test_invocation("sh ../outside_test.sh").is_err());
}

/// #2064: the accepted invocation must pin exactly the authored artifact.
#[test]
fn sh_invocation_must_target_the_authored_script() {
    let script = parse_test_invocation("sh ./witness_check.sh").unwrap();
    assert!(validate_witness_invocation("witness_check.sh", &script).is_ok());
    assert!(validate_witness_invocation("other_check.sh", &script).is_err());
}

/// #2064: a shell witness must declare itself by location or name — an
/// arbitrary script is not a witness artifact.
#[test]
fn sh_witness_paths_declare_intent() {
    assert!(is_witness_test_path("witness_check.sh"));
    assert!(is_witness_test_path("test_tls.sh"));
    assert!(is_witness_test_path("tests/verify_endpoint.sh"));
    assert!(!is_witness_test_path("deploy.sh"));
    assert!(!is_witness_test_path("setup.sh"));
}

/// #2064: when the probe found only a shell, the author is TOLD the witness
/// is a shell script — a model asked for "a test" in a runner-less
/// container writes a pytest file otherwise.
#[test]
fn a_shell_only_workspace_prompts_a_shell_witness() {
    let sh_only = witness_prompt("secure nginx", &[], "etc/nginx.conf", &["sh".to_string()]);
    assert!(sh_only.contains("NO language test framework"), "{sh_only}");
    assert!(sh_only.contains("TEST_COMMAND: sh"), "{sh_only}");

    let with_cargo = witness_prompt(
        "add a parser",
        &[],
        "Cargo.toml",
        &["cargo".to_string(), "sh".to_string()],
    );
    assert!(
        !with_cargo.contains("NO language test framework"),
        "a real toolchain must keep the ordinary guidance: {with_cargo}"
    );
}
