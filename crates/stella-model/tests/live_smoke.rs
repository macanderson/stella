//! Opt-in live smoke suite (issue #274) — one minimal REAL call per provider
//! adapter, against the provider's actual endpoint. Every other test in this
//! crate runs against `wiremock`, which proves "we parse the shape we
//! expect" but can never prove "the live API accepts the shape we send" —
//! exactly the gap that let the Anthropic adapter's legacy `thinking` shape
//! ship for months before a live call surfaced its 400 (see #240).
//!
//! **Gate**: every live call is `#[ignore]`d, so `cargo test`/`make gate`/
//! CI's default `cargo test --workspace` never makes a network call or
//! spends real money — and reports the nine as `9 ignored`, which nobody
//! reads as a pass. Running one takes naming it: `--ignored`, plus
//! `STELLA_LIVE_SMOKE=1` (see [`live_smoke_enabled`]) and that provider's
//! own credential (env var, or a `~/.stella/credentials.toml` entry — the
//! same two non-interactive steps `stella-cli`'s chain uses, see
//! [`resolve_key`]). Wire it up locally with e.g.
//! `STELLA_LIVE_SMOKE=1 ANTHROPIC_API_KEY=sk-… cargo test -p stella-model \
//! --test live_smoke -- --ignored --nocapture`.
//!
//! **A named smoke that cannot run fails.** The attribute is what makes a
//! skip visible; it is also what makes the absence of a credential a
//! failure rather than a default. Nothing reaches one of these tests unless
//! a human or the workflow asked for it by name, so "no key resolved" is a
//! failure to run what was asked for — see [`required_key`]. The eprintln
//! skip it replaces went through libtest's output capture, so a
//! credential-less run printed nothing and reported nine passes in 0.01s
//! having contacted nothing (#3856).
//!
//! **What is NOT gated**: the drift guards over [`LIVE_PROVIDERS`] — the
//! table this suite drives — run on every `cargo test`, with no credential
//! and no network call. The gate above is right for spending money; it was
//! wrong for the things a catalog can check offline for free, and applying
//! it to everything is what let a phantom slug produce a green default run
//! and surface (at most weekly, from the scheduled workflow) as an ambiguous
//! wire-shape-or-billing panic. See
//! [`every_live_smoke_slug_resolves_against_the_catalog_seed`] and
//! [`every_row_constructs_through_the_real_factory`].
//!
//! **How providers are constructed**: through
//! [`stella_model::factory::build_provider`] — the same factory
//! `stella_cli::agent::engine::build_provider_parts` delegates to. Each test
//! used to hand-build its adapter, which meant nine construction sites could
//! drift from production's one (and did: the OpenRouter attribution literals
//! were copied verbatim), and meant every one of them silently bypassed the
//! factory's anti-phantom-slug check. Adding a provider is now a row in
//! [`LIVE_PROVIDERS`] plus a four-line test.
//!
//! **What each test asserts**: wire-shape ACCEPTANCE — the live endpoint
//! returns 200 and stella's own parser reassembles a `CompletionResult` from
//! it — never model quality (no assertion on what the model actually says).
//! A wire-shape regression (a field the adapter sends that the live API now
//! rejects, or a response shape it no longer parses) fails the relevant
//! test with the real `ProviderError`, which already carries the
//! provider's own rejection reason (see `http::classify_http_status`) —
//! never a raw credential.
//!
//! **A red run names its own cause.** These tests are opt-in and cost money,
//! so a failure is typically read days later by someone with no credential to
//! re-run it. Every failing smoke therefore reports through
//! [`failure_report`]: a verdict (credential / quota / wire shape /
//! undetermined) derived from the typed `ProviderError` variant — which *is*
//! the adapter's own classification of the status code — plus the recovered
//! status and a bounded prefix of the provider's text. The message it
//! replaces stated the ambiguity instead of resolving it ("could be a genuine
//! wire-shape regression OR an unrelated account/auth/quota/billing
//! problem"), which is what left #3259's red run undiagnosable from its log.
//! The verdict path is covered offline by the witnesses beside it.
//!
//! **The Anthropic `cache_control` question** (the other half of #274):
//! `anthropic.rs`'s `stamp_tail_cache_breakpoint` places `cache_control`
//! only on content BLOCKS (the system block, and the last block of the
//! final message) — never as a top-level request field, which the assumption
//! holds would 400. That placement is asserted against a mock in
//! `anthropic::tests::request_serializes_both_cache_breakpoints`;
//! `anthropic_smoke` below checks it against the real API by sending a system
//! prompt long enough to clear Anthropic's minimum cacheable-prefix floor (so
//! a real cache write is exercised, not just accepted-but-inert) and asserting
//! the call succeeds.

use stella_model::AuxCredentials;
use stella_model::catalog::Catalog;
use stella_model::credential::{ApiKey, CredentialsFile};
use stella_model::factory::{Dialect, ProviderSpec, build_provider};
use stella_model::provider::Provider;
use stella_protocol::{CompletionMessage, CompletionRequest, ProviderError};

/// Serializes the tests in this file that mutate `STELLA_LIVE_SMOKE` (the
/// gate-behavior witnesses below) against every test that reads it
/// (`armed_key`, called at the top of every live-* test) — `setenv`/`getenv`
/// racing across threads is documented UB on POSIX regardless of which side
/// mutates, and libtest runs this binary's tests on parallel threads.
/// Mirrors `stella-cli`'s `test_env` convention. Held only across the
/// synchronous gate-check + key-resolve step, never across a network await.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Whether the live smoke suite is armed at all. Requires the LITERAL value
/// `1` (not `true`/`yes`/any other truthy-looking string) — a narrow,
/// unambiguous switch for something that spends real money and makes real
/// network calls, deliberately stricter than the repo's other `STELLA_*`
/// boolean gates (`STELLA_NO_ENV_FILE`, `STELLA_ENV_DEBUG`), which accept
/// any non-empty non-`"0"` value.
fn live_smoke_enabled() -> bool {
    std::env::var("STELLA_LIVE_SMOKE").is_ok_and(|v| v == "1")
}

/// Resolve `env_var`'s (and, in order, each of `aliases`') credential from
/// the process environment, then `~/.stella/credentials.toml` —
/// the same two non-interactive steps of `stella-cli`'s chain
/// (`ApiKey::resolve` + alias fallback), minus the CLI-flag and
/// interactive-prompt steps, neither of which make sense for an
/// unattended test. Never panics; `None` means "nothing resolves."
fn resolve_key(provider_id: &str, env_var: &str, aliases: &[&str]) -> Option<ApiKey> {
    if let Ok(key) = ApiKey::from_env(env_var) {
        return Some(key);
    }
    for alias in aliases {
        if let Ok(key) = ApiKey::from_env(alias) {
            return Some(key);
        }
    }
    CredentialsFile::load_default()
        .ok()?
        .get(provider_id)
        .map(ApiKey::new)
}

/// Core of [`armed_key`], assuming the caller already holds `env_lock()`.
/// Split out so the gate-behavior witness tests below — which need their
/// own guard held across a full mutate-assert-restore window — can call
/// this directly instead of re-entering `env_lock()` through `armed_key`:
/// `std::sync::Mutex` is not reentrant, so a witness test that already
/// holds the guard and then called `armed_key` (which locks again on the
/// same thread) would deadlock rather than assert. Always logs WHY to
/// stderr (visible with `cargo test -- --nocapture`), so a `--nocapture` run
/// names the missing half before [`required_key`] turns it into a failure.
fn armed_key_locked(provider_id: &str, env_var: &str, aliases: &[&str]) -> Option<ApiKey> {
    if !live_smoke_enabled() {
        eprintln!("[live_smoke] {provider_id}: unarmed — set STELLA_LIVE_SMOKE=1 to run");
        return None;
    }
    match resolve_key(provider_id, env_var, aliases) {
        Some(key) => Some(key),
        None => {
            let alias_note = if aliases.is_empty() {
                String::new()
            } else {
                format!(" (or {})", aliases.join("/"))
            };
            eprintln!(
                "[live_smoke] {provider_id}: no credential — no {env_var}{alias_note} env var \
                 and no `{provider_id}` entry in ~/.stella/credentials.toml"
            );
            None
        }
    }
}

/// The credential `provider_id`'s smoke test would run with, or `None` when
/// the suite isn't armed or the credential isn't resolvable —
/// [`required_key`] is what decides that `None` is a failure. Takes
/// `env_lock()` for the synchronous gate-check + resolve step only, released
/// well before any caller's network `.await` (see `env_lock`'s doc). Callers
/// that already hold the guard themselves must call [`armed_key_locked`]
/// instead, not this.
fn armed_key(provider_id: &str, env_var: &str, aliases: &[&str]) -> Option<ApiKey> {
    let _guard = env_lock();
    armed_key_locked(provider_id, env_var, aliases)
}

/// A minimal completion request: one short user turn, output capped tiny.
/// The point is wire-shape acceptance, never model quality, so content is
/// deliberately trivial — `system` is the only part callers vary (the
/// Anthropic cache-control probe needs a long one; everyone else gets a
/// short one).
fn tiny_request(system: impl Into<String>) -> CompletionRequest {
    CompletionRequest {
        messages: vec![
            CompletionMessage::system(system),
            CompletionMessage::user("Reply with the single word OK."),
        ],
        max_output_tokens: Some(16),
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    }
}

const TINY_SYSTEM_PROMPT: &str = "You are a terse smoke-test assistant.";

/// A system prompt long enough to clear Anthropic's minimum cacheable-block
/// size (documented as ~1024 tokens for Sonnet/Opus-tier models, ~2048 for
/// Haiku-tier) — deterministic, cheap-to-generate filler repeated well past
/// either floor (~150 repeats of an ~80-byte sentence is ~3,000 tokens), so
/// the smoke call can tell "cache_control accepted" (any success) apart
/// from "cache_control accepted AND actually engaged" (a real
/// `cache_creation_input_tokens`/`cache_read_input_tokens` count > 0). Only
/// `anthropic_smoke` needs this; every other provider's system prompt stays
/// genuinely tiny.
fn anthropic_cache_probe_system_prompt() -> String {
    "You are a terse smoke-test assistant. Only ever reply with the single word OK. ".repeat(150)
}

// ---- gate-behavior witnesses (always run; never touch the network) ------

#[test]
fn live_smoke_is_disabled_by_default() {
    let _guard = env_lock();
    // SAFETY: guarded by `env_lock`; this test owns the mutate-assert-
    // restore window.
    unsafe {
        std::env::remove_var("STELLA_LIVE_SMOKE");
    }
    assert!(
        !live_smoke_enabled(),
        "the live smoke suite must be OFF unless STELLA_LIVE_SMOKE=1 is explicitly set"
    );
}

#[test]
fn live_smoke_requires_the_exact_value_one() {
    let _guard = env_lock();
    // SAFETY: guarded by `env_lock`.
    unsafe {
        std::env::set_var("STELLA_LIVE_SMOKE", "true");
    }
    assert!(
        !live_smoke_enabled(),
        "STELLA_LIVE_SMOKE must require the literal value `1`, not any truthy-looking string \
         (this suite spends real money — the gate should never arm by accident)"
    );
    unsafe {
        std::env::remove_var("STELLA_LIVE_SMOKE");
    }
}

#[test]
fn armed_key_skips_cleanly_when_the_gate_is_off_even_with_a_key_present() {
    let _guard = env_lock();
    // SAFETY: guarded by `env_lock`; unique var name this test owns.
    unsafe {
        std::env::remove_var("STELLA_LIVE_SMOKE");
        std::env::set_var("STELLA_LIVE_SMOKE_WITNESS_KEY", "sk-present-but-unarmed");
    }
    assert!(
        armed_key_locked("witness-provider", "STELLA_LIVE_SMOKE_WITNESS_KEY", &[]).is_none(),
        "a resolvable key must not be enough on its own — the STELLA_LIVE_SMOKE gate must \
         also be set"
    );
    unsafe {
        std::env::remove_var("STELLA_LIVE_SMOKE_WITNESS_KEY");
    }
}

#[test]
fn armed_key_skips_cleanly_when_the_gate_is_on_but_no_key_resolves() {
    let _guard = env_lock();
    // SAFETY: guarded by `env_lock`; unique var name, deliberately left
    // unset.
    unsafe {
        std::env::set_var("STELLA_LIVE_SMOKE", "1");
        std::env::remove_var("STELLA_LIVE_SMOKE_WITNESS_UNSET_KEY");
    }
    assert!(
        armed_key_locked(
            "witness-provider",
            "STELLA_LIVE_SMOKE_WITNESS_UNSET_KEY",
            &[]
        )
        .is_none(),
        "the gate alone must not be enough — a resolvable credential is also required"
    );
    unsafe {
        std::env::remove_var("STELLA_LIVE_SMOKE");
    }
}

/// Every live call carries `#[ignore]`, and this is what keeps it that way.
///
/// libtest reports an ignored test as `ignored, <reason>` and counts it in
/// the summary line as `N ignored`; a test that returns early reports `ok`.
/// Without the attribute a credential-less run said `9 passed` in 0.01s for
/// a run that contacted nothing — indistinguishable, in the one line
/// anybody reads, from nine live endpoints accepting the shape stella sends
/// (#3856). Reads this file's own source because the fact being pinned is
/// an attribute, which no runtime value carries.
#[test]
fn every_live_call_is_ignored_by_default() {
    let source = include_str!("live_smoke.rs");
    let lines: Vec<&str> = source.lines().collect();
    let mut found = 0;
    for (index, line) in lines.iter().enumerate() {
        // Only a declaration at column zero: this guard's own mention of
        // the pattern is indented, so it cannot match itself.
        if !(line.starts_with("async fn ") && line.contains("_smoke(")) {
            continue;
        }
        found += 1;
        let attributes = &lines[index.saturating_sub(3)..index];
        assert!(
            attributes
                .iter()
                .any(|a| a.trim_start().starts_with("#[ignore")),
            "`{line}` has no #[ignore]: a credential-less run would report it as a pass"
        );
    }
    assert_eq!(
        found,
        LIVE_PROVIDERS.len(),
        "every LIVE_PROVIDERS row has one `*_smoke` test and no more"
    );
}

/// The other half: a smoke that was named and cannot run is a failure.
///
/// `#[ignore]` means nothing reaches these tests unless a human or the
/// workflow asked for one by name, so "no credential resolved" is a failure
/// to run what was asked for rather than the hermetic default it used to
/// be. Asserted against [`required_key`] directly, so this witness needs no
/// env var, no network, and no `env_lock` — the panic is the contract.
#[test]
fn a_named_smoke_with_no_credential_fails_rather_than_skipping() {
    let row = row("anthropic");
    let outcome = std::panic::catch_unwind(|| required_key(row, None));
    assert!(
        outcome.is_err(),
        "an unresolvable credential must fail a named smoke, never skip it"
    );
}

// ---- the provider matrix -------------------------------------------------

/// One row of the live matrix — the parts needed to construct a provider and
/// name the credential that unlocks it.
///
/// This mirrors `stella_cli::config::PROVIDERS`. It has to be a mirror rather
/// than a reference: `stella-cli` depends on `stella-model`, so this crate
/// cannot import it without a dependency cycle. The same constraint (and the
/// same mirroring convention) is documented on
/// `stella_model::catalog`'s `seed_covers_every_provider_stella_cli_can_select`.
///
/// The drift guards below are what make the mirror safe:
/// [`every_live_smoke_slug_resolves_against_the_catalog_seed`] fails the
/// moment a slug here stops being real, and
/// [`every_row_constructs_through_the_real_factory`] fails the moment a row
/// stops being constructible — both unconditionally, with no network call.
struct LiveProvider {
    /// Stable provider id — must match the `stella_cli::config::PROVIDERS` row.
    id: &'static str,
    env_var: &'static str,
    aliases: &'static [&'static str],
    dialect: Dialect,
    /// The slug to smoke. Matches the production row's `default_model`, so a
    /// live run exercises what an auto-detected `stella` run would actually
    /// send.
    model: &'static str,
    base_url: &'static str,
    seeded: bool,
}

impl LiveProvider {
    fn spec(&self) -> ProviderSpec<'_> {
        ProviderSpec {
            upstream_pin: &[],
            id: self.id,
            display_name: self.id,
            dialect: self.dialect,
            seeded: self.seeded,
            cache_ttl: stella_model::CacheTtl::default(),
        }
    }
}

/// Every adapter with a live smoke test, in `stella_cli::config::PROVIDERS`
/// order. Adding an adapter here is all it takes to smoke it — the drift
/// guards and the per-provider tests below both read this table.
const LIVE_PROVIDERS: &[LiveProvider] = &[
    LiveProvider {
        id: "zai",
        env_var: "ZAI_API_KEY",
        aliases: &[],
        dialect: Dialect::OpenaiCompatible,
        model: "glm-5.2",
        base_url: "https://api.z.ai/api/paas/v4",
        seeded: true,
    },
    LiveProvider {
        id: "anthropic",
        env_var: "ANTHROPIC_API_KEY",
        aliases: &[],
        dialect: Dialect::Anthropic,
        model: "claude-fable-5",
        base_url: "https://api.anthropic.com",
        seeded: true,
    },
    LiveProvider {
        id: "openai",
        env_var: "OPENAI_API_KEY",
        aliases: &[],
        dialect: Dialect::OpenaiResponses,
        model: "gpt-5.5",
        base_url: "https://api.openai.com/v1",
        seeded: true,
    },
    LiveProvider {
        id: "xai",
        env_var: "XAI_API_KEY",
        aliases: &[],
        dialect: Dialect::OpenaiCompatible,
        model: "grok-4",
        base_url: "https://api.x.ai/v1",
        seeded: true,
    },
    LiveProvider {
        id: "deepseek",
        env_var: "DEEPSEEK_API_KEY",
        aliases: &[],
        dialect: Dialect::OpenaiCompatible,
        model: "deepseek-chat",
        base_url: "https://api.deepseek.com/v1",
        seeded: true,
    },
    LiveProvider {
        id: "gemini",
        env_var: "GEMINI_API_KEY",
        aliases: &["GOOGLE_API_KEY"],
        dialect: Dialect::Gemini,
        model: "gemini-3-pro",
        base_url: "https://generativelanguage.googleapis.com/v1beta",
        seeded: true,
    },
    LiveProvider {
        id: "openrouter",
        env_var: "OPENROUTER_API_KEY",
        aliases: &[],
        dialect: Dialect::OpenaiCompatible,
        model: "openrouter/auto",
        base_url: "https://openrouter.ai/api/v1",
        // Deliberately unseeded, matching production: OpenRouter's catalog is
        // vendor-namespaced and routed, so there is no curated seed row (and
        // no list price) for its slugs.
        seeded: false,
    },
    LiveProvider {
        id: "vertex",
        env_var: "VERTEX_ACCESS_TOKEN",
        aliases: &[],
        dialect: Dialect::Vertex,
        model: "gemini-3-pro",
        base_url: "https://aiplatform.googleapis.com",
        seeded: true,
    },
    LiveProvider {
        id: "bedrock",
        env_var: "AWS_ACCESS_KEY_ID",
        aliases: &[],
        dialect: Dialect::Bedrock,
        model: "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        base_url: "https://bedrock-runtime.<AWS_REGION>.amazonaws.com",
        seeded: true,
    },
];

fn row(id: &str) -> &'static LiveProvider {
    LIVE_PROVIDERS
        .iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("no LIVE_PROVIDERS row for `{id}`"))
}

// ---- drift guards (always run; never touch the network) ------------------

/// **The anti-phantom-slug gate for this suite.**
///
/// Unconditional on purpose. Every live test below is gated behind
/// `STELLA_LIVE_SMOKE=1` *and* a resolvable credential, which is right for
/// something that spends money — but it meant a slug that quietly stopped
/// existing produced a green run on every default `cargo test --workspace`,
/// and surfaced at most weekly, as an ambiguous "wire-shape regression OR
/// account/auth/quota/billing problem" panic from the scheduled workflow.
///
/// That is the wrong failure mode for the one thing a *catalog* can check
/// offline for free. This test runs on every `cargo test`, makes no network
/// call, needs no credential, and fails by name.
///
/// The other half of that ambiguity — a live failure that named no cause at
/// all — is now answered where it happens, by [`failure_report`] (#3259).
///
/// It mirrors `stella_cli::config::tests::
/// every_provider_default_model_resolves_against_the_catalog_seed`, which
/// asserts the same property for the production table.
#[test]
fn every_live_smoke_slug_resolves_against_the_catalog_seed() {
    let catalog = Catalog::seed();
    for provider in LIVE_PROVIDERS {
        if !provider.seeded {
            // Unseeded providers (OpenRouter) have no curated rows by design —
            // asserting a seed hit here would be asserting a falsehood.
            continue;
        }
        catalog
            .resolve_for(provider.id, provider.model)
            .unwrap_or_else(|e| {
                panic!(
                    "live smoke provider `{}` smokes slug `{}`, which is not in the catalog \
                     seed: {e}. A live run would spend a real call to discover this; fix the \
                     slug here (and in stella_cli::config::PROVIDERS, which this table \
                     mirrors) or add the catalog row.",
                    provider.id, provider.model
                )
            });
    }
}

/// Every row constructs through the **real** factory
/// ([`stella_model::factory::build_provider`]) — the same function
/// `stella_cli::agent::engine::build_provider_parts` delegates to.
///
/// This is the structural half of the fix: before, each test below built its
/// adapter by hand, so nine construction sites could drift from production's
/// one (and did — the OpenRouter attribution literals were copied verbatim).
/// Now the tests call the factory, and this guard proves every row reaches a
/// constructible adapter with the right identity, offline and for free.
#[test]
fn every_row_constructs_through_the_real_factory() {
    for provider in LIVE_PROVIDERS {
        // Vertex and Bedrock resolve extra addressing/credentials from the
        // environment inside the factory; skip those two here rather than
        // depend on an AWS/GCP environment being present. Their construction
        // is still exercised live by the tests below when armed.
        if matches!(provider.dialect, Dialect::Vertex | Dialect::Bedrock) {
            continue;
        }
        let built = build_provider(
            &provider.spec(),
            provider.model,
            ApiKey::new("smoke-test-not-a-real-key".to_string()),
            provider.base_url.to_string(),
            None,
            // Empty: these rows authenticate with one key, and the two that do
            // not are skipped above. The Bedrock arm falls back to the standard
            // AWS environment variables for an empty set, which is what the
            // armed test below relies on.
            &AuxCredentials::default(),
        )
        .unwrap_or_else(|e| panic!("`{}` must construct through the factory: {e}", provider.id));
        assert_eq!(
            &built.id(),
            &provider.id,
            "`{}`'s adapter must report its own identity — that id is what error messages, \
             telemetry, and cost attribution key on",
            provider.id
        );
    }
}

/// The table stays a faithful mirror: no duplicate ids, and every row names a
/// credential env var. Cheap, but it is the kind of thing a copy-paste row
/// gets wrong silently.
#[test]
fn the_provider_table_is_well_formed() {
    let mut seen = std::collections::BTreeSet::new();
    for provider in LIVE_PROVIDERS {
        assert!(
            seen.insert(provider.id),
            "duplicate LIVE_PROVIDERS row for `{}`",
            provider.id
        );
        assert!(
            !provider.env_var.is_empty(),
            "`{}` must name the env var that unlocks it",
            provider.id
        );
        assert!(
            !provider.model.is_empty(),
            "`{}` must name a slug to smoke",
            provider.id
        );
    }
}

// ---- live smoke: one minimal real call per adapter -----------------------

/// Why a named smoke could not run, phrased as the failure it is.
///
/// Every caller of this is inside an `#[ignore]`d test, which libtest runs
/// only when someone names it — so there is no hermetic default to fall back
/// to and nothing to protect a default `cargo test` from. Reporting `ok` for
/// a call that never left the machine is the failure mode #3856 was filed
/// on; this reports the opposite.
fn unrunnable(provider_id: &str, detail: &str) -> String {
    format!(
        "[live_smoke] {provider_id}: asked for but unrunnable — {detail}. This test runs \
         only when named (`-- --ignored`), so a run that contacts nothing is a failure to \
         run what was asked for, not a skip."
    )
}

/// The credential a named smoke runs with. Panics when nothing resolved.
///
/// Takes the `Option` rather than resolving it, so the "unrunnable is a
/// failure" contract is witnessable without an env var, a network call, or
/// the environment lock — see
/// [`a_named_smoke_with_no_credential_fails_rather_than_skipping`].
fn required_key(provider: &LiveProvider, key: Option<ApiKey>) -> ApiKey {
    key.unwrap_or_else(|| {
        let alias_note = if provider.aliases.is_empty() {
            String::new()
        } else {
            format!(" (or {})", provider.aliases.join("/"))
        };
        panic!(
            "{}",
            unrunnable(
                provider.id,
                &format!(
                    "STELLA_LIVE_SMOKE is not `1`, or no {}{alias_note} env var and no `{}` \
                     entry in ~/.stella/credentials.toml",
                    provider.env_var, provider.id
                )
            )
        )
    })
}

/// Construct `provider`'s adapter through the production factory.
///
/// Panics when the suite is unarmed, the credential does not resolve, or
/// (Vertex/Bedrock) the extra addressing the factory needs is absent — all
/// three are "asked for and could not run" (see [`unrunnable`]). A factory
/// error for any *other* reason is a failure too: that is the
/// anti-phantom-slug gate and the dialect dispatch talking, and a live run
/// must not paper over them.
fn armed_provider(provider: &LiveProvider) -> Box<dyn Provider> {
    let key = required_key(
        provider,
        armed_key(provider.id, provider.env_var, provider.aliases),
    );

    // Vertex/Bedrock need more than the one credential `armed_key` resolves.
    // The factory resolves that itself (via `stella_model::credential`), so
    // pre-flighting here names the missing half while leaving the real
    // resolution to production code.
    match provider.dialect {
        Dialect::Vertex => {
            if let Err(e) = stella_model::credential::VertexAddressing::resolve() {
                panic!(
                    "{}",
                    unrunnable(
                        provider.id,
                        &format!(
                            "{} is set but Vertex addressing does not resolve: {e}",
                            provider.env_var
                        )
                    )
                );
            }
        }
        Dialect::Bedrock => {
            if let Err(e) = stella_model::credential::BedrockCredentials::resolve() {
                panic!(
                    "{}",
                    unrunnable(
                        provider.id,
                        &format!(
                            "{} is set but the AWS credential chain does not resolve: {e}",
                            provider.env_var
                        )
                    )
                );
            }
        }
        _ => {}
    }

    match build_provider(
        &provider.spec(),
        provider.model,
        key,
        provider.base_url.to_string(),
        None,
        // An armed live run supplies Bedrock's companion values the way the
        // adapter's own fallback expects them: as AWS environment variables.
        &AuxCredentials::default(),
    ) {
        Ok(built) => built,
        Err(e) => panic!(
            "`{}` failed to construct through the factory — this is a config/catalog fault, \
             not a network one, and would fail an ordinary `stella --model {}/{}` run the \
             same way: {e}",
            provider.id, provider.id, provider.model
        ),
    }
}

// ---- failure diagnosis: what a red smoke actually establishes ------------

/// Longest prefix (in characters) of the provider's own error text a failed
/// smoke prints, so a CDN error page or a multi-kilobyte gateway blob can
/// never flood a CI log.
///
/// Characters rather than bytes, for the reason `http::body_snippet` gives
/// for the same bound one layer down: a provider body is
/// environment-controlled text and a byte slice can land mid-character. 512
/// characters is at most ~2 KiB of UTF-8, and the diagnostic half of a
/// provider error — its status line and the opening sentence of its reason
/// — is always at the front, so the cut never costs the verdict. This is the
/// second, tighter bound: the adapter has already clipped the raw body to
/// `http::ERROR_BODY_SNIPPET_CHARS` before it ever reaches here.
const FAILURE_DETAIL_CHARS: usize = 512;

/// What a failed live smoke call **establishes** about its own cause.
///
/// This exists because the old failure message stated the question instead of
/// answering it — "could be a genuine wire-shape regression OR an unrelated
/// account/auth/quota/billing problem" — which left every red run needing a
/// human with a credential to re-run it before anyone knew whether the
/// adapter was broken (issue #3259). The status code already resolves three
/// of the four cases; only the fourth is genuinely undetermined, and saying
/// so is different from saying it about all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureCause {
    /// HTTP 401/403 — the provider refused the key itself.
    Credential,
    /// HTTP 429, or a refusal to fund the requested output ceiling.
    Quota,
    /// A response arrived and stella's own parser could not reassemble it.
    WireShape,
    /// None of the above; the status code does not settle it by itself.
    Undetermined,
}

impl FailureCause {
    /// The verdict line, written so a reader of a CI log needs nothing else
    /// to know whether the adapter is suspect.
    fn headline(self) -> &'static str {
        match self {
            FailureCause::Credential => {
                "CREDENTIAL — the provider refused the key itself (HTTP 401/403). The secret is \
                 revoked, mistyped, expired, or not entitled to this model. This is NOT a \
                 wire-shape regression: the request never reached shape validation. Rotate the \
                 credential and re-run."
            }
            FailureCause::Quota => {
                "QUOTA — the key was accepted and the call refused for rate or spend reasons \
                 (HTTP 429, or a refusal to fund the requested output ceiling). This is NOT a \
                 wire-shape regression: the request shape was never judged. Wait out the limit \
                 or raise the cap/credit, then re-run."
            }
            FailureCause::WireShape => {
                "WIRE SHAPE — the provider answered and stella's own parser could not \
                 reassemble a CompletionResult from it. The credential worked. This is the \
                 regression this suite exists to catch: fix the adapter under \
                 crates/stella-model/src/ and land a witness test with it."
            }
            FailureCause::Undetermined => {
                "UNDETERMINED — this failure is none of the three a status code resolves on its \
                 own, so this run does not name a cause. Read the status and detail below: a \
                 4xx naming a rejected field is a wire-shape regression; a 402 or an \
                 out-of-credits body is billing; a 5xx or a transport fault is the provider \
                 being unwell and settles nothing either way."
            }
        }
    }
}

/// Both halves of what the typed error tells us: the variant's name (for the
/// log) and the cause it establishes (for the verdict).
///
/// The test holds a [`ProviderError`], never the raw `reqwest::Response` — the
/// adapter has already read the status and the body and folded both into this
/// error (`http::classify_http_status`). So the verdict is derived from the
/// **variant**, which *is* the adapter's own classification of the status
/// code, and never from scraping its prose.
///
/// One exhaustive match on purpose: a new `ProviderError` variant must fail
/// this file to compile rather than landing silently in the `Undetermined`
/// bucket, which is the same "declared, not silent" discipline invariant #10
/// applies to events.
fn classify(error: &ProviderError) -> (&'static str, FailureCause) {
    match error {
        // 401 and 403 both land here (`classify_http_status`'s first two arms).
        ProviderError::Auth(_) => ("ProviderError::Auth", FailureCause::Credential),
        ProviderError::RateLimited { .. } => ("ProviderError::RateLimited", FailureCause::Quota),
        // Not a 429, but the same kind of answer: the account cannot afford
        // the ceiling asked for. Nothing about the request *shape* was judged.
        ProviderError::OutputBudgetExceeded { .. } => {
            ("ProviderError::OutputBudgetExceeded", FailureCause::Quota)
        }
        // "A response arrived but could not be decoded into a
        // CompletionResult" — the variant's own contract, and exactly the
        // regression signal this suite was built for.
        ProviderError::Malformed(_) => ("ProviderError::Malformed", FailureCause::WireShape),
        // A 5xx or a network fault: the provider is unwell, which is evidence
        // for neither answer.
        ProviderError::Transport { .. } => ("ProviderError::Transport", FailureCause::Undetermined),
        // A 529 brownout: the provider is unwell in the specific way that
        // waiting fixes. Still evidence for neither answer about the wire
        // shape this suite exists to watch, so it shares Transport's verdict
        // — the class is split for the *retry* decision (#2742), not this one.
        ProviderError::Overloaded { .. } => {
            ("ProviderError::Overloaded", FailureCause::Undetermined)
        }
        // A 4xx the dialect does not model (400/402/404/422 …). A rejected
        // field and an out-of-credits 402 both arrive as this, so the class
        // alone cannot separate them — the printed status and body can.
        ProviderError::Terminal(_) => ("ProviderError::Terminal", FailureCause::Undetermined),
        ProviderError::ContextOverflow { .. } => {
            ("ProviderError::ContextOverflow", FailureCause::Undetermined)
        }
        ProviderError::UnknownModel { .. } => {
            ("ProviderError::UnknownModel", FailureCause::Undetermined)
        }
        ProviderError::Cancelled => ("ProviderError::Cancelled", FailureCause::Undetermined),
    }
}

/// The numeric HTTP status the adapter stamped into its message, when it
/// stamped one.
///
/// **Display only** — no verdict depends on it. `classify_http_status` writes
/// the status as `HTTP 401` / `HTTP 400 Bad Request` into every arm that saw
/// a wire response, so recovering it puts the number on its own line instead
/// of leaving a reader to find it inside a paragraph. A miss returns `None`
/// rather than a guess: an error raised before any response (DNS, connect,
/// TLS, cancellation) genuinely has no status, and printing one would be the
/// invented evidence this whole change exists to stop.
fn recovered_status(text: &str) -> Option<u16> {
    text.match_indices("HTTP ").find_map(|(at, marker)| {
        let digits: String = text[at + marker.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if digits.len() != 3 {
            return None;
        }
        digits
            .parse::<u16>()
            .ok()
            .filter(|status| (100..=599).contains(status))
    })
}

/// `text` bounded to [`FAILURE_DETAIL_CHARS`], with the elision named
/// explicitly so nobody mistakes the cut for the provider's whole answer.
/// Mirrors `http::body_snippet`, one layer up.
fn bounded_detail(text: &str) -> String {
    let total = text.chars().count();
    if total <= FAILURE_DETAIL_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(FAILURE_DETAIL_CHARS).collect();
    let elided = total - FAILURE_DETAIL_CHARS;
    format!(
        "{head}… (+{elided} more chars, elided by live-smoke's {FAILURE_DETAIL_CHARS}-char cap)"
    )
}

/// The panic message a failed smoke reports: a verdict, the status that
/// justifies it, the error class it came from, and a bounded prefix of the
/// provider's own text.
///
/// Shared by [`smoke`] and [`anthropic_smoke`] — the two sites that carried
/// the old ambiguous wording, kept as one function so a third provider cannot
/// grow a third dialect of it. Safe to print: `ProviderError`'s own contract
/// forbids an adapter putting a credential, an `Authorization` header, or a
/// keyed URL into these strings.
fn failure_report(provider_id: &str, error: &ProviderError) -> String {
    let text = error.to_string();
    let (class, cause) = classify(error);
    let status = recovered_status(&text).map_or_else(
        || "none — this error was raised before any HTTP response arrived".to_string(),
        |status| format!("HTTP {status}"),
    );
    format!(
        "{provider_id} live smoke did not return a parseable 200.\n  \
         verdict: {}\n  \
         status: {status}\n  \
         error class: {class}\n  \
         detail (the provider's own status and body, bounded to \
         {FAILURE_DETAIL_CHARS} chars): {}",
        cause.headline(),
        bounded_detail(&text),
    )
}

// ---- failure-diagnosis witnesses (always run; never touch the network) ---

/// The failure report of a red run is the only artifact anyone reads, and it
/// is produced on a path (an armed live call against a real endpoint) that
/// cannot be exercised in CI. These witnesses cover it the way the drift
/// guards above cover the table: offline, unconditional, no credential.
///
/// Every error below is built with the literal text its adapter arm writes
/// (`http::classify_http_status`), so the recovery of the status is checked
/// against the real wording rather than a convenient invention.
#[test]
fn an_auth_refusal_is_reported_as_a_credential_problem() {
    let error = ProviderError::Auth(
        "anthropic rejected the credential (HTTP 401): invalid x-api-key — the key looks \
         revoked, mistyped, or was never loaded"
            .to_string(),
    );
    assert_eq!(classify(&error).1, FailureCause::Credential);

    let report = failure_report("anthropic", &error);
    assert!(report.contains("CREDENTIAL"), "{report}");
    assert!(report.contains("status: HTTP 401"), "{report}");
    assert!(
        report.contains("NOT a wire-shape regression"),
        "a 401 settles the question the old message asked — the report must say so: {report}"
    );
}

/// A 403 is the other half of the credential answer, and reaches the same
/// variant by a different arm of the classifier.
#[test]
fn a_forbidden_refusal_is_also_reported_as_a_credential_problem() {
    let error = ProviderError::Auth(
        "anthropic rejected the credential (HTTP 403): permission denied — the key is valid \
         but lacks permission for this request"
            .to_string(),
    );
    assert_eq!(classify(&error).1, FailureCause::Credential);
    assert!(
        failure_report("anthropic", &error).contains("status: HTTP 403"),
        "the 403 arm's status must survive into the report"
    );
}

#[test]
fn a_rate_limit_is_reported_as_quota() {
    let error = ProviderError::RateLimited {
        message: "anthropic rate limit".to_string(),
        retry_after_ms: Some(2_000),
    };
    assert_eq!(classify(&error).1, FailureCause::Quota);

    let report = failure_report("anthropic", &error);
    assert!(report.contains("QUOTA"), "{report}");
    assert!(
        report.contains("NOT a wire-shape regression"),
        "a 429 settles the question too: {report}"
    );
}

/// The affordability refusal is not a 429, but it is the same answer: the
/// account, not the request shape, is what the provider refused.
#[test]
fn an_output_budget_refusal_is_reported_as_quota() {
    let error = ProviderError::OutputBudgetExceeded {
        message: "openrouter cannot afford the requested output ceiling (HTTP 402)".to_string(),
        affordable_output_tokens: Some(117_676),
    };
    assert_eq!(classify(&error).1, FailureCause::Quota);
}

#[test]
fn an_unparseable_response_is_reported_as_a_wire_shape_regression() {
    let error = ProviderError::Malformed("missing field `content` at line 1 column 84".to_string());
    assert_eq!(classify(&error).1, FailureCause::WireShape);

    let report = failure_report("anthropic", &error);
    assert!(report.contains("WIRE SHAPE"), "{report}");
    assert!(
        report.contains("crates/stella-model/src/"),
        "the one verdict that indicts stella must say where the fix goes: {report}"
    );
    assert!(
        report.contains("status: none"),
        "a decode failure carries no status line, and the report must say `none` rather than \
         invent one: {report}"
    );
}

/// The point of the change: where the status resolves the question, the
/// report answers it and does not restate the old either/or.
#[test]
fn a_resolved_status_never_restates_the_old_ambiguity() {
    for error in [
        ProviderError::Auth("anthropic rejected the credential (HTTP 401)".to_string()),
        ProviderError::RateLimited {
            message: "anthropic rate limit".to_string(),
            retry_after_ms: None,
        },
        ProviderError::Malformed("trailing characters at line 3".to_string()),
    ] {
        let report = failure_report("anthropic", &error);
        assert!(
            !report.contains("could be"),
            "the report must name a cause, not offer a menu of them: {report}"
        );
    }
}

/// The fourth case is genuinely unresolved by the status alone, and saying so
/// plainly is the honest answer — not the same sentence pasted over the three
/// cases that ARE resolved.
#[test]
fn an_unclassified_failure_says_undetermined_rather_than_guessing() {
    for error in [
        ProviderError::Terminal("anthropic HTTP 400 Bad Request: {\"error\":{}}".to_string()),
        ProviderError::transport("anthropic HTTP 529 Overloaded: overloaded_error".to_string()),
    ] {
        assert_eq!(classify(&error).1, FailureCause::Undetermined);
        let report = failure_report("anthropic", &error);
        assert!(report.contains("UNDETERMINED"), "{report}");
        assert!(
            report.contains("does not name a cause"),
            "an undetermined run must say it is undetermined: {report}"
        );
    }
}

/// A 400's status reaches the report even though its class does not settle
/// the verdict — that number is the whole reason the fourth case is still
/// worth printing.
#[test]
fn an_undetermined_failure_still_prints_the_status_it_had() {
    let report = failure_report(
        "anthropic",
        &ProviderError::Terminal(
            "anthropic HTTP 400 Bad Request: bad `thinking` field".to_string(),
        ),
    );
    assert!(report.contains("status: HTTP 400"), "{report}");
}

/// A provider that answers a failure with a multi-kilobyte CDN page must not
/// flood the CI log — the cap is explicit and its elision is named.
#[test]
fn the_failure_report_bounds_a_huge_body() {
    let huge = format!(
        "anthropic HTTP 500 Internal Server Error: {}",
        "x".repeat(20_000)
    );
    let report = failure_report("anthropic", &ProviderError::transport(huge));
    assert!(
        report.chars().count() < 2_000,
        "a 20,000-char body must not reach the log in full; report was {} chars",
        report.chars().count()
    );
    assert!(
        report.contains("more chars, elided by live-smoke's 512-char cap"),
        "the cut must name itself so nobody reads it as the provider's whole answer: {report}"
    );
}

/// Multi-byte text must survive the bound intact — the reason the cap counts
/// characters and not bytes.
#[test]
fn the_bound_never_splits_a_multi_byte_character() {
    let bounded = bounded_detail(&"é".repeat(5_000));
    assert!(bounded.starts_with('é'));
    assert!(bounded.contains("+4488 more chars"), "{bounded}");
}

/// A short error is passed through untouched: the cap is a ceiling, not a
/// reformatter.
#[test]
fn a_short_error_is_not_truncated() {
    let short = "anthropic HTTP 400 Bad Request: no";
    assert_eq!(bounded_detail(short), short);
}

/// Status recovery is a display convenience, so it must decline rather than
/// guess: no number at all, and a number that is not an HTTP status, both
/// yield `None`.
#[test]
fn status_recovery_declines_rather_than_guesses() {
    assert_eq!(
        recovered_status("provider transport error: dns failure"),
        None
    );
    assert_eq!(recovered_status("HTTP two hundred"), None);
    assert_eq!(recovered_status("HTTP 99"), None);
    assert_eq!(recovered_status("HTTP 9999"), None);
    assert_eq!(recovered_status("anthropic HTTP 429: slow down"), Some(429));
}

/// Send `request` and report. Shared by every provider so the success log and
/// the failure wording stay identical across adapters.
async fn smoke(provider: &LiveProvider, built: Box<dyn Provider>, request: CompletionRequest) {
    match built.complete(request).await {
        Ok(r) => eprintln!(
            "[live_smoke] {}: OK — model={} finish={:?} usage={:?} cost_usd={} text={:?}",
            provider.id, r.model, r.finish_reason, r.usage, r.cost_usd, r.text
        ),
        // What this can no longer be is an unknown slug: the factory's seed
        // floor rejects those before any wire call, and
        // `every_live_smoke_slug_resolves_against_the_catalog_seed` catches
        // them offline before that.
        Err(e) => panic!("{}", failure_report(provider.id, &e)),
    }
}

/// Anthropic Messages API. Also the `cache_control` settlement: every
/// Anthropic request `stella` builds carries `cache_control` on the system
/// block and the final message's tail block (never top-level — see
/// `anthropic::stamp_tail_cache_breakpoint`'s doc). This test's long system
/// prompt ([`anthropic_cache_probe_system_prompt`]) exists specifically so
/// that placement gets genuinely exercised, not just accepted-but-inert.
///
/// **Verdict**: a successful `Ok(_)` here proves the live Messages API
/// accepts today's block-level-only `cache_control` shape; the printed
/// `cache_write_tokens`/`cached_input_tokens` additionally show whether
/// caching actually engaged for this call (0 on a cold cache is normal on
/// the FIRST call — a second run within the ~5-minute TTL should show a
/// `cached_input_tokens` read instead of a `cache_write_tokens` write).
///
/// A billing/quota 400 (`"credit balance is too low"`) is NOT a wire-shape
/// verdict: Anthropic names the offending field in `invalid_request_error`
/// when a request is actually malformed, so a balance rejection never
/// exercises the request shape at all.
#[tokio::test]
#[ignore = "live provider call — run with `-- --ignored`, STELLA_LIVE_SMOKE=1 and a credential"]
async fn anthropic_smoke() {
    let provider = row("anthropic");
    let built = armed_provider(provider);
    // The one provider whose system prompt is not tiny — see the probe's doc.
    let request = tiny_request(anthropic_cache_probe_system_prompt());
    match built.complete(request).await {
        Ok(r) => eprintln!(
            "[live_smoke] anthropic: OK — model={} finish={:?} usage={:?} \
             cache_write_tokens={} cached_input_tokens={} text={:?}",
            r.model,
            r.finish_reason,
            r.usage,
            r.usage.cache_write_tokens,
            r.usage.cached_input_tokens,
            r.text
        ),
        // Same report as every other provider's — see [`failure_report`].
        // This test hand-rolls only its SUCCESS arm (to print the two cache
        // counters); the failure arm must not drift from the shared one.
        Err(e) => panic!("{}", failure_report(provider.id, &e)),
    }
}

/// OpenAI Responses API.
#[tokio::test]
#[ignore = "live provider call — run with `-- --ignored`, STELLA_LIVE_SMOKE=1 and a credential"]
async fn openai_smoke() {
    let provider = row("openai");
    let built = armed_provider(provider);
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// Gemini direct `generateContent`. `GEMINI_API_KEY` (spec-documented alias
/// `GOOGLE_API_KEY`, same as `stella-cli`'s `config::PROVIDERS` row).
#[tokio::test]
#[ignore = "live provider call — run with `-- --ignored`, STELLA_LIVE_SMOKE=1 and a credential"]
async fn gemini_smoke() {
    let provider = row("gemini");
    let built = armed_provider(provider);
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// Vertex AI `generateContent`. Needs the OAuth2 bearer
/// (`VERTEX_ACCESS_TOKEN`, e.g. `$(gcloud auth print-access-token)`) AND a
/// project id (`VERTEX_PROJECT_ID` or `GOOGLE_CLOUD_PROJECT`).
///
/// Both the addressing resolution and the adapter construction are now the
/// factory's (`stella_model::credential::VertexAddressing` +
/// `Dialect::Vertex`), so this test cannot drift from what a real
/// `stella --model vertex/…` run does — it calls the same code.
#[tokio::test]
#[ignore = "live provider call — run with `-- --ignored`, STELLA_LIVE_SMOKE=1 and a credential"]
async fn vertex_smoke() {
    let provider = row("vertex");
    let built = armed_provider(provider);
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// Amazon Bedrock Converse. Needs the standard AWS chain
/// (`AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY`, optional
/// `AWS_SESSION_TOKEN`/`AWS_REGION`), resolved inside the factory by
/// `stella_model::credential::BedrockCredentials` — the same resolution a
/// real run uses.
#[tokio::test]
#[ignore = "live provider call — run with `-- --ignored`, STELLA_LIVE_SMOKE=1 and a credential"]
async fn bedrock_smoke() {
    let provider = row("bedrock");
    let built = armed_provider(provider);
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// OpenRouter, via the shared OpenAI-compatible adapter. The app attribution
/// and `usage: {"include": true}` accounting are applied by the factory's
/// `Dialect::OpenaiCompatible` arm, not by this test — which is the point:
/// the attribution literals used to be copy-pasted here and could drift from
/// production silently. `openrouter/auto` is the real vendor-namespaced slug
/// for OpenRouter's own auto-router model, and genuinely how `stella-cli`'s
/// auto-detected default reaches it.
#[tokio::test]
#[ignore = "live provider call — run with `-- --ignored`, STELLA_LIVE_SMOKE=1 and a credential"]
async fn openrouter_smoke() {
    let provider = row("openrouter");
    let built = armed_provider(provider);
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// Z.ai (GLM), via the OpenAI-compatible adapter under its own identity.
#[tokio::test]
#[ignore = "live provider call — run with `-- --ignored`, STELLA_LIVE_SMOKE=1 and a credential"]
async fn zai_smoke() {
    let provider = row("zai");
    let built = armed_provider(provider);
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// DeepSeek, via the OpenAI-compatible adapter under its own identity.
#[tokio::test]
#[ignore = "live provider call — run with `-- --ignored`, STELLA_LIVE_SMOKE=1 and a credential"]
async fn deepseek_smoke() {
    let provider = row("deepseek");
    let built = armed_provider(provider);
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// xAI (Grok), via the OpenAI-compatible adapter under its own identity.
#[tokio::test]
#[ignore = "live provider call — run with `-- --ignored`, STELLA_LIVE_SMOKE=1 and a credential"]
async fn xai_smoke() {
    let provider = row("xai");
    let built = armed_provider(provider);
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}
