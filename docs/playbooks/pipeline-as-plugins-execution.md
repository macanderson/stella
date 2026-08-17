---
id: pipeline-as-plugins-execution
title: "Executing the plugin extraction — the autonomous run protocol"
status: living
---

# Executing the plugin extraction — the autonomous run protocol

The plan is `doc:pipeline-as-plugins`. **This document is how it gets
executed** — the order, the audit bar every change must clear, the merge policy,
and the three points where the run must stop and ask a human.

It is written to be executed by an autonomous session with no human watching, so
everything a human would normally supply — judgement about when to stop, what
counts as done, what must never be auto-merged — is written down here instead.

---

## 0. Standing rules for the whole run

1. **The gate is the floor, not the bar.** `make gate` green is necessary and
   never sufficient. Nothing merges on a green gate alone.
2. **One logical change per PR.** A PR that touches two work items gets split.
3. **Never widen a baseline to pass.** Not `scripts/file-size-baseline.txt`, not
   `deny.toml`, not `scripts/typed-errors-baseline.txt`. If a change needs a
   ceiling raised, that is a signal to split the file, and the split is the work.
4. **Never delete a test to make a change pass.** If a test must go, name it in
   the PR description and say why.
5. **Nothing left behind.** Every defect noticed and not fixed becomes a GitHub
   issue written as a handoff before the phase closes.
6. **Stop and file rather than guess.** If a work item turns out to depend on
   something this document did not anticipate, open an issue describing the
   dependency and move to the next independent item. Do not improvise an
   architectural decision.
7. **Report the unflattering number.** If a benchmark comparison goes against
   the plugin path, say so plainly and stop the phase.

---

## 1. The adversarial audit bar

Every PR this run opens must clear an adversarial audit **before** it is
eligible to merge. A self-review does not count — the point is that the author
and the auditor are different contexts.

### The protocol

For each PR, spawn **three independent auditors** with no shared context, each
given the diff and a distinct lens:

| Lens | Asks |
|---|---|
| **Correctness** | Where is this wrong? Name inputs and the wrong output. |
| **Architecture** | Which invariant does this break? Cite it by number. Is a port becoming a concretion? |
| **Security / authority** | What can a plugin do after this that it could not before? Who is the principal? |

Each auditor is prompted to **refute**, not to approve, and defaults to
"blocking" when uncertain. A finding is only recorded if it names a concrete
failure — inputs plus wrong behaviour, or an invariant plus the line that breaks
it. "This could be cleaner" is not a finding.

### The decision

- **Any auditor raises a blocking finding** → fix it and re-audit from scratch.
  Do not argue with the auditor in place of fixing.
- **Two or more auditors independently raise the same non-blocking finding** →
  treat as blocking. Independent agreement is signal.
- **All three clear it** → eligible to merge, subject to §2.

### The audits that must be deeper

Three work items get **five** auditors instead of three, because a defect in
them is not recoverable by a follow-up PR:

- **A1 (plugin identity)** — a mistake here silently grants authority.
- **A3 (the wrapper socket)** — every plugin binds to it; its shape is
  effectively permanent once implementations exist.
- **D3 (self-driving's authority grant)** — the widest grant any plugin will
  hold.

---

## 2. Merge policy

A PR merges when **all** of the following hold. This list is exhaustive; if a
condition is not listed, it is not a reason to merge.

1. `make gate` is green **locally on the merge result**, not just on the branch.
   A green branch and a green base can still compose into a red `main` — that is
   the shared-cell failure this repository has hit repeatedly with
   `Cargo.lock` and `scripts/file-size-baseline.txt`.
2. CI is green on the actual head commit. Re-fetch before believing it: checks
   can display the previous head.
3. The adversarial audit of §1 cleared.
4. The item is **not** behind a human gate (§3).
5. The PR description names any test it deleted and cites the issues it closes,
   with `Closes #N` **both** in the description and as a commit trailer.

**Never** force-push, never merge with admin override, never merge a PR whose
base is a topic branch believing it reaches `main`.

After each merge, re-run the composition check on `main` (`make main-canary`).
If `main` goes red, **stop the entire run** and fix it before opening anything
else. A red `main` makes every subsequent PR red and destroys the signal this
whole run depends on.

---

## 3. The three human gates — stop and ask

These are architectural decisions with one-way doors. The run **must not**
decide them, and must not proceed past them by picking the option that unblocks
itself.

| Gate | Question | Where it is argued |
|---|---|---|
| **G1 — transport** | Shell-hook protocol, or MCP plus a lifecycle extension? | `doc:pipeline-as-plugins` §5, #3246 §O5 |
| **G2 — `judge`** | Does `judge` stay in-process and pure, with plugins supplying evidence and declaring rules as data? | `doc:pipeline-as-plugins` §6 |
| **G3 — self-driving authority** | May a plugin hold `gh`, AWS, `brew`, `~/.zshrc` and daemon powers, and under what gate? | `doc:pipeline-as-plugins` §10 |

At each gate: post the evidence gathered, state the recommendation and its
reasoning, name what would falsify it, and **stop**. Do not open the dependent
PRs. Work on an independent item instead, or end the run cleanly with a summary.

**G1 is preceded by a spike, not by an argument.** Implement the Stop-hook path
twice — once as a shell hook, once as an MCP server with a lifecycle extension —
compare them on latency, failure modes, and how much a non-Rust author has to
understand. Present the comparison, not a preference.

**G2 is preceded by a falsifier.** Express one genuinely different definition of
done — not Vera's — as declarative data. What will not fit is the evidence for
widening the grammar. Run this before A3 freezes the socket.

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

**A1** (plugin identity) → **A2** (#3387 blessed constructor) → **G1 spike** →
**G1 gate** → **G2 falsifier** → **G2 gate** → **A3a + A3b** (socket, both halves
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
executions row. **A side-by-side benchmark must hold before the built-in path is
deleted** — and a five-task solve rate cannot resolve anything smaller than a
catastrophe, so comparisons need repeats per task and any two runs differing by
more than one commit are confounded and must be reported as such.

Cutting `stella-cli`'s dependency on `stella-pipeline` is the **last** slice.

### Phase 5 — Track D, self-driving as a plugin

**G3 gate first.** Then the command surface, then the plugin. Keep
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

The run ends — cleanly, with a report — when any of these is true:

- All phases are complete.
- A human gate is reached and the recommendation has been posted.
- `main` is red and the run could not repair it.
- A benchmark comparison went against the plugin path.
- Two consecutive phases produced no mergeable work.

The final report states: what merged, what is open, what was filed, which gates
are waiting on a human, and — explicitly — **anything that was shipped as an
expedient**, because shipping a shortcut quietly is the one unrecoverable move.
