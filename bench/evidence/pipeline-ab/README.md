# The two-build A/B: old path against plugin path

Stella verifies work through an installed plugin. A built-in staged pipeline
did that job until it was cut from the tree.

One bar was set for the cut, in `doc:pipeline-as-plugins` §7. Run the two paths
side by side. Show the new one is not worse.

That run has not happened. This directory holds the plan for it.

## The two arms

| arm | build | how it is run | what it is |
|---|---|---|---|
| control | `f4c24c12b` | `stella run --pipeline classic` | the built-in staged pipeline |
| treatment | a build of `main` | `stella run --pipeline witness-v1` | `plugins/stella-witness` over the wrapper socket |

`f4c24c12b` is the parent of `a6d3db4f6`, the commit that deleted the built-in
path. It is the last tree where that path can be built and run.

**The build is the arm.** A flag can be dropped. A posture can claim a tier it
never fired. A build cannot be talked into being the other build, and each
trial records the commit its binary came from.

## What this run can and cannot claim

The two arms are hundreds of commits apart. Everything in that range moved with
the path under test: the tools, the prompt, the model catalog, the step loop.

So the report may say **this arm scored better or worse than that arm**. It may
not say **the move to plugins caused it**. `compare_arms.py` stamps that line on
every two-build report, and there is no way to get a report without it.

## What is fixed before any money is spent

All of it is in `preregistration.json`. The short form:

- 89 tasks, one list file, read by both arms. A task with no trial scores zero
  and stays in the count.
- Three replicates. Each one is a fresh pair of arms, one attempt per task.
  All three get reported. Picking the best is forbidden.
- No budget cap and no token cap. A cap measures the cap.
- A stop rule keyed to spend, never to a score. If the first replicate costs
  more than 600 USD, stop and report that replicate alone.
- The seed, the draw count and the decision rule, taken from `compare_arms.py`.

## What blocks the run today

One thing, and it is tree-only. `#6195` has it.

**The control arm has no binary and the run scripts hold one arm.** Its crate
is gone from this workspace, so its build comes from a checkout of
`f4c24c12b`, with that commit's own lock file and toolchain pin.
`build_sut.sh` refuses any difference between the working tree and the commit
it is handed, so it has to run from that checkout rather than from `main` with
the commit as an argument. `env.sh` then gives both arms one `STELLA_BINARY`
path and one `$TB_ROOT`, so the second build overwrites the first. The same
variable puts `$TB_REPO/bench/harbor_adapter` on `PYTHONPATH`, so aiming
`TB_REPO` at the old checkout swaps in the old adapter too, and only the
binary may differ between the arms.

## Two blockers that have shipped

The adapter could not pick a path: `loop_argv` in
`bench/harbor_adapter/stella_harbor/loop_mode.py` emitted no `--pipeline` flag
in any case (`#6178`). It emits `--pipeline <id>` now, from `STELLA_PIPELINE`.

The evidence could not say which path ran: `score_dev_baseline.py` dropped
`stella_loop_mode` off the trial manifest, so `trials.jsonl` carried no field
that answered it (`#6179`). It writes that field as `loop_mode` now, and
`--treatment-fired` below reads it.

## The analysis

`compare_arms.py` refuses two builds by default. That default is right: a split
nobody declared is the cheapest way to get a number that looks fine and means
nothing.

Declaring the split turns the refusal into checks:

```bash
python3 bench/evidence/compare_arms.py \
  bench/evidence/pipeline-ab-<date>/control/trials.jsonl \
  bench/evidence/pipeline-ab-<date>/treatment/trials.jsonl \
  --tasks 89 \
  --cross-sut <control-sha>:<treatment-sha> \
  --treatment-fired loop_mode=PLUGIN:witness-v1 \
  --markdown bench/evidence/pipeline-ab-<date>/results.md \
  > bench/evidence/pipeline-ab-<date>/comparison.json
```

Each arm must report the commit it was declared on. The two binary hashes must
differ. Every treatment trial must carry `loop_mode=PLUGIN:witness-v1`, and no
control trial may. Any of those fails and the run is refused, with the numbers
still printed so a reader can check the refusal.

The control arm's own `loop_mode` reads `PLUGIN:classic`. The adapter spells
every `--pipeline <id>` arm `PLUGIN:<id>` and does not special-case `classic`,
because which ids resolve to what is Stella's decision
(`PipelineChoice::resolve`) and an adapter-side copy of it would be a second
place to get it wrong. On the control arm's binary, `classic` is the built-in
staged pipeline rather than a plugin. Read the string as the id the command
line carried. The arm is the build.

## What counts as done

Three answers close the bar, and all three are real answers:

- the plugin path is at least as good — the bar closes with "not worse";
- the plugin path is worse — the bar closes with the loss written out;
- the run cannot tell — the bar closes saying so.

The defect was never a bad result. It was no result.
