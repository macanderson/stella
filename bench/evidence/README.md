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
compare_arms.py           two arms of one experiment -> did the change pay?
witness-ab/               the authored-witness A/B: protocol, plan, decision rule
tests/                    what keeps the three of them honest
<run-id>/
  run-manifest.json       the frozen inputs, and why the run is not a claim
  preregistration.json    what was fixed before the first trial
  trials.jsonl            one row per trial — the score's whole input
  results.md              per-task table, human-readable
  score.json              the computed number, as published
  comparator/             a second arm, when one was run beside it — same files,
                          same denominator, and its own README caveats
```

### Runs in here

| Run | What | Result |
|---|---|---|
| [`tb21-hh10-20260731`](tb21-hh10-20260731/) | Matched head-to-head, native x86_64 host, `glm-5.2`, effort `max`, no budget cap | Stella **58/89 = 65.2%** · Claude Code **44/89 = 49.4%** |

Reproducing a published number needs only that run directory:

```bash
python bench/evidence/score_dev_baseline.py score <run-id>/trials.jsonl --tasks 89
```

The multi-gigabyte Harbor job tree (raw trajectories, container logs) is **not**
committed. `trials.jsonl` is extracted from it and carries every field the score
depends on; `run-manifest.json` records the digest of each committed artifact so
a reader can tell whether what they have is what was published.

Each row also carries the posture digest, assurance arm, binary SHA, source
commit and Harbor version **the trial itself recorded**, so the manifest states
the run's identity by collapsing 89 independent observations of it rather than by
recomputing it from whatever checkout the manifest happened to be built in. Those
are the same thing only while the run is still warm.

Since #1284 a row also carries what the trial's own verification ladder did:
which witness state it reached, and whether Stella itself claimed the work
passed. That claim and the external grader's reward are two independent
opinions of the same work, and their disagreement — a task failed while
reporting success — is invisible to the score. It lived only in the uncommitted
job tree, so it evaporated with it; carried here, the honesty of a published run
is recomputable from the run directory like everything else.

### Comparing two arms

```bash
python bench/evidence/compare_arms.py <control>/trials.jsonl <treatment>/trials.jsonl \
  --tasks 89 --markdown <run-id>/results.md
```

Pairs two runs by task and reports what changed: tasks gained and lost with an
exact McNemar test, wrong "this passed" calls fixed and introduced by name, and
spend per additional task passed. It refuses — non-zero exit, `"verdict":
"refused"`, numbers still printed — when the two files are not the two arms of
one experiment, or when the treatment arm declared a verification tier it never
exercised. See [`witness-ab/`](witness-ab/) for the experiment it was written
for and the decision rule it applies.

### Reading a descriptive total

`score.json`'s descriptive block reports every partial total beside its own
denominator — `usd_reported_by`, `tool_calls_reported_by`, `trials`. Cost and
tool calls come from Stella's accounting block, which a comparator agent does not
emit, so **absent and zero are different answers** and the block distinguishes
them. A `usd_total` whose `usd_reported_by` is below `trials` is a sum over part
of the run and must not be set beside a complete one.

## Two kinds of run, and never confusing them

**Development baseline** — `stella-tb21-dev-baseline-manifest-v2`. Self-reported.
Runs on whatever host is available, with ambient credentials, through the plain
adapter path. Its manifest carries a `claim_eligibility` block that lists, in
full, every reason it is not a leaderboard row. It exists to be a held-out number
Stella can be measured against and improved against — the thing #830–#836 and
#876 both presuppose.

`v2` adds the `assurance` block (#1007): which rungs of the verification ladder
the run actually exercised, and — when a rung is off — why. A `v1` manifest has
no such block and cannot answer the question at all; every `v1` run was the
witness-off arm, which is a lower bound on the full ladder rather than a
measurement of it.

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
