//! Tests for the feedback airlock. The properties that matter are negative:
//! what must *not* reach a brief, and that a scrub failure tightens the grain
//! rather than emitting a hole.

use super::*;
use crate::ports::TestInvocation;

fn invocation(args: &[&str]) -> TestInvocation {
    invocation_for("cargo", args)
}

fn invocation_for(program: &str, args: &[&str]) -> TestInvocation {
    TestInvocation {
        program: program.to_string(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
    }
}

fn sealed<'a>(
    command: &'a str,
    output: &'a str,
    invocation: Option<&'a TestInvocation>,
    witness_paths: &'a [String],
) -> SealedFailure<'a> {
    SealedFailure {
        command,
        invocation,
        output,
        witness_paths,
    }
}

const ASSERTION_TAIL: &str = "\
running 1 test
test witness_balance_stays_consistent ... FAILED

failures:

---- witness_balance_stays_consistent stdout ----
thread 'witness_balance_stays_consistent' panicked at tests/witness_ab12.rs:14:5:
assertion `left == right` failed
  left: 4200
 right: 0
";

#[test]
fn a_brief_never_carries_the_runner_output() {
    let paths = vec!["tests/witness_ab12.rs".to_string()];
    let inv = invocation(&["test", "--test", "witness_ab12", "--exact"]);
    let failure = sealed(
        "cargo test --test witness_ab12 --exact",
        ASSERTION_TAIL,
        Some(&inv),
        &paths,
    );

    let brief = redact(&failure, DisclosureGrain::Reproduction);
    let message = brief.message();

    for leaked in [
        "4200",
        "assertion `left == right`",
        "witness_balance_stays_consistent",
        "tests/witness_ab12.rs",
    ] {
        assert!(
            !message.contains(leaked),
            "brief leaked sealed material {leaked:?}: {message}"
        );
    }
}

#[test]
fn the_witness_path_is_withheld_at_every_grain() {
    // The realistic authored-witness shape: the command names the witness
    // file, so disclosing the command verbatim would hand over the artifact.
    let paths = vec!["tests/witness_ab12.rs".to_string()];
    let inv = invocation(&["test", "--test", "witness_ab12"]);
    let failure = sealed(
        "cargo test --test witness_ab12",
        ASSERTION_TAIL,
        Some(&inv),
        &paths,
    );
    for grain in [
        DisclosureGrain::Outcome,
        DisclosureGrain::Command,
        DisclosureGrain::Symptom,
        DisclosureGrain::Reproduction,
    ] {
        let message = redact(&failure, grain).message();
        assert!(
            !message.contains("witness_ab12"),
            "grain {grain:?} disclosed the witness artifact: {message}"
        );
    }
}

#[test]
fn a_command_naming_the_sealed_test_tightens_the_grain() {
    // The command itself names the exact test, so it cannot be disclosed —
    // the brief must step down rather than emit it.
    let paths: Vec<String> = Vec::new();
    let inv = invocation(&["test", "--exact", "witness_balance_stays_consistent"]);
    let failure = sealed(
        "cargo test --exact witness_balance_stays_consistent",
        ASSERTION_TAIL,
        Some(&inv),
        &paths,
    );

    let brief = redact(&failure, DisclosureGrain::Reproduction);
    assert!(brief.grain < DisclosureGrain::Reproduction);
    assert_eq!(brief.reproduction, None);
    assert!(!brief.message().contains("witness_balance_stays_consistent"));
    // The step-down keeps the symptom: it is a classification, safe by
    // construction, and this is the realistic authored-witness shape — the
    // command always names its sealed selector, so demoting all the way to
    // `Outcome` here meant every authored-witness failure disclosed nothing
    // and the worker revised blind for its whole budget.
    assert_eq!(brief.grain, DisclosureGrain::Symptom);
    assert!(
        brief.symptom.is_some(),
        "an unscannable command must not silence the symptom classification"
    );
    assert_eq!(brief.command, None);
}

#[test]
fn a_package_argument_does_not_seal_the_command() {
    // `-p <crate>` names the suite, not the test: the command grain exists to
    // disclose exactly this shape. Over-matching it degraded every ordinary
    // `cargo test -p crate` failure to bare L0 — dropping even the symptom
    // sentence, which is safe by construction.
    let paths: Vec<String> = Vec::new();
    let inv = invocation(&["test", "-p", "stella-pipeline"]);
    let failure = sealed(
        "cargo test -p stella-pipeline",
        ASSERTION_TAIL,
        Some(&inv),
        &paths,
    );

    let brief = redact(&failure, DisclosureGrain::Reproduction);
    assert_eq!(brief.grain, DisclosureGrain::Reproduction);
    assert_eq!(
        brief.command.as_deref(),
        Some("cargo test -p stella-pipeline")
    );
    assert!(brief.symptom.is_some(), "the safe symptom survives");
    assert!(brief.reproduction.is_some());
}

#[test]
fn a_test_file_path_does_not_seal_the_command() {
    // A pytest file (with or without a directory) collects a suite; only a
    // `::node` part names the exact test.
    let paths: Vec<String> = Vec::new();
    for (command, args) in [
        (
            "pytest tests/test_payment.py -q",
            ["tests/test_payment.py", "-q"],
        ),
        ("pytest test_payment.py -q", ["test_payment.py", "-q"]),
    ] {
        let inv = invocation_for("pytest", &args);
        let failure = sealed(command, ASSERTION_TAIL, Some(&inv), &paths);
        let brief = redact(&failure, DisclosureGrain::Reproduction);
        assert_eq!(
            brief.grain,
            DisclosureGrain::Reproduction,
            "a file argument must not seal `{command}`"
        );
        assert!(brief.symptom.is_some(), "the safe symptom survives");
    }
}

#[test]
fn a_go_package_path_does_not_seal_but_its_run_filter_does() {
    let paths: Vec<String> = Vec::new();
    // Package path only: the suite, disclosable.
    let suite = invocation_for("go", &["test", "./pkg"]);
    let failure = sealed("go test ./pkg", ASSERTION_TAIL, Some(&suite), &paths);
    assert_eq!(
        redact(&failure, DisclosureGrain::Reproduction).grain,
        DisclosureGrain::Reproduction
    );

    // A `-run` filter names the exact test the detector runs — sealed, in
    // both the space-separated and `=`-joined spellings.
    for (command, args) in [
        (
            "go test ./pkg -run ^TestAuthority$",
            vec!["test", "./pkg", "-run", "^TestAuthority$"],
        ),
        (
            "go test ./pkg -run=^TestAuthority$",
            vec!["test", "./pkg", "-run=^TestAuthority$"],
        ),
    ] {
        let inv = invocation_for("go", &args);
        let failure = sealed(command, ASSERTION_TAIL, Some(&inv), &paths);
        let brief = redact(&failure, DisclosureGrain::Reproduction);
        assert!(brief.grain < DisclosureGrain::Reproduction);
        assert!(!brief.message().contains("TestAuthority"));
    }
}

#[test]
fn a_pytest_node_id_still_seals_the_command() {
    let paths: Vec<String> = Vec::new();
    let inv = invocation_for(
        "pytest",
        &["tests/test_payment.py::test_refund_rounds_down"],
    );
    let failure = sealed(
        "pytest tests/test_payment.py::test_refund_rounds_down",
        ASSERTION_TAIL,
        Some(&inv),
        &paths,
    );

    let brief = redact(&failure, DisclosureGrain::Reproduction);
    assert!(brief.grain < DisclosureGrain::Reproduction);
    assert!(!brief.message().contains("test_refund_rounds_down"));
}

#[test]
fn grain_is_monotone_in_what_it_discloses() {
    let paths: Vec<String> = Vec::new();
    let failure = sealed("pnpm test", ASSERTION_TAIL, None, &paths);

    let outcome = redact(&failure, DisclosureGrain::Outcome);
    assert_eq!(outcome.command, None);
    assert_eq!(outcome.symptom, None);
    assert_eq!(outcome.reproduction, None);

    let command = redact(&failure, DisclosureGrain::Command);
    assert!(command.command.is_some());
    assert_eq!(command.symptom, None);

    let symptom = redact(&failure, DisclosureGrain::Symptom);
    assert!(symptom.symptom.is_some());
    assert_eq!(symptom.reproduction, None);

    let full = redact(&failure, DisclosureGrain::Reproduction);
    assert!(full.reproduction.is_some());
}

#[test]
fn the_same_failure_fingerprints_identically_across_runs() {
    let first = "\
test foo ... FAILED
thread 'foo' panicked at /var/folders/9x/T/candidate-1/src/lib.rs:14:5:
assertion failed
test result: FAILED. 0 passed; 1 failed; finished in 0.31s
";
    let second = "\
test foo ... FAILED
thread 'foo' panicked at /tmp/candidate-2/src/lib.rs:14:5:
assertion failed
test result: FAILED. 0 passed; 1 failed; finished in 1.07s
";
    assert_eq!(
        FailureFingerprint::of(first),
        FailureFingerprint::of(second),
        "a temp-dir prefix and a timing must not split one failure"
    );
}

#[test]
fn a_different_failure_fingerprints_differently() {
    let assertion = "thread 'foo' panicked at src/lib.rs:14:5: assertion failed";
    let compile = "error[E0308]: mismatched types";
    assert_ne!(
        FailureFingerprint::of(assertion),
        FailureFingerprint::of(compile)
    );
}

#[test]
fn color_does_not_change_a_fingerprint() {
    let plain = "test foo ... FAILED\nassertion failed\n";
    let colored = "test foo ... \u{1b}[31mFAILED\u{1b}[0m\n\u{1b}[1massertion failed\u{1b}[0m\n";
    assert_eq!(
        FailureFingerprint::of(plain),
        FailureFingerprint::of(colored)
    );
}

#[test]
fn symptom_classification_prefers_the_most_specific_cause() {
    // A build failure that also prints "expected" is a build failure: nothing
    // ran, so there is no assertion to report.
    assert_eq!(
        SymptomClass::classify("error[E0308]: mismatched types, expected `u32`"),
        SymptomClass::BuildFailure
    );
    assert_eq!(
        SymptomClass::classify("thread 'x' panicked at src/a.rs:1:1: boom"),
        SymptomClass::Crash
    );
    assert_eq!(
        SymptomClass::classify("assertion `left == right` failed"),
        SymptomClass::Assertion
    );
    assert_eq!(
        SymptomClass::classify("error: test timed out after 60s"),
        SymptomClass::Timeout
    );
    assert_eq!(
        SymptomClass::classify("exit status 1"),
        SymptomClass::ExitFailure
    );
}

#[test]
fn every_symptom_sentence_is_behavioral_not_internal() {
    for class in [
        SymptomClass::BuildFailure,
        SymptomClass::Assertion,
        SymptomClass::Crash,
        SymptomClass::Timeout,
        SymptomClass::ExitFailure,
    ] {
        let sentence = class.sentence();
        for internal in ["assert_eq", "left", "right", "test ", ".rs"] {
            assert!(
                !sentence.contains(internal),
                "{class:?} sentence names a test internal: {sentence}"
            );
        }
    }
}

#[test]
fn the_scrubber_catches_a_reflowed_quote() {
    let paths: Vec<String> = Vec::new();
    let failure = sealed("cargo test", ASSERTION_TAIL, None, &paths);
    // Same words as the sealed output, rewrapped and recased.
    let reflowed = "ASSERTION `LEFT == RIGHT` FAILED   LEFT: 4200";
    match scrub(reflowed, &failure) {
        // The whole quoted run, collapsed: "assertion `left == right` failed
        // left: 4200" — not the 24-character probe window that detected it.
        Err(LeakKind::QuotedOutput(span)) => assert_eq!(
            span, 43,
            "the reported span is the real quoted run, not the minimum window"
        ),
        other => panic!("expected a QuotedOutput leak, got {other:?}"),
    }
}

#[test]
fn the_scrubber_admits_ordinary_guidance() {
    let paths: Vec<String> = Vec::new();
    let failure = sealed("cargo test", ASSERTION_TAIL, None, &paths);
    assert_eq!(scrub("Verification did not pass.", &failure), Ok(()));
}

#[test]
fn repeated_failures_tighten_the_ladder() {
    assert_eq!(grain_for_repeats(0), DisclosureGrain::Reproduction);
    assert_eq!(grain_for_repeats(1), DisclosureGrain::Reproduction);
    assert_eq!(grain_for_repeats(2), DisclosureGrain::Symptom);
    assert_eq!(grain_for_repeats(3), DisclosureGrain::Command);
    assert_eq!(grain_for_repeats(9), DisclosureGrain::Outcome);

    // Monotone: more repeats never discloses more.
    let mut previous = grain_for_repeats(0);
    for repeats in 1..12 {
        let current = grain_for_repeats(repeats);
        assert!(current <= previous, "grain widened at {repeats} repeats");
        previous = current;
    }
}

#[test]
fn evidence_refs_name_the_grain_and_the_failure() {
    let paths: Vec<String> = Vec::new();
    let failure = sealed("cargo test", ASSERTION_TAIL, None, &paths);
    let brief = redact(&failure, DisclosureGrain::Reproduction);
    let refs = brief.evidence_refs(&failure.fingerprint());
    assert!(refs.iter().any(|r| r.starts_with("disclosure:L")));
    assert!(refs.iter().any(|r| r.starts_with("failure:")));
}
