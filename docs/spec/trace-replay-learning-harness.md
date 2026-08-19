---
id: trace-replay-learning-harness
title: "The trace-replay learning harness — drive the learning machinery from recorded traces, with zero model calls"
status: proposed
---

# The trace-replay learning harness

How to run Stella's learning machinery backwards: instead of sessions producing
traces that feed learning, recorded traces drive learning directly — and we watch
what the agent builds (skills, tools, rules, memories) across a simulated
engagement, with **no model calls at all**.

**Status:** **Proposed.** Implements epic #2304.
**Date:** 2026-08-08. **Owner:** Mac Anderson.
**Companion:** the paid with-records bench arm, which measures *outcome*; this
document measures *formation* and deliberately claims nothing about outcome
(§10).

---

## 1. Why this can exist at all — the learner census

Every learner in the tree already sits below the model boundary or behind a
port. That is invariant 1 ("ports, not direct dependencies") and invariant 2 ("no I/O in
the engine") paying out: replay needs **no new architecture**, only a driver and
a clock.

| # | Learner | Entry point | Model calls |
|---|---|---|---|
| 1 | Memory formation | `SessionMemory::reflect_and_record` — `crates/stella-cli/src/memory/learning.rs:57` | **exactly one** (`reflect_on_turn`, `learning.rs:66`) |
| 2 | Skill learning | `SessionMemory::auto_create_skills` — `learning.rs:372` (typed `:606`, lexical `:756`) → `stella_core::skills::mine_skill_candidates` (`crates/stella-core/src/skills.rs:650`), `decide_auto_creation` (`:833`) | none |
| 3 | Rules mining | `extract_reflection_observations` (`crates/stella-cli/src/memory/observations.rs:81`) → `induce_rule_proposals` (`crates/stella-cli/src/memory/rules_mining.rs:104`) → `write_rule` (`:188`) | none |
| 4 | Tool foundry | `stella_core::detect_tool_gaps` — `crates/stella-core/src/tool_foundry.rs:198`; CLI adapter `crates/stella-cli/src/tool_foundry.rs:56` | none, **by design** |
| 5 | Selection / lifecycle | `four_class_certification` and `time_lapse_certification` — `crates/stella-core/src/records/tests.rs:462`, `:709` | none — landed in #2306 |

So exactly one seam needs a test double, and the double already exists:
`ScriptedProvider` appears in 42 files across `stella-core`, `stella-pipeline`,
`stella-engine` and `stella-cli` (canonically
`crates/stella-core/src/subagent/tests.rs:36`). Replaying canned reflection
outputs per turn **is** the no-model harness.

Learner 4 is worth dwelling on: `detect_tool_gaps` is pure over
`&[ShellInvocation]`, sorts its output by evidence strength and then by name, and
documents that "the same history always yields byte-identical output." It is
already a replay target with no adaptation whatsoever.

---

## 2. The blocker the epic does not name: there is no clock seam

The epic's Phase 1 requires that "timestamps come from the trace, never the wall
clock — replay must be deterministic." **Today that is impossible**, and closing
it is the first PR.

The port already exists, one layer down. `crates/stella-context/src/clock.rs`
ships `trait Clock` with `SystemClock` and `FixedClock` (the latter with
`advance`/`set` precisely so "a T1→T2 correction is exact and never races real
time"), and `ContextStore::open_with` takes an `Arc<dyn Clock>`
(`crates/stella-context/src/store.rs:213`). The context store's own tests already
inject `FixedClock` in eight places.

The CLI's learning path does not use it. `SessionMemory::open_with_workspace_skills`
hardcodes `Arc::new(SystemClock)`, and six sites read the wall clock directly,
routing around the port:

| Site | What it stamps | Why replay breaks |
|---|---|---|
| `crates/stella-cli/src/memory.rs:192` (`default_task_id`) | the task boundary governance counts | also embeds `std::process::id()` — nondeterministic twice over |
| `memory.rs:567` (`record_episode`) | episode end time | episodes land out of trace order |
| `memory.rs:662` (`unix_now_secs`) | episode primitive | ditto, and it is `pub(crate)` so callers multiply |
| `memory/reflection.rs:474` | reflection recording time | lesson recency ranking drifts with real time |
| `memory/learning.rs:444` (`retire_failing_context`) | the `now` handed to the truth sweep | **retirement cannot be tested at all** — the sweep always sees today |
| `memory/self_tuning.rs:276` | tuning window | self-tuning cannot be replayed |

Note the contrast with how `stella-core::records` already does it: every
lifecycle predicate takes `now: &str` as a parameter
(`records/clock.rs:219`, `:232`, `:252`; `records/decision.rs:222`). The records
plane is already replayable. The CLI's memory plane is not, and that asymmetry
is the entire gap.

### 2.1 The fix

Add `clock: Arc<dyn stella_context::Clock>` to `SessionMemory`, hand it to
`ContextStore::open_with`, and replace all six reads with `self.clock`. Keep
every existing `SessionMemory::open*` constructor defaulting to `SystemClock`, so
production behaviour is byte-identical, and add one `open_with_clock` the harness
uses. `default_task_id` loses `process::id()` in favour of an explicit
session ordinal.

`SessionMemory::set_task_id` is today `#[cfg(test)]`-gated, with a doc comment
that reads: *"Drop the gate in the same commit that adds the first real caller."*
**The replayer is that caller** — it genuinely knows where one task ends and the
next begins, which is exactly the case the comment anticipates.

**Witness test.** Open two `SessionMemory` instances against `FixedClock(T)`,
run one scripted turn through each, and assert the two `reflections.jsonl` files
are byte-identical. Fails on `main` (the task id embeds the wall clock and the
pid), passes after.

**Audit in the same PR:** `crates/stella-context/src/store/edge.rs:121` reads
`SystemTime::now()` for nanoseconds. Determine whether that value reaches any
comparable or serialized field, or is only an id nonce. If it is comparable,
it must move behind the clock too; if it is a nonce, say so in a comment so the
next reader does not re-audit it.

---

## 3. The trace format

Versioned and serde-first (invariant 4: round-trips through `serde_json`
byte-for-byte, with a round-trip test).

```rust
// crates/stella-cli/src/memory/replay/trace.rs

/// One recorded engagement: an ordered sequence of sessions, each a sequence
/// of turns. `version` gates the loader — an unknown version is a refusal,
/// never a best-effort parse.
pub struct Trace {
    pub version: u32,
    pub workspace: WorkspaceSeed,
    pub sessions: Vec<TraceSession>,
}

/// One session. `task_id` is the boundary governance counts distinct tasks by;
/// supplying it is the whole reason `set_task_id` exists (§2.1).
pub struct TraceSession {
    pub id: String,
    pub task_id: String,
    pub turns: Vec<TraceTurn>,
}

pub struct TraceTurn {
    /// Unix seconds. The replayer sets the clock to this before the turn —
    /// nothing in the harness ever reads the wall clock.
    pub at: i64,
    pub prompt: String,
    /// The transcript stub `reflect_on_turn` digests. Only the tail matters
    /// (it reverses and digests), so a stub need not be a full transcript.
    pub transcript: Vec<CompletionMessage>,
    /// What the model *would* have said. Replayed through `ScriptedProvider`.
    pub reflection: ScriptedReflection,
    /// The turn's shell history, fed to `detect_tool_gaps`.
    pub shell: Vec<ShellInvocation>,
    /// Drives the reflection prompt template and the outcome flag.
    pub succeeded: bool,
}

/// Three arms, deliberately.
pub enum ScriptedReflection {
    Lessons(Vec<ReflectionLesson>),
    /// A response the parser cannot read — `ReflectionParse::Unreadable`.
    Unreadable(String),
    /// The model call itself failed — `StandaloneCallError`.
    ModelError(String),
}
```

The two failure arms are not padding. They are the arms the live loop actually
hits, and the ones that starve learning **silently**: `reflect_and_record`
returns `recorded: 0` for "nothing worth learning" and for "I could not read the
response", and telling them apart is the whole point of the `Unreadable` branch
at `learning.rs:119`. A corpus that only ever scripts happy-path `Lessons`
cannot certify the starvation guard, which is assertion 7 below.

`ReflectionLesson` already carries everything a trace needs — `lesson`,
`domains`, `occurred_at`, `task_id`, `kind` — and `task_id` is
`#[serde(default)]`, so old logs still parse (`crates/stella-cli/src/memory.rs:121`).

**Fixtures** live at `crates/stella-cli/tests/fixtures/traces/*.json`, loaded via
`env!("CARGO_MANIFEST_DIR")` — the in-crate convention already used at
`crates/stella-cli/src/paths.rs:407` and four other places.

---

## 4. Where the replayer lives

`stella-cli` is **bin-only** — its `Cargo.toml` declares `[[bin]]` and no
`[lib]`, so the thirteen files under `crates/stella-cli/tests/` all drive the
compiled binary and none of them can reach `SessionMemory`.

The harness is therefore an in-crate module,
`crates/stella-cli/src/memory/replay/`, following the pattern already
established by `crates/stella-cli/src/memory/tests/` (`ab_control.rs`,
`skill_creation.rs`, `record_channel.rs`, `quarantine.rs`).

*Alternative considered and deferred:* extract the learning loop into a library
crate so the harness could be an ordinary integration test. That is the
structurally cleaner answer and it is a much larger change; the epic's value does
not depend on it, and doing both at once would make the clock seam unreviewable.
Filed as a follow-up rather than folded in.

### 4.1 The loop

Per turn, in order:

1. `clock.set(turn.at)` — the only time source in the system.
2. `memory.set_task_id(&session.task_id)`.
3. Build `ScriptedProvider` from `turn.reflection`.
4. `memory.reflect_and_record(&provider, …).await` — the single model boundary.
5. `extract_reflection_observations(store, reflections_jsonl)`.
6. `induce_rule_proposals(…)` and, at threshold, `write_rule`.
7. `detect_tool_gaps(window, GapDetectionConfig::default())` over the shell
   history accumulated so far.
8. Snapshot what exists on disk and in the stores.

### 4.2 The determinism contract

- **No wall clock, no `Instant`, no pid.** §2 is a hard prerequisite; the
  harness asserts it rather than assuming it.
- **A per-replay temp workspace**, never the developer's `$HOME`. Ambient
  `STELLA_*` variables leak user settings into tests that believe they are
  isolated, so the harness asserts no ambient read — the pattern
  `crates/stella-runtime/tests/no_ambient_reads.rs` already enforces for the
  runtime.
- **Two replays of one trace produce byte-identical summaries, and those
  summaries carry the trace's own instants.** This is assertion 8, not a
  comment — and the second clause is load-bearing, not emphasis.

  Agreement alone is not evidence of determinism: two replays sharing one
  ambient clock agree with each other while agreeing with nothing. Measured, not
  reasoned — #2320's originally-specified witness was exactly the weak form, and
  it **passed against the unfixed code**, because two sessions in one process
  read the same wall-clock second. It only failed on a run that happened to
  straddle a second boundary. Every determinism assertion in this harness must
  pin the injected value, not merely the agreement.

---

## 5. The assertions — the point of the harness

Each pins a mechanism that today has no end-to-end proof.

| # | Assertion | Mechanism pinned |
|---|---|---|
| 1 | A lesson repeated across **≥3 distinct tasks** yields a rule proposal; the same lesson repeated across 3 turns of **one** task does not | distinct-task counting — the exact confusion `set_task_id` exists to fix |
| 2 | A proposal reaching `auto_activate_at_confidence` (default **85**, `crates/stella-cli/src/settings/context.rs:130`) auto-activates as an **advisory** record — never blocking, never clobbering an existing file (#737) | promotion gating |
| 3 | A repeated skill-shaped lesson produces a `SKILL.md` on disk; a one-off does not | `mine_skill_candidates` → `decide_auto_creation` threshold and session cap |
| 4 | A shell shape recurring with **≥2 distinct argument sets, each retyped** yields a foundry proposal; the same command with one argument set yields none, and so does a shape whose arguments never repeat | typed-hole normalization, the `min_occurrences < 2` disable path, and the `min_reuse_ratio` floor that separates a reusable incantation from the shell itself (#2378) |
| 5 | A tombstoned lesson is **never re-learned**, including when the corpus later restates it in different words | `retain_unforgotten` matches by restatement, not equality — so the corpus must paraphrase, or the assertion is vacuous |
| 6 | A retired record stops rendering into the prompt | truth sweep + the volatile channel |
| 7 | A corpus of `Unreadable` reflections builds **nothing** and the harness **says so loudly** | the starvation guard — a clean `recorded: 0` on every turn must not read as "the agent keeps getting things right" |
| 8 | Two replays of one trace produce byte-identical summaries **and those summaries carry the trace's own instants** | the determinism contract itself |

Assertion 5 deserves a note: because suppression matches restatements, a corpus
that tombstones lesson *L* and then re-emits *L verbatim* proves almost nothing —
byte-identical content already collapses on its own via content-hash lineage.
The fixture must re-emit a **paraphrase**, which is the case the filter exists
for.

---

## 6. Metrics

Printed per replay by `make replay-learning`:

- **Artifacts built, by class** — memories, skills, rules, tools — and
  turns-to-first-artifact for each.
- **Bytes injected per turn** — the compactness axis. A learner that builds a
  great deal and spends the whole recall budget rendering it has not obviously
  helped.
- **Four-class precision on the final corpus**, reusing the classification the
  certification suite landed in #2306.
- **The corpus's own recurrence profile** — printed beside the artifact counts,
  never instead of them.

That last item is a deliberate honesty guard. An artifact count that rises
because the fixture repeats one lesson forty times measures the **fixture**, not
the learner, and it would flatter us in exactly the direction the project's
reputation cannot afford. Reporting distinct-artifact counts against the
corpus's recurrence structure is what makes the number mean anything.

---

## 7. Phase 3 — the real corpus, and a premise correction

The epic proposes an adapter from Claude Code session transcripts
(`~/.claude/projects/**/*.jsonl`) to give "a genuine months-long single-engineer
dataset."

**Measured on this machine, 2026-08-08:** 3,822 `.jsonl` transcripts across 485
project directories, 3.5 GB, spanning **2026-07-09 → 2026-08-08 — thirty days.**
Not months.

That correction changes how the corpus should be described, not whether it is
useful. What it actually provides is **recurrence structure at a density no
synthetic fixture will match**: one engineer, a handful of repositories, the same
conventions re-encountered across thousands of sessions. The *months* axis does
not come from the corpus at all — it comes from the replayer's clock (§2), which
can dilate thirty days of real recurrence across a simulated six months at will.
State it that way. Claiming a months-long corpus we do not have is the kind of
small dishonesty that costs more than it buys.

Two further caveats:

- The directory is a **rolling window** under Claude Code's own retention, so the
  span is not reproducible across machines or across time. It is a local
  exploration input, never a committed fixture — which the privacy gate below
  independently requires anyway.
- Observed line types: `assistant`, `user`, `attachment`, `last-prompt`, `mode`,
  `permission-mode`, `agent-setting`, `worktree-state`, `agent-name`,
  `ai-title`, `relocated`, `file-history-snapshot`, `queue-operation`, `system`,
  `pr-link`, `file-history-delta`, `started`, `result`, `bridge-session`,
  `custom-title`. The adapter reads `user`/`assistant` for the transcript stub
  and `result` for the outcome flag; the rest is harness bookkeeping.

### 7.1 The privacy gate — hard, per invariant 3

- **Local-only, opt-in.** A subcommand or example that must be invoked
  explicitly; never a default path, never reached by `cargo test`.
- **Output is gitignored scratch** or lands under `.stella/private/`. The
  `no-scratch` guard already asserts no tracked file matches a gitignore rule,
  so a committed derivative fails the gate on its own.
- **Every derived statement passes the ingest quarantine.** Transcript text is
  user content and model output — precisely what `origin_is_untrusted`
  (`crates/stella-core/src/ingest/gate.rs:30`) exists to classify. Derived
  statements go through `gate_proposal` (`:124`) and `quarantine_for` (`:78`)
  with the origin marked untrusted.
- **Nothing derived is committed or exported.** The committed CI corpus stays
  synthetic, permanently.
- **Witness test:** feed the adapter a transcript containing a secret-shaped
  string and assert it is quarantined rather than stored.

### 7.2 The adapter's honest limit — read this before building it

Claude Code transcripts contain **no Stella reflection JSON**. There is nothing
in them to script `ScriptedReflection::Lessons` from. So the adapter must either
derive lessons from the transcript, or decline to.

- **(a) Synthesize lessons** from CC's own summaries or from the transcript text.
  This fabricates the exact signal under test: the harness would then be
  measuring the adapter's lesson-invention heuristic, not Stella's learning.
- **(b) Use the corpus for shell-invocation history, session/turn boundaries, and
  timing only** — feeding learners 4 (foundry) and the recurrence/timing model,
  while the reflection path stays on synthetic scripted lessons.

**Recommend (b) for the first cut.** It is the honest half of what the corpus can
supply, it still lights up the foundry against thousands of real commands, and it
does not launder a heuristic into a result. If (a) is ever built, its output must
be labelled in the trace (`ScriptedReflection` gains a provenance field) so no
downstream number can silently mix invented lessons with recorded ones.

---

## 8. Phase 4 — CI wiring

The synthetic-corpus replay is an ordinary in-crate test: model-free, key-free,
and fast. It therefore joins `cargo test --workspace` and is **already covered by
`make gate`'s existing `test` step** — no new `GATE_STEPS` entry, and so no
`gate-parity` churn across the Makefile, AGENTS.md and CONTRIBUTING.md.

`make replay-learning` is a convenience target that runs one trace and prints the
§6 summary. It is deliberately *not* a gate step: a target whose job is to print
a report adds nothing as a pass/fail gate, and every added step is a shared cell
three documents must agree on.

---

## 9. Sequencing

Five PRs, each with its own witness.

| PR | Scope | Witness |
|---|---|---|
| 1 | **The clock seam** (§2). `Arc<dyn Clock>` on `SessionMemory`; six wall-clock reads replaced; `edge.rs:121` audited; `set_task_id` ungated | two `FixedClock(T)` sessions produce byte-identical `reflections.jsonl` |
| 2 | Trace format + loader + replayer skeleton + one trivial two-turn trace | serde round-trip byte-for-byte; the trivial trace replays and builds exactly one memory |
| 3 | The assertion suite (§5) + the synthetic simulated-months corpus | each of the eight assertions fails against a corpus engineered to violate it |
| 4 | Metrics + `make replay-learning` | the summary is byte-identical across two runs |
| 5 | CC transcript adapter, local-only, option (b) (§7.2) | a secret-shaped string in a transcript is quarantined, not stored |

PR 1 is a strict prerequisite for 2–4. PR 5 is independent of 3–4 and can run in
parallel once 2 lands.

---

## 10. What this deliberately does not do

- **It does not measure outcome.** Whether learned context makes Terminal-Bench
  tasks faster, cheaper, or more likely to pass is the companion paid bench arm.
  Conflating formation with outcome here would produce a number that looks like
  evidence and is not.
- **It does not certify the model's reflection quality.** It certifies
  everything downstream of the reflection — which is the part that is
  deterministic, and therefore the part that can actually be proven.
- **It does not replace the live loop's own tests.** It adds the end-to-end
  cross-learner proof none of them provide individually.

---

## 11. Risks and known interactions

- **The corpus proves the corpus.** Mitigated by §6's recurrence-profile
  reporting, not eliminated by it. Any claim from a replay must name the fixture
  it came from.
- **`edge.rs:121` may defeat determinism.** Audited in PR 1; if it reaches a
  comparable field, PR 1 grows to cover it rather than PR 2 inheriting a flake.
- **Recall assertions inherit #2288.** The only `Embedder` in the tree is
  `HashEmbedder` (character-trigram, `crates/stella-context/src/embed.rs:121`),
  which declares `SimilarityPosture::Surface`; the evidence gate shipped in
  #2298 means its cosine orders but never admits. A replay assertion about
  *which* memories are recalled would therefore be testing lexical overlap, not
  semantics. Scope recall-ranking assertions out of this harness and let #2288
  land first.
- **A single-topic synthetic corpus saturates document frequency** and will
  abstain under the evidence gate until other topics accumulate. Fixture design
  must spread topics, or assertion 6 will fail for a reason that has nothing to
  do with retirement.

---

## 12. Related

- Epic #2304 — this document's parent.
- #2283 — one control plane for steering content (memories under the
  context-record surface), and its ADR
  `doc:adr/0014-memories-join-the-record-control-plane`, which
  decides the governance of the memory surface this harness observes forming.
  If memories gain lifecycle and provenance, assertions 5 and 6 gain a second
  surface to hold to the same standard.
- #2295 — mined rules publish TOML context records (merged; the write surface
  this harness observes).
- #2299 — the volatile budget drops by load order, not precedence. Affects what
  §6's bytes-per-turn metric means.
- #2306 — the four-class and time-lapse certification suites this harness
  reuses for its precision metric.
- `doc:replay-golden-trajectories` — a different replay concern: event-stream
  drift for the pipeline, not learning formation. The two share the word
  "replay" and nothing else.

---

## 13. As built

The plan above survived contact with the code; the sequencing did not. PR 1 (the
clock seam) landed on its own as #2340 closing #2320, and PRs 2–4 land together
here — the format, the assertions and the metrics are one module, and splitting
an assertion suite from the replayer that exists only to run it produces a PR
that tests nothing.

This section records where the implementation departs from the plan, so the
document stays true to the tree. **Everything here was measured by replaying the
real loop; none of it is visible from reading any single module**, which is the
harness earning its keep before it shipped.

### 13.1 Five measurements that changed the fixtures

1. **The dedup filter and the skill miner used the same metric at the same
   threshold, so a corpus of natural repetitions built nothing.** Filed as
   **#2358** and fixed in two halves. Ordering: `partition_known` (né
   `retain_unknown`) now *diverts* a store-restatement to the mining log
   instead of dropping it before the log was appended, so re-learning — the
   most common recurrence shape — finally counts as an occurrence while the
   store still keeps one copy. Threshold: the miners' `min_similarity` moved
   from the inherited 0.5 to 0.4, a value measured in
   `stella_core::mining::terms`'s own token space (naturally-varied same-fact
   pairs score 0.40; the worst cross-fact pair 0.36) rather than borrowed from
   `stella_store::SIMILARITY_THRESHOLD`, which is measured over a tokenizer
   that keeps a path as one token where `terms` shatters it into five. The
   relationship is declared and tripwired by
   `the_dedup_and_clustering_predicates_hold_the_declared_relationship`, and
   the committed corpus is now worded the way a real engagement words itself —
   four natural phrasings plus one near-restatement — instead of being
   engineered into the accidental band the old thresholds left open.
2. **The foundry holes only a value-like argument.** `classify_argument` makes a
   positional argument a parameter only if it is a path or a number, so
   `cargo test -p <crate>` yields a different signature per crate and never
   clusters, while `rg -n "<pattern>" <path>` clusters immediately. Correct — a
   bareword is usually a subcommand — but a fixture varying a bareword measures
   nothing.
3. **A record's lineage embeds the workspace directory name.** `derive_set_id`
   falls back to it with no git remote, so two replays into different temp dirs
   produced different lineage ids for the same learned rule and assertion 8
   failed. The harness names its workspace rather than narrowing the summary to
   hide it: stripping the set id would have buried real nondeterminism behind a
   shorter report.
4. **`Path::starts_with` is lexical**, so `root/../escaped.txt` satisfies it and
   the obvious seed-path guard passed a fixture that escapes. It walks
   components now.
5. **A fixture that varies its arguments every single time proves nothing after
   #2378.** The corpus's `rg -n "<pattern>" <path>` history had one distinct
   argument set per invocation — the exact 1.0×-reuse shape the detector now
   declines to propose, because pointed at a real 81,684-command history that
   shape is what produced 2,073 unreadable proposals. Assertion 4 is stated on
   both ends now (variation *and* reuse), and the committed corpus grew a
   `git log --oneline -N` incantation retyped across sessions — a shape a
   working engineer genuinely does retype — while the never-repeating `rg`
   history stays exactly where it was, as the negative half.

### 13.2 Where the implementation diverges from §3, §4 and §5

- **The replayer's loop is shorter than §4.1's.** Steps 5 and 6 (observation
  extraction, rule induction) are already inside `reflect_and_record` —
  `auto_create_skills_typed` runs uses extraction, the retirement sweep,
  observation extraction, skill proposals and rule induction from one
  observation pool. Driving them again would shadow the shipped path with a
  second mechanism, and the harness would certify its own wiring.
- **`open_with_clock` takes `include_workspace_skills`.** With it false,
  `write_candidates` and `induce_rules` record their proposals and decline to
  write a FILE (#737), so a replay builds nothing while behaving correctly.
- **`TraceShell` and `TraceLesson` mirror the runtime types rather than reusing
  them.** `ShellInvocation` has no serde impls, and a `ReflectionLesson` carries
  `occurred_at`/`task_id`, which the replayer owns — a trace able to set them
  could contradict its own clock, and every assertion about recency or
  distinct-task counting would then be asserting the fixture.
- **`TraceTurn` gained `forget` and `removed_files`.** Assertions 5 and 6 are
  unreachable without them, and both model real events in an engagement.
- **Assertion 6 asserts against `retirement::retired_ids`, not the store's live
  nodes.** Retirement deliberately does not write `node.superseded_at`, because
  staying retrievable by id is what separates retirement from forgetting. A test
  asserting "the memory is gone" fails against a working sweep, and one
  asserting "the count went down" re-specifies retirement as deletion.
- **`turns-to-first` is exact for memories and tools, approximate for skills,
  absent for rules.** Filed as **#2359**: a placeholder must not render like a
  measurement.

### 13.3 The adapter, as built

Option (b), as §7.2 recommends. Every turn it emits carries a **fourth
`ScriptedReflection` arm, `NotRecorded`**, and the replayer skips the model
boundary entirely for it. The arm exists because every other spelling is a claim
the source does not support: an empty `Lessons` array asserts the model had
nothing to say, and `Unreadable` asserts it said something unparseable — the
second would also fabricate starvation and corrupt assertion 7's own metric,
across sixteen thousand turns. `turns_not_recorded` is counted separately, so a
corpus that was never asked does not read as a learner that stayed idle.

The privacy gate lands where the risk actually is. Under option (b) no
*statements* are derived, so there is no proposal to run through `gate_proposal`
/ `quarantine_for`; the highest-risk field the adapter touches is the **shell
command**. So `redact_secrets` runs on every command, and the gate is *stricter*
than quarantine: a command whose redaction fired is **dropped**, not kept with a
`[redacted]` hole in it. The redactor's prefix list is a good filter rather than
a complete one, and a command carrying a credential was never going to be a
recurring shape worth minting a tool from — the foundry loses nothing, and the
gate keeps its margin.

Measured against the real corpus on 2026-08-08: **496 project directories** (up
from the 485 §7 recorded — consistent with the rolling window, and the reason a
derived trace can never be a committed fixture); a 20-project sample adapted to
**16,664 turns carrying 17,343 shell commands**, every trace passing the
loader's contract.

### 13.4 Still open

- **#2359** — the placeholder metric.
- **The corpus can prove the corpus.** §6's recurrence profile mitigates it and
  does not remove it.
- **Recall-ranking assertions remain scoped out**, inheriting #2288.
- **#2321** — extracting the learning loop into a library crate would let the
  harness be an ordinary integration test rather than a `#[cfg(test)]` module.
