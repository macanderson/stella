# `bench/evidence/`

Measured Terminal-Bench results for Stella, and the two scripts that turn a
Harbor job directory into a number anyone can recompute.

Before #909 this directory did not exist. Stella shipped a complete Harbor
adapter, a SWE-bench harness, a 723-line frozen protocol, a 375-line runbook and
a 186-line readiness report — and not one measured result. Every claim it made
about itself was therefore unfalsifiable. This directory is where that stops.

## What is in here

```
score_dev_baseline.py     scoring: trials.jsonl -> pass@1 + two 95% intervals
make_manifest.py          identity: freeze every input that can move the number
<run-id>/
  run-manifest.json       the frozen inputs, and why the run is not a claim
  trials.jsonl            one row per trial — the score's whole input
  results.md              per-task table, human-readable
  score.json              the computed number, as published
```

Reproducing a published number needs only that run directory:

```bash
python bench/evidence/score_dev_baseline.py score <run-id>/trials.jsonl --tasks 89
```

The multi-gigabyte Harbor job tree (raw trajectories, container logs) is **not**
committed. `trials.jsonl` is extracted from it and carries every field the score
depends on; `run-manifest.json` records the digest of each committed artifact so
a reader can tell whether what they have is what was published.

## Two kinds of run, and never confusing them

**Development baseline** — `stella-tb21-dev-baseline-manifest-v1`. Self-reported.
Runs on whatever host is available, with ambient credentials, through the plain
adapter path. Its manifest carries a `claim_eligibility` block that lists, in
full, every reason it is not a leaderboard row. It exists to be a held-out number
Stella can be measured against and improved against — the thing #830–#836 and
#876 both presuppose.

**Audited public claim** — `bench/terminal-bench-2.1-protocol.md`, scored by
`bench/terminal_bench_analysis/tb21_analysis.py`. Requires a dedicated native
x86_64 Linux host with a committed attestation, a Management-API-minted
spend-capped key, a published intent ledger, launch through
`secure_launcher.py`, 5 attempts per task, and an external Terminal-Bench
maintainer trajectory review. None of that is optional, and a development
baseline never becomes one by relabelling.

The scoring rules are deliberately shared between the two, so they cannot
disagree about what a pass is: the external verifier's reward is authoritative,
a trial that timed out having already left a correct solution keeps its reward,
and the denominator is the preregistered task count rather than the number of
trials that happened to report.

## Rules a run in here has followed

* **Preregistered.** The SUT, dataset digest, model, sampling parameters and the
  entire analysis plan were published before any paid call, and the
  preregistration body was not edited afterwards. The URL is in the manifest.
* **Fixed denominator.** Every task counts. A trial that errored, timed out,
  crashed or hit its budget cap scores zero and stays in; a task that produced no
  row at all also scores zero. A partial run therefore cannot flatter itself.
* **No outcome-selected anything.** No task excluded, no trial re-run, no
  threshold moved, no stop triggered by a bad-looking score. Operational stops
  are disclosed and published as partial.
* **Failures published too.** The per-task table lists every task, including the
  ones that failed and the ones that failed for infrastructure reasons.
