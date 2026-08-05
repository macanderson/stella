<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Claude Code vs Stella on one GLM model, through ArenaBench

Both contestants call **the same model on the same z.ai coding plan**, so the
only thing varying is the agent around it. That is the point of the exercise:
with the model held fixed, a difference in solve rate is a difference in
scaffolding, and a difference in tokens is a difference in how much context
each agent decided it needed.

It is also the cheap way to run. The GLM Coding Plan is a subscription, so
neither arm bills per token — see [What the numbers mean](#what-the-numbers-mean)
for which scoreboard columns survive that and which do not.

---

## The one thing that is easy to get wrong

**z.ai publishes two different endpoints for the same plan, and the two agents
need different ones.**

| Contestant | Wire format | Base URL |
|---|---|---|
| Claude Code | Anthropic Messages | `https://api.z.ai/api/anthropic` |
| Stella | OpenAI chat-completions | `https://api.z.ai/api/coding/paas/v4` |

Both accept the same `ZAI_API_KEY`. The `ZAI_BASE_URL` in `.env.global.local`
is the OpenAI-shaped one — correct for Stella, wrong for Claude Code. Pointing
Claude Code at `…/coding/paas/v4` fails every trial, and a scoreboard reading
0/10 looks exactly like an agent that cannot code.

The model id is `glm-5.2` (confirmed against the plan's own `/models`).

---

## Prerequisites

```bash
harbor --version            # must print 0.6.1 — the Stella adapter's launcher
                            # refuses to start on anything else, on purpose
docker info --format '{{.MemTotal}}'
```

**Export the dataset. This is not an optimisation — it is what lets a match
finish.**

```bash
arenabench export terminal-bench-2.1
```

Given a registry ref, Harbor resolves *every task against its backend at run
time*, once per task, even though the tasks are already unpacked on disk. One
failed lookup raises out of the job and kills every remaining trial — and since
both contestants are separate processes failing for the same reason at the same
moment, the scoreboard reads like a contest result rather than an outage. Three
10-task matches died this way at tasks 3, 7 and 3 before the cause was found.

Running from an export removes the network from that path entirely. Provenance
is unchanged: the export is produced *from* the pinned digest, so the bytes are
the pinned bytes — the digest is resolved once, at download, rather than ninety
times under load. ArenaBench prefers the export automatically and warns on any
seat that has none.

**Stella's benchmark binary** must be a Linux/amd64 build; the task images
publish `linux/amd64` only, and a macOS binary uploads happily and then fails
to exec, which Harbor records as an agent crash rather than a build mistake.

```bash
cargo zigbuild --release -p stella-cli --target x86_64-unknown-linux-gnu.2.17
```

**Docker memory is the real capacity limit.** A two-contestant match runs two
task containers at once, so the ceiling per task is your Docker allocation
divided by two. At 7.7 GB that means drawing from the 2 GB tier:

| Docker allocation | Safe per-task ceiling | Tasks reachable |
|---|---|---|
| 7.7 GB (default) | 2048 MB | 68 of 89 |
| 12 GB | 4096 MB | 81 of 89 |

Raising Docker Desktop to 12 GB is the single change that most widens the
benchmark. Do not give it 16 GB on a 16 GB host — that swap-thrashes.

**Pre-pull the images for the tasks you drew.** ArenaBench runs Harbor with
`--max-retries 0`, so a registry timeout mid-match is a permanent reward-0 row
that is indistinguishable from a genuine failure.

---

## Launching

```bash
export ARENABENCH_STELLA_ADAPTER=<repo>/bench/harbor_adapter
export STELLA_BINARY=<repo>/target/x86_64-unknown-linux-gnu/release/stella
cd arenabench && PYTHONPATH=. python3 -m arenabench serve
# -> http://127.0.0.1:8900
```

Both variables are read from the arena's own environment. Everything else is
per-seat and pasted into the UI, because a match between two providers needs
two credential sets and a global one would let an ambient key quietly stand in
for a missing one.

### Seat 1 — Claude Code

| Field | Value |
|---|---|
| Agent | Claude Code |
| API | `zai` |
| Model | `glm-5.2` |
| Base URL *(advanced)* | `https://api.z.ai/api/anthropic` |
| `.env` | `ZAI_API_KEY=<your key>` |

ArenaBench aliases `ZAI_API_KEY` into `ANTHROPIC_AUTH_TOKEN`, which is the
variable Claude Code actually reads, and says so in a note on the seat. Paste
`ANTHROPIC_AUTH_TOKEN` yourself instead if you prefer — an explicit choice is
never overwritten.

Leave `Model` as the bare `glm-5.2`. Harbor forwards the name to a custom
endpoint unchanged, so a `zai/` prefix would travel to z.ai verbatim.
ArenaBench strips it for routed seats regardless, but typing it bare keeps the
UI honest about what is being sent.

### Seat 2 — Stella

| Field | Value |
|---|---|
| Agent | Stella |
| API | `zai` |
| Model | `zai/glm-5.2` — qualified, unlike the Claude Code seat |
| Base URL *(advanced)* | `https://api.z.ai/api/coding/paas/v4` |
| `.env` | `ZAI_API_KEY=<your key>` |

Stella's posture requires a `provider/model` spec and rejects a bare slug, so
this seat keeps the prefix while the Claude Code seat drops it. That asymmetry
is real, not a typo: one agent is routed *by* Harbor, the other routes itself.

To compare like with like, set both seats to the same effort tier.

### Tasks

Press **random**, set the count to 10 and the `≤ MB` box to `2048`. The seed is
printed next to the count — write it down. A slice you cannot redraw is a
result nobody can check, including you next week.

The equivalent from the CLI:

```bash
python3 -m arenabench tasks --random 10 --seed 42 --max-memory-mb 2048
```

Leave **Attempts/task** at 1 and **Concurrency** at 1. Concurrency is per
contestant, so 1 already means two containers running at once.

---

## What the numbers mean

**Solve rate, clock time, tokens in/out** are real and comparable. They come
from the verifier and from each agent's own trajectory.

**Total Cost is comparable; Self-Reported is not, and the board says which is
which.** The two columns are different measurements:

- **Total Cost** is each seat's own token counts run through one shared table
  (`arenabench/pricing.py`). Both agents count tokens the same way, so this is
  the column that ranks. On a subscription it is a *list-price equivalent* —
  what those tokens would have billed on metered access — not an invoice.
- **Self-Reported** is what the agent said it spent. It crowns nobody, and it
  is why the split exists. On the same task Claude Code reported `0.3326` and
  Stella `0.0641`: Stella's catalog *has* a `glm-5.2` entry, so its figure is a
  fair estimate of metered z.ai, while Claude Code's has none and prices GLM as
  the Anthropic model whose alias it was pointed at. Read side by side that
  says "Stella is 5× cheaper", which is not a finding about either agent — it
  is two different tables being subtracted from each other.

A model the shared table does not cover reports **no** cost rather than a
guessed one, so an empty Total Cost means unpriced, never free.

**Cache read is real and large** — the same trial reported 286,656 of 296,070
input tokens served from cache. z.ai returns `cache_read_input_tokens`, so that
dimension is comparable. It returns no `cache_creation_input_tokens`, so **cache
write will read zero** for the Claude Code arm: a measurement gap, not a
finding. Cache write crowns nobody on the scoreboard anyway.

**Expect the clock to dominate.** The ten tasks above carry 3.8 hours of agent
timeout between them. Stella has historically burned its full timeout on tasks
it had already solved, so a `reward 1.0` alongside a timeout is a known
signature worth checking in the transcript before reading it as a loss.

**Read the "never started" count before the solve rate.** Trials that died in
setup, environment start, credentials, or a broken verifier are held out of the
solve rate and counted separately, because they say nothing about coding. This
matters asymmetrically: Claude Code installs a Node toolchain into every
container while Stella uploads one static binary, so on a slow or contended
host Claude Code's arm is the one that fails to start. A 10–0 board where six
of those trials never began is not a result — raise **Setup timeout ×** and run
it again.

An install that *fails* rather than times out still surfaces as
`NonZeroAgentExitCodeError` and is counted as an agent loss. Distinguishing it
would mean substring-matching Harbor's setup command out of an exception
message; raising the setup budget addresses the cause instead.
