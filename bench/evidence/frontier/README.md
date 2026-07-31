# Frontier-Bench (dev-baseline lane)

Runs Stella on [Frontier-Bench](https://github.com/harbor-framework/frontier-bench)
— Harbor's successor to Terminal-Bench, 74 tasks across seven domains, where the
best published agents clear roughly a third.

**The adapter is unchanged.** `stella_harbor:StellaAgent` runs this benchmark
exactly as it runs Terminal-Bench 2.1, with no edit to anything under
`bench/harbor_adapter/`. That was a design constraint, not a happy accident: the
adapter's Python tree is digest-frozen for the audited claim path, so a new
benchmark has to earn its place without perturbing it. Everything specific to
Frontier-Bench lives in this directory.

## Run it

```bash
export TB_REPO=/path/to/stella TB_ROOT=/abs/scratch OPENROUTER_API_KEY=...
bench/evidence/run/build_sut.sh              # shared with the Terminal-Bench lane
bench/evidence/frontier/setup_venv.sh        # this lane's Harbor pin
bench/evidence/frontier/fetch_dataset.sh     # pinned dataset + resource plan
bench/evidence/frontier/warm_images.sh       # pull the base images the builds need
bench/evidence/frontier/sentinel.sh          # two cheap trials, end to end
bench/evidence/frontier/primary.sh small fb-small-01
```

## What differs from Terminal-Bench, and why each difference exists

**A second Harbor pin (0.20.0), not the audited 0.6.1.** Every one of the 74
tasks declares `environment_mode = "separate"` under `[verifier]`. Harbor 0.6.1
has no such field, and pydantic's default `extra="ignore"` drops it *silently* —
0.6.1 runs the whole set with the verifier sharing the agent's container instead
of the separate one the task asked for, produces rewards, and warns about
nothing. Plausible numbers answering the wrong question are worse than an error.
This lane gets its own venv; `bench/harbor_adapter/.venv` stays on 0.6.1 so
Terminal-Bench reruns keep measuring what they measured.

**Base images are warmed, not task images pulled.** No Frontier-Bench task has a
`docker_image`; all 74 ship an `environment/Dockerfile` (12 also a compose file)
and build in place. `warm_images.sh` therefore pulls the `FROM` bases that
`plan.py` collects. The reasoning is the Terminal-Bench prepull's, unchanged:
with `--max-retries 0` a registry hiccup is a permanent reward-0 row.

**Resource tiers, not an A/B split.** Terminal-Bench's one-line rule — over 4 GB
or one CPU goes serial — puts 58 of these 74 in the serial phase. Frontier-Bench
declares 1–16 CPUs, 2–32 GB memory, and 4 GB–1 TB storage. `plan.py` tiers tasks
by footprint and gives each tier the concurrency the host can actually support.

**GPU tasks are excluded by default and named.** Four tasks (`exam-pdf-eval`,
`fp8-rmsnorm-gemm`, `jax-speedrun-gpu`, `math-eval-grader`) declare `gpus = 1`.
On a GPU-less host they do not error — the container starts, the work is
impossible, the verifier returns 0.0, and the row is arithmetically identical to
Stella genuinely failing. Silently that is ~5.4 points of pass rate. They are
excluded unless `FB_ALLOW_GPU=1` *and* Docker exposes an nvidia runtime, and the
plan prints every exclusion with its reason.

**A higher budget default ($2.50/trial).** Declared agent timeouts run 30 minutes
to 8 hours against expert time estimates measured in days. Terminal-Bench's
$0.60 cap would truncate most of this set and record the truncation as failure.
`STELLA_BUDGET=` (explicitly empty) means no cap.

## The sentinel's two gates

Stage 1 runs the synthetic fixture and demands reward 1.0 — it is
oracle-solvable, so anything less is a broken harness. Stage 2 runs the cheapest
real task and demands only that the trial *ran*: binary verified in-container, a
status reported, a reward produced, no infrastructure exception. Requiring 1.0
there would gate on model quality instead of plumbing; a real task that ran and
scored 0.0 passes stage 2, correctly.

## Known local limits

`plan.py` reports what the host can take. On a 10 GB / 6-CPU Docker VM (a
typical Mac) it admits **48 of 74** tasks — 4 GPU-excluded and 22 over the memory
budget. That is a real constraint, not a bug, and it is why a submittable run
needs a bigger machine: see [SUBMISSION.md](SUBMISSION.md).

The memory exclusion is deliberately conservative. `memory_mb` is a cap Docker
accepts even when the VM is smaller, so an 8 GB task on a 10 GB daemon usually
starts — and then swaps and gets OOM-killed partway in, arriving as a reward-0
row that looks exactly like a genuine failure. Lower `FB_MEMORY_HEADROOM_MB`
(default 2048) to attempt them anyway; every exclusion is named with its reason,
so nothing disappears quietly either way.

## Two Harbor majors means the CLI is a moving contract

`--agent-import-path` is 0.6.1 spelling; 0.20.0 folds it into `--agent`, which
takes either a built-in name or an import path. This lane uses `--agent`, the
Terminal-Bench lane keeps the old flag, and both are correct for their pin. The
preflight's `fb_assert_cli_flags` asserts every flag these scripts pass is still
advertised, so the next rename costs one message rather than a run — it would
otherwise surface as `No such option` at the first trial, after the venv build,
dataset download, image warm and preflight had all passed.

The trial result schema, by contrast, is unchanged: `task_name`,
`agent_result`, `verifier_result.rewards`, and `exception_info` are identical in
both versions, so the sentinel's gates read the same fields either way.

## Harbor version and the test suite

`bench/harbor_adapter/tests/` passes 132/132 under 0.6.1 and 131/132 under
0.20.0. The single difference is
`test_hashes_exact_uploaded_binary_and_records_source_commit`, which asserts
`agent._harbor_version_value == "0.6.1"` — the Terminal-Bench claim's audited
constant, correctly failing when a different Harbor is installed. CI runs the
suite in the 0.6.1 venv and stays green. Nothing in the adapter needed changing:
every import resolves, the `name`/`install`/`run` interface is unchanged, and
`setup_venv.sh` re-proves that on every venv build.
