> **Historical plan (2026-08).** Written before the 2026-08 tool purge
> reduced the built-in surface to 12 tools; the tool names and tool-split
> tickets below (`read_output`/`clear_output`, `probe_capability`,
> `run_lint`/`format_code`/`apply_edits`, `verify_done`) describe tools that
> no longer exist. Kept as the record of the plan and its sequencing logic.

Phase 0 — The prompt batch, shipped as one release

#2688 → #2689 → #2691 → #2690 → #2692 (order within the batch is just review order)

These five are pure prompt.rs + parity-test work: no engine changes, no schema changes, lowest risk in the whole set. The reason to batch them is your own cache discipline — every prompt edit invalidates the byte-stable prefix once, so five contracts landing in five releases is five cache-churn events while one release is one. Two of them also have ordering leverage elsewhere: #2688's denial-semantics clause is the stopgap for the RequireApproval dead-end (#2676) while it waits in Phase 3, and #2691/#2690 shape how the model behaves during everything you build after.

Phase 1 — Small correctness fixes, parallelizable

#2674 (SessionStart dedup), #2675 (MCP name collision), #2673 (wire the circuit breaker)

All three are contained, low-risk corrections with no dependencies on each other — good candidates to run in parallel or hand to separate workers. #2673 goes in this phase for a structural reason: the mid-turn model fallback (#2679) re-resolves through the router, so the breaker must actually receive call outcomes before fallback has anything meaningful to consult. It's the one hard prerequisite edge in the correction set.

Phase 2 — The reliability chain (this is where the bench moves)

#2681 → #2680 → #2677 → #2686 → #2679

This sequence is deliberate:

1. #2681 (usage-anchored token accounting) first, because it shrinks the whole problem class: once context size is mostly measurement instead of mostly estimate, the 1.8x drift from #2671 collapses, and you find out how often overflow actually happens.
2. #2680 (reactive overflow recovery) second — it's the safety net for whatever estimation error survives #2681. Building the net before fixing the estimator would mean sizing it against a failure rate that's about to change.
3. #2677 (budget-aware retry parking) next; independent of the first two, but it shares the "abort becomes recovery" theme and directly answers #2667's measured loss.
4. #2686 (streaming→non-streaming fallback) — same theme, independent, can float anywhere in this phase.
5. #2679 (mid-turn model fallback) last, because it needs #2673's wired breaker and benefits from #2680's transcript-repair muscle memory being fresh.

After this phase, rerun the bench arms behind #2666/#2667/#2671 — this is the phase with quantified losses attached, so it's the one you can score.

Phase 3 — The interactive surface chain

#2676 → #2678 → #2687

#2676 (route RequireApproval to an interactive grant) goes first because it creates the reusable ask-the-human surface. #2678's elicitation support explicitly wants that surface, so it follows — though #2678's step 1 (stop degrading resource blocks to [resource: <uri>]) is a small self-contained rendering fix you can pull forward into Phase 1 if you want an early win. #2687 (auth-probe suppression + needs-auth tool) closes the MCP UX loop and touches the same toolset code paths, so it rides last while they're warm.

Phase 4 — Capability features, two independent tracks

Track A: #2682 → #2685. Invocable skills first, because #2685's most valuable restoration target — the active invoked-skill body that compaction must not destroy — doesn't exist until #2682 does. #2685's file-restoration half is independent and could land first if you split the ticket.

Track B: #2683 and #2684, in parallel with Track A and each other. The ingest-first grounding system (#2683) is store-schema work (lineages, dismissals, tiers) in a different subsystem from everything above; the hook-surface expansion (#2684) is bus-bridging work. Neither blocks nor is blocked by anything else. #2683 is the biggest single ticket in the set — starting it early in parallel is fine; just don't put it on the critical path of the reliability chain.

The dependency edges, compact

#2673 ──────────────► #2679
#2681 ──► #2680
#2676 ──► #2678 (elicitation part only)
#2682 ──► #2685 (skill-body restoration part only)
#2688 ····► #2676 (soft: prompt clause is the interim mitigation)

Everything not on an arrow is free-floating within its phase.

---

## Execution note — appended 2026-08-10 (session: "First Class Environment/State Improvements")

**What was actually built, and where it sits relative to this plan.** This session executed work that is *not on this plan*: the session-environment epic #2695 with sub-issues #2696–#2700 (scratch state plane, `get_environment`, `probe_capability`, the `read_output(clear)` split, and the tool-first single-purpose principle codified as AGENTS.md invariant #9). It jumped the queue relative to Phases 0–2 because the maintainer lifted the feature freeze on 2026-08-10 and declared the make-everything-a-tool phase.

**PRs out of this session (state as of writing):**

- **#2710** — invariant #9 + CLAUDE.md phase bullet + stella-tools README recipe step. CI green, **marked ready** (the merge signal).
- **#2717** — `clear_output` split out of `read_output` (closes #2699), plus sweep follow-ups filed as **#2712** (`run_lint(fix)` / `format_code(check)`) and **#2713** (`apply_edits(dry_run)`). Draft; CI running at time of writing.
- **#2714** — scratch state plane: `save_state` / `get_state` / `list_state` / `delete_state` + `STELLA_SCRATCH` export (closes #2696). Draft; in rework after review found a thread-local env seam that silently no-ops on tokio worker threads (being replaced with constructor injection) and a duplicated registration list.
- Not yet started: **#2698** (`probe_capability`) and **#2697** (`get_environment` + prompt block) — deliberately held as wave 2 until the registration seam settles on main.

**Parallel sessions observed on the same repo** (relevant because they *are* on this plan): PR **#2719** carries the Phase 0 prompt-contract batch *including a byte-stable session-environment block*, and PR **#2711** is a slice of #2683 (Phase 4 Track B) — so Phase 0 and Phase 4 are executing concurrently, out of the phase order above. The plan itself permits early #2683; the Phase 0 overlap below is the one that needs active reconciliation.

**Risks / dependency and merge-sequence issues to watch:**

1. **#2697 vs #2719 collision (highest risk).** Both want to own the session-environment section of the byte-stable prompt prefix. #2697 must be re-scoped *after* #2719 lands: keep its `get_environment` tool, but render tool and prompt block from one shared source rather than adding a second block. Two independent session-environment sections would be a cache-churn event and a drift pair. Do not start #2697 until this is reconciled.
2. **#2714 vs #2717 textual conflicts (certain, benign if sequenced).** Both touch `crates/stella-tools/src/process.rs`, `catalog.rs`, `registry/process_tools.rs`, and `website/content/docs/agent-tools/index.mdx` (including the advertised tool *count*, which each PR bumps differently — the rebase must re-reconcile it, not take either side). Sequence: whichever is green first merges; the other rebases before its ready flag. Merging both without the rebase will either conflict or, worse, text-merge into a wrong tool count.
3. **#2710 forward-references #2714.** Invariant #9 names the scratch tools as the reference shape; if #2710 merges first (likely), main briefly documents tools that don't exist until #2714 lands. Known, short-lived, self-healing — but don't let #2714 stall.
4. **Cache-churn discipline interaction with Phase 0.** This plan's own argument — batch prompt edits so the byte-stable prefix churns once — now also binds #2697: its prompt half should ride with (or immediately after) #2719's release, never as a separate churn event.
5. **Invariant #9 now binds this plan's later tickets.** #2687's `needs-auth` tool (Phase 3), #2678's elicitation surface, and anything Phase 4 adds must ship single-purpose (no mode flags) or they will fail review against the invariant #2710 lands. #2712/#2713 queue further splits of existing tools (`run_lint`, `format_code`, `apply_edits`) that will churn the same catalog/docs seams — batch them if possible.
6. **Tool-surface deprecation window.** #2717 removes `read_output`'s `clear` arm behind a one-release named-error deprecation; release notes for the next release must carry it, and the removal release should not coincide with other tool-schema churn that would mask it.

---

---

## Build note — Phase 0 shipped (2026-08-10)

**Completed.** All five Phase 0 issues landed as one PR, matching the plan's
batch-for-one-cache-churn intent:

- **PR #2719** (`worktree-prompt-contracts-2688`) — closes #2688, #2689,
  #2690, #2691, #2692 on merge. Four new shared contract macros
  (`action_care!`, `injection_defense!`, `faithful_reporting!`,
  `complexity_discipline!`) embedded verbatim in both static prompts with
  parity rows and per-clause pin tests, plus the `## Session environment`
  block in `assemble_system_prompt` (workspace root, git/linked-worktree
  flag, platform, OS release, shell).
- **Issue #2718** (new, filed out of this work) — the model-identity +
  knowledge-cutoff lines of #2692 were deliberately deferred: the worker
  model can be re-routed after prompt assembly, so a line naming
  `Config::model_id` could name a model that never runs the turn, and no
  per-model cutoff table exists yet. #2718 is written as a handoff.

**Out-of-order and cross-phase call-outs:**

- **Phase 4 Track B started before Phases 1–3**: PR #2711 (a #2683 ingest
  slice) is open concurrently. The plan explicitly permits this ("starting it
  early in parallel is fine") and the subsystems are disjoint (store schema
  vs. prompt assembly), so no file-level conflict is expected — but #2683
  must stay off the reliability chain's critical path as written.
- **Bench baseline moved**: #2719 changes `PIPELINE_SYSTEM_PROMPT` — the
  prompt every bench measurement reads. The Phase 2 scoring plan ("rerun the
  bench arms behind #2666/#2667/#2671") must re-baseline on a build that
  already contains #2719, or Phase 0's behavioral contracts will confound
  Phase 2's attributed gains. Do not compare a post-Phase-2 run against any
  pre-#2719 number.
- **Prompt-file merge hazard for Phases 1–3**: any later PR touching
  `crates/stella-cli/src/agent/prompt.rs` (plausible for #2676/#2678's
  interactive surface) now lands amid four new macros and their pin tests. A
  contract wording change must update its per-clause pin test and the
  `SHARED_CONTRACTS` row in `prompt/parity.rs` in the same PR, or the parity
  gate fails by name.
- **#2688 → #2676 soft edge is now live**: the denial-semantics clause
  ("a refused tool call is an instruction to change approach") is in
  production prompts, so the RequireApproval dead-end has its interim
  mitigation. When #2676 lands the real grant surface, revisit the
  `action_care!` wording in the same PR so the clause names the grant path
  instead of papering over its absence.
- **Isolation-gate expectation changed**: the claim-mode benchmark test now
  asserts persona + environment block via `expected_isolated_pipeline_prompt`
  (a `#[cfg(test)]` helper in `prompt.rs`). Any PR that adds another
  computed-not-stored prompt section must extend that helper, not weaken the
  equality.
- **Cache churn accounting**: #2719 spends the one planned prefix
  invalidation. #2718, when built, will spend another — schedule it to ride
  the next prompt-touching release rather than shipping alone.

---

Status note — appended 2026-08-10 (Claude session; progress comment, not plan content)

What has been built from this plan so far, and in what order:

- #2683 (Track B, Phase 4) was built FIRST, before Phases 0–3. Its first slice
  — ingest provenance lineages, session-start staleness alerts, per-file
  permanent dismissal (`stella ingest alerts list|dismiss|restore`,
  `--keep-dismissed`) — is PR #2711 (branch `ingest-lineage-2683`), fully
  green and armed with auto-merge (squash) after a branch update; it lands as
  soon as its refreshed CI completes. The remaining two thirds of #2683 were
  split out as handoff issues: #2708 (`--refresh` bitemporal retire-and-add
  over published records) and #2709 (pinned/scoped/retrieved auto-promotion
  tiers). Work on #2708 is in progress on branch `ingest-refresh-2708`,
  stacked on the #2711 branch.

Out-of-order assessment: the plan itself declares #2683 free-floating ("#2683
and #2684 … Neither blocks nor is blocked by anything else… starting it early
in parallel is fine"), so building it before Phases 0–3 violates no dependency
edge. The real risks are subtler than the arrows:

1. Merge-sequence hazard (stacked squash): #2711 lands as ONE squashed commit
   on main, but `ingest-refresh-2708` carries the original commits. After
   #2711 merges, `ingest-refresh-2708` MUST be rebased onto post-merge main
   (dropping the already-merged commits) before its PR opens, or its diff
   will double-count the first slice and conflict on every shared file.
2. Textual collision risk with Phase 1: the #2711 slice added a session-start
   background sweep in `crates/stella-cli/src/command_deck.rs` (beside
   `spawn_notification_poller`). #2674 (SessionStart dedup) and #2684
   (hook-surface/bus bridging) plausibly touch the same startup region —
   whichever lands second should expect a small merge there.
3. Deferred-copy dependency: the staleness alert text deliberately says
   "re-run `stella ingest <path>`" because `--refresh` does not exist yet.
   #2708 must update `alert_body` in
   `crates/stella-cli/src/ingest_cmd/lineage.rs` when the flag ships, or the
   product will advertise a stale remedy.
4. Cache-churn interaction with Phase 0: #2709 (pinned tier) injects into the
   byte-stable prompt prefix. Phase 0's whole rationale is batching prefix
   invalidations into one release — if #2709 lands near the prompt batch,
   coordinate them into the same release to avoid an extra cache-churn event.
5. Substrate gap discovered during the build (recorded in #2708): the
   published-record TOML surface has no supersession/retirement fields today;
   `EffectiveStatus::Superseded` anticipates `supersedes_record_id` but
   nothing carries it, and the promotions JSONL ledger cannot represent a
   retirement (its vocabulary is advisory/blocking enforcement grants only).
   #2708 is therefore record-format work, not just a CLI flag — its review
   should treat field naming as wire-contract.

---

---

## Status note — work completed adjacent to this plan (2026-08-10, turn-loop certification session)

Appended without editing the plan above. None of the phase tickets (#2673–#2692)
have been implemented by this session; the work below landed **before** this
plan was drawn, partly out of the order the plan would have chosen, and it
changes some of the plan's premises.

**Shipped and merged to main (`0367554` tip):**

- **PR #2656** — re-landed the seven stranded #2180-tail commits (sha-pin
  runnable without a checkout, Harbor + adapter in the runner image, cgroup
  nesting, SSM error reporting, unnamed CEs) plus the adapter-source ENV fix.
  The cloud bench substrate runs from main for the first time; every
  measurement below depends on it.
- **PR #2662** — stop-on-proof: (a) a fired flip oracle now kills in-flight
  sibling tools mid-dispatch with synthetic paired closures; (b) a confirmed
  `verify_done` fires the halt on every execute turn; (c) the prove-it
  completion gate — a task-mode turn that mutated the workspace with no
  confirmed `verify_done` gets one claim-then-challenge nudge (field-tested;
  its window-reset defect was caught by trial `gate-ab` and fixed in the same
  PR).

**Measurement corpus this plan's Phase-2 gate can score against** (all
recorded, artifact `e0007bd4-f1e6-4f5e-9fc4-3e9716d7106e`): CC-on-GLM bar
**6/8**; pre-merge Stella-GLM bare **2/8** / pipeline **3/8**; post-merge bare
**3/8** (first-ever path-tracing and GLM build-pov-ray solves; n=1 per cell);
Kimi-K3 pipeline **3/8** at ~$7.85; post-merge pipeline panel in flight.
Problem-area taxonomy from all 29 transcripts: #2666–#2671; tool-first
architecture epic: **#2694** (supersedes #2672).

**Out-of-order risks and merge-sequence issues to carry into the phases:**

1. **The prove-it gate landed before a creation-task proof form exists.** On
   creation-shaped tasks `verify_done` rejects every witness as VACUOUS
   (#2668), so the merged gate can burn budget it cannot convert into proof —
   measured at 30–70% of wall on two trials. #2668 is not in any phase above;
   it should ride Phase 2 (or epic #2694's `prove_creation`), else the gate's
   Terminal-Bench cost stays negative on that task shape.
2. **#2662's clean exits make the service-task defect worse until #2666
   lands.** Background services die with the agent; the only historical
   passes on kv-store-grpc/pypi-server were timeout trials whose daemons
   survived. Any bench scoring of service tasks before #2666 under-reads the
   agent. The plan references #2666 only as a post-Phase-2 bench gate — it is
   effectively a Phase-1-grade prerequisite for honest scoring.
3. **Phase 2 authors rebase over `0367554`.** #2662 rewrote
   `driver/dispatch.rs` and `driver/confident_zero.rs` (check now returns a
   `CompletionRuling` and owns its transcript pushes), and both `driver.rs`
   and `agent.rs` sit exactly at their god-file ceilings. #2680 (transcript
   repair) and #2679 (mid-turn fallback) touch the same seams; branches cut
   before the merge will conflict, and any line growth in those files must be
   paid for in-place (no baseline bumps).
4. **Closing discipline:** #2677 and #2681 answer the measured losses filed
   as #2667 and #2671 — their PRs should carry `Closes` for those, or the
   problem-area issues will outlive their fixes.
5. **Phase 4 Track A overlaps epic #2694** (invocable skills vs "everything
   is a tool with a fixed contract"). Decide before starting #2682 whether it
   ships under the epic's contract discipline or gets retrofitted.

---

---

## Design-alignment note — appended 2026-08-10 (session: "Tool 1st Architecture"; progress comment, not plan content)

Three execution notes above (environment/state, Phase 0, ingest) already record
the PRs in flight: #2719 (Phase 0 batch, closes #2688–#2692, remainder → #2718),
#2711 (first #2683 slice, remainder → #2708/#2709), and the off-plan
environment/state wave (#2710, #2714, #2717). This note records the *design*
work this session completed on top of the same tickets, and the risks that
creates for the merge sequence — it deliberately does not repeat the file-level
collision list the notes above already carry.

**What this session shipped (no code, all GitHub):**

- Filed **#2716** — the governance half of tool-first epic #2694: `ToolContract`
  (input **and** output schemas validated both ways; `read_only` / `idempotent`
  / `risk` / `requires_approval` as separate fields; declared events), an
  `AuthzGate` port + `Principal` with fail-closed-on-evaluation-error encoded in
  the type, `ToolCtx` in-loop events, contracts over the serve wire, and
  TOML/MCP claim parity with an explicit trust boundary. Attached as a
  sub-issue of #2694. Scoped from a read of oxagen-platform's capability-
  contract kernel (Stella will be Oxagen's engine, so the shapes must rhyme).
- Appended **tool-first alignment sections** to eleven tickets in this plan's
  universe: #2688–#2692, #2676 (now "Revision 2" — re-scoped as the
  approval-flow deliverable of the #2716 seam), #2684, #2682, #2678, #2687,
  #2675.

**Risks / dependency and merge-sequence issues this adds:**

1. **#2719 was authored against the pre-alignment ticket text and closes all
   five prompt tickets on merge.** The alignment sections appended to
   #2688–#2692 the same day ask for things the original text did not: denial
   semantics phrased against #2716's structured `Decision` taxonomy, the
   engine-marker list enumerated from one constant table (not prose), the task
   board named as the scope ledger, and a `$STELLA_SCRATCH` line in the
   environment block once #2714 lands. When #2719 merges, GitHub auto-closes
   the five and those deltas fall out of tracking. **At merge time: check each
   alignment section against the PR's macros and re-file what is absent** (or
   state in the closing comment that #2716 is the carrier). Do not let the
   close event silently discard the deltas.
2. **Several plan tickets changed meaning after this plan was written — read
   them again before building.** #2676 is no longer "wire RequireApproval to
   ask_user" freehand: it is the emit-before-block, parked-TTL, deny > ask >
   allow approval flow of the `AuthzGate` seam. #2684's shell-hook decision
   JSON must be the same `Decision` enum, not a parallel one. #2682's
   `allowed-tools` is specified as grant ∩ operator policy (`narrow_with`
   semantics), never a widening. #2675's fix should leave the wire-name
   computation in one function the future contract registry can call. A worker
   who starts Phase 1/3/4 from this plan's original one-line summaries will
   build the wrong seam.
3. **#2716 converges on the same files the state/environment wave is churning.**
   The contract registry subsumes `catalog.rs` (one row per tool becomes one
   contract), and #2714/#2717/#2712/#2713 are actively editing that file and
   its doc mirrors. Sequence #2716's implementation *after* the current
   catalog churn settles, and treat any `ToolContract` field addition as a
   deliberate prompt-prefix invalidation event under the same cache-churn
   batching discipline the notes above describe for #2718/#2697/#2709.
4. **Phase 2 is still the only phase with quantified bench losses attached,
   and nothing in flight touches it.** Every open PR is Phase 0, Phase 4B, or
   off-plan. If bench movement is the near-term goal, Phase 2 remains the
   critical path, the #2673 → #2679 edge still binds, and (per the Phase 0
   note above) its scoring must re-baseline on a post-#2719 build.

---

---

## Status note — appended 2026-08-10 by the signal-consumer-ledger session

Appended, not edited: everything above is the plan as written. This records what
is actually true against it right now, verified through `gh` rather than
inferred, so the next session does not have to re-derive it.

### First, the thing most worth knowing

**Nothing in this plan has merged.** All twenty tickets (#2673–#2692) are OPEN,
and zero PRs against them have landed. Two are in flight. Any reading of
progress that treats an open PR as done will be wrong about every line below.

### What this session completed — and why it is not from this plan

This session built an unrelated epic. Stating that plainly because the work sits
adjacent to the plan's current phase and will otherwise look like plan progress:

- **Epic #2701** — "every emitted signal names its consumer", with sub-issues
  #2702–#2707 (plus pre-existing #2217 attached).
- **PR #2720** (open) — implements #2702: a signal-consumer ledger in
  `crates/stella-protocol/src/event/consumers.rs`, one row per `AgentEvent` wire
  tag declaring what reads it, with totality tests over `KNOWN_TYPE_TAGS`, a
  down-only ratchet on unaudited rows, and negative controls. Full workspace
  tests, clippy, and the toolchain-free guards are green.

None of that is a plan ticket. It does, however, impose a new obligation on
several of them — see "New dependencies" below.

### Plan status, verified

| Phase | Tickets | State |
|---|---|---|
| 0 — prompt batch | #2688 #2689 #2690 #2691 #2692 | **PR #2719 open**, `Closes` all five |
| 1 — small corrections | #2673 #2674 #2675 | untouched, no PRs |
| 2 — reliability chain | #2681 #2680 #2677 #2686 #2679 | **untouched, no PRs** |
| 3 — interactive surface | #2676 #2678 #2687 | untouched, no PRs |
| 4A — skills | #2682 #2685 | untouched, no PRs |
| 4B — ingest / hooks | #2683 #2684 | #2683 sliced; **PR #2711 open** (`Refs`, not `Closes`); #2684 untouched |

### Out-of-order execution: what is fine, and what is not

**Fine, and should not be "corrected".** Phase 4B is in flight ahead of Phases
1–3. The plan explicitly permits this — Track B "neither blocks nor is blocked
by anything else", with the single caveat of keeping #2683 off the reliability
chain's critical path. It is off it, because that chain has not started.

**Fine, and matches intent.** Phase 0 is landing as one PR rather than five.
That is the plan's own stated reason for batching (one cache-churn event, not
five). One consequence to hold: five issues now close on a single merge, so a
revert of #2719 reopens all five at once.

**The actual risk: Phase 2 has not started.** It is the phase the plan singles
out as the one with quantified losses attached and the only one that can be
scored — "this is where the bench moves". Every phase with an open PR is a
lower-leverage phase. If sequencing is being driven by what is easy to pick up,
the plan's highest-value work is the work being deferred. That is a scheduling
call for a human, not a defect.

**No hard dependency edge has been violated.** #2673 → #2679 and #2681 → #2680
are both entirely untouched, so nothing has jumped its prerequisite. The soft
edge #2688 ····► #2676 is being honored the way the plan intended: #2719 ships
the `action_care!` denial-semantics clause as the interim mitigation for #2676.
Note the corollary — once #2719 merges, #2676 must not be written as though the
mitigation is absent.

### Merge-sequence hazards

1. **Invariant-number collision, confirmed and live.** PR #2710 (`Closes #2700`,
   from the tool-first phase) appends an invariant numbered **#9** to AGENTS.md.
   PR #2720 from this session originally did the same. This is one shared cell
   two PRs are writing, and it fails silently rather than loudly:
   `scripts/check-invariants.sh` validates citations against `1..count`, so a
   duplicate #9 passes the guard while every citation to it resolves to whichever
   entry comes first. **Resolved on our side** — #2720 defers to the older PR,
   takes #10, leaves an explanatory reserved-slot line at #9, and cites the
   number from no Rust code so a renumber is a one-line edit. Two things follow
   for whoever merges: both PRs append at the same location, so git will report a
   textual conflict — resolve by keeping both entries with distinct numbers,
   never by taking one side; and if #2710 closes without merging, #2720 must be
   renumbered to 9 before it lands.

2. **#2719 touches both static prompts.** It is the byte-stable prefix, so any
   other in-flight prompt edit merging near it invalidates the cache twice and
   risks a textual conflict in the same `macro_rules!` region. Land it alone.

3. **#2711 is `Refs`, not `Closes`.** #2683 will not close on its merge, by
   design — it was sliced into PR #2711 plus follow-on issues #2708 (bitemporal
   re-ingest) and #2709 (auto-promotion tiers). Do not read a merged #2711 as a
   finished Track B.

### New dependencies created since the plan was written

- **#2718** — model identity and knowledge cutoff for the session-environment
  block, deferred out of #2719 because the router can re-route the worker model
  after prompt assembly. Not in the plan; a genuine new edge off Phase 0.
- **#2708, #2709** — the two unbuilt slices of #2683. Track B is larger than the
  single ticket the plan costed.
- **The signal-consumer ledger (PR #2720) adds an obligation to any ticket that
  introduces an `AgentEvent` variant.** Once it merges, a new variant fails
  `the_ledger_is_total_over_known_type_tags` until it declares a consumer row.
  The plan tickets most likely to hit this are #2680 (reactive overflow
  recovery), #2679 (mid-turn model fallback), #2677 (budget-aware retry parking),
  and #2684 (expanded hook surface). The cost is one row in
  `crates/stella-protocol/src/event/consumers.rs` and the failure message names
  the tag and what to add — but it is a new step in those tickets that the plan
  above does not mention.

### Suggested merge order from here

#2719 alone (it is complete, closes five, and owns the prompt prefix) → #2710
then #2720, or #2720 alone with a renumber if #2710 stalls → #2711 → then start
Phase 1 and Phase 2 in parallel, taking #2673 before #2679 and #2681 before
#2680. This is a suggestion from the merge mechanics, not a re-planning of the
phases above.

---

**Addendum (same day):** #2718 was subsequently built into PR #2719 itself — the model-identity line plus the knowledge-cutoff data chain (models.dev `knowledge` → model cards → runtime catalog) — so it spends no second cache churn and `Closes #2718` moved onto the same PR. New residue: **#2721** (thread resolved wiring into deck/goal/fleet pipeline-prompt assembly so those surfaces can pass `Some(worker)`; until then they render no model line rather than a guessed one).

Update (same session, later on 2026-08-10): PR #2711 MERGED (slice 1) and PR #2731 MERGED (slice 2: --refresh retire-and-add, supersession at keep, #2728 promotion-ledger lifecycle events, two real bugs found by PR review bots fixed). #2708 and #2728 are CLOSED. The stacked-squash rebase in risk 1 was performed as described; risk 3 (alert copy naming --refresh) is resolved. Remaining from #2683: #2709 (promotion tiers), with risk 4 (cache-churn coordination with Phase 0) still applicable.

---
