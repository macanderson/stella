---
description: Benchmark phase — Stella vs Claude Code on Terminal-Bench 2.1, plus the cheap nightly loop-health arm.
argument-hint: "[loop|h2h] [--rig] [--out PATH]"
---

# fullauto:bench — the comparator arm

Two arms, deliberately different in cost. Run the cheap one every cycle; the
measured one when something could actually have moved.

```bash
scripts/fullauto.sh bench loop        # loop-health gate in CI, well under $1
scripts/fullauto.sh bench h2h --rig   # Claude Code vs Stella, TB2.1, measured
```

---

## The cheap arm — `bench loop`

Dispatches `.github/workflows/nightly-bench.yml`: a flash-tier model, four pinned
Terminal-Bench tasks, a per-task spend cap. It does **not** measure whether
Stella is better than Claude Code. It measures **loop health** — did the turn do
real work or abort having done nothing, did it die without saying why, was it
caught cycling.

That is the regression class that hides between releases, it is visible on a
cheap model, and it is what makes a per-cycle benchmark affordable. Run it every
cycle.

```bash
scripts/fullauto.sh bench loop
gh run watch <id>
```

## The measured arm — `bench h2h`

`arenabench/matches/fable5-claude-code-vs-stella.toml`: six Terminal-Bench 2.1
tasks, Claude Code and Stella both on Fable 5, `effort = medium` on both seats.
**The model is held constant so the agent architecture is the variable under
test.** That is the whole point — a model swap is a cost change, not evidence
about the harness.

The seats authenticate differently on purpose: Claude Code on the Anthropic
subscription, Stella metered through OpenRouter (Stella's pipeline makes ~3 calls
per step, and a shared requests-per-minute cap would measure the quota rather
than the agent).

Required in the environment, names only, never values:

```
CLAUDE_CODE_OAUTH_TOKEN     # the Claude Code seat
OPENROUTER_API_KEY          # the Stella seat
```

A seat with no credential scores zero, and a zero is indistinguishable from a
real result on a scoreboard. The runner refuses to start rather than produce one.

### Where it runs

**Native x86_64 Linux Docker host only.** Not the Mac. Task images publish
`linux/amd64`; on Apple silicon a multi-arch base builds arm64 and then cannot
exec the agent binary, which Harbor records as an *agent crash* rather than a
platform mistake. `bench h2h` refuses a non-Linux/x86_64 host unless you set
`FULLAUTO_ALLOW_EMULATED=1`, and that flag is for throwaway signal, never a
number anyone quotes.

`--rig` drives the EC2 host instead:

- Instance `stella-vs-cc-rig`, `us-east-1`, 32 vCPU / 123 GB.
- Key at `~/.stella/keys/tb909-key.pem` (override with `FULLAUTO_RIG_KEY`),
  user `ubuntu`. The public IP changes across stop/start — always read it from
  AWS, never hardcode.
- **It bills $1.90/hr and $45.56/day.** Left idle from one run it alone pushed a
  ~$68/month account to a $1,006 forecast and tripped the budget alarm. The
  script stops it on exit, interrupt, and failure — but check `aws ec2
  describe-instances` afterwards anyway.

### Subscribe to the match while it runs

Do not wait 110 minutes to learn an arm was dead from minute one. Alongside
the match (same host, read-only), run the watcher and react to what it emits:

```bash
arenabench watch <match-id> --follow --format jsonl   # agent monitor protocol
```

One JSON object per line (`docs/spec/agent-monitor-protocol.md`), any agent,
any arm. What the phase does with each rule:

- `zero-token` (**critical**) — the arm never made a model call
  (credential/rate-limit failure scored as a loss). The match's numbers are
  **void for that arm**: relaunch under a new name or drop the arm. Never
  publish, never let it into a denominator.
- `stall` — a trial has gone silent; check the seat before its whole time
  allowance burns.
- `premature-complete` / `late-verdict` — quality signals about the losing
  trial; keep them for the postmortem, they do not void anything.

A one-shot `arenabench watch <match-id>` at match end belongs in the phase
regardless: exit code `3` means the scoreboard must not be recorded in the
ledger as a result.

## Reading the result — three traps

**1. The reward is nested.** Per trial, it is
`verifier_result.rewards.reward` in `result.json`. `verifier_result.reward` is
always `None` and silently scores every run as zero passes. Read the wrong one
and a clean sweep looks like a total failure.

**2. An operational abort is not a loss.** These score zero and are *not*
evidence about either agent — they must never land in a denominator:

| Signature | What it is |
|---|---|
| 0 tokens + nonzero steps | a 401 — the key never authenticated |
| trial dies mid-match, Claude Code arm | the subscription's 5-hour cap |
| ~$0.0002 spent, 1 step, balance still positive | OpenRouter credit exhaustion |
| `exception_info.exception_type` set with `None` reward | the trial errored, the agent did not fail |

Relaunch under a new run name, or drop the arm. Never quietly count it.

The first row no longer needs eyes: the scoreboard classifies steps-with-zero-
observed-spend as an infrastructure void (unjudged, out of every denominator),
and `arenabench watch` flags it `critical` while the match is still running.
The other rows still need a human reading.

**3. Local telemetry lags and disagrees.** `~/.stella/bench.db` and
`~/stella-bench/bench.db` hold different, partly-voided subsets. **The rig's job
directories are the source of truth** (`~/tb21/jobs/<run>-armA-stella`,
`-armB-claudecode`).

## What a result means for the cycle

Compare against the previous cycle's entry in the ledger
(`scripts/fullauto.sh state`).

- **Regressed** → file a `P0` `area:core` issue with both result files attached,
  and **block the ship phase**. Shipping a measured regression is the one thing
  this loop exists to prevent.
- **Flat or better** → record it and continue.
- **Did not run** → the cycle records `skipped`. Never `passed`.
