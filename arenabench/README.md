<!-- SPDX-License-Identifier: Apache-2.0 -->

# ArenaBench

**Coding-agent benchmarks as a live, side-by-side contest.**

Pick a benchmark. Check the tasks you want. Seat as many contestants as you
like — each one an agent, a full engine configuration, and its own `.env`.
Then watch them race the same task list in real time: a tournament scoreboard
across seven dimensions, live transcripts streaming per task, and a real MP4
screen recording of every trial.

```bash
pip install arenabench
arenabench serve
# -> http://127.0.0.1:8900
```

Zero Python dependencies. No build step. The whole client is three files.

---

## Commands

Everything below is `arenabench <verb>`. `serve` is for exploring; `run` is how
a match becomes repeatable.

### Kick off a match from the browser

```bash
arenabench serve                    # -> http://127.0.0.1:8900
arenabench serve --port 8930 --no-browser
```

Then: **new run → set up from scratch** (or load a template) → pick the
benchmark → check tasks → seat contestants → **start the match**.

### Kick off a match from a file (CI)

```bash
arenabench template -o matches/glm-headtohead.toml   # starter file to edit
arenabench run matches/glm-headtohead.toml           # run it, no browser
arenabench run matches/glm-headtohead.toml --progress --results out.json
```

`run` reads each seat's credentials from the **process environment** against
the `required` list the template declares, and refuses to start if one is
missing — an unauthenticated arm scores zero, and a zero is indistinguishable
from a real result on a scoreboard. Override with `--allow-missing-env` if you
genuinely mean it.

```yaml
# .github/workflows/bench.yml
- run: arenabench run matches/nightly.toml --results results.json
  env:
    OPENROUTER_API_KEY: ${{ secrets.OPENROUTER_API_KEY }}
```

### Prepare the host

```bash
arenabench datasets                             # what's registered / fetched
harbor download terminal-bench/terminal-bench-2-1@sha256:7d7bdc...
arenabench tasks terminal-bench-2.1             # list them
arenabench tasks terminal-bench-2.1 --random 5 --exclude-heavy
arenabench agents                               # who can compete
arenabench build-recorder                       # one-time, for MP4 capture
./scripts/setup-terminal-bench.sh               # all of the above, checked
```

### Free smoke test — no key, no spend

Seat `oracle` (applies the reference solution — a 100% ceiling) against `nop`
(does nothing — a 0% floor). Neither makes a model call, so the whole arena
works end to end for $0.

---

## `arenabench.toml` — a match as a document

A match is otherwise something you assemble in a browser and lose. The TOML
form makes it readable, diffable, committable, and runnable without a UI.

Download one from the setup screen (**download this match**), or generate a
starter with `arenabench template`. Upload one to skip the wizard entirely.

```toml
[match]
name = "glm5.2 head-to-head"
dataset = "terminal-bench-2.1"
tasks = ["fix-git", "regex-log"]     # empty = the whole dataset
attempts = 1
concurrency = 1                      # trials PER CONTESTANT (~2GB each)
record_video = true
setup_timeout_multiplier = 6.0       # agents that npm-install need > 1

[[contestant]]
id = "stella"
name = "Stella"
agent = "stella"

  [contestant.engine]
  api = "openrouter"
  model = "z-ai/glm-5.2"
  effort = "medium"
  max_tokens = 128000
  budget_usd = 6.0

    [contestant.engine.roles.verifier]     # checks the worker's output
    model = "openai/gpt-5.5"               # a different family = independent
    effort = "xhigh"

    [contestant.engine.roles.triage]
    model = "z-ai/glm-4.7-flash"
    effort = "low"

  [contestant.env]
  required = ["OPENROUTER_API_KEY"]        # NAMES only — never values

[[contestant]]
id = "claude-code"
name = "Claude Code"
agent = "claude-code"

  [contestant.engine]
  api = "openrouter"
  model = "z-ai/glm-5.2"                   # same worker model as Stella
  effort = "medium"
  base_url = "https://openrouter.ai/api/v1"   # Anthropic-shaped endpoint

  [contestant.env]
  required = ["OPENROUTER_API_KEY"]
```

**Templates never contain secrets.** A seat declares the variables it needs by
name; values arrive from the environment at launch. That is what makes the file
safe to commit.

**`verifier` here is Stella's internal judge role** — the one that checks its
own work. It is *not* the benchmark's verifier, which is Harbor's and decides
the reward. The file uses `verifier`; the Stella wire key stays
`pipeline_judge_model`.

**Seating one agent on another vendor's model.** Set `base_url` and the runner
routes that seat there — Claude Code reads `ANTHROPIC_BASE_URL`, and because
OpenRouter serves an Anthropic-shaped `/v1/messages`, both arms above hit one
endpoint with one key. Same provider, same quota, no routing confound.

---

## What it does

**One benchmark run, N contestants.** A *contestant* is an agent plus an engine
configuration plus an environment. Everyone runs the identical task list from
the identical digest-pinned dataset, in their own Harbor job, concurrently and
independently.

**The full engine config, not just a model.** Per contestant you choose the API,
provider, model, reasoning on/off, and effort tier. For agents with a pipeline
(Stella) you also configure each role independently — worker on one model at
`xhigh`, verifier on a different family so verification is independent of the
thing being verified, triage cheap and thinking-off.

**Per-seat credentials.** Paste that seat's `.env` straight into the form. A
match between two providers needs two credential sets, so credentials are
per-contestant rather than global. Values live in the arena process and are
handed only to that contestant's subprocess — the API returns key *names*,
never values.

**Seven dimensions, live.**

| Dimension | Direction | Why |
|---|---|---|
| Solve Rate | higher wins | verifier rewards / trials **judged** |
| Clock Time | lower wins | wall clock across all trials |
| Total Cost | lower wins | provider spend |
| Tokens In | lower wins | uncached prompt tokens billed |
| Tokens Out | lower wins | completion tokens billed |
| Cache Read | **higher wins** | prompt tokens you did not pay full price for |
| Cache Write | neutral | a real cost paid to enable future reads |

Cache write crowns nobody on purpose. More writes is not obviously better or
worse, and a scoreboard that confidently picks a direction there is lying.

**Live transcripts by task.** Click a task and every contestant's transcript
streams side by side — reasoning, tool calls, results, per-step token and cost
accounting — pushed over SSE as it is written, not polled.

**Real MP4 recordings.** Opt in and each trial is filmed by a sidecar container
running Xvfb + xterm + `ffmpeg -f x11grab`. Genuine pixel capture to H.264, not
a terminal-cast replay format. See [Recording](#recording) for the honest
caveats.

---

## Install

ArenaBench itself is standard library only. Running a match additionally needs:

- **[Harbor](https://github.com/laude-institute/harbor)** on `PATH` — the
  benchmark runner that owns datasets, containers and verifiers.
- **Docker** — task containers, and the recorder if you enable it.

```bash
pip install arenabench          # or: uv tool install arenabench
arenabench datasets             # what is registered, and whether it is fetched
harbor download terminal-bench/terminal-bench-2-1@sha256:7d7bdc...   # once
arenabench serve
```

### Free smoke test — no API key, no spend

Harbor ships two reference agents that make no model calls at all. Seat them
against each other to see the whole arena work end to end before spending a
cent:

- **`oracle`** applies the reference solution — a 100% ceiling.
- **`nop`** does nothing — a 0% floor.

They emit no transcript (they have no model to narrate), but the scoreboard,
task grid, verdicts and timing are all real.

### Running Stella as a contestant

Stella's Harbor adapter is a separate, AGPL-licensed package that lives in the
Stella repository. Point ArenaBench at it:

```bash
export ARENABENCH_STELLA_ADAPTER=/path/to/stella/bench/harbor_adapter
export STELLA_BINARY=/path/to/stella/target/x86_64-unknown-linux-gnu/release/stella
```

The binary must be a Linux/x86_64 build — task images publish `linux/amd64`
only. A native macOS build will be uploaded happily and then fail to exec
inside the container, which Harbor records as an agent crash rather than a
build mistake.

---

## Recording

Each recorded trial gets a sidecar container:

```
Xvfb :99  ->  xterm running the live renderer  ->  ffmpeg -f x11grab  ->  H.264 MP4
```

```bash
arenabench build-recorder     # one-time, ~1 minute
```

Then tick **Record MP4** when launching. Videos land at
`<trial>/arena/recording.mp4` and appear inline in the transcript lane.

**Why a sidecar and not the task container.** A Terminal-Bench task image is
canonical — the comparability argument rests on every agent getting the exact
same utility set. Installing Xvfb, xterm and ffmpeg into it would change what
the agent can see and do, and in most images would fail outright. The sidecar
shares exactly one thing with the trial: a **read-only** bind mount of the log
directory. It has no network. It cannot influence the run it films.

**What is actually on screen — stated plainly.** In a benchmark the agent runs
headless and one-shot inside the task container, with no TTY and no interface.
There is no native GUI to film. What the recorder shows is ArenaBench's own
renderer drawing that agent's real event stream in a real terminal. The capture
is genuine and the data is live; the *interface* is ArenaBench's, not the
agent's. If you need the agent's own TUI on video, that is a different
experiment — it changes the run conditions and therefore the numbers.

**Recording never costs you a match.** No Docker, no image, or a failing
container produces a match with no videos and a note saying why. The camera is
not allowed to break the benchmark.

---

## Adding a benchmark

One entry:

```python
from arenabench.registry import DEFAULT_REGISTRY, Dataset

DEFAULT_REGISTRY.add(Dataset(
    key="swe-bench-verified",
    title="SWE-bench Verified",
    harbor_id="princeton-nlp/swe-bench-verified@sha256:...",
    namespace="swe-bench",
))
```

Keep the `@sha256:` suffix. An unversioned name is not a freeze, and every
number ArenaBench reports is only comparable to another number from the same
digest.

## Adding an agent

Harbor's ~20 built-in agents are already registered. Adding another is one
`AgentSpec` in `arenabench/agents.py`, declaring which engine knobs it actually
honours. Anything you set that an agent ignores is reported to you rather than
silently dropped — two arms that were secretly identical is the one failure a
head-to-head cannot survive, because it produces a clean number for a contest
that never happened.

---

## Architecture

```
arenabench/
  model.py         contest vocabulary — Engine, Contestant, MatchSpec, dimensions
  registry.py      which datasets exist; enumerates tasks offline-first
  agents.py        which agents can compete, and what each one honours
  harbor_agent.py  ArenaBench's Stella adapter (swaps the frozen posture)
  runner.py        one `harbor run` per contestant; credential isolation
  telemetry.py     artifact -> metrics + transcripts; incremental, cached
  recorder.py      MP4 sidecar supervisor
  server.py        stdlib HTTP + SSE
  web/             the arena UI (3 files, no build step)
recorder/          Dockerfile + record.sh + render.py
```

**Everything displayed is read from files the run already writes.** Nothing
talks to a container, an agent, or a provider. That is why the same code
renders a live match and a six-month-old archive, and why watching a contestant
can never change its number.

Two design rules worth knowing if you extend it:

1. **`resolved` is tri-state.** `True`, `False`, and `None` for "no verifier has
   spoken yet". Solve rate divides by *judged* trials. Dividing by attempted
   makes every contestant start near 0% and climb, which looks like a result and
   is an artifact of progress.
2. **A contestant with no judged trial leads nothing.** A seat that has not
   spent a token has cost 0, clock 0, tokens 0 — and would otherwise sweep four
   dimensions for the first minutes of every match.

---

## Security

The arena binds to loopback and should stay there. It holds provider
credentials in memory and can spawn runs that spend real money; anyone who can
reach the port can do both. There is no authentication — this is a local tool.

---

## Licence

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

Stella's Harbor adapter, which ArenaBench's Stella contestant subclasses, is
AGPL-3.0-only and distributed separately as part of the Stella project. It is
an optional runtime dependency; ArenaBench does not vendor or redistribute it.
