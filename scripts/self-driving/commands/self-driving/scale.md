---
description: The resource governor — how a cycle sizes itself to the disk, memory and compute actually available, and learns what this machine survives.
argument-hint: "[--explain]"
---

# self-driving:scale — sizing the cycle to the machine

```bash
scripts/self-driving.sh plan --explain     # what this cycle would do, and why
scripts/self-driving.sh calibrate --show   # what the controller has learned
```

The same `/self-driving` command has to be correct on a 16 GiB laptop on battery
with a benchmark match running, and on a 123 GiB / 32-core rig doing nothing.
Nobody is going to keep a threshold file current across both. So the loop
measures instead.

---

## Supply × demand × calibration

```
supply         what the box has RIGHT NOW
               cores · load · total & free memory · free disk · battery · contention

demand         what the work actually needs
               open defect count · how many are P0 · is main red

calibration    what previous cycles PROVED this box survives
               batch_ceiling · parallel_ceiling · clean-run streak
                              ↓
                        tier + concrete knobs
```

## The tier ladder

Each rung states its reason, because a governor whose decisions cannot be
explained gets overridden and then ignored.

| Trigger | Tier | What changes |
|---|---|---|
| free disk < 15 GB | `light` | **no local build** — a workspace target dir is 10–20 GB; starting a build here ends as a killed compiler |
| a build or match owns the box | `light` | **no local build**, serial — a concurrent cargo run corrupts a measured match |
| free memory < 4 GB | `light` | **no local build**, serial — this swap-thrashes |
| on battery | `light` | serial, no bench — shed compute, keep the cycle useful |
| load ≥ cores | `light` | serial, no bench — the box is already saturated, and a bench timed here is a number the loop cannot trust |
| ≥32 GB, ≥100 GB free, load < half the cores | `heavy` | full workspace scope, measured head-to-head |
| otherwise | `normal` | the calibrated ceiling |

**Demand overrides supply in exactly one direction.** An open P0 turns a
stand-down into a *minimal* cycle — batch shrunk to the P0 count, audit set to
fast — never a skipped one. Urgent work gets a smaller cycle, not no cycle.

An empty queue caps the batch regardless of what the box could afford. Capacity
is not a reason to do work that does not exist.

## The controller: AIMD

The ceilings are not configured. They are learned, and there is exactly one
place they move:

```
clean cycle        batch += 2      (and parallel += 1 every third clean cycle)
resource failure   batch /= 2      (and parallel drops straight back to 1)
```

Additive increase, multiplicative decrease — the shape that provably converges
instead of oscillating. Three properties make it the right choice here:

- **It recovers slowly and retreats fast.** A resource failure is not a hint, it
  is proof the last plan did not fit; halving is the honest response. Growth is
  gradual because the cost of overshooting is a dead cycle.
- **Parallelism earns its increase more slowly than batch size** — one extra
  worktree is the knob that hurts most when it is wrong.
- **It seeds optimistically**, at the hard maximum. A ceiling that starts low and
  climbs wastes a capable machine for a week; AIMD's decrease is fast enough that
  starting high costs at most one degraded cycle.

## What counts as a resource failure

This is the single most important distinction in the whole system, because the
controller acts on it and nothing else:

| Outcome | `--outcome` | Why |
|---|---|---|
| killed compiler, OOM, ENOSPC, thrash, aborted for load | `resource-fail` | the plan did not fit the box |
| **red gate, failing test, clippy error** | `ok` | the *batch* was wrong, not too big |
| benchmark regression | `ok` | a finding, not a capacity problem |
| cycle ran fine but fixed nothing | `ok` | a queue problem — `metrics` flags it separately |

Reporting a red gate as a resource failure teaches the controller to shrink away
from work it could handle perfectly well, and it never recovers because the
smaller batches keep going red for the same unrelated reason.

## Reading a plan

```
self-driving plan — tier light
  only 3GB free of 16GB — a workspace build here swap-thrashes

  supply    10 cores (load 2) · 16GB RAM (3GB free) · 346GB disk · battery=1 · busy=1
  demand    8 open defects (0 P0)
  ceilings  batch<=20 parallel<=2 (AIMD, 0 clean runs)

  batch     5 defects
  parallel  1 worktree(s)
  build     PUSH TO CI — do not compile here
  audit     deep (aperture: rubric)
  bench     off
```

`PUSH TO CI` is not advice. The cycle branches, pushes, and reads the gate from
CI instead of compiling locally. That is a *better* cycle on a constrained box,
not a degraded one — CI has more memory than the laptop and the result is the
one that gates the merge anyway.

## Contention: watchers are not work

The probe deliberately ignores `tail -f` on a match log and shells that merely
mention `cargo build`. Those outlive the run they were watching, and counting
them would pin the loop to the light tier **permanently** — a failure that is
silent, looks like caution, and never recovers.

It counts real workload processes and, separately, running Docker containers,
because on a Linux rig the benchmark drives Docker and nothing on the host names
it.

## Overrides

Every knob has an environment variable, and using one is a decision to be
explained in the cycle report:

| Variable | Default |
|---|---|
| `SELF_DRIVING_BATCH_MAX` | `20` |
| `SELF_DRIVING_PARALLEL_MAX` | `4` |
| `SELF_DRIVING_DISK_FLOOR_GB` | `15` |
| `SELF_DRIVING_MEM_FLOOR_GB` | `4` |

To reset a controller that has learned the wrong thing — after fixing whatever
was actually starving the box — delete `calibration.json` from the state dir. It
reseeds at the hard maximum and relearns within a few cycles.
