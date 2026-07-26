# stella-media

Image, SVG, and video generation for Stella — client-side, BYOK, behind one
[`MediaProvider`](src/provider.rs) port, with the same artifact discipline as
the rest of the engine. It reaches users as the `generate_image`,
`generate_svg`, `generate_video`, and `poll_video` tools, which live in
[`stella-tools`](../stella-tools/src/media.rs) and drive this crate.

The hard boundary is isolation from `stella-model`. The workstream spec
nominally put vendor media HTTP clients next to the chat adapters; they live
here instead so this crate does **not** depend on `stella-model`, which is why
[`credential::ApiKey`](src/credential.rs) and
[`error::MediaError`](src/error.rs) are minimal self-contained copies of that
crate's `ApiKey`/`ProviderError` shapes rather than imports — folding the
adapters into `stella-model`'s provider set is the recorded migration follow-up
([`src/lib.rs`](src/lib.rs)). Three further boundaries: providers never touch
the filesystem (they return in-memory `MediaArtifact` bytes for the caller to
persist through `ArtifactStore`, so the artifact jail is enforced in one
place); `emit` returns `AgentEvent` *values* because this crate has no channel
dependency; `preview` builds strings and never writes to a TTY. Audio, 3D, and
image *understanding* (the chat `vision` role) are out of scope.

## Where it sits

The only workspace dependency is `stella-protocol` (`MediaKind`,
`MediaJobState`, `MediaArtifactRef`, and the media events, all re-exported from
`lib.rs`). `stella-tools` and `stella-cli` depend on it; nothing else does, and
it builds no binary. `libc` is a `cfg(unix)` dependency of the operation
journal's secure-open discipline. `stella-cli` uses it directly only to open
the SQLite operation journal; everything else arrives through the
`stella-tools` media tools.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The authoritative crate description, the public re-export surface, and the recorded architecture deviation. Read it first. |
| [`src/provider.rs`](src/provider.rs) | The `MediaProvider` trait, its request/response types, and `MediaCapabilities` rate-card estimation. Open it to change the port. |
| [`src/adapters/`](src/adapters/mod.rs) | One file per vendor endpoint: `zai_image.rs` (CogView), `zai_video.rs` (CogVideoX), `openai_image.rs` (gpt-image). `mod.rs` also owns the live-smoke env gate. |
| [`src/http.rs`](src/http.rs) | Private, shared adapter plumbing: the bounded `reqwest` client, `classify_http_error`, `Retry-After` parsing, and the size-capped `download_bytes`. |
| [`src/artifact.rs`](src/artifact.rs) | `ArtifactStore` — the single writer to `.stella/artifacts/`, plus the `manifest.json` upsert. |
| [`src/svg.rs`](src/svg.rs) | `SvgPipeline`: validate → sanitize → optimize, and the bounded model-repair loop. Pure text in, pure text out. |
| [`src/jobs.rs`](src/jobs.rs) | `JobStore` (`jobs.json` beside the artifacts) and `resume`, the load-then-poll-live flow. |
| [`src/operation_journal.rs`](src/operation_journal.rs) | `MediaOperationJournal` and its SQLite implementation — durable, cross-process idempotency for paid submissions. The largest file here, and mostly secure-open discipline. |
| [`src/cost_gate.rs`](src/cost_gate.rs) | `MediaSpendGate`, the content-free `MediaSpendRequest`, and the deny-by-default implementation. |
| [`src/preview.rs`](src/preview.rs) | The terminal preview ladder: kitty → iTerm2 → a plain path line, as pure string builders. |
| [`src/emit.rs`](src/emit.rs) | Job transition → `AgentEvent` mapping, so a `MediaJobStatus` translates one way, not per call site. |
| [`src/error.rs`](src/error.rs) / [`src/credential.rs`](src/credential.rs) | The `MediaError` category set with its retry classification, and the redacted `ApiKey`. |
| [`tests/operation_journal.rs`](tests/operation_journal.rs) | The only integration test file: journal concurrency and Unix permission witnesses. |

## Key concepts

**One port, three wired adapters.** `ZaiImageProvider` (`cogview-4`, `POST
/images/generations`, which may answer with a URL or inline `b64_json` — the
adapter downloads the URL so the caller never holds an expiring link),
`ZaiVideoProvider` (`cogvideox-3`, `POST /videos/generations` to submit, `GET
/async-result/{id}` to poll), and `OpenAiImageProvider` (`gpt-image-1`, inline
`b64_json`, no second round trip). Which one a user gets is decided *outside*
this crate, in `detect_media_backend`
([`../stella-tools/src/media/backend.rs`](../stella-tools/src/media/backend.rs)):
`ZAI_API_KEY` wins when both are set and is the only backend with video;
`OPENAI_API_KEY` yields an image-only backend. What an adapter cannot do
returns `MediaError::CapabilityUnavailable` naming the env var that would
enable it — never a panic or a silent no-op.

**Money crosses two host-owned ports before the wire.** `MediaSpendGate`
authorizes each submission from a content-free `MediaSpendRequest` (provider,
kind, estimate — never the prompt or label), and callers without an injected
host gate get `DenyMediaSpendGate`. `MediaOperationJournal` then makes the
submission idempotent: `claim` returns `New`, `Existing(state)` to replay, or
`Expired`. No bundled adapter exposes remote idempotency
([`src/provider.rs:17`](src/provider.rs)), so the local claim is all that
stands between a retry and a second charge. Relatedly, neither image adapter
forwards `req.n` — a caller passing `n > 1` gets one image, billed for one
([`src/adapters/openai_image.rs:100`](src/adapters/openai_image.rs)), even
though `stella-tools` quotes the gate `estimate_image(n, …)` and so asks the
user to approve an n× charge. The charge can never exceed the approval, but the
approval overstates the charge; multi-candidate generation is a recorded
follow-up, not a supported flag.

**A persisted job handle is never reported from cache (L-V3).** `jobs::resume`
loads the handle and polls the provider; a `404` on the poll endpoint comes
back as `MediaJobState::Failed` — a definite terminal "gone", not an error and
not a stale `Running`. Unrecognized statuses map optimistically to `Running`,
on the reasoning that a new status name is far more likely to be in-flight than
terminal; the cost is that an unrecognized *terminal* status polls until the
caller gives up, and the age bound belongs in that caller
([`src/adapters/zai_video.rs:239`](src/adapters/zai_video.rs)).

**LLM-authored SVG is untrusted code (L-V2).** `SvgPipeline::process` is pure
and deterministic: parse with `roxmltree` under default options, which reject
DTDs and so block XXE and billion-laughs before a renderer ever sees them;
re-serialize through a deny-list (drop `<script>`, `<style>`,
`<foreignObject>`, `<metadata>`; drop every `on*` attribute; keep only
same-document `#fragment` hrefs; drop any value containing `//` or
`javascript:`); then optimize lightly (comments/PIs dropped, whitespace
collapsed, a `viewBox` backfilled from `width`/`height`). Depth is bounded
twice against `MAX_NESTING_DEPTH` (256) — textually before parsing, because
roxmltree's tokenizer recurses per level and would overflow the stack *inside*
`Document::parse`, and again over the built tree, because this module's
serializer recurses too.

**`ArtifactStore` is the single writer to `.stella/artifacts/`.** Ids are
generated here, never caller-supplied, and every filename component is reduced
to `[a-z0-9_-]`, so no id, label, or extension can carry a separator or a `..`.
Files are opened `create_new` (on unix, `O_EXCL|O_CREAT`, which also refuses a
planted symlink), and the manifest row for a path is *replaced*, not appended.

## Gotchas

- **The operation journal is blocking and Unix-only.** Every method is SQLite
  I/O behind a `Mutex`, plus bounded `thread::sleep` backoff on a contended
  first open (~1s in `open`, ~2s in schema init), so an async caller must reach
  it through `spawn_blocking`. The file-backed constructors fail closed off
  unix rather than persist a weaker journal; `open_in_memory` is portable but
  forgets every claim on a crash — which is the exact failure the persisted
  journal exists to prevent.
- **`MediaError::SidecarRace` is not caller-retryable.** `is_retryable()` is
  `false`: the journal retries the benign concurrent-first-open race itself
  (40 attempts × 25 ms), and the classifier keys on the *variant*, never on a
  message substring (#456). It escapes only if the race never settles.
- **Prompt-derived text never reaches disk here.** `jobs.json` persists
  identifiers and money only, so a resumed job comes back labelled with its
  artifact id rather than its prompt-derived label; the journal schema stores
  hashed keys and opaque handles; `MediaSpendRequest` is content-free and uses
  `deny_unknown_fields`. Tests assert the absence, not just the intent.
- **Never build a `reqwest::Client` directly in an adapter** — use
  `http::client()`. There is no outer tool-level timeout on the media path, so
  an unbounded client turns a stalled provider into a turn that hangs forever.
  Downloads are capped (64 MiB image / 256 MiB video); the overflow is
  `Malformed`, non-retryable on purpose, since re-fetching an oversized asset
  just repeats the memory exhaustion the cap prevents.
- **Both Z.ai adapters report `id() == "zai"`.** A job's `provider_id` alone
  does not say whether the image or the video adapter owns it.
- **`with_base_url` exists but nothing wires it.** All three adapters take a
  base-URL override, and only the tests call it: `detect_media_backend` builds
  each adapter on its vendor default, so there is no flag, setting, or env var
  that points the media family at a gateway or a compatible proxy. The seam is
  there; the escape hatch is not.
- **`preview` and `emit` have no callers.** Both are complete, tested ports
  that nothing in the workspace drives — the media tools return a text
  `ToolOutput` naming the saved path. Treat them as unshipped surface area, not
  as behaviour a user sees.
- **The poll URL interpolates `provider_job_id` verbatim.** It arrives from the
  vendor's submit response and then from `jobs.json`, which sits in the
  model-writable workspace, so a crafted id is a path fragment inside the
  vendor base URL on a request that carries the user's API key. The host is
  pinned by `base_url` and cannot be changed this way; the path can.
- **Two SVG holes are recorded, not fixed.** Rule 6 keys on `//` and
  `javascript:`, so a `data:` URI in a non-`href` attribute survives, and a
  `style="…"` *attribute* has no rule of its own (only the `<style>` element
  does). Neither is reachable through the preview ladder, which never renders
  SVG — both matter once an artifact is inlined into HTML.

## Testing

```bash
cargo test -p stella-media
```

There is no `make` target for this crate — `make test` runs the workspace.
Unit tests sit beside the code in `#[cfg(test)] mod tests`;
[`tests/operation_journal.rs`](tests/operation_journal.rs) is the only
integration file, covering cross-process claims, concurrent first-open, expiry
pruning, and the Unix permission witnesses: a permissive parent or database, a
symlinked or hardlinked database, and injected WAL/SHM sidecars are each
refused, and refused without being repaired or mutated.

Adapter coverage is **wiremock**: each test starts a `MockServer` and points
the adapter at it with `with_base_url`, then pins both the happy path and the
failure shapes — `401` → `Auth`, `429` with `Retry-After` →
`RateLimited { retry_after_ms }`, a `400` whose body mentions safety →
`ContentPolicy`, an empty or field-less payload → `Malformed`, and `404` on the
video poll → `Failed` (gone). `http.rs` carries its own wiremock tests for the
download caps (including the case where `Content-Length` lies or is absent) and
proves a stalled response times out instead of hanging.

Each adapter family also carries one **live smoke** that fires only when the
vendor key *and* `STELLA_MEDIA_LIVE=1` are present, so CI never calls a paid
API. The gate requires the literal `1` — `true`, `yes`, and `" 1"` all leave it
skipped — because a CogVideoX submit spends real money; `live_smoke_armed_by`
is the pure half, so the gate is witnessed without a `set_var` that would race
every other test thread. (`OXAGEN_MEDIA_LIVE` is a deprecated alias.)

## Extending it

To add a provider:

1. Add `src/adapters/<vendor>_<kind>.rs`, copying `zai_image.rs`'s shape, and
   implement `MediaProvider`. Return `MediaError::CapabilityUnavailable`,
   naming the enabling env var, for the methods this vendor does not serve.
2. Build the HTTP client with `crate::http::client()` and route every
   non-success response through `classify_http_error` — a per-adapter status
   match is how a `401` starts meaning something different per vendor.
3. Fill `MediaCapabilities` with the vendor's documented rates: those numbers
   are what the spend gate shows the user before charging.
4. Register the module in [`src/adapters/mod.rs`](src/adapters/mod.rs)
   (`pub mod` + `pub use`).
5. Add wiremock tests for the happy path plus 401 / 429 / content-policy /
   malformed, and one live smoke behind `live_smoke_enabled()`.
6. Wire it into `detect_media_backend` in
   [`../stella-tools/src/media/backend.rs`](../stella-tools/src/media/backend.rs).
   Until you do it compiles and tests green but never reaches `generate_image`
   — this crate has no provider registry of its own.

## See also

- [`../AGENTS.md`](../AGENTS.md) — "Architecture: ports, not concretions" (why
  every vendor is an adapter behind `MediaProvider`) and "Testing approach",
  whose wiremock-adapter-test line names this crate.
- [`../stella-tools/src/media.rs`](../stella-tools/src/media.rs) — the tools
  that drive this crate, their conditional registration, and where the spend
  gate, operation ids, and journal are injected.
- [`../stella-protocol/src/event.rs`](../stella-protocol/src/event.rs) —
  `MediaKind`, `MediaJobState`, `MediaArtifactRef`, and the `MediaProgress` /
  `MediaComplete` events `emit` builds.
- [`../website/content/docs/agent-tools/index.mdx`](../website/content/docs/agent-tools/index.mdx)
  — the user-facing tool table and each media tool's registration condition.
