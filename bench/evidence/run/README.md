# Running a development baseline

The scripts beside this file are the exact ones that produced the run in
`bench/evidence/<run-id>/`. They are committed because #909's acceptance is that
the score be reproducible, and a number whose runner lives in someone's scratch
directory is not.

This is the **development-baseline** path. It is not the audited public claim —
that one goes through `secure_launcher.py` on a dedicated native Linux host with
an intent ledger and host attestations, and is documented in
[`../../RUNBOOK.md`](../../RUNBOOK.md). Read [`../README.md`](../README.md) for
the line between the two before publishing anything from here.

## Prerequisites

* Docker, with `linux/amd64` able to run (native, or Rosetta/qemu emulation).
* `uv`, `zig`, `cargo-zigbuild`, `rustup`.
* `OPENROUTER_API_KEY` in the environment.
* The adapter venv: `uv sync --project bench/harbor_adapter --locked --extra dev`.
  This installs **Harbor 0.6.1**, which is an audited constant, not an upgrade
  candidate — see the comment on the pin in `bench/harbor_adapter/pyproject.toml`.

## Order

```bash
export TB_ROOT=/absolute/path/for/run/scratch      # dataset, jobs, logs
export TB_REPO="$(git rev-parse --show-toplevel)"

# 1. Freeze and build the SUT. Refuses if anything that feeds the compiler
#    differs from origin/main, so the binary's provenance is checkable.
bench/evidence/run/build_sut.sh

# 2. Fetch the pinned 89-task dataset and classify tasks by resource class.
bench/evidence/run/fetch_dataset.sh

# 3. Pre-pull every task image, with retries. NOT optional — see below.
bench/evidence/run/prepull.sh

# 4. One paid synthetic trial that must return reward 1.0 before 89 depend on
#    the same path.
bench/evidence/run/sentinel.sh

# 5. The measured run, in two phases (see below). Publish the preregistration
#    BEFORE this step, not after.
bench/evidence/run/primary.sh B "<job-name>-phaseB"
bench/evidence/run/primary.sh A "<job-name>-phaseA"

# 6. Score and freeze the evidence.
bench/evidence/run/finalize.sh "<run-id>" "<job-name>"
```

## Why the pre-pull is not optional

The first attempt at this run lost four trials in its opening minutes to
`TLS handshake timeout` while Docker Hub served image blobs. The container never
started, so the agent never ran and spend was `$0.0000` — but with
`--max-retries 0` each is a permanent reward-`0` row, and under a fixed
denominator that is arithmetically indistinguishable from Stella failing the
task. Registry flakiness must not be able to enter the score. Pull first, with
retries, and treat image availability as a precondition.

The full set is ~18.5 GiB. On a slow connection this dominates the schedule;
`prepull.sh` reports progress and never silently skips a failure.

## Why two phases

Concurrency is split by what each task's `task.toml` asks for, because a VM with
less memory than the largest task cannot honestly run it alongside anything else:

* **Phase A** — tasks wanting ≤4 GiB and 1 CPU, at `n_concurrent=3`.
* **Phase B** — tasks wanting >4 GiB or >1 CPU, at `n_concurrent=1`.

The union is every task, one attempt each, scored into one number over one
denominator. No task is filtered, dropped or re-run. On a host with enough memory
for the largest task plus its neighbours, run a single phase instead — the split
is a host limitation, not part of the measurement.

Run phase B first if images are still downloading: it is the smaller set and
leaves bandwidth for the rest.

## Rules the run must follow

* **Preregister before spending.** SUT, dataset digest, model, sampling and the
  whole analysis plan, published in advance and not edited after data arrives.
* **Fixed denominator.** Every task counts. Errors, timeouts, budget-cap hits and
  missing rows all score 0 and stay in.
* **No outcome-selected anything.** A bad-looking early score is not a reason to
  stop, change a parameter, or start over. Operational aborts (registry, VM,
  balance, credentials) are allowed, must be disclosed, and must relaunch under a
  **new job name** — never a resume.
* **Publish failures.** The per-task table lists every task, including the ones
  that failed for infrastructure reasons.

## Known: every trial burns its full timeout

Until [#960](https://github.com/macanderson/stella/issues/960) is fixed, Stella's
headless process does not exit after completing its turn, so Harbor kills it at
the agent timeout and records `AgentTimeoutError` even on trials that scored
`1.0`. Budget accordingly: wall clock is the sum of the task timeouts, not the
sum of the work. Scores are unaffected — the verifier runs after the agent phase
against files already on disk.
