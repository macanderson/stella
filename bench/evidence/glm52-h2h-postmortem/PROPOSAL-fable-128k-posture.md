# Proposal: the Fable-class ceiling set — 128,000 output cap + scaled `model_timeout`

**Status: APPROVED and REGISTERED (2026-08-03).** The maintainer approved the
Fable ceiling set; it is implemented and its digests are registered below.
This memo is kept as the reasoning of record — why these numbers, and how they
were derived — rather than rewritten into a changelog entry.

What shipped:

| ceiling | value | where |
|---|---|---|
| `params.max_tokens` (default/worker/judge) | **128,000** | `posture.py::_OUTPUT_CAP_BY_SLUG`, pinned to `catalog.rs` by `TestOutputCeilingParity` |
| `model_timeout` | **1,572s** | `posture.py::_MODEL_TIMEOUT_BY_SLUG`, emitted as the `model_timeout_secs` posture key |
| `--turn-budget` | unchanged | adapter, per trial |

**Only the Fable arm moved.** Sonnet 5 keeps 64,000 and no timeout key, so its
registered digest `3c428a22…` is bit-identical — its comparator stops at
64,000, so parity says it stays.

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

| ceiling | Sonnet-5 arm | Fable arm (approved) | where it lives | what it moves |
|---|---|---|---|---|
| `params.max_tokens` (default/worker/judge) | 64,000 | **128,000** (the model ceiling) | `posture.py::_OUTPUT_CAP_BY_SLUG` | the hashed posture → the Fable digests |
| `model_timeout` | engine default (816s), key omitted | **1,572s** | `posture.py::_MODEL_TIMEOUT_BY_SLUG` → `model_timeout_secs` | the hashed posture → the Fable digests |
| `--turn-budget` | per-trial, Harbor agent timeout − 60s | unchanged | adapter, per trial | nothing |

> **Amended.** When this memo was written `model_timeout` was an `EngineConfig`
> constant with no configuration path, so this row read "the SUT binary →
> re-freeze of the SUT commit" and half the change was maintainer-only for
> mechanical reasons rather than judgement ones. It is now an ordinary posture
> key, so both ceilings move the same way: the digest, and no binary.
>
> Both are also keyed by **model** rather than shared, which is what stops the
> two from separating. Selecting Fable selects 128,000 *and* 1,572s together;
> there is no way to raise one and forget the other, which is exactly how
> 16384 → 32000 → 64000 each relocated the cliff instead of removing it.

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

## Registered digests

Emitted by `_benchmark_engine_posture` after the change, which is the
authoritative source — a hand-computed value that disagrees with the machine
manifest is the `harbor==0.6.1` failure in a new file. Pinned by
`TestFableCeilingSet::test_the_registered_fable_digests`.

| model | registered digest | cap | timeout |
|---|---|---|---|
| anthropic/claude-fable-5 | `5d42e2364755534c5632189ca988b892d108f54c30901d988fc88037407b2bfe` | 128,000 | 1,572s |
| openrouter/anthropic/claude-fable-5 | `18a1ba22e2ffef7fb8634504a0d6aff39c3117a52e48dd0f22159693641fd572` | 128,000 | 1,572s |
| anthropic/claude-sonnet-5 (unchanged) | `3c428a22435228b8b11731e7c90031f4b606a8ec8c96eadde9dd266a7ffdb104` | 64,000 | engine default |

Two notes on reading these.

**They are not this memo's precomputed values.** The draft predicted
`b640fec3…` for Fable at 128,000. That figure assumed the timeout stayed an
engine constant and therefore never appeared in the posture. It is now the
`model_timeout_secs` key, so it is inside the hash — which is the point of
moving it. The predicted value described a posture that would have carried
the raised cap and an unrecorded timeout.

**Direct and gateway hash differently on purpose.** Same model, same ceilings,
different `default_model`, and the digest describes the whole posture. A run
still cannot claim one route's number under the other's digest.

## Why this needed a decision rather than a default

Two reasons stood when this memo was written; one has since been removed.

1. **It changes the frozen digest.** Registered thresholds and any published
   number describe the posture that produced them, so a silently moved
   constant makes the digest describe a posture nobody registered. This is
   why the change landed as an approval with the new digests recorded above
   — the same path §8.4.2 and §8.4.3 took.
2. ~~**Half of it is a SUT change.**~~ **Removed.** `model_timeout` was an
   engine constant, so the Fable arm would have required re-freezing the SUT
   commit. It is now `agent_engine_config.model_timeout_secs`, so both
   ceilings are posture keys and no binary moves.
3. **1,572s is derived, not measured.** Registered as provisional. If a
   Fable smoke run shows rewarded steps longer than 1,512s, re-derive from
   the measurement — longest rewarded comparator step + 60s — exactly as 816
   was derived. Tracked separately.

## Decision record

* [x] **Approved** — the Fable arm shape: `max_tokens: 128000` for
      default/worker/judge, `model_timeout: 1572s`, turn budget unchanged,
      triage unchanged. Approved by the maintainer 2026-08-03.
* [x] **Per-arm, keyed by model.** Both ceilings are properties of the model
      (`_OUTPUT_CAP_BY_SLUG`, `_MODEL_TIMEOUT_BY_SLUG`) rather than shared
      constants, so selecting Fable selects both together and neither can be
      left behind. `STELLA_MODEL_TIMEOUT` still overrides per run.
* [x] **Registered** — digests above, catalog row corrected to Fable's real
      128,000 ceiling, posture pinned to it by `TestOutputCeilingParity`.

Still open, and deliberately not part of this approval: whether Sonnet 5's own
catalog ceiling of 64,000 is correct. It is the *observed comparator* ceiling,
which is what parity needs, but it may be below the model's real API limit.
Changing it would move a published digest, so it is tracked as its own piece
of work rather than folded in here.
