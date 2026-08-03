# Task: fix Stella completing turns prematurely on z-ai/glm-5.2

You are working in the `stella` repo (`~/Projects/stella`). There is a
reproducible defect where Stella ends a turn reporting success after doing
little or no work. It shows up sharply on `openrouter/z-ai/glm-5.2` but the
completion path you will be looking at is model-independent, so do not assume
it is only a glm problem until you have checked.

## Evidence

Unzip `~/Desktop/stella-vs-claudecode-traces-2026-07-31.zip`. It contains 86
Terminal-Bench 2.1 trials captured on 2026-07-31 — real runs, not synthetic.
Start with `bundle/analysis/REPORT.md` and `bundle/analysis/all_trials.csv`.

The per-trial artifacts you want are:

```
bundle/*/jobs/<job>/<task>__<id>/agent/stella-events.jsonl   <- Stella's own event stream
bundle/*/jobs/<job>/<task>__<id>/agent/trajectory.json       <- ATIF steps + final_metrics
bundle/*/jobs/<job>/<task>__<id>/agent/stella-run.stdout.txt
bundle/*/jobs/<job>/<task>__<id>/result.json                 <- Harbor's verdict (ground truth)
```

Eleven trials exhibit the defect. Best single example:

```
bundle/live-run-hh8/jobs/hh8-armA-stella/regex-log__AYyurLy/
```

Others: `polyglot-rust-c__iZZZ4jE`, `distribution-search__i3gGWop`,
`write-compressor__r2sneBP`, `circuit-fibsqrt__eTMXYUd` (search the bundle by
task name; several appear in more than one run).

## What the traces already establish

Every one of the eleven follows an identical path:

```
stage triage   -> usage_incomplete {role: triage, reason: "timeout", duration_ms: ~10001}
stage execute  -> 0 to 2 tool calls
stage verify   -> proof {kind: warrant, required: true, diff_lines: 0}
               -> proof {kind: verification_unavailable, reason: "UNVERIFIABLE ...
                         file-change events recorded = 0"}
judge_verdict  -> {"passed": true}          <-- the core defect
stage complete -> turn ends reporting success
```

`regex-log` produced **123 `reasoning` events and zero tool calls** before
completing. The model thought hard and then acted on nothing.

Two distinct problems, and they are independent:

**A. The judge passes an unverifiable, zero-work turn.** The ladder correctly
emits `UNVERIFIABLE` — it names every dead channel: "flip oracle not armed (no
test command); touched tests not run; the diff probe could not read the working
tree; file-change events recorded = 0". Then `judge_verdict` returns
`passed: true`. A turn with `diff_lines: 0`, zero `FileChange` events, and no
verification channel should not be able to report success. This is the abstain
rung failing open where it should fail closed. **This is the primary bug.**

**B. Triage times out at ~10s on glm-5.2.** 9 of the 11 show
`usage_incomplete {role: triage, reason: timeout, duration_ms: ~10001}`.

### A confound on (B) that you must account for

These runs used a **patched** benchmark posture. `bench/harbor_adapter/stella_harbor/posture.py`
on `main` sets `"triage": {"effort": "low", "reasoning": "off"}` with a comment
stating that is deliberate. For this head-to-head it was patched to
`{"effort": "max", "reasoning": "on"}` for all four roles. The exact diff is at
`bundle/live-run-hh8/hh8_harness.diff` and `bundle/analysis/live_harness_patches.diff`.

So the triage timeout may be an artifact of running triage at max effort rather
than a standing defect. **Check whether it reproduces with the committed
low/off posture before treating (B) as a product bug.** Note that (A) is
unaffected by this — a passed verdict on a zero-work turn is wrong under any
posture.

## What to do

1. Reproduce (A) locally. You do not need Terminal-Bench or an EC2 box — you
   need a turn where the working tree is unreadable / no tests run / zero file
   changes, and then to observe what the judge returns. A unit or property test
   at the verification-ladder seam is the right level.
2. Find where `UNVERIFIABLE` is converted into `judge_verdict.passed`. Decide
   what the correct behaviour is — abstain is not success, and it is also not
   necessarily failure. Whatever you choose, a turn that changed nothing must
   not report completion as though it had.
3. Determine whether (B) reproduces under the committed `low`/`off` triage
   posture. If it does, fix the timeout or make triage failure non-silent. If
   it does not, say so and leave the posture alone.
4. Add a regression test for (A) that would have caught this. There is an
   existing verification-ladder test module — extend it rather than starting a
   new one.

## Constraints

- Do not change the benchmark posture or the harbor adapter to make a number
  look better. They are frozen measurement artifacts.
- `rg` / `fd`, not `grep` / `find`.
- Check gates by exit code, not by grepping output — this repo forces ANSI
  colour into piped output and `rg "^error"` misses real failures.
- No Claude attribution in commits, tags, or PR text.
- Work in a git worktree; open a draft PR when the fix is green.

## Deliverable

A PR fixing (A) with a regression test, plus a clear written verdict on (B):
reproduces under committed posture, or was an artifact of the max-effort patch.
If you conclude the judge behaviour is intentional, say so and explain what a
zero-work turn is supposed to report instead — but the eleven trials in the
bundle are Stella declaring success on tasks it did not touch, and Harbor scored
every one of them 0.0, so something in that chain is wrong.
