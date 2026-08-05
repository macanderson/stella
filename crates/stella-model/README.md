# stella-model

Every byte stella sends to a model vendor and every byte it parses back: the
concrete `Provider` adapters plus the vendor-neutral machinery they share — SSE
decoding, tool-call dialect translation, request signing, credential resolution,
model-catalog lookups.

The `Provider` trait itself is deliberately **not** defined here — it lives in
[`stella-protocol`](../stella-protocol/src/provider.rs) and is only re-exported
by [`src/provider.rs`](src/provider.rs), so `stella-core` drives every model call
through `&dyn Provider` without depending on any adapter. The other half of the
boundary: the seam is one port and *five wire dialects*, not one adapter per
vendor. Anything that genuinely speaks OpenAI Chat Completions — xAI, DeepSeek,
OpenRouter, a local server, a settings-defined gateway — rides the one adapter in
[`src/zai.rs`](src/zai.rs), re-identified through `ZaiProvider::with_identity`,
so a new OpenAI-compatible endpoint costs a config row in `stella-cli` rather
than a module here. A new module is justified by a structurally different
request/response shape, never by a new vendor name.

## Where it sits

Depends on exactly one workspace crate, `stella-protocol` (`Provider`,
`CompletionRequest`/`CompletionResult`, `ProviderError`, `Attachment`); the rest
are third-party — `reqwest`/`tokio`/`futures-util` for transport, `hmac`+`sha2`
for Bedrock SigV4, `rpassword` for the credential prompt, `toml` for
`~/.stella/credentials.toml`. Only `stella-cli` depends on it (`stella-core`
never does, by design), and it constructs adapters in exactly one place:
`build_provider_parts` in
[`../stella-cli/src/agent/engine.rs`](../stella-cli/src/agent/engine.rs). No
binary of its own.

## Boundary — does this change belong here?

If a change alters the bytes stella puts on the wire to a model vendor, or how
the bytes coming back are parsed — request JSON, SSE framing, tool-call dialect
translation, request signing, credential resolution, catalog and pricing lookup
— it belongs here. If it decides *when* to call a model or what the answer
means for the session (retry, budget, compaction), it belongs in `stella-core`,
which sees only `&dyn Provider`; if it decides *which* provider a user session
gets — seeded provider rows, base-URL policy, boot notices, what to persist
from a models.dev refresh — it belongs in `stella-cli`. The types on the seam
itself (`Provider`, `CompletionRequest`, `ProviderError`) belong in
[`stella-protocol`](../stella-protocol), never here: this crate implements the
port, it does not define it.

A new vendor is a new adapter module behind the port — never a new crate, and
never a rewrite of the engine side (AGENTS.md invariant #1) — and a module only
when its wire shape is genuinely new: per the dialect rule above, an
OpenAI-compatible vendor is a `stella-cli` config row and lands no adapter code
here at all. In either case the provider's rows in
[`src/provider_parity.rs`](src/provider_parity.rs) land in the same PR (AGENTS.md
invariant #8); "Extending it" below walks the mechanics and names the tests
that fail until you do.

A new crate instead of a module is almost never the right split from here. The
workspace-wide rule: a new crate is justified only when functionality (a) sits
behind a port and would drag heavy new dependencies into a crate that is
deliberately light — this crate is already the deliberately *heavy* side of the
`Provider` port (`reqwest`, `tokio`, SigV4 hashing), so a vendor's dependency
weight lands here by design; (b) needs a dependency direction the current graph
forbids — also already solved here, since the trait lives in `stella-protocol`
precisely so nothing but `stella-cli` has to link the adapters; or (c) is a
genuinely separate deliverable with its own binary and release cadence, which a
wire adapter never is. Otherwise extend this crate: a new crate costs an
AGENTS.md workspace-table row, an impacted-crates scope, CI time, and a README,
and a wrong split is harder to undo than a wrong merge. If one is truly
justified, AGENTS.md's workspace table and the root `Cargo.toml` members list
change in the same PR.

## God files — do not add lines

The gate's `file-size` guard (`scripts/check-file-size.sh`) enforces a
1500-line ratchet: a NEW file over the limit is a hard failure with no baseline
escape, and the files below are grandfathered at a recorded ceiling in
`scripts/file-size-baseline.txt`. They are god files — already too big, closed
to growth. Plan work so no new line lands in them: new logic goes in a new
submodule, following this crate's own precedent — the
[`src/anthropic/`](src/anthropic) and [`src/zai/`](src/zai) directories carry
tests split out beside their adapters — and code you touch in one is a
candidate to extract. One trap when extracting tests: parity witnesses are only
visible in files `adapter_sources()`
([`src/provider_parity.rs`](src/provider_parity.rs)) embeds, so a split that
moves witnesses into a new file fails the witness checks until that list gains
the file.

| God file | Ceiling (lines) |
|---|---|
| [`src/anthropic/tests.rs`](src/anthropic/tests.rs) | 1677 |
| [`src/openai.rs`](src/openai.rs) | 2072 |
| [`src/zai.rs`](src/zai.rs) | 1572 |
| [`src/zai/tests.rs`](src/zai/tests.rs) | 1843 |

A ceiling can move only via `make file-size-update`, which lands as a
reviewable baseline diff justified like any other change — treat it as an
escape hatch for an irreducible line (a module declaration in an oversized
`lib.rs`), never as a planning assumption.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The crate map (read this first) and the public re-exports: `Provider`, `Catalog`, `ApiKey`, the cache-economics helpers. |
| [`src/provider.rs`](src/provider.rs) | Two-line re-export of the port. Open it to be reminded where the trait actually lives. |
| [`src/zai.rs`](src/zai.rs) (+ [`src/zai/tests.rs`](src/zai/tests.rs), [`src/zai/tests/error_classify.rs`](src/zai/tests/error_classify.rs)) | The shared OpenAI Chat Completions adapter. One adapter serving Z.ai/GLM, xAI, DeepSeek, OpenRouter, `local`, and settings-defined gateways; per-identity behavior (OpenRouter's root `cache_control` + sticky `session_id`, GLM's `thinking`, xAI's `reasoning_effort`) is gated on `self.id` inside it. |
| [`src/anthropic.rs`](src/anthropic.rs) (+ [`src/anthropic/tests.rs`](src/anthropic/tests.rs)), [`src/openai.rs`](src/openai.rs), [`src/gemini.rs`](src/gemini.rs), [`src/vertex.rs`](src/vertex.rs), [`src/bedrock.rs`](src/bedrock.rs) | One adapter per structurally distinct wire dialect: Messages API, Responses API, `generateContent` (direct and Vertex's project-scoped enterprise path, sharing wire types and the stream aggregator), Bedrock Converse. All follow the same shape — `new(ApiKey, model)` capturing catalog pricing, `with_base_url`, `http::client()`, `SseDecoder`, `http::classify_http_status`, `impl Provider` (plus `complete_observed`, the mid-stream tool-call announcement the engine speculates on — every streaming adapter implements it; Bedrock inherits the no-op default). Open the one whose vendor you are debugging. |
| [`src/catalog.rs`](src/catalog.rs) | `Catalog`, `CatalogEntry`, `Pricing`, `ToolDialect`, and the compile-time seed. The only sanctioned slug → model resolution. |
| [`src/credential.rs`](src/credential.rs) (+ [`src/credential/aux.rs`](src/credential/aux.rs)) | `ApiKey` (the non-`Display` secret wrapper), the flag → env → file → prompt chain, `CredentialsFile` (keys *and* the `[credential_fields.<provider>]` companions), the multi-variable resolvers `VertexAddressing` / `BedrockCredentials`, and `AuxCredentials` — the redacting, zeroizing set a host uses to carry the values a provider needs beyond one key (Bedrock's secret access key, session token, and region). |
| [`src/provider_parity.rs`](src/provider_parity.rs) | `CachePosture` / `ReasoningPosture` — the per-provider matrix. A new provider id lands here or tests fail. |
| [`src/cache_economics.rs`](src/cache_economics.rs) | Cache savings arithmetic (`Pricing::cache_savings_usd`) and `diagnose_cache`, which reads the parity matrix to tell an opt-in bug from prefix instability. |
| [`src/sse.rs`](src/sse.rs) | Dependency-free SSE line parser + incremental UTF-8 decoder every streaming adapter feeds. |
| [`src/http.rs`](src/http.rs) | Crate-private plumbing: the timeout-bounded `reqwest` clients, `classify_http_status`, and the two shared stream-failure errors. Change error retryability here, not in an adapter. |
| [`src/attachment.rs`](src/attachment.rs) | Crate-private. Turns `Attachment`s into dialect-neutral `WirePart`s once, so each adapter only maps parts onto its own JSON. |
| [`src/modelsdev.rs`](src/modelsdev.rs), [`src/provider_listing.rs`](src/provider_listing.rs) | Fetch-and-parse only: the models.dev master list and provider-native `/models` discovery. Deciding what to store belongs to `stella-cli`. Best-effort by contract — a dead or shape-drifted endpoint returns an `Err(String)` the caller reports and moves past, so one provider can never fail a refresh of the others. |

## Key concepts

**One port, five dialects; identity is a field.** `ToolDialect`
([`src/catalog.rs`](src/catalog.rs)) is the axis adapters are cut on — four
vendors share `OpenaiJson`, two Google surfaces share `GeminiFunctions`.
`ZaiProvider::with_identity(id, label)` exists because without it every gateway
on the shared adapter misreported itself: an xAI 401 read "Z.ai rejected the API
key", pointing the user at the wrong credential.

**The catalog is a gate, not a hint.** A slug not in the catalog is an immediate,
named `ProviderError::UnknownModel` — never a silent fallback to a default (the
TS-era phantom `glm-5.2-turbo` lesson). Adapters call
`Catalog::resolve_for(provider, slug)`, not `resolve`, because the same slug
genuinely exists under several providers and an unscoped lookup takes whichever
row sits first.

**The parity matrix is the crate's law.**
[`src/provider_parity.rs`](src/provider_parity.rs) records, per provider id, how
its prompt cache is engaged/observed (`CachePosture`) and how reasoning is
controlled on the wire (`ReasoningPosture`). Born from a real defect: Anthropic
models routed through OpenRouter ran with zero prompt caching across a $2+
session because Anthropic's cache is opt-in while most are implicit, and
DeepSeek's cache-hit field was dropped the same week because it spells the
telemetry differently; the reasoning axis was added after the same shape recurred
for a pinned `effort`. Enforced from both sides — `stella-cli`'s config tests
fail when a seeded provider has no row, this crate's tests fail when a row's
witness test no longer exists.

**Errors are classified once, centrally.** `http::classify_http_status` is the
shared ladder every adapter applies after its vendor-specific pre-check: 401/403
→ non-retryable `Auth` (403 split into credits / model-not-enabled / permission),
402 → billing `Terminal`, 429 → `RateLimited` with `Retry-After`, 5xx → retryable
`Transport`, everything else → non-retryable `Terminal`. EOF before a stream's
terminal event is retryable; tool-call JSON truncated at the output-token limit
is not, because retrying re-truncates identically.

## Gotchas

- **Adding a request field can be a cost regression.** Optional sampling params
  carry `skip_serializing_if = "Option::is_none"` so a request without overrides
  serializes byte-identical to what shipped before — prompt-cache hits depend on
  a byte-stable prefix.
- **Pricing is captured at construction**, not per request. `Catalog::install_runtime`
  must run before any adapter is built, or every cost lands on seed pricing.
- **Anthropic's `cache_control` goes on content blocks only** — the system block
  and the last block of the final message. Never a top-level request field.
- **Gemini has no wire call ids.** Calls correlate by function *name*, so the
  adapter mints `call_0`, `call_1`, … and rides Gemini 3's `thoughtSignature`
  inside the id after a `#` — `ToolCall` has no slot for a provider-private blob,
  and omitting the signature degrades or rejects the next turn.
- **Session ids are volatile by design.** OpenRouter's `session_id` and OpenAI's
  `prompt_cache_key` pin a session to one cache shard but ride as request
  parameters and must never enter the cached bytes. Distinct per adapter
  construction, so fleet siblings don't serialize onto one shard.
- **Bedrock is non-streaming `Converse`, not `ConverseStream`** — the streaming
  variant speaks binary `application/vnd.amazon.eventstream`, a separate
  transport decoder. Its SigV4 signing is pinned by golden vectors generated from
  botocore, because signing code looks right while producing signatures a real
  endpoint rejects.
- **An attachment never fails a request.** One a dialect cannot ingest (audio on
  Anthropic, video on OpenAI, an unreadable payload file) degrades to a text note
  describing what was attached — the conversation replays every turn, so a hard
  error there would brick the session permanently.
- **`ZAI_GLM_CODING_PLAN=1` swaps the Z.ai base URL** to the coding-plan
  endpoint. Resolved by `stella-cli`'s `Config::effective_base_url` (the one
  home of base-URL policy), never inside this crate — `ZaiProvider::new`
  always starts at the standard endpoint and callers route via
  `with_base_url`.

## Testing

```bash
make test-model          # or: cargo test -p stella-model
```

Adapter tests are `wiremock`-based and live beside the code: inline
`#[cfg(test)] mod tests` in `openai.rs`, `gemini.rs`, `bedrock.rs`, `vertex.rs`,
out-of-line in [`src/anthropic/tests.rs`](src/anthropic/tests.rs) and
[`src/zai/tests.rs`](src/zai/tests.rs). They assert both directions — the exact
bytes that go out, and a `CompletionResult` reassembled from a canned SSE stream.
No credential or network required.

Two integration tests in [`tests/`](tests):

- [`tests/live_smoke.rs`](tests/live_smoke.rs) — one minimal *real* call per
  adapter, asserting wire-shape acceptance (200, and stella's own parser
  reassembles the result), never model quality. A clean skip unless
  `STELLA_LIVE_SMOKE=1` is set **and** that provider's credential resolves, so
  `make gate` and CI never make a network call:
  `STELLA_LIVE_SMOKE=1 ANTHROPIC_API_KEY=sk-… cargo test -p stella-model --test live_smoke`.
- [`tests/credential_prompt_degrade.rs`](tests/credential_prompt_degrade.rs) —
  the regression guard for "an interactive prompt that cannot read stdin degrades
  to `NotFound`", never a hang and never a raw `PromptFailed`.

## Extending it

Adding a provider. **Step 0 is the one most new providers stop at.**

0. **Does it speak OpenAI Chat Completions?** Then no code lands in this crate.
   Add a row to `PROVIDERS` in
   [`../stella-cli/src/config.rs`](../stella-cli/src/config.rs) with
   `dialect: Dialect::OpenaiCompatible` (or let a user define one in
   `settings.json`); `build_provider_parts` builds a `ZaiProvider` and calls
   `with_identity(id, label)`. Skip to step 3 — the parity matrix still applies.
1. **Genuinely different wire shape** → new module in `src/`, declared `pub mod`
   in [`src/lib.rs`](src/lib.rs). Copy the closest adapter's shape rather than
   inventing one: constructor resolving `Catalog::resolve_for(<id>, slug)`
   pricing, `with_base_url`, `http::client()`, `SseDecoder`,
   `http::classify_http_status`, `attachment::wire_parts` for user content,
   `impl Provider` (+ `complete_observed` if it streams). Add a `ToolDialect`
   variant in [`src/catalog.rs`](src/catalog.rs), and a matching `Dialect`
   variant plus `build_provider_parts` arm in `stella-cli`.
2. **Seed the catalog.** A row in `Catalog::seed()` for the provider's
   `default_model`, or `every_provider_default_model_resolves_against_the_catalog_seed`
   ([`../stella-cli/src/config/tests.rs`](../stella-cli/src/config/tests.rs))
   fails — and the provider would hard-error on first use.
3. **Declare both parity rows, in the same PR.** Add the provider id to *both*
   `CACHE_POSTURE` and `REASONING_POSTURE` in
   [`src/provider_parity.rs`](src/provider_parity.rs).
   - Cache: `OptIn` when the adapter must SEND a marker, `Implicit` when it must
     PARSE hit telemetry, `NotApplicable` only when no billed cache exists.
     Reasoning: `Controllable` when the effort preference reaches the request
     body, `Unsupported` when the adapter deliberately drops it (an honest
     degradation — `stella-cli` surfaces a boot notice), `FixedOn`/`FixedOff` for
     a model with no dial at all.
   - `OptIn`, `Implicit`, and `Controllable` must name a `witness` — the exact
     test function proving the behavior on the wire — and it must live in one of
     the six files `adapter_sources()` embeds with `include_str!`
     (`anthropic/tests.rs`, `bedrock.rs`, `openai.rs`, `gemini.rs`, `vertex.rs`,
     `zai/tests.rs`); a witness in a *new* adapter's own test module is invisible
     until you add that file there too. No-control variants carry a `note` instead.
   - What fails if you skip it: `every_seeded_provider_declares_a_cache_posture`
     / `every_seeded_provider_declares_a_reasoning_posture` (in `stella-cli`) on
     a missing row, `both_axes_cover_the_same_provider_ids` if you add one axis
     and forget the other, `provider_ids_are_unique` and its reasoning twin on a
     duplicate, and `every_witness_test_exists_in_the_adapter_sources` — which
     matches the literal `fn <witness>(` — the moment a witness is renamed.
4. **Multi-variable credentials belong here**, not in `stella-cli` — follow
   `VertexAddressing` / `BedrockCredentials` in
   [`src/credential.rs`](src/credential.rs), so a second host of the engine gets
   the same variable names, fallback order, and named errors without copying them.
5. **Add a gated live-smoke test** to [`tests/live_smoke.rs`](tests/live_smoke.rs).

A *new* per-provider divergence (attachment dialects, tool schemas) means a third
axis in `provider_parity.rs` — record it as a matrix, not as adapter folklore.

## See also

- [`../../AGENTS.md`](../../AGENTS.md) — "Architecture: ports, not concretions",
  invariant 8 ("Provider feature parity is declared, not assumed"), and the
  `stella-model` row in "Workspace layout — where a change goes".
- [`../stella-protocol/src/provider.rs`](../stella-protocol/src/provider.rs) —
  the `Provider` / `ToolCallObserver` contract every adapter here implements.
- [`../../website/content/docs/api-providers/`](../../website/content/docs/api-providers)
  — the user-facing page per provider id, and
  [`../../website/content/docs/configuration/credentials.mdx`](../../website/content/docs/configuration/credentials.mdx)
  for the resolution chain from the user's side.
