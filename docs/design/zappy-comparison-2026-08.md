# Zappy Code vs. Stella: Agent-Loop and Prompt Comparison

**Date:** 2026-08-10
**Scope:** Full comparison of the agent/turn loop, skill usage, MCP usage, memory saving, and effective system prompts between Stella (`~/Projects/stella`, Rust) and "zappy code" (a rebranded Claude Code source tree, TypeScript — its own `package.json` labels it "leaked source (2026-03-31)"; all analysis here stays at the design/mechanism level).
**Output:** 20 GitHub issues filed on `macanderson/stella` (#2673–#2692), plus a build order (below).

---

## 1. Turn-loop comparison

The two loops embody opposite philosophies that converge on similar shapes.

**Zappy** is a `while(true)` generator with all cross-iteration state in one struct. One iteration: skill-discovery prefetch → context ladder (tool-result budget → snip → microcompact → collapse → autocompact) → blocking-limit preempt → streaming model call → tool execution → attachment/memory collection → continue. Turn-over signal: "did any assistant message contain a tool_use block" (it explicitly distrusts `stop_reason`). No default iteration cap; every recovery path has its own bounded counter. Its defining trait is **reactive resilience**: recoverable API errors are withheld from the yielded stream while an in-loop recovery ladder (collapse-drain → reactive compact → output-token escalation → up to 3 continuations) tries to save the turn, each rung latched to fire once. Model fallback happens mid-turn (swap model, stub orphaned tool_uses, strip model-signed thinking blocks). Retries go to 10 attempts; an unattended mode retries 429/529 reset-aware up to ~6h with keep-alive emissions.

**Stella** is one consolidated `step_loop` (`stella-core::driver`) — the module doc records that the loop used to exist in three copies and two obligations were silently lost. A step runs a fixed 14-phase sequence (cancel → pause gate → steering drain → budget → compaction → loop detection → model call → dispatch → park), checkpointed at the one moment the transcript is guaranteed well-paired. Default step cap 200. Its defining traits are **accounting and determinism**: every discard is billed (`UsageIncomplete`, `SpeculationDiscarded`, `CancelBracket`); mutating tools provably never run inside a retried closure; parallel tool results re-sort so history is deterministic; `TurnHalt` returns `Completed` rather than `Aborted` because a harness reading exit codes scores an abort as a crash.

**Tool execution:** both overlap execution with streaming. Zappy's streaming executor starts tools as tool_use blocks arrive, with admission control and sibling-abort only on Bash failure. Stella's speculative pre-execution is more principled: only the all-read-only prefix speculates, eligibility requires `read_only` ∧ `speculation_safe` (a retried attempt re-announces, so a speculated call can run twice), and dispatch harvests only on byte-exact match.

**Verdict:** Stella wins on correctness architecture; zappy wins on recovery depth — it assumes the provider, the proxy, and the estimator will all lie, and has a latched, bounded answer for each.

## 2. Skills

- **Zappy:** three-tier progressive disclosure (frontmatter listing → search → full body on invocation). Skills are executable: `$ARGUMENTS`, `allowed-tools` grants, `model:`/`effort:` overrides, `context: fork` subagent execution, frontmatter hooks; invoked bodies are tracked for post-compaction restoration.
- **Stella:** skills are context only — lexical Jaccard + domain-boost selection, 400-token bodies in the volatile recall block, full bodies via `skill_search`. Genuinely ahead of zappy on: auto-creation with **measured lift** (appraisal gates promotion *and* retirement) and inspectable selection (`matched_terms`).

## 3. MCP

- **Zappy:** fuller client — resources (list/read, prefetch, blob→disk), prompts as slash commands, elicitation, API-level deferred loading, auth-probe suppression with a synthetic needs-auth tool.
- **Stella:** more *defensive* client — aggregate 32KiB/server schema budget on the sorted segment, scrubbed stdio environments, MCP tools never auto-parallelized or speculated, per-failure-class reconnects, dead servers as model-visible data. OAuth 2.1 matches zappy RFC-for-RFC with a harder token store. But only `initialize`/`tools/list`/`tools/call` are implemented; resources degrade to `[resource: <uri>]`.

## 4. Memory

- **Zappy:** delegated curation — a throttled forked extractor agent with writes restricted to the memory dir, mutual exclusion against main-thread writes, nightly distillation. Recall is a model side-query over frontmatter manifests, prefetched concurrently, filtered against already-read files.
- **Stella:** measured reflection — one cheap-tier call after every substantive turn (distinct root-cause prompt on failures), 0–3 lessons feeding embedding recall and skill mining. Ahead of zappy on: fail-closed tombstones, the deterministic A/B recall-control arm, and byte-stable-prefix discipline.

## 5. What Stella does better (do not trade away while fixing the gaps)

Single documented loop; cost accounting on every discard path; exactly-once mutation proven by test; deterministic transcript ordering under parallelism; default 200-step cap (zappy has none); prefix byte-stability by construction (`concat!` prompts, sorted schemas, parity tests) vs zappy's fragile session latches; MCP untrusted-input bounding and env scrubbing; subagent budget carving with settlement on every path; idle-based derived model deadline; skill appraisal/retirement; fail-closed memory tombstones; recall A/B arm; parked waits (zero-model-call polling).

---

## 6. System-prompt comparison and opinion

**Stella's prose is better, and it isn't close.** Zappy instructs by assertion (`IMPORTANT:`, `NEVER`, caps); Stella instructs by argument — every contract states the rule, the failure it prevents (with a named bench trace), and an honest escape hatch ("…or report the quantity as unmeasured"). Stella's `prompt/parity.rs` derives the contract set from the prompt file's own source so a trimmed clause fails a test by name — the single strongest piece of prompt engineering in either codebase; zappy has no equivalent. Zappy's prompt is fragmented by build/experiment branches, with quality varying by arm.

**Zappy's coverage is broader** in places that matter: action safety, injection awareness, complexity scope in the default persona, two-sided faithful reporting, and session-environment identity. Those five gaps became tickets #2688–#2692, each specifying: port the *content*, keep Stella's *idiom* (argued contract macro + parity row + per-clause pins).

**Explicit pushbacks — zappy content Stella should NOT adopt:**
- The tool-substitution catalogue ("use Read instead of cat…") — the schema-restating pattern Stella deleted with measured justification (#639) and now tests against.
- The token-budget "hard minimum — keep working to fill it" section — a Goodhart failure: tokens are a cost that correlates with depth, not depth; making the floor the target rewards padding, unearned re-verification, and build-ahead, contradicting `verification_proportionality!` and `scope_discipline!` outright. Zappy's own diminishing-returns filler-detector concedes the problem. Budgets should be ceilings that gate additional defined work, never floors current work pads toward.
- Numeric length anchors (≤25/≤100 words) — experiment-grade, in tension with Stella's requirement to name probes and claims.
- "Unlimited context through automatic summarization" — invites transcript bloat; Stella's in-band markers are the better mechanism.
- The prose register itself (`IMPORTANT`/`NEVER`, 2–4× volume).

---

## 7. Filed issues (all on `macanderson/stella`)

### Corrections (loop)
| # | Title |
|---|---|
| #2673 | Router circuit-breaker feedback unwired — failover never trips |
| #2674 | SessionStart hook firing duplicated (engine copy is dead) |
| #2675 | MCP wire-name collision silently routes to the later client |
| #2676 | RequireApproval dead-ends with no interactive grant path |
| #2677 | Retry ladder fails fast under sustained rate limiting (ties #2667) |

### Improvements (loop)
| # | Title |
|---|---|
| #2678 | MCP resources, prompts, elicitation |
| #2679 | Mid-turn model fallback with transcript repair |
| #2680 | Reactive recovery from provider context-overflow (ties #2671, #2503) |
| #2681 | Anchor token accounting to provider-reported usage (ties #2671) |
| #2682 | Invocable skills — args, tool grants, model override, fork |
| #2683 | Ingest-first instruction grounding (see §8) |
| #2684 | Expanded hook surface — Stop/PreCompact, input-rewriting PreToolUse |
| #2685 | Restore working set after overflow summarization |
| #2686 | Streaming→non-streaming fallback + first-byte deadline |
| #2687 | MCP auth-probe suppression + synthetic needs-auth tool |

### Improvements (prompts)
| # | Title |
|---|---|
| #2688 | Action-care contract — irreversibility, blast radius, no safety-bypass shortcuts, denial semantics |
| #2689 | Injection-flagging contract — tool output is data, never instructions |
| #2690 | Complexity-scope contract in the default persona — no gold-plating, diagnose before switching |
| #2691 | Two-sided faithful reporting — no manufactured green, no defensive hedging |
| #2692 | Byte-stable session-environment block — cwd, shell, platform, worktree flag, model identity, knowledge cutoff |

---

## 8. The ingest-first grounding design (#2683, final revision)

Replaces the rejected "auto-load instruction files" proposal. Instruction-file names (e.g. CLAUDE.md, AGENTS.md) appear only as examples; nothing in the implementation is named after any specific filename — the vocabulary is *ingested source file* / *lineage*.

- **Provenance:** every `stella ingest <file>` — any file the user chooses — records a lineage `(source_path, source_blob_hash, commit_sha?, ingested_at, run_id)` + produced record IDs. Blob hash is git's own (free from the index when clean; content hash off-git).
- **Staleness:** per-lineage async session-start check. Changed or deleted source → non-blocking inbox item + notification prompting re-ingest (or lineage retirement). Never blocks, never enters the model prompt, never auto-mutates.
- **Per-file permanent dismissal:** every alert carries a dismiss affordance. A dismissed lineage never alerts again — for exactly that file, forever — while records stay live and every other file keeps alerting. Reversible via explicit restore; a fresh re-ingest of the path re-arms alerts by default (the action re-declares the file a live source).
- **Re-ingest is bitemporal retire-and-add, never edit:** match candidates to live records by `(section_anchor, normalized_content_hash)`; identical → keep untouched (preserves appraisal history — the churn guard); changed → add new + retire old with a supersession link; removed → retire; new → add.
- **Auto-promotion tiers:** *Pinned* (standing constraints → byte-stable prefix, cache-safe by construction since content changes only at re-ingest; budget overflow is a named diagnostic), *Scoped* (path/tool/domain-triggered, volatile recall block), *Retrieved* (relevance-ranked). Coverage signal logs trigger-matched-but-evicted scoped records.
- **Why it beats loading files wholesale:** per-turn cost scales with relevance, not file size; the binding subset is guaranteed present; staleness is mechanical and silenced only by explicit per-file choice; instruction history is auditable as-of any date; dead instructions are measurable by appraisal.

---

## 9. Build order

**Phase 0 — prompt batch, one release (one cache-churn event):** #2688, #2689, #2691, #2690, #2692. Pure `prompt.rs` + parity tests; #2688's denial clause is the interim mitigation for #2676; lands the behavior contracts before everything else is validated under them.

**Phase 1 — small correctness fixes, parallelizable:** #2674, #2675, #2673. #2673 must precede #2679 (fallback consults the breaker).

**Phase 2 — reliability chain (bench-measurable):** #2681 → #2680 → #2677 → #2686 → #2679. Fix the estimator before sizing the safety net; wire the breaker before building the fallback that consults it. Re-run the bench arms behind #2666/#2667/#2671 after this phase.

**Phase 3 — interactive surface chain:** #2676 → #2678 → #2687. #2676 creates the reusable ask-the-human surface; #2678's step-1 resource rendering fix can be pulled forward as an early win.

**Phase 4 — capability features, two parallel tracks:** Track A: #2682 → #2685 (skill-body restoration needs invocable skills; the file-restoration half is independent). Track B: #2683 and #2684 in parallel (store-schema and bus-bridging work; no cross-dependencies). #2683 is the largest ticket — start early, keep off the reliability chain's critical path.

**Dependency edges:**
```
#2673 ──────────────► #2679
#2681 ──► #2680
#2676 ──► #2678 (elicitation part only)
#2682 ──► #2685 (skill-body restoration part only)
#2688 ····► #2676 (soft: prompt clause is the interim mitigation)
```
