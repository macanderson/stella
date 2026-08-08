---
id: self-driving-missions
title: "Self-driving missions — the objective schema, the improvement contract, and the session scorecard"
status: proposed
---

# Self-driving missions

The generic layer that turns self-driving from a perpetual maintenance loop
into the pursuit of a **declared objective**: a mission names the system under
improvement, the dimensions on which "better" is measured, the baseline and
target for each, the budget that bounds the pursuit, and the process rules the
pursuit must obey — and the harness proves every claimed improvement the same
way Stella proves a task done: measured on the old system, measured on the
new, under one pinned protocol, with the verdict a pure function anyone can
recompute from the ledger.

**Status:** **Proposed.** Generalizes the campaign manifest of
`doc:self-driving-foundry` (epic #2081); where the two documents state the
manifest differently, this one wins (§11).
**Date:** 2026-08-08. **Owner:** Mac Anderson.

---

## 1. The gap this closes

Today's self-driving loop is a stewardship engine: fix a batch, audit, file,
bench, ship, repeat (`crates/stella-core/src/self_driving.rs`). It is
deliberately never-terminating and has no destination — "no defects" is a
statement about a lens, not the code. `doc:self-driving-foundry` adds the
destination for one hard-coded case: Stella improving Stella, scored by
arenabench on Terminal-Bench 2.1.

What neither gives us is the general instrument, and the general instrument is
the point. Stella will not always be improving Stella; it will always be
improving *some* system it works in — and every such system already has the
two properties the campaign machinery needs:

- **It can be cloned.** The system under improvement is a git workspace, so a
  branch (materialized as a worktree) is a full-fidelity twin, and the diff
  between twin and base *is* the hypothesis under test.
- **It has some concept of improvement.** Not always arenabench — a customer's
  concept might be a latency benchmark, a test-suite pass rate, an error
  budget. But there is always a procedure that maps "a version of the system"
  to numbers, or there is nothing to optimize.

So the mission layer captures exactly those two universals and nothing else:
a schema for *what improvement means here* (§3–§4), a port for *how it is
measured here* (§5), and a scorecard for *how the pursuit itself is judged*
(§9). Everything downstream — twins, the generation state machine, the
ledger, durability, the promotion gate — is the campaign machinery of
`doc:self-driving-foundry`, unchanged: **a campaign executes exactly one
mission.**

Seven properties, stated as requirements because each is a review criterion:

1. A mission declares one or more **dimensions of improvement** — e.g.
   quality (solve rate), overall cost, token efficiency, speed — each with a
   direction, a target, and a pre-registered smallest creditable step.
2. **Baselines are measured, never asserted.** The operator may claim "this
   sits at 55% today"; the ledger's own generation-0 measurement is the m₀
   every delta is computed against.
3. **Budget, not deadline, bounds the pursuit.** A mission may run for an
   unknowable time — that is the nature of open-ended objectives — but never
   for unbounded spend.
4. **Process rules are declarative.** "Run the panel once; mine the logs for
   behavior X; if it exists, you fail" is expressible in the manifest and
   enforced by the harness, not remembered by the model.
5. **Hypotheses are pre-registered and raced best-of-N.** Several
   implementations of one idea run as sibling twins against the champion on
   the same panel in the same match; only a measured, significant, guard-clean
   lift promotes. Everything else is archived with its evidence.
6. **Improvement is verified, or it is not claimed** — the witness contract
   (fails on old, passes on new) generalized from tests to measurements (§4.4).
7. **The session itself is scored deterministically.** "How is the mission
   going" has one answer, computed by a pure fold over the ledger, identical
   for every reader (§9).

---

## 2. Vocabulary

This table adds to `doc:self-driving-foundry §1`; *twin*, *champion*,
*challenger*, *generation*, *trial*, *promotion*, *suspension*, and *exit*
are used here with exactly the meanings defined there.

| Term | Meaning |
|---|---|
| **mission** | The portable half of a campaign manifest: system, intent, dimensions, evaluator, budget, probes, playbook. What improvement means and what may be spent finding it. |
| **system** | The workspace under improvement — this repository when Stella improves Stella, any other git workspace otherwise. |
| **dimension** | One axis of improvement: a named metric with a direction, an optional target, a smallest creditable step, and a guard tolerance for when some *other* dimension is under test. |
| **evaluator** | The port that maps (twin, panel) → measurements. arenabench is the first adapter; `command:` is the general one (§5). |
| **probe** | A declarative predicate over trial artifacts and traces, with a declared consequence (`warn`, `fail-arm`, `fail-mission`). The enforcement half of "process instructions" (§6). |
| **playbook** | Ordered prose steps injected into PLAN-stage prompts — steering for the model, byte-stable for the life of the mission (invariant 7 discipline). The advisory half of "process instructions". |
| **experiment** | One generation's race: champion + the arms of one or more funded hypotheses, same panel, same protocol, one match. |
| **verified step** | A promotion's evidence: a paired, significant, guard-clean, pre-registered lift on one dimension (§4.4). The unit the scorecard counts. |
| **scorecard** | The `MissionReport` / `GenerationReport` pair — the deterministic answer to "how did that go" (§9). |

---

## 3. The mission manifest

### 3.1 Location and relationship to the campaign manifest

There is **one file**: `.stella/self-driving/campaigns/<name>.toml`, tracked
in git, content-hashed, edit-journaled — everything
`doc:self-driving-foundry §4.1` says. This document re-specifies the blocks
of that file that state intent and measurement (the mission); the blocks that
state execution machinery (`[twins]`, `[exit]`'s non-target predicates,
`[dataset]`, `[training]`, `[durability]`) are unchanged and stay normative
in the foundry document. In a customer workspace the same path convention
applies to *their* repository — the mission travels with the system it
improves, for the same reason `.stella/rules/` does.

### 3.2 The worked example — the operator's own ask

"The dimension of overall answer quality, which sits at 55% resolved on this
dataset running on main today, must improve to 81%. Spend at most $1,500.
Run the panel once and mine the failure shapes before hypothesizing; a trial
where the worker ran on an unpinned model is void."

```toml
schema = 1

[mission]
name = "tb21-quality-81"
system = "."                     # the workspace under improvement (this repo)
intent = """
Raise Terminal-Bench 2.1 resolve rate to 81% without buying it with cost or
latency regressions. Keep only changes whose lift survives the paired gate.
"""

# --- dimensions: what "better" means, one block per axis -------------------

[[dimension]]
name = "quality"
metric = "solve_rate"            # must be declared by the evaluator (§5)
direction = "maximize"
claimed_baseline = 0.55          # advisory; generation 0 measures the real m0
target = 0.81
confidence = 0.95
min_step = 0.04                  # smallest creditable verified step
weight = 0.7                     # budget-allocation prior (§7)

[[dimension]]
name = "cost"
metric = "priced_cost"           # per-task, operational aborts excluded
direction = "minimize"
guard_tolerance = 0.15           # when another dimension is under test
weight = 0.2

[[dimension]]
name = "speed"
metric = "clock_time"
direction = "minimize"
guard_tolerance = 0.25
weight = 0.1

# --- evaluator: how a twin becomes numbers ---------------------------------

[evaluator]
kind = "arenabench"

[evaluator.arenabench]
benchmark = "terminal-bench-2.1" # dataset key, digest-pinned at adoption
tasks = "train-split"            # selection never sees the eval split
attempts = 3                     # repeats — a single pass is not evidence
concurrency = 4
rig = "ec2:i-07d46341dcc9a31b3"

# --- budget: the only hard bound on an open-ended pursuit ------------------

[budget]
usd = 1500                       # at least one axis must be finite (§7)
tokens = 0                       # 0 = unbounded axis
trials = 0
wall_clock_days = 0              # deliberately unbounded — no deadline
per_experiment_usd = 120

# --- process rules: enforced (probes) and advisory (playbook) --------------

[[probe]]
name = "worker-model-pinned"
on = "trial"
query = "builtin:role_model_census"   # step_usage (role, model) join (§6)
on_hit = "fail-arm"

[[probe]]
name = "no-silent-loop"
on = "trial"
query = "builtin:loop_detector_fired"
on_hit = "warn"

[[playbook.step]]
title = "Baseline forensics"
prose = """
Run the panel once before proposing anything. Mine the per-task failure
shapes from the traces; every hypothesis must cite at least one trace.
"""
```

Blocks not shown (`[exit]`, `[twins]`, `[promotion]`, `[dataset]`,
`[training]`, `[durability]`) keep their foundry semantics — except that
`[objective]` is gone (dimensions replace it), `[promotion]`'s metric list is
now derived rather than authored (§4.3), and `[bench]` has become
`[evaluator.arenabench]` (§11).

### 3.3 The same schema, two very different missions

**Token efficiency — "become the cheapest agent on this benchmark":**

```toml
[mission]
name = "tb21-cheapest"
system = "."
intent = "Minimize tokens per solved task; quality is a floor, not a goal."

[[dimension]]
name = "tokens"
metric = "tokens_total"
direction = "minimize"
target_ratio = 0.60              # reach 60% of measured baseline
min_step = 0.05                  # creditable steps of ≥5% of baseline
weight = 0.8

[[dimension]]
name = "quality"
metric = "solve_rate"
direction = "maximize"
guard_tolerance = 0.02           # spend no more than 2 points buying it
weight = 0.2
```

A `minimize` dimension may state its target as `target_ratio` against the
measured baseline instead of an absolute — "cheapest known" is a moving claim
about the world, but "40% cheaper than where we started, quality intact" is a
measurable one, and the scorecard reports the absolute numbers beside it.

**A customer system — not Stella, not arenabench:**

```toml
[mission]
name = "checkout-p95"
system = "."                     # their repository, their .stella/
intent = "Cut checkout p95 latency to 150ms without raising the error rate."

[[dimension]]
name = "latency"
metric = "p95_ms"
direction = "minimize"
claimed_baseline = 220
target = 150
min_step = 10
weight = 1.0

[[dimension]]
name = "errors"
metric = "error_rate"
direction = "minimize"
guard_tolerance = 0.0            # zero regression tolerated

[evaluator]
kind = "command"

[evaluator.command]
run = "scripts/bench/checkout.sh"     # digest-pinned at adoption (§5.3)
metrics = ["p95_ms", "error_rate"]    # declared, then witnessed (§5.2)
attempts = 5
```

Same twins, same ledger, same gate, same scorecard. The only Stella-specific
things in the first example were the adapter block and the metric names.

---

## 4. Dimensions — the improvement contract

### 4.1 Fields

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | Stable handle; ledger records and reports key on it. |
| `metric` | yes | A metric id the evaluator declares (§5.2). Validation refuses an undeclared metric — parity is declared, not assumed (foundry invariant 13). |
| `direction` | yes | `maximize` or `minimize`. There is no `hold` — a dimension you only want held is a guard: give it `guard_tolerance` and no target. |
| `target` / `target_ratio` | one, iff the dimension is a goal | Absolute value, or ratio of the measured baseline. A dimension with neither is a pure guardrail. |
| `confidence` | with a target | The significance the paired gate must reach for target attainment (foundry §5.4 supplies the machinery). |
| `min_step` | with a target | The pre-registered smallest creditable improvement. Anything smaller is noise by declaration, whatever a point estimate says. |
| `claimed_baseline` | no | The operator's belief, recorded for the audit trail. Never an operand (§4.2). |
| `guard_tolerance` | no | Allowed relative regression when *another* dimension is under test. Absent → this dimension does not guard. |
| `weight` | no | Budget-allocation prior in [0,1]; defaults to equal split among goal dimensions (§7). |

### 4.2 Baselines are measured

Generation 0 of every campaign is a **baseline generation**: no challengers,
just the starting champion measured on the eval split and the train split
under the mission's full protocol. Those tables are m₀ — journaled, task-level,
with the repeat spread. Two consequences:

- The scorecard reports `claimed_baseline` beside the measured m₀. A material
  disagreement is journaled as `baseline_disputed` and surfaced in `status` —
  the mission proceeds against the measurement, and the operator learns their
  belief was stale before it costs anything.
- The repeat spread at generation 0 is the **noise floor estimate**.
  `campaign validate` (already in foundry §4.1) gains a check: if `min_step`
  is below what the configured panel size and attempts can certify at
  `confidence` — the sample-floor and significance rules of
  `self_tuning::select_winner` — validation warns with the smallest
  certifiable step, and adoption journals the same warning. A five-task panel
  cannot resolve anything smaller than a catastrophe; the manifest should
  find that out at validation time, not after four generations of
  inconclusive verdicts.

### 4.3 Every other dimension guards by default

The foundry manifest's `[promotion] primary + guards` list was a copy of
information the dimensions already carry, and a copy is a drift cell. Derived
instead:

- An experiment's **primary** is its hypothesis's predicted dimension (§8.1).
- Its **guards** are every other dimension that declares a `guard_tolerance`.
- `GuardBlocked` remains a loss, exactly as foundry §5.4 rules — a quality
  win bought with a blown cost guard does not promote, and the verdict says
  which guard blocked it.

So the operator states the trade-off space once, as dimensions, and every
experiment inherits it. "Improve tokens subject to quality" and "improve
quality subject to cost" are the same manifest with different hypotheses
funded — not two manifests.

### 4.4 The improvement witness

Stella's definition of done is a test that fails on the old code and passes
on the new (`doc:witness-protocol`). A mission holds improvements to the
same shape, generalized from tests to measurements. A **verified step** on
dimension D requires all of:

1. **Fails on old / passes on new, as measurement.** Champion measured m₀ and
   twin measured m₁ under the *same* protocol — same panel, same attempts,
   same evaluator digest, comparability keys equal in everything but the
   declared delta (foundry invariant 3).
2. **Paired and significant.** The lift clears `select_winner`'s gate at the
   dimension's `confidence`, on paired tasks only (foundry §5.4).
3. **Guard-clean.** No guarding dimension regresses past its tolerance.
4. **Pre-registered.** D is the dimension the hypothesis predicted, and the
   lift ≥ D's `min_step` (foundry invariant 11).
5. **Trace-joined and probe-clean.** The verdict survives the trace join, and
   no `fail-arm` probe fired (foundry §5.4, §6 here).

Nothing else is ever called an improvement, anywhere in the ledger, the
scorecard, or `status`. The symmetry with the task-level contract is the
point: the same skepticism Stella applies to "I finished your feature" now
applies to "I made myself better."

---

## 5. The evaluator port

### 5.1 The port

Measurement is a port, not a concretion (invariant 1). The pure plane sees
only its output:

```rust
// stella-core/src/self_driving/mission.rs — types only; the trait's
// implementations live with the I/O in stella-cli.
pub struct Measurement {
    pub twin: TwinId,
    pub panel_digest: String,
    pub attempts: u32,
    /// metric id → per-task samples. Verdicts consume samples, never means:
    /// pairing and significance need the distribution.
    pub samples: BTreeMap<String, Vec<TaskSample>>,
    /// Operational aborts: excluded from every denominator, named here.
    pub aborts: Vec<AbortRecord>,
}
```

Two adapters ship first, and the set is open:

| `kind` | What it wraps | Metrics declared |
|---|---|---|
| `arenabench` | A match via the existing arena machinery: per-seat `sut_ref` pins each twin (landed — `Contestant.sut_ref` in `arenabench/arenabench/model.py`), trial artifacts and `stella-events.jsonl` come back as today. | `solve_rate`, `priced_cost`, `clock_time`, `tokens_total` — computed by the `TrialMetrics → ArmTrials` bridge of foundry Phase 0, from the trace, not the surface summary. |
| `command` | Any executable in the system's own repository. | Whatever the manifest declares and the witness run confirms (§5.2). |

### 5.2 Declared metrics — parity, not assumption

A dimension naming a metric nothing measures must be a validation failure,
not a runtime surprise. The `arenabench` adapter declares its metric set in
code, like a provider declares its cache posture (AGENTS.md invariant 8). A
`command` evaluator cannot declare in code, so it declares in the manifest
(`metrics = [...]`) and is **witnessed at adoption**: generation 0's first
invocation must produce exactly the declared ids, or adoption fails closed
with the diff. Declared-then-witnessed, the same two-step the provider
parity matrix uses.

### 5.3 The command contract

Deliberately a contract over an executable, not a plugin API — evaluation
stacks churn faster than this repository, which is the same reasoning as the
trainer port (foundry §8.1):

```text
run <script> with:
  STELLA_MISSION_WORKSPACE=<twin worktree>     # the system to measure
  STELLA_MISSION_OUT=<artifact dir>            # where evidence lands
  STELLA_MISSION_ATTEMPT=<n>                   # repeat ordinal, 1-based

it must write:  $STELLA_MISSION_OUT/metrics.json   # {metric_id: number}
exit 0        → samples ingested
exit nonzero  → operational abort: excluded from denominators, named in the
                report, retried per the suspension machinery — never scored
                as a regression (foundry invariant 6)
```

The harness owns repeats, pairing, and panel identity; the script only ever
measures one attempt. That division is what keeps the statistics in one
audited place instead of re-implemented per customer.

Pinning: the script's digest (and its declared metric list) is journaled at
adoption and re-checked per generation. Which raises the obvious attack:

### 5.4 Protocol tamper exclusion

A twin whose diff touches the evaluator script, the probe scripts, the panel
definition, or the mission manifest itself is **quarantined, not measured** —
the pipeline's witness tamper exclusion (`doc:verification-gate`), applied
one level up. The optimizer must never be able to improve its number by
editing the ruler. The excluded path set is computed at adoption from the
manifest (evaluator `run`, every `command:` probe, the manifest path) and
journaled with it; a legitimate protocol change is an operator edit to the
manifest, which is already hash-journaled and re-validated (foundry §4.1),
never a challenger's diff.

---

## 6. Probes and the playbook — process instructions

The operator's process rules split into two kinds, and the split is the
design: what a machine can check is **enforced**, what only a model can
follow is **steered**.

### 6.1 Probes (enforced)

```toml
[[probe]]
name = "worker-model-pinned"
on = "trial"            # trial | generation | mission
query = "builtin:role_model_census"
on_hit = "fail-arm"     # warn | fail-arm | fail-mission
```

- `builtin:` predicates are exactly the trace checks foundry §5.4 already
  mandates, exposed as nameable rules: `role_model_census` (every
  `step_usage` row matches the seat's pinned models), `loop_detector_fired`,
  `verdict_trace_disagreement`, `operational_abort:<class>`. The v1 catalog
  is deliberately this small — each builtin is a check that already exists,
  not new machinery.
- `command:` predicates are the escape hatch, under the §5.3 contract shape:
  the script gets `STELLA_MISSION_TRIAL_DIR` (artifacts + trace), exits 0 for
  clean and 1 for hit, and is digest-pinned and tamper-excluded like the
  evaluator. "Mine the logs for this behavior; if it exists, you fail" is a
  twelve-line script, not a feature request.
- Consequences: `warn` journals and continues; `fail-arm` voids the arm's
  trials for that experiment (the arm loses, the experiment proceeds);
  `fail-mission` suspends the campaign with the probe named — an operator
  decision is required, because a mission-level probe firing means the
  process itself is broken, and spending budget past that point is waste.
- Every firing is a ledger record: probe name, query digest, trial/generation
  coordinates, evidence pointer. Probe outcomes appear on the scorecard (§9).

### 6.2 The playbook (advisory)

Ordered prose steps, rendered verbatim into the PLAN stage's prompt preamble
alongside `intent` — fixed for the life of the mission, so the prompt prefix
stays byte-stable (invariant 7). The playbook steers hypothesis generation
("baseline forensics first", "prefer config-axis experiments until the first
promotion"); it is never consulted by a verdict. A playbook step the model
ignores costs experiments, which the funnel makes visible (§9) — that is the
correct enforcement mechanism for advice.

---

## 7. Budget

The budget block is the only bound on a mission's duration, by design — the
operator's ask was explicitly "no time limit, but resources are finite."

- **Axes:** `usd`, `tokens`, `trials`, `wall_clock_days`; `0` means
  unbounded. Validation requires **at least one finite axis** — an unbounded
  pursuit on every axis is a runaway, not a mission.
- **Spend is a ledger fold** (foundry §6.1's `SpendLedger`), attributed per
  stage, per experiment, and — via the hypothesis's predicted dimension —
  per dimension. `BUDGET_EXHAUSTED` stays a foundry exit predicate; it now
  fires on any finite axis.
- **Safe boundaries only.** Budget is consulted between stages and between
  experiments, never mid-trial — the engine-level rule (AGENTS.md
  invariant 6) applied at campaign scale. A trial in flight when an axis
  exhausts completes and is scored; nothing new is funded after.
- **Weights shape allocation.** The FUND decision (§8.2) spends each
  generation's `per_experiment_usd` slots across funded hypotheses in
  proportion to their dimensions' `weight` priors, adjusted by the ledger's
  observed yield (a dimension whose hypotheses keep refuting gets cheaper
  probes, not more of the same). The allocator is a pure function of
  (manifest, ledger) with lexicographic tie-breaks — deterministic,
  property-testable, and dull, which is what an auditor wants an allocator
  to be. Anything cleverer (bandits, expected-information-gain) is an open
  question (§13), and must keep the same purity contract.

---

## 8. Hypotheses and best-of-N experiments

### 8.1 Pre-registration

The hypothesis card is foundry §5.1's, with the prediction made load-bearing
and the arms made plural:

```json
{
  "id": "h-2026-08-08-loop-stagnation",
  "hypothesis": "the stagnation rung fires too late on long-horizon tasks",
  "predicted": { "dimension": "quality", "min_step": 0.04 },
  "evidence": ["trace:exec-841#step-19", "issue:2210"],
  "axis": "code",
  "arms": [
    { "name": "a-earlier-rung",  "ref": "worktree-h1-earlier" },
    { "name": "b-adaptive-rung", "ref": "worktree-h1-adaptive" },
    { "name": "c-two-signal",    "ref": "worktree-h1-two-signal" }
  ]
}
```

- A hypothesis **must** predict a dimension before it can be funded, and the
  JUDGE stage credits **only** that dimension, at no less than its
  `min_step`. An off-dimension effect observed mid-experiment is evidence for
  a *new* hypothesis card, never a win for this one (foundry invariant 11).
  This is the anti-p-hacking rule: an optimizer that may claim whatever
  moved will always find something that moved.
- **Best-of-N is arms on one card.** Several implementations of one idea run
  as sibling twins — champion + N arms, same panel, same attempts, one match
  (per-seat `sut_ref` makes this one match, not N). The best surviving arm
  is the promotion candidate; the others are archived with their deltas on
  the card. This is the operator's "implement several versions on different
  branches and race them" made structural.
- **Dedup by digest.** Cards are deduplicated by a normalized digest of
  their hypothesis text (the `finding_digest` discipline the loop already
  uses for findings), checked against every prior card in the ledger —
  including refuted ones. A refuted hypothesis is not re-funded on the same
  evidence; re-proposing it requires new evidence cited on the card. The
  refuted set is knowledge, and it is the cheapest knowledge the mission
  produces.

### 8.2 Where hypotheses come from

The PLAN stage generates cards from the mission's evidence sources — and
this is where the existing self-improvement machinery plugs in, each lever
becoming a hypothesis family rather than a free-running loop:

| Lever | Existing machinery | As a hypothesis |
|---|---|---|
| Trace lessons | trace capture + reward labels (#1042/#1043); formation certified by `doc:trace-replay-learning-harness` | Failure shapes mined from prior generations' traces → `code`-axis cards (the playbook's "baseline forensics"). |
| Context records | `.stella/rules/` + `stella context keep/promote` (`doc:context-pr`) | A proposed record is a **branch diff** (rules are tracked), so a steering change races like any code change: `config`/`code`-axis cards. |
| Memories / skills | reflection mining, skill auto-creation | Ride the twin's **config overlay** (they are not git-tracked); overlay deltas are digest-pinned in the twin id, so the race stays honest. |
| Tool foundry | `detect_tool_gaps` + the foundry witness/gate | "The system lacks tool X" → an arm whose overlay adopts and enables the gated tool; the reuse fold supplies the evidence line. |
| The backlog | `gh` reads the loop already does | An open issue predicting a dimension is a card waiting for evidence. |

The levers keep their own gates — a record still promotes through
`doc:context-pr`, a tool still needs its flip witness. The mission adds the
missing outer question every lever shares: *did adopting this actually move
a number we declared we care about?* — answered by the same paired gate as
everything else.

### 8.3 What a losing experiment buys

A refuted card is journaled with its measured deltas, its trace pointers, and
its arms' diffs; the branch is archived, never deleted (foundry §5.4). The
HARVEST stage already folds divergent-outcome siblings into preference pairs
(foundry §7.2) — so a lost race is training signal, a refuted hypothesis is a
dedup entry, and the scorecard's funnel counts both. "Only keep what has
impact" cuts code, not evidence.

---

## 9. The scorecard — measuring the session itself

### 9.1 Two reports, one fold

Both are pure functions of (manifest, ledger) in
`stella-core/src/self_driving/mission.rs`, rendered by `status`, journaled at
GATE, and byte-identical however many times they are recomputed — the same
projection discipline as the rest of the plane, with the same rule: the
ledger is ground truth and the report is a view of it.

**`GenerationReport`** — what one turn of the loop bought: experiments run,
per-arm deltas with verdicts, probe firings, spend, and the one-line answer
("promoted b-adaptive-rung: quality +0.05 [0.02, 0.08], guards clean" / "no
promotion: both arms refuted").

**`MissionReport`** — where the pursuit stands:

```json
{
  "mission": "tb21-quality-81",
  "manifest_hash": "…",
  "verdict": "advancing",
  "dimensions": [{
    "name": "quality",
    "claimed_baseline": 0.55,
    "measured_baseline": { "value": 0.52, "spread": 0.03, "generation": 0 },
    "current": 0.63,
    "target": 0.81,
    "verified_steps": [
      { "generation": 4, "hypothesis": "h-…-loop-stagnation",
        "arm": "b-adaptive-rung", "delta": 0.05, "commit": "…" },
      { "generation": 9, "hypothesis": "h-…-recall-gate",
        "arm": "a-strict", "delta": 0.06, "commit": "…" }
    ]
  }],
  "funnel": { "proposed": 31, "funded": 14, "confirmed": 2,
              "refuted": 9, "inconclusive": 3 },
  "spend": { "usd": 812.40, "per_verified_step_usd": 406.20,
             "by_dimension": { "quality": 685.15, "cost": 127.25 } },
  "probes": { "warn": 3, "fail_arm": 1, "fail_mission": 0 },
  "aborts": { "excluded": 4, "classes": { "provider_credit": 3, "rig_unreachable": 1 } }
}
```

Note what the example encodes: the claimed 55% measured at 52%, disputed and
survived; current position is the *sum of verified steps from the measured
baseline*, never a lone re-measurement (each step was paired; a raw re-read
of the champion would reintroduce exactly the unpaired noise the gate
exists to exclude); and the funnel's refuted column is reported as plainly
as the confirmed one.

### 9.2 The verdict taxonomy

Computed, never vibes: `achieved` (every targeted dimension at target at
confidence on the eval split, sustained two MEASUREs — foundry
`TARGET_REACHED` generalized to all-targets), `advancing` (≥1 verified step
within the plateau window), `plateaued` (none within it — the
`MODEL_CONSTRAINED` predicate counting down), `suspended` (a named
suspension holds, distance unchanged), `exited(reason)` (a foundry exit
record: target / plateau / budget / operator). `status` always shows
distance-to-target per dimension, spend against every finite axis, and
generations-since-last-step against the plateau window — the "are we there,
and is it still worth it" readout.

### 9.3 Sessions, and the meta-number

A *session* — one supervised span between adoption/resume and
suspension/stop — contributes a contiguous slice of ledger records under one
lease; its report is the fold of that slice, so "what did tonight's run
accomplish" and "where does the mission stand" are the same code path over
different ranges. And across missions, one number survives aggregation
honestly: **spend per verified step**, per dimension. That is the number the
`evolve` phase should mine when self-driving turns its instruments on its own
process — the mission ledger is a trace like any other, and "improving the
concept of improvement" is a mission whose system is the mission machinery.
This spec keeps that recursion in scope as evidence (the scorecards
accumulate) and out of scope as automation (§13).

---

## 10. What is deterministic — stated honestly

The claim "measure improvement deterministically" has to be precise about
where the nondeterminism lives, because a model drives the middle of this
loop and pretending otherwise would be the flattering kind of measurement
error this repository treats as worse than a loss.

| Layer | Guarantee | Mechanism |
|---|---|---|
| Hypothesis generation, implementation | **None.** Model-driven, deliberately creative. | Contained by pre-registration, `min_step`, dedup-with-refuted, and the funnel making waste visible. |
| Measurement | **Pinned and repeated**, not deterministic — tasks are stochastic. | Digest-pinned protocol, paired same-match arms, declared attempts, aborts excluded and named, trace-joined verdicts. |
| Verdicts, funding, exits, reports | **Deterministic.** Pure functions of (manifest, ledger); property-tested; two readers cannot disagree. | The core mission module; `select_winner`; the ledger fold. |

The scientist is stochastic; the method is not. What the mission layer
promises is exactly what a rigid scientific process promises: not that every
attempt succeeds, but that no claim survives without evidence, the evidence
protocol is pinned before the attempt, and anyone can re-derive every verdict
from the record.

---

## 11. Reconciliation with doc:self-driving-foundry

The campaign spec landed first and remains the normative home for twins, the
generation state machine, the ledger and durability, the data plane, the
training loop, and the exit machinery. This document re-bases its manifest's
intent-and-measurement blocks:

| Foundry block | Becomes | Why |
|---|---|---|
| `[objective]` (single metric/target/confidence/attempts) | `[[dimension]]` (§3–§4); attempts move to the evaluator | One metric cannot say "improve tokens *subject to* quality"; the operator's missions are multi-dimensional from the first example. |
| `[promotion] primary + guards` | Derived from dimensions (§4.3); `min_paired_tasks`, `exclude_operational_aborts` stay authored | The guards list duplicated the dimensions — a drift cell. |
| `[bench]` | `[evaluator]` + `[evaluator.arenabench]` (§5) | The measurement machinery becomes a port with a second adapter; the arena fields survive verbatim inside the adapter table. |
| `[exit] target_reached` | All targeted dimensions at target (§9.2) | Multi-target missions. |
| (new) | `[[probe]]`, `[[playbook.step]]`, `[budget]` axes + weights | §6, §7. |

Because nothing has shipped against `schema = 1`, this is a design-time
re-base, not a migration — the same PR that lands this document adds a
pointer note to the foundry spec's §4.2 and appends invariants 11–13 to its
list (pre-registration bounds credit; baselines are measured, never
asserted; a dimension's metric is declared by the evaluator and validated).
Append-only, per that list's own rule.

---

## 12. Routing and sequencing

Placement follows the house split (AGENTS.md invariants 1–2), matching how
foundry placed the campaign machinery:

- **`stella-core/src/self_driving/mission.rs`** — pure: manifest types and
  validation (dimensions, budget axes, probe/playbook shapes, declared-metric
  parity), `Measurement`/verdict types, the improvement-witness check, the
  allocator, the report folds. Serde round-trip tests for every type that
  crosses the core/CLI boundary (invariant 4); property tests beside the
  existing `self_driving` ones.
- **`stella-cli/src/self_driving_cmd/mission.rs`** — I/O: manifest load,
  evaluator adapters (`arenabench` shell-out, `command`), probe execution,
  tamper-set computation, ledger append.
- **arenabench** — no further changes beyond foundry Phase 0's bridge; the
  per-seat pin it needs is already merged.

Sequencing slots into epic #2081 rather than beside it — the mission schema
is what its Phase 1 should implement as "the manifest":

| PR | Scope | Witness |
|---|---|---|
| 1 | Mission types + `validate` + report folds (pure core) | round-trips byte-for-byte; validation refuses each of: undeclared metric, all-axes-unbounded budget, `min_step` absent on a targeted dimension; `MissionReport` folds a synthetic ledger to the §9.1 example byte-for-byte, twice |
| 2 | Evaluator port + `command` adapter + declared-then-witnessed check | a fixture script's metric drift fails adoption with the diff; a nonzero exit lands as a named abort, never a sample |
| 3 | Probes + protocol tamper exclusion | a `fail-arm` probe voids exactly one arm's trials; a twin whose diff touches the evaluator script is quarantined with the ledger record naming the path |
| 4 | Wire into campaign Phases 1–2 as they land: FUND consumes the allocator, JUDGE consumes the improvement witness, generation 0 measures baselines | the §3.2 mission runs one generation end-to-end against a rigged fixture evaluator, deterministically, and its two reports match golden files |

PRs 1–3 are independent of the campaign ledger work and can land first; PR 4
is the junction and depends on foundry Phase 1.

---

## 13. Open questions (decisions the maintainer owns)

1. **Allocator sophistication.** The v1 weighted allocator is deliberately
   dull. Bandit-style allocation by observed yield-per-dollar is a natural
   successor — worth it only once a few missions' funnels show the dull one
   leaving real money on refuted families, and any successor keeps the
   purity contract (§7).
2. **Pareto missions.** v1 races one predicted dimension per hypothesis with
   the rest as guards. A true multi-objective gate (dominance over the
   dimension vector) is well-defined but needs more samples than a panel
   affordably gives; defer until a mission genuinely cannot be expressed as
   goal-plus-guards.
3. **Probe language.** `builtin:` + `command:` is deliberately not a query
   DSL. If probe scripts proliferate into copies of each other, that is the
   evidence a jq-shaped trace query language would need — collect it first.
4. **Governance surface.** Should mission manifests join `stella context
   validate`'s CI check as a governed record kind (foundry open question 5,
   now sharpened: the mission is tracked steering, exactly what
   `doc:context-pr` governs)?
5. **The meta-mission.** §9.3 keeps "improve the improvement process" manual
   — the evolve phase reads scorecards. Automating it (a mission whose
   evaluator scores mission machinery on spend-per-verified-step) is
   coherent but should wait for a corpus of real scorecards to calibrate
   against.

---

## 14. Related

- `doc:self-driving-foundry` — the campaign machinery this layer rides:
  twins, ledger, durability, data plane, training loop, exits. Epic #2081.
- `doc:witness-protocol`, `doc:verification-gate` — the task-level done
  contract §4.4 generalizes, and the tamper-exclusion posture §5.4 borrows.
- `doc:trace-replay-learning-harness` — certifies the formation half of the
  learning levers §8.2 turns into hypothesis families (epic #2304).
- `doc:context-pr` — the governance path a record-flavored hypothesis still
  promotes through; candidate governance model for missions themselves.
- `doc:agent-monitor-protocol` — the supervision channel a running campaign
  session reports through.
- #830/#832/#834/#835/#836 — the self-improvement track; #1046 (train-cycle
  automation) becomes foundry Phase 4's core; #1065 (shadow-first
  experiments) and #1066 (prompt/routing arms) are `config`-axis hypothesis
  families under §8.2.
