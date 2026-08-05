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

Zero Python dependencies. The server is standard library only, and the web
client ships pre-built inside the package — installing and running ArenaBench
never needs Node. Light and dark mode, defaulting to dark.

---

## Commands

Everything below is `arenabench <verb>`. `serve` is for exploring; `run` is how
a match becomes repeatable.

### Kick off a match from the browser

```bash
arenabench serve                    # -> http://127.0.0.1:8900
arenabench serve --port 8930 --no-browser
```

Loopback only. A `--host` that is reachable from elsewhere is refused unless
you add `--allow-remote` — see [Security](#security).

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

While the match runs, `run` also applies the [watch rules](#watch-a-running-match-for-the-failures-that-invalidate-it)
inline (`!!` lines in the log) and exits `3` if a detection invalidated the
match — so a CI job fails at the failure, not at the publish step.

```yaml
# .github/workflows/bench.yml
- run: arenabench run matches/nightly.toml --results results.json
  env:
    OPENROUTER_API_KEY: ${{ secrets.OPENROUTER_API_KEY }}
```

### Watch a running match for the failures that invalidate it

```bash
arenabench watch <match-id>                      # scan once; exit 3 if invalid
arenabench watch <match-id> --follow             # keep watching, ^C to stop
arenabench watch <match-id> --format jsonl       # for a subscribing process
```

Every failure worth catching in a live match is visible in the artifacts the
run is already writing — an arm whose credentials never worked (steps but
zero tokens, scored as *losses*), a confident three-step "complete" that
passed nothing, a verification failure as a trial's final word, a hung trial
burning its allowance. `watch` tails those artifacts and prints one line per
detection (`<severity> <arm> <task> <rule> <evidence>`), read-only with
respect to the run, and exits `3` when a detection invalidates the match —
so CI fails the publish step instead of publishing an inverted scoreboard.

`--format jsonl` emits **agent monitor protocol** events (one JSON object
per line) instead — a deliberately ArenaBench-agnostic envelope any
supervising process can subscribe to and any run-watching tool can emit; the
normative spec lives in the Stella repository as
`docs/spec/agent-monitor-protocol.md` (`doc:agent-monitor-protocol`).
Rules: `zero-token`, `premature-complete`, `late-verdict`, `stall` by
default, `usage-incomplete` opt-in via `--rules`; thresholds via
`--min-steps`, `--min-output-tokens`, `--stall-minutes`.

The zero-token signature is also wired into scoring itself: a finished trial
with a nonzero step count and zero observed spend is counted as an
infrastructure void (unjudged), never as a loss — the agent that swallows
its own 401/429 and exits nonzero must not hand its opponent a win nobody
can defend once the logs are opened.

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

**`verifier` here is Stella's own internal role** — the one that checks its
own work. It is *not* the benchmark's verifier, which is Harbor's and decides
the reward. The file and the Stella wire key agree on the name
(`pipeline_verifier_model`); a template written against the older `judge`
spelling still loads.

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

**Eight dimensions, live.**

| Dimension | Direction | Why |
|---|---|---|
| Solve Rate | higher wins | verifier rewards / trials **judged** |
| Clock Time | lower wins | wall clock across all trials |
| Total Cost | lower wins | every seat's tokens through **one** price table |
| Self-Reported | neutral | what each agent said it spent, on its own table |
| Tokens In | lower wins | uncached prompt tokens billed |
| Tokens Out | lower wins | completion tokens billed |
| Cache Read | **higher wins** | prompt tokens you did not pay full price for |
| Cache Write | neutral | a real cost paid to enable future reads |

Cache write crowns nobody on purpose. More writes is not obviously better or
worse, and a scoreboard that confidently picks a direction there is lying.

Neither does the self-reported column, for a sharper reason. Every agent prices
its own run from its own table and no meter checks it; on one measured
head-to-head those tables turned a 2.5x token gap into a reported 7.9x cost
gap. Subtracting two tables is not a cost difference, so the cost *crown* is
computed in `pricing.py` from the token counts both agents report accurately,
run through one table for both seats. A model that table does not cover reports
no cost rather than a guessed one — a plausible wrong figure populates a column,
sorts against the other arm, and crowns a winner on a dimension nobody measured.

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

## When did it actually solve it

A Terminal-Bench trial is graded **once**, after the agent is dead, against
whatever the workspace looks like then. Nothing watches for the moment the tests
start passing — so three things go unmeasured:

- every step after that moment can only lose: it cannot raise a reward already
  at 1.0, and it costs tokens and clock;
- an agent can **destroy its own passing solution** and score 0,
  indistinguishable from never solving it;
- a trial killed by the wall clock can still pass — being interrupted is not
  failing. (Measured: a Stella trial ended `AgentTimeoutError` at 900s and
  scored a pass anyway.)

Turn on capture, then replay afterwards:

```toml
[match]
capture_snapshots = true
snapshot_interval = 30.0   # the floor on how precisely a flip can be located
```

```bash
arenabench flip <trial-dir>
# flip at snapshot 6/23 (t+180s)
# kept running 510s after it was already passing — 5 verifier runs, 0 unknown
```

**How capture stays invisible to the run.** Snapshots are taken with
`git --git-dir` pointed *outside* the workspace and `--work-tree` pointed at it,
so the workspace gets no `.git`, no index, and no ignore file. That is not
fastidiousness: `fix-git` is a real task in this dataset whose whole subject is
repository state, and a snapshotter that ran `git init` in the workspace would
silently rewrite the task it was measuring.

**Why the replay is afterwards, not during.** A task's verifier is a directory
of tests that must be copied into the container to run. Doing that mid-trial
would leave the answer key in the agent's own filesystem, readable. End-only
grading is what keeps the oracle hidden, so the replay runs against fresh
containers once the agent is gone and cannot learn from it.

**Cost shapes the search.** One probe pays the verifier's full setup — ~142s for
`sqlite-with-gcov`, mostly `apt-get` and a `uv` install. So the flip is found by
bisection (`log2(n)` probes) plus a short backwards walk to catch an agent that
solved the task, broke it, and fixed it again — the one case bisection cannot
see. A probe that could not run at all counts as *unknown*, never as a failure:
scoring it 0 would move the flip earlier than the truth.

Capture is off by default and degrades to nothing — no Docker, or no `git` in
the task image, yields a run with no snapshots and a warning.

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
  monitor.py       failure detection over telemetry; emits the agent
                   monitor protocol (`arenabench watch`)
  recorder.py      MP4 sidecar supervisor
  server.py        stdlib HTTP + SSE
  web/             the arena UI, BUILT — generated from ui/, committed so
                   `pip install arenabench` needs no Node. Do not edit.
ui/                the arena UI, SOURCE — Next.js App Router, Tailwind v4,
                   shadcn-style components on Base UI, dark/light themes
recorder/          Dockerfile + record.sh + render.py
```

### Developing the UI

The client is a static export: the Python server owns every `/api` route and
serves `web/` as plain files, so the built app has no Node runtime, no SSR,
and no server of its own.

```bash
arenabench serve --no-browser        # the API, on :8900
cd ui && npm install && npm run dev  # live-reload dev server on :3900,
                                     # /api proxied to :8900
npm run build                        # export + sync into arenabench/web/
```

Commit the regenerated `web/` together with the `ui/` change that produced
it — the committed export is what pip users run.

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

This is a local tool and there is no login. What that buys has to be paid for
somewhere, so the local-only assumption is enforced rather than assumed:

- **Loopback or an explicit flag.** A non-loopback `--host` is refused unless
  you also pass `--allow-remote`. The arena holds provider credentials and
  `POST /api/matches` spawns processes with them; anyone who can reach the port
  can spend your money, so that has to be a decision rather than a typo.
- **Only its own address.** Every request must carry a `Host` naming the
  interface actually bound, and any `Origin` must be this arena or another
  loopback port (which is what `npm run dev` is). That is what stops a page you
  are merely *visiting* from driving the arena through your browser — DNS
  rebinding included, since a name an attacker controls can point at
  `127.0.0.1` but still arrives as their name.
- **Writes must be JSON.** An HTML form can send exactly three content types
  and none of them is `application/json`, so a cross-origin form cannot reach a
  write route at all.
- **A seat carries credentials and endpoints, nothing else.** A contestant's
  pasted `.env` becomes the environment of a subprocess on this machine, so it
  is screened to names ending in `_API_KEY`, `_AUTH_TOKEN`, `_BASE_URL` and
  friends. `PATH`, `PYTHONPATH`, `LD_PRELOAD`, `DOCKER_HOST` and `STELLA_BINARY`
  are ways to make that subprocess run something other than the benchmark, and
  no contestant needs any of them. Anything dropped is reported on the seat.

---

## Licence

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

Stella's Harbor adapter, which ArenaBench's Stella contestant subclasses, is
AGPL-3.0-only and distributed separately as part of the Stella project. It is
an optional runtime dependency; ArenaBench does not vendor or redistribute it.
