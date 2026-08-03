# Readiness probe: the 5 clean-fail tasks

A development (non-claim) Harbor run of the five tasks the GLM-5.2
head-to-head showed Stella failing cleanly while Claude Code passed —
`fix-git`, `sanitize-git-repo`, `openssl-selfsigned-cert`, `kv-store-grpc`,
`fix-code-vulnerability` (selection rationale:
[`../glm52-h2h-postmortem/README.md`](../glm52-h2h-postmortem/README.md) §4).
All five are verifier-decided capability misses with no ceiling or timeout
confound, which makes them the cheapest honest probe of whether the
post-head-to-head harness work (#1212 git baseline, #1213 exit-cause +
provider streaming, #1214 worker lifecycle) moved the class.

## Run design

* SUT: `e5f9e07dee94b053021bbd262f8af2257b05dd8e` (this branch's base),
  cross-built `x86_64-unknown-linux-gnu.2.17` via `cargo-zigbuild`,
  offline smoke test 5/5.
* Dataset: the pinned 89-task export
  (`terminal-bench/terminal-bench-2-1@sha256:7d7bdc1c…`), five tasks staged
  locally; images pre-pulled first (registry availability as a precondition,
  per `bench/evidence/run/README.md`).
* Plain `harbor run` + `stella_harbor:StellaAgent`, `--n-attempts 1`,
  `--max-retries 0`, `--n-concurrent 2`. Ambient `STELLA_*` selectors are
  scrubbed at launch so the trial executes the frozen control posture, not an
  accidental triage/judge-pin arm (this sandbox exports
  `STELLA_TRIAGE_MODEL`, which is a registered arm selector).
* **Non-claim by construction**: no secure launcher, no intent ledger, and
  the environment adaptations below.

## Environment adaptations (disclosed, dev-only)

The run executes inside a sandbox that re-signs **all** egress TLS at an
upstream gateway with a private CA. The frozen binary verifies against its
bundled webpki roots and correctly refuses that chain, so trial containers
cannot reach any provider endpoint directly. Adaptations, none touching the
frozen adapter or SUT:

1. **Host-side TLS relay** — a plain-HTTP listener on the docker bridge that
   rewrites `Host`, forwards over TLS to the real provider endpoint trusting
   the gateway CA, and streams responses (SSE-safe). Stella reaches it via
   `STELLA_BASE_URL`; the provider key still travels only
   container → relay → provider on one VM.
2. **HTTP/1.1 shim for Harbor's own tooling** — the gateway proxy cannot
   carry HTTP/2 and harbor's supabase client stack hard-codes `http2=True`;
   a venv-local `.pth` hook coerces httpx to HTTP/1.1. Dev venv only; the
   secure claim launcher runs `python -S` and never loads it.
3. **Provider route** — the canonical `openrouter/*` path pins
   `https://openrouter.ai/api/v1` inside the adapter and therefore cannot
   traverse the relay, so the run uses Stella's `local/` (OpenAI-compatible)
   provider pointed at the relay with the OpenRouter key as
   `LOCAL_API_KEY`. Model: `z-ai/glm-5.2` — the head-to-head's own model.

## Status

Infrastructure proven end-to-end; scored run pending credentials.

* **Attempt 1 (anthropic/claude-sonnet-5, job
  `stella-readiness-5task-20260803`):** all five trials launched, containers
  built, binary verified in-container, triage fired, and every trial reached
  the live Anthropic API through the relay — which answered
  `"Your credit balance is too low to access the Anthropic API"` on the
  worker's first paid call. Every Anthropic key provisioned in this
  environment resolves to the same zero-credit account, so the attempt is an
  operational abort (credentials), not an agent outcome: rewards were never
  observed, and per the run rules the relaunch uses a fresh job name.
* The AWS credentials in this environment can invoke
  `us.anthropic.claude-sonnet-4-6` on Bedrock (verified with a 1-token
  converse), but `stella_harbor` deliberately refuses Bedrock's multi-value
  credential chain, so that route is out for Harbor trials.
* The OpenRouter relaunch is staged turnkey
  (`/home/user/tb-run/launch-openrouter.sh` in the run sandbox) and blocked
  solely on an `OPENROUTER_API_KEY`.

Per-task results, event streams, and the scored table land here when the
run completes.
