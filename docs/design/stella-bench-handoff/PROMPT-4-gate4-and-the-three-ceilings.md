# Handoff: run gate4, then close the three ceilings

Everything below is measured from trial artifacts, not inferred. Where a number
appears, it came from a file on the bench box that you can re-read.

## State right now

- **PR #1173 is merged to main as `22345e4b`.** It is the *only* change since
  gate3c. Gate4 exists to attribute it. Do not stack another change on top
  before gate4 reports, or neither is attributable.
- **The SUT binary is built and provenance-verified.** Confirmed on the box:
  - `sut_commit.txt` = `22345e4b000f2a51ce55b79ffbca8158580715ff`
  - `binary_sha256.txt` = `6a541de055a258428c79504ef6213c7829f2f994554c3f610866e700d4f39957`
  - `assert_portable_binary` passed (glibc floor OK for every task image).
  Re-check these two files before running the gate; if they differ, something rebuilt.
- Config otherwise unchanged: both arms `claude-sonnet-5`, first-party API,
  effort `xhigh`, `params.max_tokens=32000`, `MAX_LENGTH_CONTINUATIONS=4`,
  `--turn-budget 840`, witness OFF, separate key per arm, no proxy.

## What #1173 changed

`plan_continuation` returned `None` for two unrelated reasons — allowance spent,
or too little wall clock to finish another continuation — and the caller read
`None` one way: fall through to terminal handling, whose empty-text branch
aborts. So declining *for time* exited nonzero, which is scored exactly as the
harness killing the agent. `ContinuationPlan` now names which decline happened;
`OutOfTime` completes the turn with a truthful partial instead of aborting.

## The next command

```sh
ssh -i /Users/macanderson/Projects/stella/docs/stella-bench-handoff/tb909-key.pem ubuntu@<IP>
~/race-gate.sh gate4 10 ~/tb21/gate10.tasks     # exports TB_REPO/TB_ROOT itself
~/gate_verdict.py gate4
```

Run it detached (`nohup ... &`) — it is long and an SSH drop kills a foreground run.

## gate3c ground truth — what gate4 is being compared against

Read per trial from `exception.txt`, `verifier/reward.txt`, `agent/stella-events.jsonl`.

| trial | outcome | cause |
|---|---|---|
| circuit-fibsqrt | exit 1 | OutOfTime abort — **#1173 targets this** |
| regex-chess | exit 1 | OutOfTime abort — **#1173 targets this** |
| schemelike-metacircular-eval | exit 1 | OutOfTime abort — **#1173 targets this** |
| write-compressor | exit 1 | OutOfTime abort — **#1173 targets this** |
| torch-tensor-parallelism | exit **137** | SIGKILL. Different bug, untouched. |
| gpt2-codegolf | AgentTimeoutError | 900s |
| path-tracing | AgentTimeoutError | 1800s |
| cobol-modernization | reward 1, clean | — |
| kv-store-grpc, mteb-leaderboard | reward 0, clean exit 0 | ordinary fails |

All four aborts fired on their **first** cap hit with **zero** continuations
spent — the allowance was never the reason, and the abort message ("did so on
every one of its 4 continuations") was false on all four.

### Prediction for gate4

**NonZero 5 → 1, blocking 7 → 3.** Pass rate probably still 1/10: Harbor runs
the benchmark verifier even after a NonZero, so those four already scored 0.
This converts four zero-scoring *crashes* into four zero-scoring *results*.
That is the point — it makes the gate a measurement. It is not a score fix.

### The single most likely way gate4 disappoints

`pipeline_status_result` maps **both** `Aborted` **and** `VerificationFailed` to
`Err` → exit 1 → `NonZeroAgentExitCodeError`. So stella's own "did not verify"
verdict reads to Harbor as an agent crash. #1173 works only because those four
turns had real tool calls and file changes, so the ladder will not reach
`NothingAttempted` (which is `passed:false` → `VerificationFailed` → exit 1).

**If gate4 still shows NonZero and the reason string is `verification failed:
...` rather than `output-token limit ...`, that is a second, separate defect.**
Do not treat it as #1173 failing.

## THE BIG FINDING — why we are far off the old numbers

Claude Code **passed all four** of the tasks stella aborted on. Measured from
its ATIF `trajectory.json` (`steps[].metrics.completion_tokens` + timestamps):

| task | CC biggest step | that step took | stella cap | stella result |
|---|---|---|---|---|
| circuit-fibsqrt | **45,001** tok | 527s | 32,000 | truncated, 0 tools, abort |
| regex-chess | **64,000** tok | 756s | 32,000 | truncated, 0 tools, abort |
| schemelike-metacircular | **64,000** tok | 624s | 32,000 | truncated, 0 tools, abort |
| write-compressor | 25,965 tok | — | 32,000 | truncated, 0 tools, abort |

Sonnet fills the budget in *both* agents. The difference is the budget's size.
The steps where stella died at exactly 32,000 with nothing to show are the same
steps where Claude Code spent 45–64k and finished, tool call included.

Cost, same task: **CC circuit-fibsqrt $2.52 → pass. Stella $2.02 → fail.**
Stella burned ~117k output tokens there; CC used 97.6k. Stella spent *more*
output and got nothing, because 32k of it was truncated reasoning thrown away.

### Three ceilings, all below what the model needs AND below the comparator

1. `params.max_tokens` = **32,000** vs Claude Code's **>=64,000**.
2. `model_timeout` = **600s** — but CC's two winning steps took **624s and
   756s**. Even at 64k, stella would kill them. This is precisely why the
   earlier 64000 experiment "traded truncation for model_timeout": the steps
   genuinely need 600-760s.
3. `--turn-budget` = **840s** — but CC's regex-chess run took **2,746s** of wall
   clock and passed. Stella declines continuations roughly 3x too early. Note
   the harness agent timeout is per-task (900s and 1800s both observed), so a
   single global 840s is the wrong shape as well as the wrong size.

**(1) and (2) are one change, not two** — raising the cap alone only relocates
the cliff into the timeout, which has already been measured once. (3) is
separate and should follow its own gate.

### Do NOT lower effort

An earlier read in this session suggested dropping below `xhigh`. The evidence
reversed it and that suggestion is withdrawn: lowering effort makes the model
think less in order to fit a cap that is itself the defect. Raise the ceilings
toward parity instead.

## Where the evidence lives

- Stella arm: `~/tb21/jobs/gate3c-armA-stella/<task>__*/`
  - `exception.txt` (last line is the exception class), `verifier/reward.txt`
  - `agent/stella-events.jsonl` — `step_usage` events carry `duration_ms`,
    `output_tokens`, `tool_calls`, `role`. Summing these reconstructs the turn.
  - `agent/stella-run.json` — envelope with `status` / `reason`.
- Claude Code arm: `~/tb21/jobs/gate3c-armB-claudecode/<task>__*/agent/trajectory.json`
  (ATIF v1.2: `steps[].metrics.completion_tokens`, `final_metrics`, timestamps).

## Box + operational traps (each of these cost a restart today)

- Instance `i-07d46341dcc9a31b3`, us-east-1, m6id.8xlarge, 32 cores. Keep it up.
  IP changes on stop/start:
  `aws ec2 describe-instances --instance-ids i-07d46341dcc9a31b3 --region us-east-1 --query 'Reservations[0].Instances[0].[State.Name,PublicIpAddress]' --output text`
- Key: **use the absolute path** `/Users/macanderson/Projects/stella/docs/stella-bench-handoff/tb909-key.pem`.
  The relative path does not resolve from inside a git worktree.
- **`rg` is not installed on the box.** Use `grep` there. (`rg` is only the local rule.)
- `build_sut.sh` needs `TB_REPO` and `TB_ROOT` exported, and `rustup`/`cargo` are
  **not** on the non-interactive SSH PATH. Working invocation:
  ```sh
  cd ~/stella && PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH" \
    TB_REPO=$HOME/stella TB_ROOT=$HOME/tb21 \
    nohup bash bench/evidence/run/build_sut.sh > ~/tb21/build_sut.log 2>&1 &
  ```
  `race-gate.sh` exports TB_REPO/TB_ROOT itself, so it does not need this.
- **The box carries an uncommitted adapter patch** — `bench/harbor_adapter/stella_harbor/__init__.py`,
  +31 lines, adding `STELLA_TURN_BUDGET` to `_HOST_ONLY_STELLA_ENV` and
  `--turn-budget` to the argv. It is NOT on main. Do not `git checkout`/`reset`
  it away; `git merge --ff-only origin/main` preserves it. Backup: `/tmp/adapter_backup.py`.
  It should be committed to main eventually — unversioned run config is a hazard.
- `build_sut.sh` defines the SUT as `origin/main` and refuses any `.rs` drift, so
  **Rust changes must be merged to main before they can be measured.** `bench/`
  and `docs/` changes do not trip it.
- Pushes: the owner asked for `git push --no-verify` for now and to let GitHub
  burn the CI minutes. A backgrounded push reports success while having been
  rejected — **always verify with `git ls-remote`.** (One push here also died on
  SIGPIPE, exit 141, purely from piping into `tail`.)
- Local `make build-release` takes 20-30 min: `lto = true` + `codegen-units = 1`
  means a single-threaded whole-program link. It is not hung. It is also not
  needed — the SUT is built on the box. For a fast local release binary:
  `CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo build --release -p stella-cli`.

## Standing rule from the owner

One measured change, then a small task set. Never two at once — two gates here
were already confounded and neither could be attributed. Do not spend on the
full 89 until the gate is clean; it is ~$900 at Sonnet 5 prices.
