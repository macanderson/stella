//! Which providers the live smoke suite is armed for, and why the rest are
//! not.
//!
//! A provider in `LIVE_PROVIDERS` is armed, or it has a row in [`UNARMED`].
//! There is no third state. [`stale_declaration`] re-asks each row's reason
//! on every run, so a row cannot outlive what it claims.
//!
//! `doc:adr/0038-a-live-smoke-provider-is-armed-or-declared-unarmed` is the
//! decision and the argument behind it.

/// Why a provider is not armed here today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnarmedReason {
    /// No credential for this provider reaches the job, so there is nothing
    /// to call. The smoke prints the row and passes.
    NoCredential,
    /// The credential works and the account behind it holds no money. The
    /// smoke calls the endpoint anyway, and passes only on a refusal that
    /// names the balance.
    Unfunded,
}

/// One declared gap in the live matrix.
pub(crate) struct UnarmedProvider {
    /// The `LIVE_PROVIDERS` id this row speaks for.
    pub(crate) id: &'static str,
    pub(crate) reason: UnarmedReason,
    /// The number of the issue that owns the gap, so a green run points at
    /// where it is being settled. Stored bare and printed with a `#`.
    pub(crate) issue: &'static str,
    /// What is missing, in the words a reader of a green run needs.
    pub(crate) note: &'static str,
}

/// The providers this repository cannot smoke today.
///
/// Every note comes from a run. Run 34126157647 (2026-09-07) failed all
/// eight. Six named the environment variable they wanted. Two carried the
/// provider's own words about the balance.
pub(crate) const UNARMED: &[UnarmedProvider] = &[
    UnarmedProvider {
        id: "anthropic",
        reason: UnarmedReason::Unfunded,
        issue: "5595",
        note: "ANTHROPIC_API_KEY works and the account is empty. Anthropic answers HTTP 400 \
               `Your credit balance is too low` (run 34126157647, 2026-09-07). It last passed \
               on run 32007329950, 2026-08-17.",
    },
    UnarmedProvider {
        id: "zai",
        reason: UnarmedReason::Unfunded,
        issue: "5595",
        note: "ZAI_API_KEY works and the account is empty. Z.ai answers HTTP 429 `Insufficient \
               balance or no resource package` (run 34126157647, 2026-09-07).",
    },
    UnarmedProvider {
        id: "openai",
        reason: UnarmedReason::NoCredential,
        issue: "5595",
        note: "no OPENAI_API_KEY in the repository secrets.",
    },
    UnarmedProvider {
        id: "gemini",
        reason: UnarmedReason::NoCredential,
        issue: "5595",
        note: "no GEMINI_API_KEY in the repository secrets.",
    },
    UnarmedProvider {
        id: "deepseek",
        reason: UnarmedReason::NoCredential,
        issue: "5595",
        note: "no DEEPSEEK_API_KEY in the repository secrets.",
    },
    UnarmedProvider {
        id: "xai",
        reason: UnarmedReason::NoCredential,
        issue: "5595",
        note: "no XAI_API_KEY in the repository secrets.",
    },
    UnarmedProvider {
        id: "bedrock",
        reason: UnarmedReason::NoCredential,
        issue: "5595",
        note: "no AWS_ACCESS_KEY_ID or AWS_SECRET_ACCESS_KEY in the repository secrets. An IAM \
               user scoped to `bedrock:InvokeModel` is what this needs.",
    },
    UnarmedProvider {
        id: "vertex",
        reason: UnarmedReason::NoCredential,
        issue: "5595",
        note: "no GCP_SERVICE_ACCOUNT_KEY in the repository secrets, so the job mints no \
               VERTEX_ACCESS_TOKEN. VERTEX_PROJECT_ID is absent too.",
    },
];

/// The row for `provider_id`, when the matrix declares one unarmed.
pub(crate) fn unarmed(provider_id: &str) -> Option<&'static UnarmedProvider> {
    UNARMED.iter().find(|row| row.id == provider_id)
}

/// The message a row that stopped matching its environment fails with.
///
/// `credential_resolves` comes from the credential chain alone. The
/// `STELLA_LIVE_SMOKE` gate never feeds it. Whether a secret is configured is
/// a fact about the job, and turning the gate off must not read as a secret
/// going missing.
pub(crate) fn stale_declaration(
    entry: &UnarmedProvider,
    credential_resolves: bool,
) -> Option<String> {
    match (entry.reason, credential_resolves) {
        (UnarmedReason::NoCredential, true) => Some(format!(
            "[live_smoke] {}: the unarmed row is stale — it says no credential is configured, \
             and one resolves. Delete the `{}` row from UNARMED \
             (crates/stella-model/tests/live_smoke/arming.rs) and let the live call run.",
            entry.id, entry.id
        )),
        (UnarmedReason::Unfunded, false) => Some(format!(
            "[live_smoke] {}: the unarmed row is stale — it says the account is empty, which \
             needs a working credential, and none resolves. Restore the credential, or change \
             the `{}` row's reason to NoCredential \
             (crates/stella-model/tests/live_smoke/arming.rs).",
            entry.id, entry.id
        )),
        _ => None,
    }
}

/// A success from a provider this table declares unfunded means the account
/// has money again, so the row is wrong and the run says so.
pub(crate) fn success_contradicts(provider_id: &str) -> Option<String> {
    let entry = unarmed(provider_id)?;
    (entry.reason == UnarmedReason::Unfunded).then(|| {
        format!(
            "[live_smoke] {provider_id}: the unarmed row is stale — it says the account is \
             empty, and the call succeeded. Delete the `{provider_id}` row from UNARMED \
             (crates/stella-model/tests/live_smoke/arming.rs) so this provider is watched again."
        )
    })
}

/// What a failed call settles for `provider_id`.
///
/// `Ok` carries the line a confirmed row prints; `Err` carries the report the
/// run fails with. Only an `Unfunded` row can turn a failure into a pass, and
/// only on the `Billing` verdict a reader of the log would act on. A rejected
/// field resolves to WIRE SHAPE and still fails, which is what keeps the two
/// unfunded adapters under the guard rather than outside it.
pub(crate) fn failure_settles(
    provider_id: &str,
    error: &stella_protocol::ProviderError,
) -> Result<String, String> {
    let report = super::failure_report(provider_id, error);
    let Some(entry) = unarmed(provider_id) else {
        return Err(report);
    };
    if entry.reason != UnarmedReason::Unfunded
        || super::resolve(error).1 != super::FailureCause::Billing
    {
        return Err(report);
    }
    Ok(format!(
        "[live_smoke] {provider_id}: unarmed by declaration (#{}) — {} The provider confirmed it \
         on this run: {}",
        entry.issue,
        entry.note,
        super::bounded_detail(&error.to_string()),
    ))
}

// ---- witnesses (always run; never touch the network) ---------------------

/// A row that names nothing real is a row nobody can act on.
#[test]
fn every_unarmed_row_names_a_live_provider_an_issue_and_a_reason() {
    let mut seen = std::collections::BTreeSet::new();
    for entry in UNARMED {
        assert!(
            super::LIVE_PROVIDERS.iter().any(|p| p.id == entry.id),
            "UNARMED names `{}`, which is not a LIVE_PROVIDERS row",
            entry.id
        );
        assert!(
            seen.insert(entry.id),
            "duplicate UNARMED row for `{}`",
            entry.id
        );
        assert!(
            entry.issue.parse::<u32>().is_ok(),
            "`{}` must cite the number of the issue that owns the gap",
            entry.id
        );
        assert!(
            entry.note.len() > 20,
            "`{}` must say what is missing, not just that something is",
            entry.id
        );
    }
}

/// The point of the table: a gap that closes itself says so.
#[test]
fn a_credential_that_resolves_makes_a_no_credential_row_stale() {
    let entry = unarmed("openai").expect("openai is declared unarmed");
    let stale = stale_declaration(entry, true).expect("a resolving credential is stale");
    assert!(stale.contains("Delete the `openai` row"), "{stale}");
    assert!(stale_declaration(entry, false).is_none());
}

/// The other direction: an `Unfunded` row claims the key works, so a key
/// that stopped resolving means the row names the wrong thing.
#[test]
fn a_credential_that_vanishes_makes_an_unfunded_row_stale() {
    let entry = unarmed("anthropic").expect("anthropic is declared unarmed");
    let stale = stale_declaration(entry, false).expect("a vanished credential is stale");
    assert!(
        stale.contains("change the `anthropic` row's reason"),
        "{stale}"
    );
    assert!(stale_declaration(entry, true).is_none());
}

/// The refusal an `Unfunded` row predicts, in the provider's own words, is
/// the one failure that confirms the row instead of failing the run.
///
/// Both bodies are quoted from run 34126157647 (2026-09-07). Both reach
/// `resolve` as `Terminal`. Anthropic spells an empty account as a 400 and
/// Z.ai spells one as a 429, so the status settles neither. The body does.
#[test]
fn a_balance_refusal_confirms_an_unfunded_row() {
    let anthropic = stella_protocol::ProviderError::Terminal(
        "terminal provider error: Anthropic HTTP 400 Bad Request: {\"type\":\"error\",\"error\":\
         {\"type\":\"invalid_request_error\",\"message\":\"Your credit balance is too low to \
         access the Anthropic API.\"}}"
            .to_string(),
    );
    let confirmed = failure_settles("anthropic", &anthropic).expect("a balance refusal confirms");
    assert!(confirmed.contains("unarmed by declaration"), "{confirmed}");
    assert!(
        confirmed.contains("5595"),
        "the confirmation must point at the issue that owns the gap: {confirmed}"
    );

    let zai = stella_protocol::ProviderError::Terminal(
        "terminal provider error: Z.ai HTTP 429: Insufficient balance or no resource package. \
         Please recharge."
            .to_string(),
    );
    assert!(failure_settles("zai", &zai).is_ok());
}

/// The half that keeps the guard pointed at the adapter: a row saying the
/// account is empty excuses an empty account and nothing else. Anthropic is
/// the adapter `#240` was filed on, so a wire-shape failure there must still
/// redden the run.
#[test]
fn a_wire_shape_failure_still_fails_a_provider_declared_unfunded() {
    let malformed = stella_protocol::ProviderError::Malformed(
        "missing field `content` at line 1 column 84".to_string(),
    );
    let report = failure_settles("anthropic", &malformed).expect_err("a wire-shape failure fails");
    assert!(report.contains("WIRE SHAPE"), "{report}");

    let rotated = stella_protocol::ProviderError::Auth(
        "anthropic rejected the credential (HTTP 401): invalid x-api-key".to_string(),
    );
    let report = failure_settles("anthropic", &rotated).expect_err("a revoked key fails");
    assert!(report.contains("CREDENTIAL"), "{report}");
}

/// A provider with no row is judged the way it always was, whatever its
/// failure says.
#[test]
fn a_provider_with_no_row_is_never_excused_by_one() {
    let billing = stella_protocol::ProviderError::Terminal(
        "terminal provider error: OpenRouter HTTP 402: your credit balance is too low".to_string(),
    );
    assert!(failure_settles("openrouter", &billing).is_err());
}

/// The last direction a row can go stale: the account is funded again, the
/// call succeeds, and a row saying it cannot is now wrong.
#[test]
fn a_success_fails_a_provider_declared_unfunded() {
    let stale = success_contradicts("anthropic").expect("a funded call is stale");
    assert!(stale.contains("Delete the `anthropic` row"), "{stale}");
    assert!(
        success_contradicts("openrouter").is_none(),
        "an armed provider's success is a pass, not a contradiction"
    );
    assert!(
        success_contradicts("openai").is_none(),
        "a NoCredential row makes no call, so it has no success to contradict"
    );
}
