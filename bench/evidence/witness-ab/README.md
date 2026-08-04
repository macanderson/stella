# The authored-witness A/B

Does an independent checker earn its cost? ([#1284](https://github.com/macanderson/stella/issues/1284))

Stella decides whether finished work is correct on a ladder. Two of its rungs
are model calls, and they are not equally strong:

| rung | what it is | why it is stronger or weaker |
|---|---|---|
| **authored witness** | a *second* model writes a test that must fail on the old code and pass on the new | the model that did the work does not write its own exam |
| **model judge** | a model reads the work and gives an opinion | the same model that did the work can grade it, and does |

**Every Terminal-Bench number Stella has published ran with the authored
witness structurally off.** Not by choice: the benchmark posture pinned one
model for every role, Stella refuses to let a worker author the test that
verifies it, and so each trial logged `continuing without an authored witness:
no author independent of the worker` and carried on with the judge alone. Those
numbers are a lower bound on the ladder, not a measurement of it
([`../../READINESS.md`](../../READINESS.md) §9).

Two things used to block turning it on, and both are fixed:

* [#1007](https://github.com/macanderson/stella/issues/1007) added
  `STELLA_WITNESS_AUTHOR_MODEL`, which names a second model on the worker's
  provider and reaches Stella only as `pipeline_judge_model` inside the hashed
  posture;
* [#1225](https://github.com/macanderson/stella/issues/1225) gave each task
  folder a git baseline, so a witness has something to be diffed against.

This directory holds the protocol, the preregistered analysis plan and the
decision rule for the run that answers the question. **The run itself has not
happened yet** — it needs a spend-capable credential and a host that can run
89 Docker task images, neither of which any amount of code in this repository
can supply. Results land in a sibling `witness-ab-<YYYYMMDD>/` directory and
this file gains a row pointing at them.

## What gets measured

Two runs, same SUT, same task set, same everything except one variable:

| arm | `STELLA_WITNESS_AUTHOR_MODEL` | authored witness | model judge |
|---|---|---|---|
| control | unset | **off** — no author independent of the worker | on, *same model as the worker* |
| treatment | a second model on the worker's provider | on | on, independent of the worker |

The arm changes `stella_engine_posture_sha256`, and therefore the registered
configuration. That is intended: two arms that hash the same are one arm run
twice.

### Outcomes, fixed before the first paid call

1. **Primary — tasks passed.** Paired by task over the preregistered
   denominator. Reported as gained / lost / both / neither, with an exact
   McNemar test over the discordant pairs and a percentile bootstrap interval
   on the paired difference (seed `20260803`, 50,000 draws).
2. **Secondary — wrong "this passed" calls.** A trial where Stella's own
   verdict said the work was done and the benchmark's external grader scored it
   zero. This is what the rung exists to remove, and it is invisible to the
   score: a task failed while claiming success costs the same reward as a task
   failed honestly, but only the first one lies to whoever ships it. Counted
   overall and restricted to verdicts a *model* opined
   (`self_verdict_deterministic: false`) rather than ones the flip oracle
   decided.
3. **Cost.** Total spend, the treatment/control ratio, and spend per additional
   task passed. Wall clock is reported but is not a time measurement while
   [#960](https://github.com/macanderson/stella/issues/960) is open — Stella's
   headless process does not exit after its turn, so Harbor kills it at the
   agent timeout and every such trial burns the full timeout regardless of how
   long the work took.

### The decision rule

Encoded in [`../compare_arms.py`](../compare_arms.py) (`decide()`), applied
mechanically, and fixed here before any data exists:

* **Reject** if the treatment arm loses more tasks than it gains at p < 0.05,
  **or** if wrong "this passed" calls increase. Either makes the rung worse
  than what already ships.
* **Adopt** — the witness arm becomes the default for benchmark runs — if it
  gains more tasks than it loses, that gain is either significant at p < 0.05
  **or** accompanied by fewer wrong "passed" calls, **and** it costs at most
  **1.5×** the control arm.
* **Inconclusive** otherwise. Which changes nothing, and says so.

Nothing above is chosen after the data arrives. A threshold moved once a number
is in hand is not a decision rule, it is a preference with arithmetic attached.

### Two refusals, before any of the above is computed

`compare_arms.py` exits non-zero, and reports `"verdict": "refused"`, when:

* **the arms are not one experiment** — a trial that does not record its
  assurance arm (pre-#1007 evidence), an arm whose trials disagree about which
  arm they ran, or two arms that ran different SUT commits or binaries;
* **the treatment arm authored no witness on any task.** A posture that
  *declares* the rung is not a run that *exercised* it. This is
  [#1147](https://github.com/macanderson/stella/issues/1147) exactly: an author
  Stella's offline seed catalog did not carry failed model validation, the
  judge pin was dropped, and the control arm executed under a treatment-arm
  digest. The run's own proof stream is the only thing that can answer it, and
  every trial now carries that answer as a field.

### Choosing the author

Two hard constraints, both enforced fail-closed by `_validated_witness_author`:
the author must differ from the worker (otherwise the tier is off with a hash
saying otherwise), and it must sit on the **worker's provider** — a trial
carries exactly one provider credential over the anonymous FD, resolved from
the worker's provider, so a cross-provider author authenticates against
nothing.

One soft constraint, which is where the money is. `openrouter` is deliberately
an *unseeded* provider (`crates/stella-cli/src/config/providers.rs`): it fronts
hundreds of `vendor/model` slugs that change weekly, so slug validation there
is permissive by design and a typo is not caught before the call. What the
seed catalog does carry for the `openrouter` route is the row that supplies a
model's context window, output ceiling and prices —
`moonshotai/kimi-k3`, `anthropic/claude-sonnet-5`, `anthropic/claude-fable-5`,
`anthropic/claude-haiku-4.5` at the time of writing
(`crates/stella-model/src/catalog.rs`). An author without such a row runs against the
engine's global defaults and reports spend only from the gateway's own usage
accounting, which is a worse position to measure a cost question from. Prefer
an author the seed carries; if the experiment needs one it does not, seed the
row first.

## Running it

```bash
export TB_REPO="$(git rev-parse --show-toplevel)"
export TB_ROOT=/absolute/path/for/run/scratch

# Steps 1–4 of ../run/README.md first: build_sut.sh, fetch_dataset.sh,
# prepull.sh, sentinel.sh. Then fix the task list ONCE — both arms read it:
python3 -c "import json;p=json.load(open('$TB_ROOT/phases.json'));print('\n'.join(sorted(p['phaseA']+p['phaseB'])))" \
  > "$TB_ROOT/witness_ab.tasks"

# The author must be a second model on the worker's provider, and one the
# offline seed catalog carries. The script refuses an unusable one on the host,
# before a container exists.
export STELLA_WITNESS_AUTHOR_MODEL=openrouter/deepseek/deepseek-v4-pro

# Run the sentinel once on the treatment arm too. It costs one trial and is the
# cheapest possible check that the rung fires at all: its report line carries
# `arm=witness-on witness=authored`. `witness=not_reported` there can still be
# legitimate (the warrant may decide this task needs no new test), but
# `arm=witness-off` on a run launched with the author exported is #1147 and
# must be resolved before spending on 89 tasks.
bench/evidence/run/sentinel.sh sentinel-witness-on

bench/evidence/run/witness_ab.sh off wab1-off
bench/evidence/run/witness_ab.sh on  wab1-on

# Each arm becomes its own evidence directory, scored on its own terms. The
# `env -u` is load-bearing: finalize.sh reads the author from the environment,
# and the control arm's manifest must not name one it did not run with.
# (make_manifest.py refuses that mismatch rather than recording it, but a
# refusal at the end of a paid run is a worse place to learn it than here.)
env -u STELLA_WITNESS_AUTHOR_MODEL \
  bench/evidence/run/finalize.sh witness-ab-20260803/control wab1-off
bench/evidence/run/finalize.sh witness-ab-20260803/treatment wab1-on

# And then the comparison, which is the answer.
python3 bench/evidence/compare_arms.py \
  bench/evidence/witness-ab-20260803/control/trials.jsonl \
  bench/evidence/witness-ab-20260803/treatment/trials.jsonl \
  --tasks 89 --markdown bench/evidence/witness-ab-20260803/results.md \
  > bench/evidence/witness-ab-20260803/comparison.json
```

Order matters in one direction only: run the **control** arm first if anything
is still downloading, so a partially warm cache cannot advantage the arm under
test. Neither arm may be re-run after seeing its score, and an operational
abort relaunches under a new job name rather than resuming — the rules in
[`../README.md`](../README.md) apply here unchanged.

## Cost of the run

89 tasks × 2 arms, one attempt each. The published `tb21-hh10-20260731` run
spent **$88.29** over 89 Stella trials on `glm-5.2` with no per-trial cap, so
the control arm is that order of magnitude and the treatment arm is that plus
whatever the second model's authoring costs — which is the number this
experiment exists to measure rather than predict. `STELLA_BUDGET` defaults to
`0.60` per trial in `../run/env.sh`; a capped run measures a different agent, so
decide the cap once and use the same one in both arms.

## Why the tooling changed for this

The comparison needs one field per trial that the committed evidence did not
carry: what Stella itself claimed about the work the external grader then
scored. It existed only as a `judge_verdict` event inside the multi-gigabyte
Harbor job tree, which is never committed — so the agreement rate and the
false-pass count were computable only while the run was still on the disk that
produced it, and evaporated with it. The adapter now folds that verdict into
trial metadata beside the witness observations it already recorded (#1007), and
`score_dev_baseline.py extract` carries both into `trials.jsonl`. The evidence
for this experiment is therefore reproducible from the run directory alone,
which is the promise [`../README.md`](../README.md) makes about every number in
here.
