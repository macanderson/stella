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

## Running the two arms of an A/B instead

`witness_ab.sh` replaces step 5 when the run is a paired experiment rather than
a score — currently the authored-witness A/B (#1284), whose protocol,
preregistered analysis plan and decision rule are in
[`../witness-ab/`](../witness-ab/). It takes the arm as an argument rather than
reading it out of the ambient environment, pins the task list so both arms
cannot drift apart, and refuses an unusable witness author on the host before a
container exists:

```bash
export STELLA_WITNESS_AUTHOR_MODEL=openrouter/deepseek/deepseek-v4-pro
bench/evidence/run/witness_ab.sh off "<job>-off"
bench/evidence/run/witness_ab.sh on  "<job>-on"
```

Finalize each arm into its own evidence directory, then compare them with
`../compare_arms.py`. Finalize the control arm under
`env -u STELLA_WITNESS_AUTHOR_MODEL`: the author stays exported across both
arms, and a manifest that names one the arm did not run with is a mislabeled
arm (`make_manifest.py` refuses it rather than recording it).

## The SUT binary must be the portable build

`build_sut.sh` cross-compiles against `x86_64-unknown-linux-gnu.2.17` via
`cargo-zigbuild`. That glibc floor is what lets one binary run in every task
container. **Do not** substitute a plain host build:

```bash
# This is the mistake. It produces a host-glibc binary wearing the portable
# build's filename, in the exact path env.sh exports as STELLA_BINARY.
cargo build --release --locked -p stella-cli --bin stella
cp target/release/stella target/x86_64-unknown-linux-gnu/release/stella
```

On 2026-07-31 a run provisioned that way lost five trials before the agent ever
started. Every integrity check still passed, because they all answer a different
question: the host/container SHA-256 comparison confirms *the file arrived
intact* — and it did, it was the same file on both sides. Nothing answered *can
this file run here*. The first thing to touch the dynamic loader was
`stella --version`, inside the container, per trial:

```
/usr/local/bin/stella: /lib/x86_64-linux-gnu/libc.so.6:
    version `GLIBC_2.34' not found (required by /usr/local/bin/stella)
```

Harbor recorded `NonZeroAgentExitCodeError` and scored each trial 0.0 —
indistinguishable, under a fixed denominator, from Stella failing those tasks.

`preflight` now refuses to start a run whose `STELLA_BINARY` requires a glibc
symbol above the floor, on the host, before any container is created. The floor
and the target triple are exported once from `env.sh` and consumed by both
`build_sut.sh` and the check, so the build and the assertion cannot drift apart.
To inspect a binary directly:

```bash
python3 bench/harbor_adapter/stella_harbor/portability.py "$STELLA_BINARY" --json
```

### Is a musl build needed as well? No.

glibc 2.17 fixes containers with *older glibc*. It does nothing for a container
with no glibc at all, so the question had to be settled against the dataset
rather than assumed. Every base image across all 89 Terminal-Bench 2.1 tasks:

| base image | tasks | libc |
|---|---|---|
| `python:3.13-slim-bookworm` | 41 | glibc |
| `ubuntu:24.04` | 39 | glibc |
| `python:3.11-slim` | 2 | glibc |
| `python:3.10-slim-bookworm` | 2 | glibc |
| `debian:bullseye-slim` | 2 | glibc **2.31 — the oldest in the set** |
| `debian:13.0-slim` | 2 | glibc |
| `python:3.11` | 1 | glibc |

No task image is musl-based, so **no `x86_64-unknown-linux-musl` target is
added**. An unused second build path is a liability: it doubles what a release
has to prove and rots silently between the runs that would exercise it.

The name `qemu-alpine-ssh` is the trap here — it is one of the two
`debian:bullseye-slim` tasks. Alpine appears as an ISO the *agent* downloads and
boots inside QEMU; it is the guest OS, never the container Stella runs in. The
loader error confirms this independently: it names
`/lib/x86_64-linux-gnu/libc.so.6`, a path that exists only where glibc does.

Two independent methods agree on that container's glibc. Statically, its
Dockerfile says bullseye (glibc 2.31). At runtime, the loader reported
`GLIBC_2.32`, `2.33` and `2.34` missing while saying nothing about the
`GLIBC_2.18` … `2.30` the same binary also required — so it provides at least
2.30 and lacks 2.32, which is 2.31 exactly.

This answer is a property of the pinned dataset, not of Docker. If `TB_DATASET`
is ever repinned, re-run the tally over the new task set before trusting it:

```bash
rg --no-filename -N -i -m1 '^\s*FROM\s+(\S+)' -o -r '$1' \
  "$DATASET_DIR"/*/*/environment/Dockerfile | sort | uniq -c | sort -rn
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
