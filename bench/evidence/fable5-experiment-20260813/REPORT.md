# Experiment report: Stella vs Claude Code on Fable 5 (Terminal-Bench 2.1)

- **Experiment id:** `stella-vs-claude-code-fable5-tb21`
- **Status:** hypothesis untested — evidence incomplete
- **Date:** 2026-08-13
- **Canonical data:** the experiment document in `~/.arenabench/experiments.db`
  (table `experiment_results`), assembled by
  `arenabench/scripts/exp-fable5-cc-vs-stella-tb21.py`
  (calculation version `exp-fable5-cc-vs-stella-tb21/1`), which reads only
  ground-truth trial artifacts: each trial's `result.json`,
  `verifier/reward.txt`, and each arm's own journal
  (`agent/claude-code.txt` / `agent/stella-events.jsonl`). Nothing in this
  report comes from a dashboard, screenshot, or stale summary; every number
  below is reproducible by re-running that script.

## 1. The hypothesis, stated as a hypothesis

> Stella can reliably outperform Claude Code running Fable 5 across at least
> four independent, matched Terminal-Bench 2.1 repetitions while achieving
> materially lower cost per verified solve.

**No claim is made in either direction.** The completed evidence cannot
support superiority, parity, or cost advantage:

- **Matched repetitions completed: 0 of the 4 required.**
- The single head-to-head that exists (`dd52a57a6f49`, 6 tasks, one attempt
  each) lost 4 of its 6 Claude Code trials to provider quota, leaving **2
  usable paired cells** — and those two lean *toward* Claude Code (CC 2/2,
  Stella 1/2), the opposite of the raw per-arm headline (CC 2/6, Stella 3/6).
  Two paired cells resolve nothing; both readings are noise.
- The two 8-task runs are unpaired across time, effort-matched to each other
  but not to the head-to-head, and one of them was operator-aborted.

## 2. Subject resolution: what "Fable 5" is

The supplied label resolves to model id **`claude-fable-5`**, served two ways
(source: `matches/dd52a57a6f49/jobs/*/config.json` `model_name`, and the match
specs in `arenabench/matches/`):

| Arm | Route | Provider | Auth / billing |
|---|---|---|---|
| Claude Code | `anthropic/claude-fable-5` | Anthropic first-party (no `base_url`) | Claude subscription OAuth (`CLAUDE_CODE_OAUTH_TOKEN`), unmetered |
| Stella | `openrouter/anthropic/claude-fable-5` | OpenRouter | Metered credits (`OPENROUTER_API_KEY`) |

Effort was `medium` on both arms in the head-to-head and `xhigh` in both
8-task runs — the two campaigns are not comparable to each other. Stella's
pipeline arm also carries two role models (`moonshotai/kimi-k3` verifier at
high, `anthropic/claude-haiku-4.5` triage at low); their tokens are included
in Stella's cost. No model substitution occurred anywhere in the recorded
runs. The exact provider serving OpenRouter's `anthropic/*` route is not
pinned or recorded (#2419) — a known comparability caveat.

## 3. Outcome taxonomy and evidence rules

Every trial is classified into exactly one of: `verified_solve`,
`verified_failure`, `solved_then_timeout`, `timeout_before_solve`,
`invalid_infrastructure`, `provider_failure`, `budget_rejected`, `aborted`,
`pending`, `unverified`. Classification uses structured fields (exception
class, Claude Code's terminal `api_error_status`, Stella's parsed `error`
events) plus strict phrase needles — never bare numbers, which have matched
timestamps before. **Provider failures, budget rejections, aborts, pending
and infrastructure voids are never counted as agent losses.** A trial that
solved and then timed out counts as a solve with its timeout disclosed
(`solved_then_timeout`). Full rules, precedence, and per-trial evidence
strings are in the stored document.

Solve rate = verified solves ÷ valid attempts, where valid attempts =
`verified_solve + verified_failure + solved_then_timeout +
timeout_before_solve`. Cost per verified solve = cost over valid trials ÷
verified solves. Costs are reported on two labeled bases and never mixed:

- **measured** — each arm's own journal (CC: terminal `total_cost_usd`,
  computed by CC itself against a subscription that bills nothing per token;
  Stella: sum of `step_usage.cost_usd` as charged by OpenRouter). The two
  arms' measured numbers are **not subtractable from each other**.
- **repriced** — one list-price table applied to both arms' own token counts
  (`arenabench/pricing.py`; `claude-fable-5`: $10/$50 per Mtok in/out, cache
  read $1.00, write $12.50; unpriced or unmeasured ⇒ null, never a guess).
  Timed-out CC trials leave no terminal usage record, so their usage is
  **unmeasured, not zero**; affected totals are labeled lower bounds.

## 4. What the completed evidence shows

### 4.1 `dd52a57a6f49` — the only head-to-head (2026-08-04/05, effort medium, 6 tasks, 1 attempt)

| Task | Claude Code | Stella | Paired cell usable? |
|---|---|---|---|
| regex-log | verified_solve | verified_solve | yes |
| large-scale-text-editing | verified_solve | verified_failure | yes |
| fix-git | provider_failure (429 before turn 1) | timeout_before_solve | no |
| nginx-request-logging | provider_failure (429 before turn 1) | verified_solve | no |
| openssl-selfsigned-cert | provider_failure (429 before turn 1) | verified_solve | no |
| sqlite-with-gcov | provider_failure (throttled: 8 rate-limit events, then timeout) | verified_failure | no |

The four CC provider failures are the Claude subscription's five-hour limit
(`api_error_status: 429`, "You've hit your session limit"), confirmed in CC's
own journal per trial. They measure the quota, not the agent.

| Aggregate | Claude Code | Stella |
|---|---|---|
| Valid attempts | 2 | 6 |
| Verified solves | 2 | 3 |
| Solve rate (valid basis) | 2/2 | 3/6 |
| Measured cost, valid trials | $0.95 *(subscription-computed)* | $6.32 *(OpenRouter-metered)* |
| Measured spend on excluded trials | $0.47 | $0 |
| Repriced cost, valid trials (one table) | $0.83 | $6.32 |
| Cost per verified solve (repriced) | $0.41 | $2.11 |

The per-solve cost gap is real in the recorded data but rests on n=2 vs n=6
valid trials of different tasks — **not a like-for-like comparison**, and
Stella's number includes its verifier/triage role tokens. Explanation status
for the gap: **unknown** (confounded by task mix, arm asymmetry, and
denominator).

### 4.2 The unpaired 8-task panel (effort xhigh — not comparable to 4.1)

- **`41c7e447666a` (Claude Code, 2026-08-05, complete):** 5/8 verified
  solves (4 clean + 1 `solved_then_timeout`: cobol-modernization), 2
  `timeout_before_solve` (path-tracing, qemu-alpine-ssh), 1
  `verified_failure` (extract-elf). Measured cost $4.35 (lower bound — 3
  timed-out trials left no usage record). Repriced total: null (missing
  beats wrong).
- **`96b5d7b9710c` (Stella, 2026-08-08, operator-aborted):** 0 solves in 3
  valid attempts (extract-elf, pypi-server failures; cobol timeout), 1
  `budget_rejected` (path-tracing — a $5 `budget_usd` cap was set, violating
  the no-caps bench rule), 1 `aborted` (kv-store-grpc cancelled), 3
  `pending` (never ran). $10.58 spent in total.

These two runs differ in date (3 days), concurrency (2 vs 1),
setup-timeout multiplier, co-tenant load (the Stella host was sharing with
an Opus 5 run per the spec's own notes), and completion state. **They do not
constitute a comparison**, and 96b5d7b9710c is not a usable arm at all.

- **`VOID-ambient-key-095a17dda694`:** operator-voided (wrong credential);
  excluded by its own record, counted nowhere.

### 4.3 Comparability-key audit

No run in the runtime carries a complete comparability key (now #3214):
the head-to-head's provenance is `source: backfilled, measured: false` with
an empty dataset digest, null harness version, and no SUT/agent identity —
harbor 0.6.1 is recoverable only from a launcher assertion (labeled
inference), and the Stella SUT commit for it is **unknown** (the pointer file
is mutable state that cannot date the run). No provenance generation records
a runner-image digest at all. The strongest record (`96b5d7b9710c`) has
dataset digest `sha256:7d7bdc1c…`, harbor 0.6.1, SUT
`7edec3b2333e0a8649bb1fcc7190ad97242d9e3e` — the aborted run, ironically.

## 5. Related context (labeled, not evidence for this hypothesis)

`h2h891-full` — Claude Code vs Stella *five-tool bare loop* on
**claude-sonnet-5**, full 89-task panel (2026-08-11/13). Different model,
different tool posture, mixed SUT builds across slices — **not comparable to
the Fable 5 experiment**; included because it is the improvement-history
context and the largest run in the runtime. Classified under this
experiment's single taxonomy (210 assembly cells, all readable):

| Aggregate | Claude Code | Stella (five-tool bare loop) |
|---|---|---|
| Attempted cells | 88 | 122 |
| Provider failures (excluded, never losses) | 0 | 50 (credit-exhaustion 402s; one 401) |
| Budget-rejected / aborted | 0 | 1 |
| Valid attempts | 88 | 71 |
| Verified solves | 72 | 60 |
| Solve rate (valid basis) | 81.8% | 84.5% |
| Measured cost, valid trials | $82.22 *(subscription-computed)* | $31.59 *(OpenRouter-metered)* |
| Wasted spend on dead trials | $0 | $42.04 |
| Repriced cost, valid trials (one table, $3/$15) | $67.05 | $47.38 |
| Cost per verified solve (repriced) | $0.93 | $0.79 |

Two readings of the same artifacts and why they differ: the raw grid
(72/88 vs 61/88) shows an 11-task CC lead that is **entirely the dead arm** —
50 Stella trials died unfunded and score as losses in the canonical
telemetry today (#3209, confirmed independently by this derivation).
On the valid basis Stella edges ahead on rate and repriced per-solve cost.
Explanation status: the *402-contamination* mechanism is **confirmed**
(per-trial terminal events); the *residual Stella edge* is **plausible but
unconfirmed** (single run, no repetition, mixed SUT builds across slices).

## 6. Replication plan — what it takes to actually test the hypothesis

**Census: 0 matched repetitions exist. Required: ≥ 4.** Definition: one full
run of one task set with both arms producing valid trials under one
comparability key.

The instrument already exists: `arenabench/matches/fable5-cc-vs-stella-10task.toml`
(10-task stratified panel — 1 easy / 6 medium / 3 hard, mirrors the dataset's
difficulty mix, 6 tasks carry over from the head-to-head for
cross-cycle comparability; both arms pinned to effort `medium`). It was
designed and never executed. Protocol per repetition (r = 1..4):

1. **Preflight the Claude Code credential** (until #3216 lands, run the
   pattern from `~/.arenabench/run-fable5-repair.sh`): one 4-token request;
   any 429/401 ⇒ do not launch. Verify OpenRouter balance ≥ $30.
2. **Pin the apparatus once, before repetition 1**: SUT = latest `main` at
   that moment (record sha + binary sha256), dataset
   `terminal-bench/terminal-bench-2-1@sha256:7d7bdc1c…`, harbor 0.6.1 —
   identical for all four repetitions. No budget caps, no token caps.
3. Launch `arenabench run matches/fable5-cc-vs-stella-10task.toml`,
   probe within 5 minutes that both arms are producing real turns.
4. Classify with the experiment's classifier; a repetition is valid only if
   ≥ 8 of 10 paired cells survive (both arms valid). A failed repetition is
   an operational abort — named, re-run, never averaged in.
5. Store each repetition's document in `experiments.db`; no peeking rule:
   the decision reads all four repetitions together, once.

**Pre-registered decision rule (proposed — needs Mac's sign-off before
launch):** "reliably outperform" = Stella's valid-basis verified-solve rate
strictly higher in ≥ 3 of 4 repetitions *and* higher pooled; "materially
lower cost" = pooled repriced cost per verified solve ≥ 20% lower on the
one-table basis. Anything else: report the numbers, make no claim.

**Estimated spend** (basis: dd52a57a6f49 measured per-task costs scaled to
10 tasks × 4 repetitions): Stella arm ≈ $42 metered; CC arm $0 marginal on
subscription (or ≈ $22 metered if #3216's preflight forces an API key).
Wall clock ≈ 1–1.5 h per repetition at concurrency 3. **Launching is a
spend decision and, per the standing bench protocol, needs settings
confirmed with Mac first — nothing has been launched.**

## 7. Regression-correction plan

Stage 1 — **measurement plane** (fix before spending on repetitions,
otherwise the new data inherits the old distortions):

| Issue | Defect | State |
|---|---|---|
| #3209 | Mid-run provider 402 scores as an agent loss (34 of 50 dead trials had spend and count as losses in canonical telemetry; 3 solved-then-402 trials lose their pass in `score-match.py`) | open, pre-existing; independently confirmed here |
| #3216 | No quota preflight for subscription-OAuth seats — 4 of 6 head-to-head CC trials measured the quota | **filed by this experiment** |
| #3214 | No complete comparability key anywhere; no image-digest field in provenance; backfilled-era records unidentifiable | **filed by this experiment** |
| #3208 | Cloud submit accepts phantom task ids (task "89" shrank the 89-panel to 88) | open, pre-existing |
| #3215 | Experiments store unwired (server route / UI / normalization decision) | **filed by this experiment**, Refs #2889 |

Copy-paste fix prompt for stage 1:

> Work the ArenaBench measurement-plane fixes in this order: #3209 (classify
> terminal provider-payment/auth failures as infrastructure voids, keep a
> 402'd trial's earned pass, surface an unfunded-trials count), #3216
> (launch-time quota preflight for subscription-OAuth seats, fail closed
> with the reset time), #3214 (record runner-image identity and complete the
> comparability key in provenance), #3208 (reject task ids not in the
> dataset at submit time). Each fix needs a witness test that fails on
> current main. Evidence and repro paths are in each issue and in the
> experiment document `stella-vs-claude-code-fable5-tb21` in
> `~/.arenabench/experiments.db` (`arenabench/scripts/exp-fable5-cc-vs-stella-tb21.py --out /tmp/doc.json` to inspect).

Stage 2 — **engine regressions**: no engine regression is *confirmed* by the
Fable 5 evidence (the failed tasks lack matched historical baselines on an
identified SUT — the apparatus gap in #3214 is precisely why). Candidates
worth carrying into the repetitions, per the diagnosis workflow in
`docs/tools/2 - Diagnost regressions task.md`: Stella's
`large-scale-text-editing` loss where CC solved it (verifier: 2/8 pytest
cases passed), and the `fix-git` timeout (task family with a long fix
history). After the four repetitions, any task that regresses against its
own history gets a transcript-level comparison and an
"Engine Regressions Correction:"-prefixed issue with the full spec —
**none is filed now because none is evidenced now.**

## 8. Where everything lives

- **Database:** `~/.arenabench/experiments.db`, table `experiment_results`
  (single non-null JSONB `results` column), one stored document.
- **Store module:** `arenabench/arenabench/experiments.py` (+
  `arenabench/tests/test_experiments.py`, 8 tests).
- **Document builder:** `arenabench/scripts/exp-fable5-cc-vs-stella-tb21.py`.
- **This report:** `bench/evidence/fable5-experiment-20260813/REPORT.md`.
- Issues filed by this experiment: #3214, #3215, #3216; confirmation
  comment on #3209.
