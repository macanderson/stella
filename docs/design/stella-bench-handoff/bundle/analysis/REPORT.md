# Terminal-Bench evidence bundle — Stella vs Claude Code

Trials captured: **86**. Model `glm-5.2`, effort `max`, thinking on, no budget cap. Stella runs over OpenRouter, Claude Code over z.ai; that endpoint split is the only intended difference.


## Runs in this bundle

| run | what it was | verdict |
|---|---|---|
| `hh8` | preregistered full 89-task run, Stella arm only (stopped at 18 scored) | partial; Claude arm never started (arms ran back-to-back) |
| `smoke1` | 2-task plumbing check | valid |
| `DISCARDED-dev1` | 20-task, both arms, concurrency 20/arm | **DISCARDED** — Docker address-pool exhaustion contaminated it |
| `dev2` | same, after the address-pool fix | **INCOMPLETE** — infrastructure was clean, but 35/40 trials were cancelled when the run was stopped mid-flight. Only the 5 non-cancelled trials carry a real verdict. |

## dev2 — paired head-to-head (PARTIAL: run stopped mid-flight) (n=20)

- **Stella 0/20**, **Claude Code 0/20**
- discordant: Stella-only **0** [], Claude-only **0** []
- NOTE: pass counts below undercount both arms — cancelled trials score as non-passes.
  Treat these as a *behavioural* sample, not a score.
- McNemar discordant pairs: b=0, c=0

| task | S res | S steps | S tools | C res | C steps | C tools | Stella failure class |
|---|---|---|---|---|---|---|---|
| break-filter-js-from-html | — | 12 | 11 | — | 16 | 7 | AGENT:CancelledError |
| build-cython-ext | — | 58 | 85 | — | 39 | 22 | AGENT:CancelledError |
| code-from-image | — | 20 | 18 | — | 18 | 2 | AGENT:CancelledError |
| constraints-scheduling | — | 7 | 6 | — | 10 | 5 | AGENT:CancelledError |
| distribution-search | — | 3 | 0 | — | 1 | 0 | AGENT:CancelledError |
| extract-elf | — | 5 | 5 | — | 28 | 13 | AGENT:CancelledError |
| fix-ocaml-gc | — | 10 | 15 | — | 34 | 20 | AGENT:CancelledError |
| gcode-to-text | — | 27 | 26 | — | 9 | 4 | AGENT:CancelledError |
| headless-terminal | — | 13 | 9 | — | 6 | 2 | AGENT:CancelledError |
| llm-inference-batching-scheduler | — | 9 | 8 | — | 12 | 5 | AGENT:CancelledError |
| make-mips-interpreter | — | 15 | 25 | — | 36 | 21 | AGENT:CancelledError |
| model-extraction-relu-logits | — | 4 | 2 | — | 1 | 0 | AGENT:CancelledError |
| polyglot-rust-c | fail | 2 | 0 | — | 1 | 0 |  |
| qemu-alpine-ssh | — | None | 0 | — | 21 | 7 | INFRA:glibc-binary-cannot-execute |
| qemu-startup | — | None | 0 | — | 25 | 8 | INFRA:glibc-binary-cannot-execute |
| regex-log | fail | 2 | 0 | — | 1 | 0 |  |
| schemelike-metacircular-eval | — | 7 | 4 | — | 7 | 2 | AGENT:CancelledError |
| tune-mjcf | — | 12 | 10 | — | 8 | 3 | AGENT:CancelledError |
| video-processing | — | 25 | 24 | — | 1 | 0 | AGENT:CancelledError |
| write-compressor | fail | 3 | 2 | — | 1 | 0 |  |

## Defects found

### 1. Stella binary cannot execute in some task containers (5 trials)

```
/usr/local/bin/stella: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.32' not found
```
The binary is dynamically linked against glibc 2.35 (Ubuntu 22.04 build host). Task images on older glibc cannot run it — it dies on `--version`, before the agent starts. **This scores as a task failure but is a packaging defect.** Claude Code is immune (Node app installed in-container), so it is also an unfair asymmetry in the comparison. Fix: build `x86_64-unknown-linux-musl` (static), or build on an older glibc.

Affected trials: ['qemu-alpine-ssh', 'qemu-startup']

### 2. Premature completion on glm-5.2 (12 trials at <=3 steps)

Stella ends the turn after 2–3 steps with the task untouched. Same model, same effort, same container as the comparator — so this is loop behaviour, not model capability. Reported independently by the user as a known glm-5.2-specific issue.

| run | task | steps | tool calls |
|---|---|---|---|
| DISCARDED-dev1 | distribution-search | 3 | 0 |
| DISCARDED-dev1 | model-extraction-relu-logits | 3 | 2 |
| DISCARDED-dev1 | polyglot-rust-c | 3 | 0 |
| DISCARDED-dev1 | regex-log | 2 | 0 |
| dev2 | distribution-search | 3 | 0 |
| dev2 | polyglot-rust-c | 2 | 0 |
| dev2 | regex-log | 2 | 0 |
| dev2 | write-compressor | 3 | 2 |
| hh8 | circuit-fibsqrt | 3 | 2 |
| hh8 | regex-log | 2 | 0 |
| hh8 | write-compressor | 3 | 2 |
| smoke1 | write-compressor | 3 | 2 |

### 3. Docker address-pool exhaustion (fixed; dev1 discarded)

At 40 concurrent trials Docker ran out of bridge subnets (`all predefined address pools have been fully subnetted`) — the default pool allows ~31 networks and every Compose project needs one. These surfaced in `result.json` as trial failures indistinguishable from agent failures. Fixed with `default-address-pools: [{base: 10.192.0.0/10, size: 24}]` (16,384 networks); dev2 ran clean at the same concurrency.


## Failure-class census (all runs)

| class | trials |
|---|---|
| `AGENT:CancelledError` | 37 |
| `INFRA:docker-address-pool` | 11 |
| `AGENT:timeout` | 6 |
| `INFRA:glibc-binary-cannot-execute` | 5 |
| `AGENT:nonzero-exit` | 3 |

## Contents

- `live-run-hh8/jobs/` — preregistered run, per-trial `result.json`, `agent/trajectory.json`, `agent/stella-events.jsonl`
- `rig-runs/jobs/` — smoke1, DISCARDED-dev1, dev2 (both arms each)
- `analysis/all_trials.csv` — every trial, one row
- `*.diff` — the uncommitted harness patches the runs used
- `*.log` — launcher and per-arm logs

