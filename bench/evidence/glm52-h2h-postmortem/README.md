# GLM-5.2 head-to-head post-mortem: the 106 cap hits and the 6.4× input gap

A reading of the evidence from the 89-task Terminal-Bench 2.1 same-model
head-to-head (`stella-docs/benchmarks/terminal-bench-2-1-glm-5-2.json` — the
data behind the public "same model, different harness" page). Stella won the
run, 58/89 against Claude Code's 44/89, and this document is about the two
things the winning number hides: **106 output-cap hits** (Claude Code: 0) and
**366M input tokens against 57M** (6.40×).

Every figure below is recomputed by [`analyze.py`](analyze.py) beside this
file, from the committed table alone — stdlib only, no network, no arguments.
If this prose and that script disagree, the script wins.

> **What survives of the raw evidence.** The per-trial `stella-events.jsonl`
> streams (~900 MB/run) are not committed; the table is the durable reduction
> (`bench/harbor_adapter/stella_harbor/live_feed.py::_reduce`, path+SHA
> provenance in the telemetry store schema). `exit_cause.py` (#1213) now
> captures the SIGKILL half of a death at the moment it happens; this
> post-mortem is the reading half over what the run recorded. Where the table
> cannot answer a question, that is said explicitly rather than guessed.

## 1. Headline

| | Stella | Claude Code |
|---|---:|---:|
| passed / 89 | **58** | 44 |
| input tokens | 366,226,022 | 57,233,585 |
| output tokens | 5.31M | 2.23M |
| cost | $78.25 | $63.35 |
| cap hits | **106** | 0 |

`cap_hits` counts `step_usage` events with ≥16,384 output tokens **and zero
tool calls** — a step that spent a huge output budget without acting. Under
the frozen posture (`max_tokens: 64000`) a counted step may sit anywhere in
[16,384, 64,000]; a literal truncation at the cap is the worst case of the
class, not the whole class. Claude Code recorded zero because its steps
either act or stay small — the failure shape is Stella-specific.

## 2. The 106 cap hits

### 2.1 Most cap hits were absorbed, not fatal

The surprise: **tasks with cap hits passed at a *higher* rate (32/44 = 72.7%)
than tasks without (26/45 = 57.8%)**, and 74 of the 106 hits happened inside
passing trials. Cap hits cluster on the hard, long tasks — the ones Stella
grinds out and Claude Code often doesn't attempt for long (regex-chess: 10
hits, PASS; feal-linear-cryptanalysis: 6, PASS; llm-inference-batching:
6, PASS). The loop demonstrably recovers from a burned step: it retries, the
next step emits the tool call, the trial proceeds.

So the cap-hit story is **not** "the cap loses tasks" at the margin — it is
mostly a *spend and wall-clock* tax on tasks Stella wins anyway, plus a small
fatal tail (§2.3).

### 2.2 The tax is a third of all output

Each hit burned ≥16,384 output tokens by definition, so the 106 hits burned
**≥1.74M output tokens — 32.7% of everything Stella emitted in the run** —
as reasoning that produced no action and was then thrown away. At GLM-5.2
output pricing this is single-digit dollars; the real costs are wall-clock
(each burned step is a full model call, and the biggest cap-hit tasks are the
ones that also died at `AgentTimeoutError` with reward already earned) and
context pollution (the truncated reasoning lands in history and is re-sent
every subsequent step — see §3).

### 2.3 The fatal tail: 12 failures with cap hits, 3 distinct shapes

Lower-bound share of the trial's output burned in cap-hit steps
(`cap_hits × 16384 / out_tokens`):

| shape | tasks | signature |
|---|---|---|
| **FATAL-EARLY** | `write-compressor` (3 hits / 7 steps, ≥99.4% of output), `gpt2-codegolf` (3 / 11, ≥85.2%) | The trial essentially *was* its cap hits: a few giant zero-action reasoning dumps, near-zero input tokens, dead before doing anything. This is exactly the READINESS §8.4.2 shape ("budget spent on reasoning, no visible response") surviving the 64k raise — on tasks whose first step is one enormous think. |
| **CAP-HEAVY** | `protein-assembly` (8 / 39, ≥75.8%), `financial-document-processor` (1 / 29, ≥57.8%), `path-tracing-reverse` (2 / 30, ≥52.0%) | Repeated burned steps dominating a short trial; the loop recovered each time but the budget/timeout ran out before the work did. |
| **late/mixed** | `build-pov-ray`, `constraints-scheduling`, `dna-assembly`, `make-doom-for-mips`, `pytorch-model-recovery`, `raman-fitting`, `video-processing` | 1–4 hits inside long trials; the hits cost time but the failure has other co-causes (timeouts, verifier disagreement). |

Two operational conclusions:

1. **The recovery path works and should be kept** — it converted what would
   have been ~30 additional failures into passes.
2. **The remaining fatal shape is "first-step mega-reasoning"** on
   code-golf/compression/assembly puzzles, i.e. exactly the class the
   READINESS §8.4.3 head-to-head showed Claude Code winning with 45k–64k
   token steps. The cap cannot be tuned away *under this model's 64k posture*
   (both agents cap at 64k there); what changes it is the Fable-class arm —
   see [`PROPOSAL-fable-128k-posture.md`](PROPOSAL-fable-128k-posture.md).
   What *can* be fixed model-independently is the burned-step cost: a step
   that returns with zero tool calls and ≥16k output should be retried with
   a "act now, reason less" continuation rather than a plain retry, and its
   truncated reasoning should not enter the standing context (§3 makes it a
   compounding cost).

### 2.4 What the evidence cannot say

The table records *counts*, not per-step sizes, so it cannot distinguish a
step that stopped at exactly 64,000 (a literal truncation) from a 20k
zero-action step (model verbosity). It also cannot say what the reasoning
was *about*. Both questions need the per-step `stella-events.jsonl`, which
future runs keep and which `exit_cause.py` now annotates for the SIGKILL
subset. The 5-task readiness run in [`../readiness-5task/`](../readiness-5task/)
retains full streams for exactly this reason.

## 3. The 6.4× input gap is context engineering, not configuration

### 3.1 Decomposition

**6.40× = 1.58× more steps × 4.06× more input per step.**

Stella took 6,287 steps to Claude Code's 3,986 — more, but not wildly more,
and partly *why it won* (it keeps grinding). The dominant factor is input per
step: 58,251 tokens/step vs 14,359. No posture knob controls this; it is how
the harness assembles context.

### 3.2 The growth-rate signature

Fitting per-task mean input-per-step against trial length:

```
stella: in/step ≈ 14,025 + 378·steps
claude: in/step ≈  5,922 + 111·steps
```

Stella starts ~2.4× heavier *and grows 3.4× faster*. On short trials
(<50 steps) Stella averages 23k/step to Claude Code's 10k; on long trials
(≥100 steps) 70k/step to 20k. Since total input is per-step context summed
over steps, Stella's total grows ~quadratically with a 3.4× larger
coefficient — which is why the top-10 longest tasks alone carry 46% of the
366M (regex-chess: 27.9M input over 247 steps at 113k/step; Claude Code
solved... well, failed it, in 10 steps and 0.1M).

The mechanism: every tool result, every recalled block, and every burned
cap-hit reasoning dump accrues into the standing context and is re-sent on
each subsequent call. Claude Code's curve says it holds the standing context
roughly flat past ~20k via aggressive compaction/clearing; Stella's says its
compaction (150k budget, keep-recent 8) engages too late and keeps too much.

### 3.3 What it is worth

Holding Stella's own step counts fixed and pricing them at Claude Code's
context curve gives **119M input instead of 366M — a 3.1× reduction from
recall/context engineering alone**, before touching step count, pass rate, or
the posture. The levers, in expected order of yield:

1. **Tool-result retention** — clear or summarize tool outputs older than
   N steps (the single biggest divergence from Claude Code's curve).
2. **Cap-hit debris** — never retain the truncated reasoning of a
   zero-action step (§2.2's 1.74M output becomes recurring *input* every
   step after it happens).
3. **Compaction trigger** — the 150k `compaction_budget_tokens` roughly
   matches where Stella's long-trial average lands (70k mean ≈ 140k tail),
   i.e. it fires only at the very end of exactly the trials it should have
   been shaping from the middle.

### 3.4 Why cost didn't explode with it

Blended input price: Stella $0.21/M vs Claude Code $1.11/M — provider-side
prompt caching absorbs most of the re-sent prefix, which is why 6.4× tokens
became only 1.24× cost. The gap is therefore *not* primarily a bill problem
today; it is latency (105,237s wall vs 73,462s), timeout pressure on the
exact tasks in §2.3's tail, and dependence on a provider discount that a
different arm (first-party API, Fable-class pricing) does not extend at the
same rate.

## 4. The 8 tasks Stella failed and Claude Code passed

The readiness question — "are we ready for the big one" — turns on these,
because they are the asymmetric losses:

| task | Stella's death | class |
|---|---|---|
| `fix-git` | 15 steps, 70s, clean exit, reward 0 | clean-fail |
| `sanitize-git-repo` | 16 steps, 128s, clean exit, reward 0 | clean-fail |
| `openssl-selfsigned-cert` | 25 steps, 195s, clean exit, reward 0 | clean-fail |
| `kv-store-grpc` | 25 steps, 208s, clean exit, reward 0 | clean-fail |
| `fix-code-vulnerability` | 98 steps, 287s, clean exit, reward 0 | clean-fail |
| `constraints-scheduling` | 2 cap hits, NonZeroAgentExitCodeError | ceiling death |
| `dna-assembly` | 4 cap hits, 20.6M input, AgentTimeoutError at 1913s | ceiling death |
| `crack-7z-hash` | AgentTimeoutError at 2047s (compute-bound, 0 cap hits) | timeout death |

The split matters. The three ceiling/timeout deaths are §2/§3's problems and
the posture proposal's territory. The **five clean-fails are pure capability
misses**: Stella finished quickly, said done, and the verifier said no — no
caps, no timeouts, no infrastructure noise. Two are git-shaped (`fix-git`,
`sanitize-git-repo` — the class #1212's pre-agent git baseline targets), and
all five are exactly the class the worker-lifecycle work in #1213/#1214
(judge-context sealing, single recall, persona parity) is meant to move.

That makes them the correct 5-task readiness probe: cheap (all under 300s
and 3.3M input in the failing run), verifier-decided, and each one a task
Claude Code demonstrably passes. The run and its evidence live in
[`../readiness-5task/`](../readiness-5task/).
