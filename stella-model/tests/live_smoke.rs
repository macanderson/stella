//! Opt-in live smoke suite (issue #274) — one minimal REAL call per provider
//! adapter, against the provider's actual endpoint. Every other test in this
//! crate runs against `wiremock`, which proves "we parse the shape we
//! expect" but can never prove "the live API accepts the shape we send" —
//! exactly the gap that let the Anthropic adapter's legacy `thinking` shape
//! ship for months before a live call surfaced its 400 (see #240).
//!
//! **Gate**: every test here is a no-op unless `STELLA_LIVE_SMOKE=1` is set
//! (see [`live_smoke_enabled`]) AND that provider's own credential resolves
//! (env var, or a `~/.stella/credentials.toml` entry — the same two
//! non-interactive steps `stella-cli`'s chain uses, see [`resolve_key`]).
//! Neither condition met -> the test prints why to stderr and returns
//! `Ok` (a clean skip, never a failure) — so `cargo test`/`make gate`/CI's
//! default `cargo test --workspace` never makes a network call or spends
//! real money. Wire it up locally with e.g.
//! `STELLA_LIVE_SMOKE=1 ANTHROPIC_API_KEY=sk-… cargo test -p stella-model \
//! --test live_smoke -- --nocapture`.
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
use stella_protocol::{CompletionMessage, CompletionRequest};

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
/// stderr (visible with `cargo test -- --nocapture`) so a run makes it
/// obvious which providers were actually exercised versus skipped.
fn armed_key_locked(provider_id: &str, env_var: &str, aliases: &[&str]) -> Option<ApiKey> {
    if !live_smoke_enabled() {
        eprintln!("[live_smoke] {provider_id}: skipped — set STELLA_LIVE_SMOKE=1 to run");
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
                "[live_smoke] {provider_id}: skipped — no {env_var}{alias_note} env var and no \
                 `{provider_id}` entry in ~/.stella/credentials.toml"
            );
            None
        }
    }
}

/// The credential to run `provider_id`'s smoke test with, or `None` to skip
/// cleanly — never a failure — when the suite isn't armed or the
/// credential isn't resolvable. Takes `env_lock()` for the synchronous
/// gate-check + resolve step only, released well before any caller's
/// network `.await` (see `env_lock`'s doc). Callers that already hold the
/// guard themselves must call [`armed_key_locked`] instead, not this.
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
            id: self.id,
            display_name: self.id,
            dialect: self.dialect,
            seeded: self.seeded,
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

/// Construct `provider`'s adapter through the production factory, or `None`
/// to skip cleanly.
///
/// Returns `None` — never a failure — when the suite is unarmed, the
/// credential does not resolve, or (Vertex/Bedrock) the extra addressing the
/// factory needs is absent. A factory error for any *other* reason is a real
/// failure: that is the anti-phantom-slug gate and the dialect dispatch
/// talking, and a live run must not paper over them.
fn armed_provider(provider: &LiveProvider) -> Option<Box<dyn Provider>> {
    let key = armed_key(provider.id, provider.env_var, provider.aliases)?;

    // Vertex/Bedrock need more than the one credential `armed_key` resolves.
    // The factory resolves that itself (via `stella_model::credential`), so
    // pre-flighting here keeps "not configured" a clean skip while leaving the
    // real resolution to production code.
    match provider.dialect {
        Dialect::Vertex => {
            if let Err(e) = stella_model::credential::VertexAddressing::resolve() {
                eprintln!(
                    "[live_smoke] {}: skipped — {} is set but Vertex addressing does not \
                     resolve: {e}",
                    provider.id, provider.env_var
                );
                return None;
            }
        }
        Dialect::Bedrock => {
            if let Err(e) = stella_model::credential::BedrockCredentials::resolve() {
                eprintln!(
                    "[live_smoke] {}: skipped — {} is set but the AWS credential chain does \
                     not resolve: {e}",
                    provider.id, provider.env_var
                );
                return None;
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
        Ok(built) => Some(built),
        Err(e) => panic!(
            "`{}` failed to construct through the factory — this is a config/catalog fault, \
             not a network one, and would fail an ordinary `stella --model {}/{}` run the \
             same way: {e}",
            provider.id, provider.id, provider.model
        ),
    }
}

/// Send `request` and report. Shared by every provider so the success log and
/// the failure wording stay identical across adapters.
async fn smoke(provider: &LiveProvider, built: Box<dyn Provider>, request: CompletionRequest) {
    match built.complete(request).await {
        Ok(r) => eprintln!(
            "[live_smoke] {}: OK — model={} finish={:?} usage={:?} cost_usd={} text={:?}",
            provider.id, r.model, r.finish_reason, r.usage, r.cost_usd, r.text
        ),
        // Deliberately does NOT say "rejected the request shape": a failure
        // here can be an account-billing 400 that never reached shape
        // validation, so pre-judging the cause would print something false on
        // the exact failure this suite exists to distinguish from a real
        // regression. `e` already carries the provider's own status + body
        // (never a raw credential), so a human reading it tells wire-shape
        // apart from auth/quota/billing.
        //
        // What it can no longer be is an unknown slug: the factory's seed
        // floor rejects those before any wire call, and
        // `every_live_smoke_slug_resolves_against_the_catalog_seed` catches
        // them offline before that.
        Err(e) => panic!(
            "{} live smoke did not return a parseable 200 — could be a genuine wire-shape \
             regression OR an unrelated account/auth/quota/billing problem; read the status \
             and body below to tell which: {e}",
            provider.id
        ),
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
async fn anthropic_smoke() {
    let provider = row("anthropic");
    let Some(built) = armed_provider(provider) else {
        return;
    };
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
        Err(e) => panic!(
            "anthropic live smoke did not return a parseable 200 — could be a genuine \
             wire-shape regression OR an unrelated account/auth/quota/billing problem; \
             read the status and body below to tell which: {e}"
        ),
    }
}

/// OpenAI Responses API.
#[tokio::test]
async fn openai_smoke() {
    let provider = row("openai");
    let Some(built) = armed_provider(provider) else {
        return;
    };
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// Gemini direct `generateContent`. `GEMINI_API_KEY` (spec-documented alias
/// `GOOGLE_API_KEY`, same as `stella-cli`'s `config::PROVIDERS` row).
#[tokio::test]
async fn gemini_smoke() {
    let provider = row("gemini");
    let Some(built) = armed_provider(provider) else {
        return;
    };
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
async fn vertex_smoke() {
    let provider = row("vertex");
    let Some(built) = armed_provider(provider) else {
        return;
    };
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// Amazon Bedrock Converse. Needs the standard AWS chain
/// (`AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY`, optional
/// `AWS_SESSION_TOKEN`/`AWS_REGION`), resolved inside the factory by
/// `stella_model::credential::BedrockCredentials` — the same resolution a
/// real run uses.
#[tokio::test]
async fn bedrock_smoke() {
    let provider = row("bedrock");
    let Some(built) = armed_provider(provider) else {
        return;
    };
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
async fn openrouter_smoke() {
    let provider = row("openrouter");
    let Some(built) = armed_provider(provider) else {
        return;
    };
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// Z.ai (GLM), via the OpenAI-compatible adapter under its own identity.
#[tokio::test]
async fn zai_smoke() {
    let provider = row("zai");
    let Some(built) = armed_provider(provider) else {
        return;
    };
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// DeepSeek, via the OpenAI-compatible adapter under its own identity.
#[tokio::test]
async fn deepseek_smoke() {
    let provider = row("deepseek");
    let Some(built) = armed_provider(provider) else {
        return;
    };
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}

/// xAI (Grok), via the OpenAI-compatible adapter under its own identity.
#[tokio::test]
async fn xai_smoke() {
    let provider = row("xai");
    let Some(built) = armed_provider(provider) else {
        return;
    };
    smoke(provider, built, tiny_request(TINY_SYSTEM_PROMPT)).await;
}
