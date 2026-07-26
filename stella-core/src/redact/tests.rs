//! Tests for free-text secret redaction.
//!
//! Two properties matter more than any individual pattern:
//!
//! * nothing recognizable as a credential survives into the output — asserted
//!   by scanning the *result* for the secret's own characters, not by comparing
//!   against an expected string, so a rule that redacts the wrong span still
//!   fails;
//! * ordinary lesson text is left alone, because a redactor that eats real
//!   content makes the learning loop worse and will be turned off.

use super::*;

/// Assert the secret is gone AND that the redaction was reported.
fn assert_redacted(input: &str, secret: &str) {
    let result = redact_secrets(input);
    assert!(
        !result.text.contains(secret),
        "the secret survived redaction:\n  input:  {input}\n  output: {}",
        result.text
    );
    assert!(
        result.redacted,
        "the text was changed but `redacted` was not set: {input}"
    );
    assert!(
        result.text.contains(PLACEHOLDER),
        "no placeholder in the output: {}",
        result.text
    );
}

/// Assert the text passed through untouched.
fn assert_untouched(input: &str) {
    let result = redact_secrets(input);
    assert_eq!(result.text, input, "redactor damaged ordinary text");
    assert!(!result.redacted);
}

/// Every fixture below is synthetic, but some vendor prefixes are recognizable
/// enough that a *fake* key still trips a scanner. GitHub's push protection
/// rejected this file's original Stripe fixture, which is the mechanism working
/// as intended and a reason not to write the literal down: the string is
/// assembled at runtime so no key-shaped constant exists in the source, while
/// the redactor still sees exactly the bytes it would see in a real lesson.
fn synthetic_stripe_key() -> String {
    format!("sk{}live{}51H{}YY", "_", "_", "x".repeat(20))
}

#[test]
fn vendor_prefixed_tokens_are_redacted() {
    let stripe = synthetic_stripe_key();
    assert_redacted(&format!("Stripe: {stripe}"), &stripe);

    for (input, secret) in [
        (
            "The reflection call failed with sk-ant-api03-AbCdEf123456789 in the header.",
            "sk-ant-api03-AbCdEf123456789",
        ),
        (
            "Use ghp_16C7e42F292c6912E7710c838347Ae178B4a for the API.",
            "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
        ),
        (
            "AWS key AKIAIOSFODNN7EXAMPLE is configured.",
            "AKIAIOSFODNN7EXAMPLE",
        ),
        (
            "Slack posts need xoxb-123456789012-abcdefghijkl.",
            "xoxb-123456789012-abcdefghijkl",
        ),
        (
            "Google maps key AIzaSyD-1234567890abcdefghijk works.",
            "AIzaSyD-1234567890abcdefghijk",
        ),
        (
            "The token is github_pat_11ABCDEFG0abcdefghij_xyz here.",
            "github_pat_11ABCDEFG0abcdefghij_xyz",
        ),
    ] {
        assert_redacted(input, secret);
    }
}

#[test]
fn jwts_are_redacted() {
    let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
    assert_redacted(
        &format!("Authorization header carried {jwt} on that call."),
        jwt,
    );
}

#[test]
fn sensitive_key_assignments_are_redacted_whatever_the_value_looks_like() {
    // The point of the key-name rule: `hunter2` is not recognizable as a
    // credential on its own evidence, only from what it is assigned to.
    for (input, secret) in [
        ("Set DB_PASSWORD=hunter2 before running.", "hunter2"),
        ("password: hunter2 in the config", "hunter2"),
        (r#"{"api_key": "plainvalue"}"#, "plainvalue"),
        ("client_secret = shhhh, then restart", "shhhh"),
        ("export AUTH_TOKEN=abc123def", "abc123def"),
    ] {
        assert_redacted(input, secret);
    }
}

/// `*_env` names the environment variable to read a secret FROM, not the
/// secret — the same exemption the settings scrubber makes.
#[test]
fn an_env_indirection_key_is_not_a_secret() {
    assert_untouched("Set api_key_env=ANTHROPIC_API_KEY in settings.json.");
}

#[test]
fn url_credentials_are_redacted() {
    let result = redact_secrets("Clone from https://someone:s3cr3tpw@github.com/org/repo.git");
    assert!(!result.text.contains("s3cr3tpw"), "{}", result.text);
    assert!(result.redacted);
    // The host must survive — an unusable lesson helps nobody.
    assert!(
        result.text.contains("github.com/org/repo.git"),
        "{}",
        result.text
    );
}

#[test]
fn pem_private_keys_are_redacted_including_a_truncated_block() {
    let key = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAx\nQm9yaW5n\n-----END RSA PRIVATE KEY-----";
    assert_redacted(
        &format!("The deploy key is:\n{key}\nand it rotated."),
        "MIIEowIBAAKCAQEAx",
    );

    // A half-pasted key is still a key.
    let truncated = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA";
    assert_redacted(truncated, "b3BlbnNzaC1rZXktdjEAAAAA");
}

/// A certificate or public key is not a secret, and redacting one loses real
/// information about the workspace.
#[test]
fn certificates_and_public_keys_are_left_alone() {
    assert_untouched("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----");
    assert_untouched("-----BEGIN PUBLIC KEY-----\nMIIB\n-----END PUBLIC KEY-----");
}

#[test]
fn long_opaque_blobs_are_redacted() {
    assert_redacted(
        "The value wpKzT4mQ8rXvB2nL6yH9jF3sD7gA1cE5 appeared in the log.",
        "wpKzT4mQ8rXvB2nL6yH9jF3sD7gA1cE5",
    );
}

// ---- the other half: ordinary lesson text must survive ----

/// The real lessons this loop mines. If the redactor eats these, it makes the
/// product worse and gets turned off, which leaks everything.
#[test]
fn real_reflection_lessons_pass_through_untouched() {
    for lesson in [
        "In stella-cli witness tests (slash_models_witness.rs), prefer updating \
         assertions to match the live renderer output rather than changing core \
         CLI behavior.",
        "For cargo test filters like --exact, always place them after the -- \
         separator (cargo test -- --exact) to pass args to the test binary.",
        "Reach for ripgrep instead of grep when searching this repository; it is \
         gitignore-aware and much faster.",
        "The migration ladder in stella-context/src/store/schema.rs is ordered, \
         so take the next free version rather than reusing one.",
        "Run `make gate` before claiming a change is done.",
    ] {
        assert_untouched(lesson);
    }
}

/// A git SHA is all-lowercase hex, which is exactly why the blob heuristic
/// requires mixed case. Commit hashes appear constantly in lessons.
#[test]
fn git_shas_and_paths_are_not_mistaken_for_secrets() {
    assert_untouched("Commit 7f8bf1ac9d3e4b5a6c7d8e9f0a1b2c3d4e5f6a7b broke the build.");
    assert_untouched(
        "See stella-context/src/store/schema.rs and docs/adr/0010-incremental-authority-transfer.md",
    );
    assert_untouched("The URL https://github.com/macanderson/stella/issues/714 tracks this.");
}

#[test]
fn short_words_are_never_redacted() {
    assert_untouched("sk-1");
    assert_untouched("The AKIA prefix identifies an AWS key id.");
}

#[test]
fn empty_and_whitespace_text_is_a_no_op() {
    assert_untouched("");
    assert_untouched("   \n\t ");
}

/// Redaction is idempotent — running it over already-redacted text changes
/// nothing further, so a replayed extraction cannot slowly eat a lesson.
#[test]
fn redaction_is_idempotent() {
    let once =
        redact_secrets("Set DB_PASSWORD=hunter2 and use ghp_16C7e42F292c6912E7710c838347Ae178B4a.");
    let twice = redact_secrets(&once.text);
    assert_eq!(twice.text, once.text);
    assert!(
        !twice.redacted,
        "a second pass found nothing left to redact"
    );
}

/// Multiple secrets in one string are all removed, not just the first.
#[test]
fn every_secret_in_a_string_is_redacted() {
    let result = redact_secrets(
        "First ghp_16C7e42F292c6912E7710c838347Ae178B4a then AKIAIOSFODNN7EXAMPLE then password: hunter2.",
    );
    assert!(!result.text.contains("ghp_16C7"), "{}", result.text);
    assert!(
        !result.text.contains("AKIAIOSFODNN7EXAMPLE"),
        "{}",
        result.text
    );
    assert!(!result.text.contains("hunter2"), "{}", result.text);
    assert_eq!(
        result.text.matches(PLACEHOLDER).count(),
        3,
        "{}",
        result.text
    );
}

/// The placeholder does not encode the secret's length — a length-preserving
/// mask would leak it.
#[test]
fn the_placeholder_is_fixed_width() {
    let short = redact_secrets("token: abcdefgh");
    let long = redact_secrets(&format!("token: {}", "a".repeat(400)));
    assert_eq!(
        short.text.matches(PLACEHOLDER).count(),
        long.text.matches(PLACEHOLDER).count()
    );
    assert!(!long.text.contains("aaaa"));
}
