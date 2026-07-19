# ⚔ Stella Arena

Head-to-head coding-agent benchmarking, rebuilt from scratch as a pnpm package
(the original `stella arena` subcommand was removed in #119 pending exactly
this). Zero runtime dependencies — plain Node ≥ 20.

Run contender CLIs on small, self-contained coding tasks; each trial executes
in an isolated workspace and is judged **only** by the task's offline
`verify.mjs` (exit 0 = solved), never by the agent's exit code. Every trial
leaves receipts: `arena/results/<run-id>/` holds `trials.jsonl`, `run.json`,
and the full per-trial workspaces. "Receipts or it didn't happen."

## Run a match

```bash
pnpm arena run --agents oxagen,claude-code --model anthropic/claude-sonnet-5 --trials 3
```

| Flag | Meaning | Default |
|---|---|---|
| `--agents a,b,…` | contenders (see `pnpm arena agents`) | required |
| `--model` | `provider/model` slug; adapters translate per CLI | required for real agents |
| `--trials N` | trials per (agent × task) | 1 |
| `--tasks t,u,…` | subset of `arena/tasks/` | all |
| `--timeout SECS` | per-trial agent timeout | 600 |
| `--concurrency N` | trials in flight | 3 |
| `--dry-run` | print the exact commands, run nothing | off |

Credentials are whatever each CLI already uses on your machine (logged-in
session / BYOK env) — the arena never handles a key.

## The web app

```bash
pnpm arena dev            # or: pnpm arena:dev, or: pnpm --filter stella-arena dev
```

Serves the dashboard at `http://127.0.0.1:4646` (`--port` to change): standings
tiles, pass-rate and timing charts, and the full trial table for every local
run. Local-only (binds 127.0.0.1), reads straight from `arena/results/`.

`pnpm arena leaderboard` prints standings aggregated across all local runs.

## Agents

| Name | Invocation |
|---|---|
| `claude-code` | `claude -p … --output-format json --permission-mode acceptEdits` (Anthropic models only; `anthropic/` prefix stripped) |
| `oxagen` | `oxagen --model <slug> --output-format json <prompt>` |
| `stella` | `stella --model <slug> --output-format json run <prompt>` |
| `mock-solver` | offline — applies `tasks/<t>/solution/`; must pass everything (self-tests the verifiers) |
| `mock-noop` | offline — does nothing; must fail everything |

Free full-pipeline smoke (CI-safe, no keys):

```bash
pnpm --filter stella-arena smoke
```

## Tasks

Each `arena/tasks/<id>/` has `task.json` (prompt), `fixture/` (copied into the
trial workspace, includes `verify.mjs`), and `solution/` (a known-good answer,
used by `mock-solver` to prove the verifier is satisfiable). Tasks are
edit-only — agents never need to execute commands, so `claude-code` runs safely
under `acceptEdits`.

To add one: create those three pieces, then check
`pnpm arena run --agents mock-solver,mock-noop --tasks <id> --trials 1` shows
solver PASS + noop FAIL.
