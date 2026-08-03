# Proposal: the Fable-class ceiling set — 128,000 output cap + scaled `model_timeout`

**Status: PROPOSED — preregistration decision, maintainer sign-off required.
Nothing in this PR changes the frozen posture, the engine constant, or any
registered digest.** This memo exists so the decision can be made once,
deliberately, before a paid Fable-class run — not discovered mid-run the way
16384 → 32000 → 64000 each were (READINESS.md §8.4.2–§8.4.3).

## The rule, restated

The rule that set every previous ceiling is unchanged: **never be the side
that stops first.** A ceiling below what the comparator is allowed is not a
tuning choice; it is a handicap the score then reports as a capability
difference. The frozen posture's `max_tokens: 64000` is correct *for the
Sonnet-5 arm* because 64,000 is where the comparator's own steps stop —
gate3c Arm B landed steps on exactly 64,000 twice and still emitted the tool
call. It is **not** the model API ceiling: Sonnet 5's and Fable 5's output
ceiling is 128,000 (`/v1/models` `max_tokens`; the engine's own catalog
carries the same value).

A Fable-class worker (`anthropic/claude-fable-5`) changes the comparator.
Claude Code on the first-party API does not cap itself below the model
ceiling, and Fable is documented to run minutes-long single steps on hard
tasks at `xhigh`. Freezing a Fable arm at 64,000 would re-create the exact
asymmetry §8.4.3 removed: Stella truncated at half the height the comparator
is allowed to fill.

The GLM-5.2 post-mortem beside this memo is the other half of the motive: the
fatal cap-hit shape that survives the 64k posture is "first-step
mega-reasoning" on golf/compression/assembly tasks — the class where the
comparator's winning steps were already brushing its own ceiling.

## The three ceilings, and what each change touches

Same one-budget coupling as §8.4.3 — moving one alone relocates the cliff:

| ceiling | Sonnet-5 arm (frozen) | Fable-class arm (proposed) | where it lives | what changes |
|---|---|---|---|---|
| `params.max_tokens` (default/worker/judge) | 64,000 | **128,000** (the model ceiling) | `bench/harbor_adapter/stella_harbor/posture.py::_benchmark_engine_posture` | the hashed posture → every posture digest |
| `model_timeout` | 816s (engine default) | **1,572s** (provisional, see below) | `agent_engine_config.model_timeout_secs`, selected by `STELLA_MODEL_TIMEOUT` | the hashed posture → every posture digest |
| `--turn-budget` | per-trial, Harbor agent timeout − 60s | unchanged | adapter, per trial | nothing |

> **Amended.** When this memo was written `model_timeout` was an `EngineConfig`
> constant with no configuration path, which is why the row above used to read
> "the SUT binary → re-freeze of the SUT commit". It is now an ordinary posture
> key (`agent_engine_config.model_timeout_secs`, host-side selector
> `STELLA_MODEL_TIMEOUT`), so both ceilings move the same way: the digest.
> Unset omits the key, so every digest registered before the selector existed
> still describes the posture that produced it — the frozen Sonnet-5 arm still
> hashes to `3c428a22…`. **This changes how the decision is executed, not what
> is being decided.** The numbers below are still the maintainer's to approve.

`triage` stays at the engine default in both arms — low effort, three-line
classification, the cap was never near binding (same reasoning as every
prior generation).

### On the `model_timeout` number

816s was set 60s above the longest single step the comparator was ever
rewarded for (756s, gate3c). No Fable-class comparator telemetry exists yet,
so the honest derivation is the observed throughput scaled to the new cap:
the comparator's 64,000-token rewarded step took 756s (~85 tok/s); a
128,000-token step at the same rate is ~1,512s; the same +60s margin gives
**1,572s**. Two qualifiers, both in the constant's own doc comment:

* `model_timeout` bounds **idle silence between stream fragments**, not
  elapsed time — a generation that keeps streaming is never cut by it. The
  number is a margin against a provider that stopped answering, sized so it
  can never bind before the output cap does.
* If a Fable-class smoke run observes rewarded steps longer than 1,512s, the
  rule says re-derive from the measurement (longest rewarded comparator step
  + 60s), exactly as 816 was derived. The provisional number is registered as
  provisional for that reason.

## Reference digests for the proposed arm

Computed with the adapter's own canonicalization (the posture dict from
`_benchmark_engine_posture`, caps patched, normalized and hashed by the same
`sort_keys`/compact-JSON/SHA-256 rule). **Reference only** — the
authoritative values are whatever `_benchmark_engine_posture` emits after the
constant actually moves, and the launcher manifest is the value of record; a
hand value that disagrees with the machine manifest is the `harbor==0.6.1`
failure in a new file.

| model | frozen (`xhigh`, 64000) | proposed (`xhigh`, 128000) |
|---|---|---|
| anthropic/claude-fable-5 | `640b9469d5d3bc1394af20967915cca0f6841180648dec4daedbbd5f629ef510` | `b640fec3867ed632d742f8a78c5f899b222ce77422192a5c9ee395ceb2a004aa` |
| anthropic/claude-sonnet-5 | `3c428a22435228b8b11731e7c90031f4b606a8ec8c96eadde9dd266a7ffdb104` | `29b57409d8448e565dadce0697cde353c6d84919a46a50467d7515522aba2848` |

(The frozen Sonnet-5 value matches READINESS.md §8.4.3's registered table,
which is the check that the reference machinery agrees with the launcher's.)
The Sonnet-5 row's proposed column is listed because the constant is shared,
**not** because this memo proposes changing the Sonnet-5 arm — under the
parity rule the Sonnet-5 comparator stops at 64,000, so its frozen posture
stays. If the maintainer instead prefers per-arm caps, the implementation
should follow the existing arm pattern (a host-side selector that lands in
the hash, like `STELLA_WORKER_EFFORT`), so that which cap a run used is a
property of its digest rather than of its logs.

## Why this is not automated away

Three separate reasons, each sufficient:

1. **It changes the frozen digest.** Registered thresholds and any published
   number describe the posture that produced them; a silently moved constant
   makes the digest describe a posture nobody registered. The change must
   land as its own preregistration (or amendment) with the new digests in
   the protocol tables — the same path §8.4.2 and §8.4.3 took.
2. ~~**Half of it is a SUT change.**~~ **Resolved.** `model_timeout` was an
   engine constant when this memo was written, so the Fable arm required a
   re-freeze of the SUT commit — maintainer-only under the protocol. It is now
   `agent_engine_config.model_timeout_secs`, so the arm is a posture change
   like the output cap beside it and no binary moves. What remains is reason 1,
   which applies to both ceilings equally.
3. **The number itself is provisional.** 1,572s is derived, not measured;
   the preregistration should say which it is registering.

## Decision asked of the maintainer

* [ ] Approve the Fable-class arm shape: `max_tokens: 128000` for
      default/worker/judge, `model_timeout: 1572s`, turn-budget wiring
      unchanged, triage unchanged.
* [x] ~~Choose global-constant vs per-arm-selector implementation.~~
      **Per-arm selector**, and it is built: `STELLA_MODEL_TIMEOUT` lands in
      the hashed posture exactly like `STELLA_WORKER_EFFORT`, so which timeout
      a run used is a property of its digest rather than of its logs. Chosen
      rather than deferred because the two options are not symmetric — the
      selector is the only one of the two that leaves every already-registered
      digest reproducible, and it subsumes the global constant (an operator
      who wants one arm just sets one value).
* [ ] Register it. **What this now takes:** the output cap is still a literal
      in `posture.py` pinned by `TestOutputCeilingParity` to the seeded
      ceiling in `stella-model/src/catalog.rs`, so approving 128,000 means
      moving the catalog row and letting the ratchet pull the posture with it.
      The timeout needs no code at all — it is `STELLA_MODEL_TIMEOUT=1572` on
      the run. Then recompute digests via `_benchmark_engine_posture`, update
      the protocol/analyzer/READINESS tables, and publish before any paid
      Fable-class trial.
