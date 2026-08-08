# Benchmark methodology — the run behind the investor deck

This document is the authoritative source for every benchmark figure in
[`investor-deck.html`](./investor-deck.html). One run backs them all. If a
number in the deck is not in this document, it is not a benchmark claim.

## The claim, stated precisely

> On the full 89-task Terminal-Bench 2.1 suite, with the same model in both
> arms and the endpoint as the only permitted difference, stella resolved
> 58/89 (65.17%) against Claude Code's 44/89 (49.44%) — +15.73 percentage
> points, 31.8% more tasks solved — at $1.35 per solved task, inference only.

This is a **self-reported development baseline**, preregistered and
reproducible from committed artifacts. It is **not** an audited leaderboard
row, and the deck says so wherever the number appears.

## Run identity

| Field | Value |
|---|---|
| Run id | `tb21-hh10-20260731` |
| Date (UTC) | 2026-07-31, 21:02:19 → 23:02:16 |
| Preregistration | [`preregistration.json`](https://github.com/macanderson/stella/blob/main/bench/evidence/tb21-hh10-20260731/preregistration.json), written 2026-07-31T21:02:14Z — five seconds before the first trial. Preregistration issue: [#1013](https://github.com/macanderson/stella/issues/1013) |
| Harness | Harbor 0.6.1, adapter `bench/harbor_adapter` (`stella_harbor:StellaAgent`) |
| Dataset | `terminal-bench/terminal-bench-2-1@sha256:7d7bdc1cbedad549fc1140404bd4dc45e5fd0ea7c4186773687d177ad3a0699a` — all 89 tasks, no filters, no exclusions |
| Verifier | the dataset's own, unmodified |
| Model (both arms) | `glm-5.2`, effort `max`, reasoning on, unbounded per-trial budget |
| Attempts | 1 per task (pass@1), `max_retries = 0`, concurrency 20 |
| System under test | commit `62a8ae2d5d3d0839e52c268e0c2f5f0f3e3c4a49` (`v0.5.68-147-g62a8ae2d`, version 0.6.29), an unmodified ancestor of `main` |
| Binary | SHA-256 `a6392dccd79ad6669f2e851dd6830e31672b2bad738a348d184c2a0be37f5d77`, build stamp verified against the uploaded binary on every trial |
| Host | AWS EC2 `m6id.8xlarge` (32 vCPU / 123 GiB), native x86_64 Linux, no emulation; both arms back-to-back on this host in one session |

None of this is asserted on trust: every one of the 89 Arm A trials recorded
the posture digest, assurance digest, binary SHA, source commit, and Harbor
version it actually ran with, and all 89 agree on a single value for each
(`run-manifest.json`, `recorded_by_the_run`).

## Harness parity

The design rule was that **the endpoint is the only permitted difference**:
stella reaches `glm-5.2` through OpenRouter, Claude Code through z.ai's
Anthropic-compatible endpoint (OpenRouter serves no `/v1/messages`; z.ai
serves no alternative). Matched between the arms: host, day, task set,
verifier, Harbor version, effort, thinking, attempts, retries, concurrency,
per-task resources, and timeouts at 1.0x.

## Results

| | stella (Arm A) | Claude Code (Arm B) |
|---|---|---|
| pass@1 | **58/89 = 65.17%** | **44/89 = 49.44%** |
| 95% CI, seeded bootstrap (seed 20260729, 50,000 draws) | [55.06%, 75.28%] | [39.33%, 59.55%] |
| 95% CI, Clopper–Pearson exact | [54.33%, 74.96%] | [38.67%, 60.25%] |

Win/loss panel: passed by both **36** · stella only **22** · Claude Code only
**8** · neither **23**.

No inferential test is reported. The preregistration committed to publishing
raw counts and did not name a test; choosing one after seeing the counts is
what preregistration exists to prevent.

## Cost accounting

- **Method:** inference cost only, at provider list prices, summed per trial.
  No amortized training or fine-tuning cost exists to include — neither arm
  ran a model we trained.
- **stella:** $78.25 total → **$1.35 per solved task** (78.25 / 58). The
  committed score table (`score.json`) extracts $75.83 → $1.31 from the trial
  rows; the published results page extracts $78.25 from the trajectories. The
  difference is the extraction layer, not the run. **The deck quotes the
  higher figure.**
- **The arms' costs are not comparable**, for three independent reasons, any
  one of which is enough: (1) the arms bill through different providers, so a
  dollar difference is at least as likely route pricing as agent behavior;
  (2) the arms' token counters do not define "input token" the same way —
  Arm A's 353.7M input tokens are dominated by cache reads; (3) Arm B reports
  cost on only 71 of its 89 trials, so its $63.35 total is a partial sum.
  For these reasons the deck reports **no Claude Code cost figure at all.**

## Contamination control

Both arms ran **stock open weights** (`glm-5.2`) with no fine-tuning by
Oxagen — there is no Oxagen training corpus whose overlap with Terminal-Bench
could inflate this result. When customer-model training ships, Terminal-Bench
tasks and derivatives are excluded from every training corpus by protocol;
the exclusion procedure will be published with the first training run.

## What this run is not

**The witness tier was structurally off.** The benchmark posture pins one
model for every role, and the authored witness requires an author independent
of the worker, so the tier could not be authored on any task (every trial
recorded `assurance_arm: witness-off`). stella's 65.17% is therefore a
**lower bound on the full verification ladder**, not a measurement of it.

**It is not an audited leaderboard row.** The manifest's `claim_eligibility`
block names the five reasons, quoted in full:

1. "credentials came from the ambient environment, the adapter's
   environment-fallback source, not a Management-API-minted spend-capped key"
2. "no host attestation was collected or committed"
3. "no six-comment intent ledger; a single human-readable preregistration
   instead"
4. "launched through the plain adapter path, not secure_launcher.py"
5. "no external Terminal-Bench maintainer trajectory review"

Closing these — and submitting an audited public row — is a named use of
funds in the deck. The audit path is specified in
[`bench/terminal-bench-2.1-protocol.md`](https://github.com/macanderson/stella/blob/main/bench/terminal-bench-2.1-protocol.md).

## The public leaderboard, and why our number is not a row on it

The deck shows the published Terminal-Bench 2.1 leaderboard
([tbench.ai](https://www.tbench.ai), retrieved August 2026) for context:
Claude Code + Fable 5 at **83.8% ± 1.2%** (Jun 7, 2026), Codex + GPT-5.5 at
**83.1%**, Terminus 2 + Fable 5 at **80.4%**. Those rows run frontier models;
our run holds an open-weight model constant across both arms to isolate the
harness. The two measurements answer different questions and the deck does
not rank one against the other.

## Raw results

Everything needed to recompute the numbers is committed:

- [`bench/evidence/tb21-hh10-20260731/`](https://github.com/macanderson/stella/tree/main/bench/evidence/tb21-hh10-20260731) —
  `run-manifest.json` (frozen identity), `preregistration.json`,
  `trials.jsonl` + `score.json` + `results.md` (Arm A), `comparator/` (Arm B,
  same three files, same denominator)
- [`docs/benchmarks/terminal-bench-2-1-glm-5-2.html`](https://github.com/macanderson/stella/blob/main/docs/benchmarks/terminal-bench-2-1-glm-5-2.html) —
  the published per-task results page

Reproduce:

```bash
python bench/evidence/score_dev_baseline.py score \
  bench/evidence/tb21-hh10-20260731/trials.jsonl --tasks 89
python bench/evidence/score_dev_baseline.py score \
  bench/evidence/tb21-hh10-20260731/comparator/trials.jsonl --tasks 89
```

Both intervals are deterministic given the committed rows: the bootstrap is
seeded and Clopper–Pearson needs no seed.

---

*Document written 2026-08-08. Figures trace to the artifacts above; if this
document and an artifact disagree, the artifact wins.*
