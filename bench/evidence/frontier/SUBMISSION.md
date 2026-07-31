# Getting Stella onto the Frontier-Bench leaderboard

What the leaderboard requires, what this lane already satisfies, and the two
things that genuinely block a submission today. Researched 2026-07-31 against
the frontier-bench repo, its pending leaderboard PR, and the live pipelines on
the sibling benchmarks; re-check before acting, because the intake is still
moving.

## The intake is not open yet

Frontier-Bench has no merged self-serve submission pipeline as of 2026-07-31.
The tooling exists as [PR #1405](https://github.com/harbor-framework/frontier-bench/pull/1405)
("Add leaderboard package and PR-submit CI for Harbor Hub") and has only been
exercised end-to-end on a fork. Two "Leaderboard Submission" PRs against
upstream (#1408 Claude Fable 5, #1419 Kimi K3) were both closed by their own
author as opened against the wrong repo during that fork test. The identical
pipeline is already live for `harbor-index` and `terminal-bench-2-1`, so the
mechanics below are stable in shape even though the door is shut.

**Consequence for us:** the run can be produced now; the PR cannot be filed
until #1405 merges. Producing the run first is still the right order — it is the
long pole by a wide margin.

## What a submission is

Two parts. A public job upload, then a one-file PR.

```bash
harbor run -d frontier-bench/frontier-bench --agent <agent> -m <provider/model> --upload --public
cd leaderboard && uv run lb submit https://hub.harborframework.com/jobs/<uuid> [...]
```

`lb submit` writes one JSON per unique (agent, agent version, model, reasoning
effort) into `leaderboard/submissions/` and opens one PR per file — CI requires
exactly one added file. The PR carries only job links and metadata; CI re-derives
every trial from the uploaded job, so trajectories must be public. Verification
is audit-based, not re-execution: static analysis, then promotion into
leaderboard-owned copies, then an LLM judge that reviews *every* trajectory for
reward hacking, then merge.

## Requirements, and where this lane stands

| Requirement | Status |
|---|---|
| Dataset pinned to the leaderboard's `DATASET_REF` | **Done.** `env.sh` pins `sha256:97fd2ba3…`, the leaderboard's own ref |
| Harbor ≥ 0.20.0 | **Done.** This lane pins 0.20.0 |
| Default execution settings; no timeout or resource overrides | **Done.** Nothing here overrides either; tiering only chooses what runs *concurrently* |
| All 74 tasks, no subsetting | **Blocked by hardware** — see below |
| ≥ 5 trials per task | **Supported, off by default.** `FB_ATTEMPTS=5`; default 1 is the dev baseline |
| `--upload --public` | **Not wired.** Deliberate: uploading publishes trajectories, which is a decision to take explicitly, not a flag to inherit |
| Agent + model disclosure metadata | Supplied at `lb submit` time, not by this lane |

Note the metric: **accuracy = trials with reward > 0, over all trials**. Not
reward == 1.0, and errored trials count as reward 0 rather than being dropped.
Infrastructure flakiness therefore lands directly in the score, which is exactly
why `warm_images.sh` and the preflight exist.

## The two real blockers

**1. GPUs.** Four tasks need one, and the benchmark authors ran them on a single
H100 each. Subsetting is not allowed for a submission, so those four cannot be
excluded the way the dev baseline excludes them — they must actually run.

**2. Host size.** On a 10 GB / 6-CPU Docker VM, `plan.py` admits 48 of 74 tasks;
22 more are simply over the memory budget. The full set declares up to 32 GB
memory, 16 CPUs, and ~1.9 TB of storage in aggregate.

Both point the same way: **a submission run does not happen on a Mac.** The
benchmark's own guidance is Modal (`uv tool install 'harbor[modal]'`, `--env
modal`), with Daytona as an alternative that recently added GPU support. Harbor's
repo CI defaults to `env: modal`.

## Cost, honestly

74 tasks × 5 trials = 370 trials, against declared agent timeouts averaging
around two hours. At this lane's $2.50/trial default that is roughly $925 in
model spend before Modal compute, and the budget cap would truncate the longer
tasks — for a leaderboard run you would want `STELLA_BUDGET=` (uncapped), which
removes the ceiling on that estimate. This is a four-figure decision, not a
weekend experiment, and it should be preregistered like the Terminal-Bench claim
was.

## Suggested order

1. Land this lane; run the sentinel locally to prove the plumbing on the new
   Harbor pin. Cheap.
2. Run the dev baseline on the 48 locally-runnable tasks at one attempt. Cheap
   enough to be worth it, and it tells you where Stella actually lands before
   you spend four figures.
3. Only if that number justifies it: preregister, move to Modal with GPU access,
   run all 74 at five attempts uncapped with `--upload --public`.
4. File the PR once #1405 merges.

Steps 1 and 2 are what this directory is for. Step 3 needs a Modal environment
this lane does not yet configure — `primary.sh` hardcodes `--env docker`, and
adding a Modal path is the next piece of work, not something already done.
