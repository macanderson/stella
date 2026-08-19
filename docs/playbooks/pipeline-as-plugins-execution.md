---
id: pipeline-as-plugins-execution
title: "Executing the plugin extraction — the autonomous run protocol"
status: living
---

# Executing the plugin extraction — the autonomous run protocol

The plan is `doc:pipeline-as-plugins`. **This document is how it gets
executed** — the order, when an audit is worth running, and how to merge.

It is written for an autonomous session with no human watching. The bias
throughout is **shipping over ceremony**: there are no customers on this yet, so
nearly every mistake is one commit away from being fixed, and the protocol is
deliberately lighter than this repository's usual bar. Where it is strict, it is
strict about the handful of things that are genuinely hard to undo.

---

## Status, 2026-08-19 — the dependency cut has landed

Read against `doc:pipeline-as-plugins`, which carries the item-level detail;
this is the phase-level rollup the execution order below promises.

**The built-in path is removed.** `crates/stella-pipeline` — Phase 4's
"dependency cut, the last slice" — is deleted from the workspace (#3865,
landed on this branch). `stella run --pipeline classic` is refused outright.
This did **not** wait for the rest of Phase 4's stated order below
(`stella-plan` → `vera` → `stella-candidates` → `stella-goal` all remain
unstarted or blocked) or for the benchmark bar §7 of the plan states — see
`doc:pipeline-as-plugins` §7's update for why that sequencing call was made
anyway. Treat the "2026-08-18" rollup immediately below as it was written,
before that deletion: it is the record of what had landed up to Track B's
first extraction, not a claim about the state after this branch.

## Status, 2026-08-18 — re-verified against the tree, per phase

- **Phase 0 (Orientation).** N/A — a per-run step, not something that lands.
- **Phase 1 (independent work).** All four items landed: D1 (self-driving's
  decision core moved to the `stella-autonomy` leaf crate, 5c5c325); #3280
  (`stella-runtime/Cargo.toml` declares no `stella-pipeline` dependency);
  #3473 (the ladder-outcome count correction is in both `AGENTS.md` and
  `doc:turn-loop-wrappers` §4); #3408 (the `[wrapper]` manifest is read and,
  as of #3672 below, driven).
- **Phase 2 (Track A substrate).** Landed in full: A1 through A10 (§4 A1–A10
  of the plan), the D-2 transport spike (decided: the subprocess hook path —
  `doc:plugin-transport-spike`), and the D-1 grammar falsifier (run — §6.1 of
  the plan). The one item this phase's landed note underclaimed at the time:
  A3's "socket exists but nothing drives a live turn through it" gap closed
  separately, after Phase 2 landed — `stella_runtime::WrapperDispatch` (#3494)
  is now the host sequence, and `stella run --pipeline <variant>` drives it.
- **Phase 3 (Track C, the language proof).** No evidence in this repository
  either way — `verify-rs`/`verify-py`/`verify-ts` are specified to live in
  `macanderson/stella-examples`, a separate repository this census did not
  have access to. Treat as **not verified**, not as landed.
- **Phase 4 (Track B extraction).** Started, not finished. Of the five-item
  order (`stella-research` → `stella-plan` → `vera` → `stella-candidates` →
  `stella-goal`), only the first has shipped: `plugins/stella-research`
  (`before_turn` + `recall`, read-only, no worktree — exactly the "safest
  possible first real plugin" this phase named), graded against committed
  vectors in `crates/stella-runtime/tests/research_plugin_*.rs` and driven
  end-to-end by `WrapperDispatch`. Its runs write their variant id to
  `executions.pipeline_variant` via `crates/stella-cli/src/wrapper_plugin.rs`'s
  `RawTurnDriver::run_turn` (`TurnDoor::new("run").wrapped_by(self.variant)`),
  so the side-by-side comparison this phase's bar depends on can already
  distinguish a `stella-research` run from an unwrapped one in the store — see
  `doc:pipeline-as-plugins` §7. `stella-plan`, `vera`, `stella-candidates` and
  `stella-goal` remain unstarted; the dependency cut (the phase's stated last
  slice) has not started either — the reference count grew, not shrank, in
  the same work (§7 of the plan).
- **Phase 5 (Track D, self-driving as a plugin).** Started, not finished.
  `plugins/stella-selfdriving/plugin.toml` exists as the settled D-3 consent
  declaration — the widest grant self-driving needs, written out and provable
  expressible/showable before install — but its own header states plainly
  what it is not: "installing this copies a declaration and a README, and
  starts nothing." No command surface drives it, and no `Principal::Plugin`
  grant binds its declared capabilities to an `AuthzGate` rule yet.
  `scripts/self-driving.sh` remains the sole working driver, untouched, per
  this phase's own instruction not to delete it before a replacement is
  proven.
- **Phase 6 (Documentation).** In progress, across more than one pass. This
  document, `doc:pipeline-as-plugins`, and `crates/stella-pipeline/README.md`
  were rewritten to the post-flip state in this pass; `crates/stella-plugin/README.md`
  and `crates/stella-runtime/README.md` were partially rewritten by an
  earlier commit (#3672) and had residual stale lines corrected in this pass.
  Spot-checked against the tree while writing this status: `run.mdx`,
  `goal.mdx`, `fleet.mdx`, and `arena.mdx` under `website/content/docs/commands/`
  already state the raw-default/`--pipeline <variant>`-opt-in shape correctly
  as of this update, and `inference-pipeline.mdx` opens with "the raw step
  loop is the default on every door" — so at least part of the external
  surface has already moved, likely in a separate pass of this same effort.
  `agent-fleets.mdx:50` ("Pass `--no-pipeline` to run the raw step loop") is a
  counter-example found in the same spot-check — still backwards, not
  rewritten. Do not trust this paragraph as a full audit of
  `website/content/docs/`: verify each page against the tree before assuming
  it is done or stale. Also not yet verified as done:
  `macanderson/stella-examples`'s `plugins/README.md` (separate repository,
  not in this checkout), `llms.txt`'s generated description text, and the
  turn-loop deck under `website/public/presentations/turn-loop/`. This phase
  is explicitly not optional and is not closed by this pass.

---

## 0. Standing rules for the whole run

**Bias to shipping.** There are no customers on this yet and every mistake here
is recoverable by another commit. Velocity is worth more than ceremony, so the
rules below are deliberately few. When a rule and progress conflict, and the
mistake would be cheap to undo, ship and fix forward.

1. **Say what you did.** The one rule that never relaxes. If you widened a
   baseline, deleted a test, added an `#[allow]`, or shipped a shortcut — write
   it in the PR description. Undoing a mistake is cheap; finding an
   undocumented one six weeks later is not.
2. **One logical change per PR**, where that is natural. Do not spend time
   splitting a coherent change to satisfy the rule.
3. **Prefer fixing to filing**, and prefer filing to forgetting. A one-line
   issue is fine here — the handoff-quality bar applies to work you are handing
   off, not to a note for yourself next week.
4. **Report the unflattering number.** If a benchmark goes against the plugin
   path, say so. This one is not ceremony: a flattering benchmark spends the
   exact credibility the project is trying to build.
5. **Do not improvise an architectural decision** that would be expensive to
   reverse — the socket's shape, or who a principal is. Everything else, decide
   and move.

## 1. Adversarial audit — scaled to the blast radius

Audits are for the changes where a defect is *expensive*, not for every diff.
Most of this work is mechanical and its errors are loud.

**Skip the audit entirely** for documentation, renames, dependency deletions,
file moves, and anything the compiler and tests already prove.

**One auditor** for ordinary code changes. Give it the diff, prompt it to
**refute** rather than approve, and ask for concrete failures only — inputs plus
wrong output, or an invariant plus the line that breaks it. "This could be
cleaner" is not a finding. Fix what it finds if the fix is quick; file it and
merge if not.

**Three auditors, and they block**, for the four changes where a defect is not
recoverable by a follow-up PR:

| Item | Why it blocks |
|---|---|
| **A1** plugin identity | a mistake here silently grants authority |
| **A3** the wrapper socket | every plugin binds to it; its shape sets once implementations exist |
| **A5/A3b** the wire contract | the same, for every non-Rust plugin |
| **D3** self-driving's grant | the widest authority any plugin will hold |

Use three lenses there: correctness, architecture (cite the invariant by
number), and authority (what can a plugin do now that it could not before?).
Anything one of them blocks gets fixed before merge.

## 2. Merge policy — light

Merge when the change works and the audit (if §1 required one) cleared. That is
the whole policy.

Specifically:

- **CI green is the goal, not a gate.** If CI is red for a reason your diff did
  not cause — a known flake, an already-broken base, an unrelated guard — say so
  in the PR and merge anyway. Re-running a flake three times is time spent
  buying nothing.
- **Do not run the full `make gate` locally on every PR.** Run what your change
  plausibly touches. The whole workspace test suite is not a per-PR cost worth
  paying here.
- **Do not stop the run when `main` goes red.** Note it, open a fix, keep
  working. Repair it before the phase closes, not before the next commit.
- **Fix forward over revert**, unless the breakage blocks other work.

Two things still hold, because they are about not losing information rather
than about caution: never force-push over someone else's work, and never merge a
PR whose base is a topic branch while believing it reaches `main`.

## 3. Decisions — what is settled, and what the evidence decides

**No decision blocks this run.** An earlier draft of this playbook held three
open gates; two of them were questions the evidence answers, and holding them
open was over-caution rather than caution. What follows is the current state.

### D-1 — `judge` stays in-process. SETTLED (Mac, 2026-08-17).

`judge` remains synchronous, I/O-free and total, so "calls no model" stays a
property of the signature rather than a rule someone polices
(`doc:turn-loop-wrappers` §9.2). Plugins participate by:

- supplying **evidence** out-of-process, in any language, inside the subprocess
  budget — running a test suite is exactly the workload the in-process bus
  cannot host; and
- declaring their **verdict rule as data** — the closed condition grammar,
  `[requirements]`, and the `[oracle]` flip/tamper policy. Data has no
  programming language, so a Python author and a Rust author write the identical
  artifact.

Do not re-litigate this per plugin. Cite this section.

**One task still runs, and it is a falsifier, not a gate.** Before A3 freezes
the socket, express one genuinely different definition of done — not Vera's — as
declarative data. Anything that will not fit is evidence for **widening the
grammar**, not for reopening the decision. Report what did not fit; widen the
grammar in the same phase if the gap is real.

### D-2 — transport: decided by the spike, not by a preference.

Implement the Stop-hook path twice — once as a shell hook, once as an MCP server
with a lifecycle extension — and compare on latency, failure modes, and how much
a non-Rust author must understand to write a plugin.

**Then pick the winner and proceed.** This is a measurement, and a run that
stops to ask a human which number is larger is not automating anything. Record
the comparison in the PR so the choice is reviewable.

Escalate **only** if the spike is genuinely inconclusive — neither option clearly
better on the three axes — and say precisely which axis tied.

### D-3 — self-driving authority: the default is "no new power".

Self-driving already holds `gh`, AWS, `brew`, `~/.zshrc` and daemon powers
today, as a shell script running with full user authority. Making it a plugin
**relocates** that authority; it does not grant it. So the default is settled:
**the plugin gets exactly what the shell script has today, and not one
capability more.**

The real requirement this places on Track A is on the authority system, not on
the decision: `Principal` and the gate must be **able to express** a grant that
wide, and a user must be able to see it at install time. Build to that.

Escalate only if A1 lands and it turns out the grant **cannot** be expressed or
**cannot** be shown to the user before install — that is a design gap worth a
human, and it is a different question from "may it".

### The standing rule underneath all three

Decide from evidence and proceed. Escalate when the evidence is genuinely
ambiguous or when a choice would grant authority nobody asked for — not merely
because a decision feels weighty. When escalating, post the evidence, the
recommendation, and what would falsify it, then work an independent item rather
than idling.

---

## 4. Execution order

Phases are ordered by dependency. Within a phase, items may run in parallel only
where marked; otherwise sequential, because each builds on the last.

### Phase 0 — Orientation

Read `doc:pipeline-as-plugins`, `doc:turn-loop-wrappers`, `AGENTS.md`, and the
capability audit comment on #3380. Re-verify the plan's claims against the tree
before acting on them — the plan cites file:line precisely so this is cheap, and
the tree will have moved. **Any claim that no longer holds is reported, and the
plan is corrected in the same run.**

### Phase 1 — Independent work that unblocks nothing else

Can start immediately, in parallel, gated by nothing:

- **D1** — move the self-driving decision core out of `stella-core` into a
  **shared leaf crate**. Not into a plugin: `stella-observatory` links it, and
  burying it re-creates the #1613 drift where the dashboard and the CLI
  disagreed about loop health. Property tests move with it, unchanged.
- **#3280** — delete `stella-runtime`'s unused `stella-pipeline` dependency.
- **#3473** — correct the ladder outcome count in `AGENTS.md` and
  `docs/spec/turn-loop-wrappers.md`.
- **#3408** — wire the `[wrapper]` manifest so something reads it.

### Phase 2 — Track A substrate, sequential

**A1** (plugin identity) → **A2** (#3387 blessed constructor) → **transport
spike** (D-2: run it, pick the winner, record the comparison) → **grammar
falsifier** (D-1: express a non-Vera definition of done as data; widen the
grammar if it does not fit) → **A3a + A3b** (socket, both halves
authored together) → **A4** (loader) → **A5** (`[runtime]` manifest block) →
**A6** (structured verdicts) → **A7** (`max_holds`) → **A8** (stages and
signals) → **A9** (plugin events and the trace fold) → **A10** (serializable
worktree handle).

A1 is first and is not negotiable. Nothing that grants a plugin any capability
merges before Stella can tell a plugin apart from its operator.

### Phase 3 — Track C, the language proof

Runs **immediately after A5**, not at the end. The examples are the test that
Track A produced a platform rather than a library, and finding that out late is
the expensive failure mode.

Three plugins in `macanderson/stella-examples` under `plugins/`, implementing
the same behaviour: `verify-rs`, `verify-py`, `verify-ts`. Identical manifests
except `[runtime].argv`. No SDK — stdlib and a JSON parser only. CI runs all
three on every PR there, and a smoke check runs in `stella`.

**If the Python or TypeScript plugin needs a manifest shape the Rust one does
not, that is a Track A bug. Stop, file it, fix it in Track A.**

### Phase 4 — Track B extraction, sequential by risk

`stella-research` → `stella-plan` → **vera** → `stella-candidates` →
`stella-goal`.

Each ships behind `--pipeline <variant>` with the wrapper id recorded on the
executions row, so both paths coexist and can be compared later.

**Do not gate each extraction on a benchmark.** Keeping both paths alive is what
makes that safe — a plugin that turns out worse is a flag flip away from being
bypassed, not a rollback. Benchmark once, near the end, before deleting the
built-in path for good. When you do, say plainly if the number goes the wrong
way; a five-task solve rate resolves only catastrophes, so do not read a small
difference as a result.

Cutting `stella-cli`'s dependency on `stella-pipeline` is the **last** slice —
it is the one step that removes the fallback.

### Phase 5 — Track D, self-driving as a plugin

Per D-3 the authority question is settled — the plugin gets exactly what the
shell script holds today and nothing more — so proceed straight to the command
surface, then the plugin. Escalate only if that grant cannot be expressed by the
authority system or shown to the user before install. Keep
`make self-driving-test` green with its assertions intact — if one has to be
relaxed, behaviour changed and that is a bug in the move. Verify the
observatory's `/api/self-driving` route still works rather than assuming it.

### Phase 6 — Documentation, internal and external

**This phase is not optional and does not get skipped for time.** A platform
whose documentation still describes the previous architecture is not shipped.

**Internal:**

- `AGENTS.md` — the workspace layout table, the crate routing rules, the
  invariants list if the socket added one, and the god-file table if any file
  moved. `CLAUDE.md` if the hard rules changed.
- Every affected crate `README.md` — boundary, layout, invariants, gotchas,
  extension recipe. `stella-core`'s must say it has no built-in wrappers;
  `stella-plugin`'s must stop saying it has no consumers.
- `doc:turn-loop-wrappers` and `doc:pipeline-as-plugins` — mark what shipped and
  correct anything the run discovered was wrong. A spec left saying `proposed`
  after it shipped is a lie by omission.
- The three-copy god-file rule: `AGENTS.md`, each crate README, and
  `scripts/file-size-baseline.txt` must agree, and `make god-files` enforces it.

**External:**

- `website/content/docs/` — the plugin platform: what a plugin is, the
  participation grades, the four points, how to install one, how to write one in
  Rust, Python and TypeScript. The inference-pipeline page must stop describing
  stages the engine no longer owns.
- `macanderson/stella-examples` — a top-level `plugins/README.md` explaining the
  model, linked from the repo README.
- `llms.txt` and the commands parity lock, if command surfaces changed.
- The turn-loop deck under `website/public/presentations/` — it is the artifact
  the original directive came from, and it now describes a superseded design.
  Slides must fit the 1600x900 canvas; `deck-fit.yml` checks it.

**The documentation bar:** a competent developer who has never seen this
repository should be able to read the external docs and ship a working Python
plugin without reading any Rust. If they cannot, the phase is not done.

---

## 5. Ending the run

Keep going while there is mergeable work. End with a report when the phases are
done, when an escalation under §3 is genuinely warranted, or when two
consecutive phases produce nothing mergeable.

The report says: what merged, what is open, what you filed, anything awaiting a
human — and **explicitly, every shortcut you shipped**. That last one is the
whole reason the light protocol above is safe: a documented shortcut is a task,
an undocumented one is a trap.
