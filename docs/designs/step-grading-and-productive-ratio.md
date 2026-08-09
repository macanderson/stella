---
id: step-grading-and-productive-ratio
title: "Step grading and the agent productive step ratio"
status: proposed
---

# Step grading and the agent productive step ratio

## 0. Summary

A pipeline that reads the transcripts Stella already writes for benchmark
runs — Terminal-Bench/ArenaBench trials ingested by
`bench/telemetry_store/ingest.py`, and any other run recorded the same way —
and grades every step, turn, and execution along two axes: **time/cost**
(wall clock, model time, USD) and **direction** (did this step move the task
toward done, away from it, or sideways). Grades land in three new tables,
`step_grades`, `turn_grades`, and `execution_grades`, joinable back to
`trials`/`events` by the same keys those tables already use. The headline
metric this unlocks is the **agent productive step ratio**:
`productive_steps / total_steps`, computed at all three grains.

Grading is deterministic wherever the artifacts already on disk settle the
question — a failing test that starts passing, a diff that regresses code the
final patch keeps, a retry budget exhausted — and only escalates to a
model-judged fallback for the residue no rule resolves. See
doc:agent-monitor-protocol for the precedent this follows (agent-agnostic
detections layered over per-agent telemetry) and doc:pipeline-journey for the
stage vocabulary steps are graded against.

## 1. Problem statement

`bench/telemetry_store` answers "did the trial pass, and what did it cost" —
one row per trial, aggregated. It cannot answer "of the 40 steps this agent
took, how many were pointed at the goal", because nothing today reduces a
step to a direction. That question matters for three reasons:

- **Comparing agents at equal outcome.** Two trials can both pass with
  identical `cost_usd_norm` while one agent took a direct path and the other
  thrashed — three failed greps, a revert, a second attempt — and arrived at
  the same patch by a costlier route that the top-line numbers cannot see.
- **Diagnosing a failed trial.** `verifier_tests` says which assertions
  failed; it does not say whether the agent's steps were *trending toward* a
  fix and ran out of budget, or drifted from the first step onward.
- **Attributing spend to progress.** `trials.cost_usd_norm` and
  `wall_seconds` are undifferentiated totals. Model time embedded in a step
  that regressed the diff is spend with no return; today it is
  indistinguishable from model time that fixed the bug.

Today's schema and readers (`telemetry.py`, `transcript.py`, `render.py`) all
reduce or replay the event stream; none of them *judges* a step. This design
adds the judgment layer without touching any of them.

## 2. Goals and non-goals

**Goals**

- Grade every step, turn, and execution (trial) already recorded by the
  existing telemetry pipeline, at the same trial-level granularity
  `bench/telemetry_store` uses today.
- Prefer deterministic signals already present in `events`/`tool_calls`/
  `verifier_tests` over model judgment; only fall back to a grading agent
  when no deterministic rule resolves a step, and record which path was used
  on every row.
- Define and compute the **agent productive step ratio**
  (`productive_steps / total_steps`) at the step-window, turn, and execution
  grain, with the exact denominator convention pinned so two runs are
  comparable.
- Record wall-clock, model time, and cost per graded unit, keyed to the
  run's own price-clocked timestamps (`StampedEvent.ts`, origin-anchored —
  see §4), so a grade and the transcript line it judges can be paired inline
  in one join.
- Keep grading re-runnable and versioned: an improved ruleset produces a new
  row set (`grader_version`), never an overwrite, matching `runs.void_reason`
  and `trials.cost_norm_status`'s keep-the-history-of-what-was-wrong posture.

**Non-goals**

- Not a live in-loop critic. Grading runs against a trial's artifacts after
  the fact (or on a finished prefix), the same "replay, never perturb"
  posture `transcript.py`'s module docstring states for the existing reader.
  It does not steer a running agent.
- Not a replacement for `verifier_tests` or `trials.reward`/`passed`. Pass/
  fail is still the benchmark's own grader's verdict; step grading is a
  complementary *process* signal, and `execution_grades.verdict_alignment`
  is explicitly a sanity cross-check, never a scoring input back into pass/
  fail.
- Not a new wire format. No change to `AgentEvent`, `StampedEvent`, or the
  stream-json contract. Everything graded is read from what a trial already
  wrote.
- Not scoped to ArenaBench specifically. The design reads `trials`/`events`
  as ingested by `bench/telemetry_store`; any producer that ingests into
  that schema is gradable, including future non-benchmark Stella sessions,
  provided they are ingested the same way.

## 3. Data model

Three tables, additive to `bench/telemetry_store/schema.sql`, following its
existing rules verbatim: valid on SQLite and Postgres (no `AUTOINCREMENT`, no
`BOOLEAN`, integers for 0/1, `TEXT` timestamps in ISO-8601), identifiers
supplied by the grader rather than generated by the engine, and a
`*_status`-style column wherever a numeric figure can be legitimately absent
— mirroring `trials.cost_norm_status`'s rule that an absent measurement must
never render as a computed zero.

```sql
-- One row per graded step-window: the span between two adjacent StepUsage
-- (or step-shaped) events for one (trial, turn_instance, step, call_index).
-- `call_index` disambiguates auxiliary calls sharing a step the same way
-- StepManifest's own key does (the worker call is 0; triage/plan/verifier/
-- compaction calls riding the same step take 1, 2, ...) so a step with three
-- model calls grades as three rows, not one averaged row.
CREATE TABLE IF NOT EXISTS step_grades (
    step_grade_id       TEXT PRIMARY KEY,
    trial_id             TEXT NOT NULL REFERENCES trials(trial_id) ON DELETE CASCADE,
    turn_instance        INTEGER NOT NULL,
    step                 INTEGER NOT NULL,
    call_index           INTEGER NOT NULL DEFAULT 0,
    -- The event range this grade covers, inclusive, into events(seq) for the
    -- same trial_id. This is the join key a report uses to pair a grade with
    -- the transcript lines it judged -- see section 7.
    event_seq_start      INTEGER NOT NULL,
    event_seq_end        INTEGER NOT NULL,
    -- Price-clocked timestamps: milliseconds from the trial's own first
    -- stamped event (StampedEvent.ts), the same origin transcript.py anchors
    -- TranscriptState.origin_ms to. Never wall-clock read time -- a trial
    -- read live and one read six months later must grade identically.
    ts_start_ms           INTEGER,
    ts_end_ms             INTEGER,
    wall_clock_ms         INTEGER,     -- ts_end_ms - ts_start_ms
    -- Time actually inside the provider call (StepUsage.duration_ms summed
    -- over this window) -- distinct from wall_clock_ms, which also counts
    -- tool execution between calls.
    model_time_ms         INTEGER,
    cost_usd              REAL,
    -- Mirrors trials.cost_norm_status: 'priced' is the only state in which
    -- cost_usd means anything; a query that aggregates it must filter here.
    cost_norm_status      TEXT NOT NULL DEFAULT 'unmigrated',
    tool_calls_count      INTEGER NOT NULL DEFAULT 0,
    -- 'productive' | 'unproductive' | 'neutral'. Neutral is a real verdict,
    -- not a default -- a step with no measurable effect (restating a plan,
    -- a read-only lookup that changes nothing downstream) is neither credit
    -- nor blame.
    direction             TEXT NOT NULL,
    -- 'deterministic' | 'agent_judge' | 'escalated_human'. Which path in
    -- section 5/6 produced this row -- never inferred from direction alone.
    direction_source      TEXT NOT NULL,
    -- The specific rule or judge note that fired, e.g. 'verifier_delta:+2',
    -- 'loop_detected', 'agent_judge:low_confidence'. Short, machine-parseable,
    -- so a report can group by reason without re-deriving it.
    direction_reason      TEXT,
    -- 1.0 for every deterministic rule (they are exact by construction);
    -- the agent judge's self-reported confidence otherwise. NULL only for
    -- escalated_human rows pending a decision.
    confidence            REAL,
    grader_version        TEXT NOT NULL,
    graded_at             TEXT NOT NULL
);

-- One grader_version's grade for a given step-window is a single fact,
-- never duplicated by re-running the same grader over the same trial.
CREATE UNIQUE INDEX IF NOT EXISTS step_grades_identity_idx
    ON step_grades(trial_id, turn_instance, step, call_index, grader_version);
CREATE INDEX IF NOT EXISTS step_grades_trial_idx ON step_grades(trial_id);
CREATE INDEX IF NOT EXISTS step_grades_turn_idx  ON step_grades(trial_id, turn_instance);

-- One row per (trial, turn_instance, grader_version): the roll-up of that
-- turn's step_grades. A turn is Stella's own unit (one run_turn / one
-- LoopDetected turn_instance), matching the key StepManifest and
-- LoopDetected already carry on the wire.
CREATE TABLE IF NOT EXISTS turn_grades (
    turn_grade_id         TEXT PRIMARY KEY,
    trial_id               TEXT NOT NULL REFERENCES trials(trial_id) ON DELETE CASCADE,
    turn_instance           INTEGER NOT NULL,
    ts_start_ms              INTEGER,
    ts_end_ms                INTEGER,
    wall_clock_ms             INTEGER,
    model_time_ms             INTEGER,     -- sum of step_grades.model_time_ms
    cost_usd                  REAL,        -- sum of step_grades.cost_usd
    cost_norm_status          TEXT NOT NULL DEFAULT 'unmigrated',
    step_count                INTEGER NOT NULL DEFAULT 0,
    productive_steps          INTEGER NOT NULL DEFAULT 0,
    unproductive_steps        INTEGER NOT NULL DEFAULT 0,
    neutral_steps             INTEGER NOT NULL DEFAULT 0,
    -- productive_steps / step_count. NULL when step_count = 0 (never 0/0 --
    -- see section 8 for why a division that cannot happen must not silently
    -- render as a comparable zero).
    productive_step_ratio     REAL,
    -- Mirrors how the turn ended on the wire: 'complete' (AgentEvent::
    -- Complete), 'error' (AgentEvent::Error), 'loop_aborted'
    -- (LoopDetected{aborted:true}), or 'in_progress' for a prefix graded
    -- before the turn closed.
    outcome                   TEXT,
    grader_version             TEXT NOT NULL,
    graded_at                  TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS turn_grades_identity_idx
    ON turn_grades(trial_id, turn_instance, grader_version);
CREATE INDEX IF NOT EXISTS turn_grades_trial_idx ON turn_grades(trial_id);

-- One row per (trial, grader_version): the roll-up of that trial's whole
-- execution -- every turn it ran. "Session" in the sense the task uses it is
-- this row: one trial's complete attempt, start to finish or to whatever
-- prefix was graded.
CREATE TABLE IF NOT EXISTS execution_grades (
    execution_grade_id          TEXT PRIMARY KEY,
    trial_id                     TEXT NOT NULL REFERENCES trials(trial_id) ON DELETE CASCADE,
    -- Denormalized from trials.run_id so a cross-run rollup (e.g. "productive
    -- ratio by model, across every run this month") is one scan of this
    -- table rather than a join for every query.
    run_id                       TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
    ts_start_ms                   INTEGER,
    ts_end_ms                     INTEGER,
    -- Derived independently from the graded events, not copied from
    -- trials.wall_seconds -- the two are expected to agree and a report that
    -- shows both is a cheap cross-check on the ingester and the grader
    -- agreeing about where a trial started and ended.
    wall_clock_ms                 INTEGER,
    model_time_ms                 INTEGER,
    cost_usd                      REAL,
    cost_norm_status              TEXT NOT NULL DEFAULT 'unmigrated',
    turn_count                    INTEGER NOT NULL DEFAULT 0,
    step_count                    INTEGER NOT NULL DEFAULT 0,
    productive_steps              INTEGER NOT NULL DEFAULT 0,
    unproductive_steps            INTEGER NOT NULL DEFAULT 0,
    neutral_steps                 INTEGER NOT NULL DEFAULT 0,
    -- THE metric: productive_steps / step_count over the whole execution.
    -- NULL when step_count = 0, same rule as turn_grades.
    agent_productive_step_ratio   REAL,
    -- Does the ratio's trend agree with trials.passed? One of 'consistent'
    -- (high ratio + passed, or low ratio + failed), 'discordant' (passed
    -- despite a low ratio, or failed despite a high one -- both worth a
    -- human look), or NULL when trials.passed is itself NULL/unjudged.
    -- Explicitly a sanity signal (goal 3/non-goal 2) -- never fed back into
    -- pass/fail.
    verdict_alignment              TEXT,
    grader_version                  TEXT NOT NULL,
    graded_at                       TEXT NOT NULL,
    notes                           TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS execution_grades_identity_idx
    ON execution_grades(trial_id, grader_version);
CREATE INDEX IF NOT EXISTS execution_grades_run_idx ON execution_grades(run_id);
```

Nothing here duplicates `events`/`tool_calls`/`verifier_tests`: those stay
the single source of raw fact, and `*_grades` rows are always derivable from
them plus the trial's own artifacts. Deleting every `*_grades` row and
re-running the grader must reproduce the same table, given the same
`grader_version` — the same replay guarantee `transcript.py` already gives
the UI.

## 4. Ingestion and join strategy

Grading is a **second pass over an already-ingested trial**, not a change to
`bench/telemetry_store/ingest.py`'s ingestion path. It runs after
`ingest.py` has populated `trials`/`events`/`tool_calls`/`verifier_tests` for
a run, reads those tables (never the raw JSONL directly — ingestion already
did the parsing and the drop-list filtering in `DROP_EVENT_TYPES`), and
writes only into the three `*_grades` tables. Concretely:

```
python3 grade.py --db bench.db --run TAG [--grader-version vN]
```

**Step windowing.** A step-window's boundary events are read from `events`
where `type = 'step_usage'` (payload mirrors `AgentEvent::StepUsage`,
carrying `step`, `role`, `duration_ms`, `cost_usd`, `tool_calls`,
`finish_reason`) plus `type = 'file_change'`/`'tool_start'`/`'tool_result'`/
`'verdict'` rows that fall inside `(event_seq_start, event_seq_end]` for that
step. `turn_instance` comes along for the ride on every event that carries
it (`step_usage`, `loop_detected`, `retries_exhausted`) — the grader groups
by it directly rather than re-deriving turn boundaries from `Stage` events.

**Price-clocked correlation.** Every graded window's `ts_start_ms`/
`ts_end_ms` are read from the same `ts` field `StampedEvent` stamps on the
wire and `TranscriptReader`/`TranscriptState` already anchor to
(`origin_ms` = the trial's first stamped event). The grader uses the
identical anchor, computed once per trial and reused for every window in
it, so:

- a grade computed today and one computed after a schema migration six
  months from now assign the *same* `ts_start_ms` to the *same* step, and
- a grade and the transcript line `render.py`/`TranscriptReader` would print
  for that step line up on one timeline without re-deriving anything —
  pairing them is `events.seq BETWEEN step_grades.event_seq_start AND
  step_grades.event_seq_end`, an index-only join (`events_trial_idx` already
  covers `(trial_id, seq)`).

A trial recorded before `ts` existed on the wire (pre-#2111 streams,
per `transcript.py`'s own note) has no `ts_start_ms`/`ts_end_ms`; the grader
still computes `direction` from the deterministic rules (§5), which do not
need timestamps, and leaves the timing columns `NULL` rather than
backfilling a guess — the same absence-over-invention posture as
`cost_norm_status`.

**Idempotency.** Like `ingest.py`, re-running the grader for a
`(trial_id, grader_version)` pair is `INSERT OR REPLACE`, keyed on the
identity indexes in §3. Grading with a *new* `grader_version` adds rows
beside the old ones rather than overwriting — a report can compare
`v3` against `v4` on the same trial to see exactly which steps a ruleset
change reclassified.

## 5. Deterministic scoring: rule-based classifiers

Rules run in a fixed priority order per step-window; the first rule whose
precondition is met assigns `direction` and `direction_reason`, and no later
rule is consulted. Every rule reads only `events`/`tool_calls`/
`verifier_tests` rows already in the store — no new instrumentation, no
network calls, no model tokens spent.

1. **Loop/retry rules (highest priority — these are the engine's own
   verdict).** A window containing `type = 'loop_detected'` →
   `unproductive`, reason `loop_detected:<verdict>` (the payload's
   `exact_repeat`/`short_cycle`/`stagnation` string, mirroring
   `stella-core::loop_detect::LoopVerdict`). A window ending in
   `type = 'retries_exhausted'` or an `'error'` with `retryable = false` →
   `unproductive`, reason `retries_exhausted` / `fatal_error`.

2. **Verifier-delta rule.** For windows with a `verifier_tests` snapshot
   available before and after (CTRF re-run mid-trial, when the harness
   supports it): newly-passing test count increases →
   `productive` (`verifier_delta:+N`); newly-failing increases →
   `unproductive` (`verifier_delta:-N`); no change falls through.

3. **Diff-convergence rule.** For windows with a `type = 'file_change'`
   event: diff the window's file delta against the trial's *final* accepted
   patch (read once per trial, cached). Lines the window adds that survive
   into the final patch, or lines it removes that the final patch also
   removes, count toward convergence; lines it adds that the final patch
   does not keep, or lines it removes that the final patch keeps, count
   against it. Net-positive convergence → `productive`
   (`diff_convergent:+N/-M`); net-negative → `unproductive`
   (`diff_divergent:+N/-M`); a no-op diff (touches only whitespace/comments
   the final patch also touches identically) → `neutral`.

4. **Tool-exit rule.** For windows whose `tool_calls` rows include a
   build/test/lint invocation (matched by tool name against a small
   configured set — `cargo test`, `pytest`, `make check`, etc., the same
   family `bench/run_swebench.py` already shells out to): exit code moving
   from nonzero to zero across the window → `productive`
   (`tool_exit:fixed`); zero to nonzero → `unproductive`
   (`tool_exit:broke`); nonzero-to-nonzero with an unchanged error signature
   (same first failing assertion/panic message) → `unproductive`
   (`tool_exit:repeated_failure`); anything else falls through.

5. **No-op rule.** A window with zero `file_change`, zero non-read-only
   `tool_start`, and a `Stage` unchanged from the window before it (restating
   a plan, a lookup that informs nothing measurable downstream) →
   `neutral` (`no_measurable_effect`). This is a floor, not a default: it
   only fires when the four rules above all declined to match, so a window
   is `neutral` because nothing detectable happened, never because grading
   gave up.

A window matching none of the above (no test signal, no diff, no verifier
movement, no explicit failure, but *not* a clean no-op either — e.g. a
read-only investigation step whose payoff shows up two steps later) is left
unclassified by the deterministic pass and handed to §6.

## 6. Agent-assisted grading fallback

Only unclassified windows reach a model, and the model that grades never
authored the trial being graded — the same "a model is a poor verifier of
its own output" premise `.stella/skills/ultra-audit/reference/model-panel.md`
states for code review, applied here to step direction. The grading agent
receives, per window: the window's tool call(s) and result(s) verbatim, the
immediately preceding and following two windows for context (so an
investigation step's payoff two steps later is visible), and the fact that
the deterministic pass found no signal — never asked to re-decide a window a
rule already settled.

It returns **structured output only**:

```json
{"direction": "productive" | "unproductive" | "neutral",
 "reason": "<one short clause>",
 "confidence": 0.0-1.0}
```

Free-form prose is never treated as the grade; a response that fails to
parse as this shape is a hard error for that window, not a silent `neutral`.

**Escalation.** A response is written with `direction_source = 'agent_judge'`
only when `confidence` clears a configured floor (default 0.6) *and* it does
not contradict a strong deterministic signal on an adjacent window (e.g. the
window right before a `verifier_delta:+N` claiming `unproductive` is a
contradiction worth flagging, not trusting). Anything else — low confidence,
or an adjacent-window contradiction — is written with
`direction_source = 'escalated_human'`, `direction` set to the judge's best
guess, `confidence` preserved, and surfaced in the join report (§7) as
needing review rather than silently counted toward the ratio. This mirrors
`ScopeDecision::Abort`'s fail-closed posture in
`crates/stella-cli/src/command_deck/scope_gate.rs`: an unresolved case is
marked unresolved, never guessed past quietly.

## 7. Aggregation formulas

All three levels use the identical formula, `productive / total`, with
`total` = `productive + unproductive + neutral` (i.e. every graded window
counts in the denominator regardless of direction — a `neutral` step is not
excluded, since excluding it would let a grader inflate the ratio just by
labeling more steps neutral).

- **Step level.** `step_grades` has no ratio column of its own — a single
  step's "ratio" is just its `direction`, which is the input the other two
  levels aggregate. (A report may still show it as 1.0/0.0/— per step for
  display; nothing is stored redundantly.)
- **Turn level.**
  `turn_grades.productive_step_ratio = productive_steps / step_count`
  where `step_count = productive_steps + unproductive_steps + neutral_steps`
  for that `(trial_id, turn_instance)`. `NULL` when `step_count = 0` (a turn
  graded before any step landed) — never `0`, which would be
  indistinguishable from "every step was unproductive."
- **Execution level — the agent productive step ratio.**
  `execution_grades.agent_productive_step_ratio = productive_steps /
  step_count` summed over every turn in the trial. Equivalently, the
  weighted mean of the turn-level ratios weighted by each turn's
  `step_count` — the two must agree by construction, and a test in §9 pins
  that identity so a future change to either aggregation path cannot drift
  from the other silently.
- **Cross-trial and cross-run rollups** (by model, by arm, by task) are
  queries over `execution_grades`, not a fourth table: `run_id` is already
  denormalized onto every row for exactly this, e.g.
  `SELECT run_id, AVG(agent_productive_step_ratio) FROM execution_grades
  WHERE grader_version = 'vN' GROUP BY run_id`.

Time and cost aggregate by plain summation at every level
(`turn_grades.wall_clock_ms` = sum of its `step_grades.wall_clock_ms`, and
so on to `execution_grades`) — no weighting, since these are already
additive measured quantities rather than ratios.

## 8. Join report: pairing transcripts and grades inline

The report a human reads is a **line-by-line merge** of what
`TranscriptReader`/`render.py` already produce and the grade for the window
that line falls in — not a new transcript renderer. Concretely, for a given
`trial_id`:

1. Replay the trial with `TranscriptReader.read()` exactly as the live UI
   does, producing its ordered entries (each carrying the `seq` assigned by
   `TranscriptState._next_seq`).
2. For each entry, look up the `step_grades` row whose
   `[event_seq_start, event_seq_end]` contains that `seq` — one indexed
   lookup, no re-parsing of the trial's artifacts.
3. Render each transcript line with its grade inline: direction as a
   left-hand gutter glyph (green/red/grey, matching `render.py`'s existing
   `STYLES`-driven colour convention rather than inventing a second palette),
   `direction_reason` as a hover/footnote, and running `wall_clock_ms` /
   `model_time_ms` / `cost_usd` totals in the pinned footer `render.py`
   already reserves for summary state.
4. A trailing section per turn and per execution shows that grain's
   `productive_step_ratio` / `agent_productive_step_ratio` alongside
   `trials.passed` and `verdict_alignment`, so a reader sees in one place
   whether the ratio's story agreed with the outcome.

Anything written `escalated_human` (§6) is marked distinctly in the report
(not folded into either color) so a reviewer's eye lands on exactly the
windows nothing — rule or model — settled with confidence, and a reviewer's
override writes back as a new `grader_version` row rather than mutating the
`agent_judge` row it replaces, preserving the same before/after history
`step_grades`'s versioning already guarantees.

The report is generatable both as a static artifact (HTML, for archiving
next to a run the way `artifacts` rows already archive raw bytes-adjacent
provenance) and as a live query (for a dashboard iterating `execution_grades`
across a whole run) — the underlying join is identical either way; only the
rendering target differs.

## 9. Rollout and testing plan

**Phase 0 — schema.** Land the DDL in §3 as an additive migration to
`bench/telemetry_store/schema.sql` (or a sibling file loaded the same way),
with `tests/test_ingest.py`-style fixtures asserting the tables create
cleanly on both SQLite and Postgres and that the identity indexes reject a
duplicate `(trial_id, ..., grader_version)` insert.

**Phase 1 — deterministic grader only.** Ship the rule chain in §5 against
already-ingested runs, `grader_version = 'v1-deterministic'`. No agent
fallback yet; unclassified windows are written with `direction = 'neutral'`,
`direction_source = 'deterministic'`, `direction_reason =
'unclassified_floor'` so the gap between "ruled neutral" and "no rule fired"
stays visible in the data rather than hidden by silently deferring to a
judge that does not exist yet. Test by hand-labeling a small fixture set of
known-shape trials (a clean pass, a thrash-then-pass, a clean fail, a
loop-detected abort) and asserting the rule chain reproduces the expected
label per window — the same golden-trajectory posture `arenabench`'s own
fixtures already use.

**Phase 2 — agent fallback.** Add the §6 judge behind
`grader_version = 'v2-with-judge'`, run it side-by-side with v1 on the same
trials, and confirm in the join report that every window v1 marked
`unclassified_floor` now has a v2 verdict with a recorded `confidence`, while
every window v1 already classified deterministically is byte-identical in
v2 (the judge must never be consulted where a rule already fired — assert
this directly, not just spot-check it).

**Phase 3 — aggregation and report.** Compute `turn_grades`/
`execution_grades` from v2's `step_grades`, assert the weighted-mean identity
from §7 (`agent_productive_step_ratio` equals the `step_count`-weighted mean
of its turns' `productive_step_ratio`) as a standing test, and ship the
join-report renderer. Validate `verdict_alignment` against a run with known
outcomes before trusting it as a sanity signal on new data.

**Phase 4 — dogfood on a real run.** Grade one already-ingested scored run
end to end, publish the join report, and have a human spot-check a sample of
`escalated_human` rows and a sample of confident `agent_judge` rows against
the raw transcript before treating `agent_productive_step_ratio` as a number
worth citing in an ArenaBench readout.

No phase requires touching `ingest.py`, `telemetry.py`, `transcript.py`, or
`render.py` — this is additive on top of the store those files already
populate and read.
