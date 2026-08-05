# File budget, module boundaries, and test placement — a limit an agent cannot quietly exceed

**Status:** **Proposed.** Supersedes the ratchet in `scripts/check-file-size.sh`
(#629, #825) rather than tuning it; §4 explains why tuning was not enough.
**Date:** 2026-08-02. **Owner:** Mac Anderson.
**Normative home:** this document owns the *reasoning* and the *policy*. The
enforceable *numbers* live in exactly one place — `scripts/check-file-size.sh`
— and `AGENTS.md` restates the rules in imperative form under a parity test
(§8.3). Three copies of a number is how the last limit died.

---

## 1. Why a codebase written entirely by agents needs *tighter* file limits, not looser ones

This is the first question to answer, because the intuitive answer is wrong and
the wrong answer is why the tree got here.

Several classic arguments for small files genuinely **do not apply** to us:

- "A file should fit in one sitting." Nobody is sitting.
- "Keep it under what a person can hold in their head." No head.
- "Long files intimidate new contributors." There is no onboarding.

If that were the whole list, this document should not exist. It is not the
whole list. The remaining arguments get **sharper** under agent authorship, and
three of them exist *only* because agents write the code.

**1.1 — A large file is never read; it is re-read.**
*(Rewritten 2026-08-05. The original claim — "an agent must first read the whole
file, roughly 60,000 tokens to change three lines" — was **measured and
refuted**. See §2.7 / EXP-1. Keeping a refuted argument because it supports the
right conclusion is how the last limit died.)*

Agents do not read large files whole. Measured over 557 `read_file` calls in
`.stella/private/store.db`, the fraction of reads that are *ranged*
(`limit`/`offset`/symbol rather than the whole file) rises monotonically with
file size and hits a ceiling:

| File size | Reads | Ranged | Whole-file |
|---|---:|---:|---:|
| <400 | 52 | 77% | 23% |
| 400–1000 | 165 | 94% | 6% |
| 1000–2000 | 149 | 98% | 2% |
| 2000+ | 154 | **100%** | **0%** |

Not one whole-file read of a 2,000+ line file, in the entire store. The toll is
real but its shape is the opposite of what this section originally claimed: a
file too large to hold is not loaded once at great expense, it is **peeked at
repeatedly**, and each peek must first be *located*. That locate step is the
grep traffic — 293 calls returning ~262k tokens against 557 reads returning
~729k. The tax is search-and-fragment, not bulk load.

This matters for policy: a cost model based on "size of the file you must load"
is wrong, and a budget justified by it would be justified by nothing. The
defensible version is §1.5 (a file with no internal names is expensive to
navigate) plus §2.7's growth-curve finding.

**1.2 — Irrelevant context makes the model worse, not merely slower.**
This is a correctness argument, not an efficiency one. A human skims past
unrelated code at no cost. A model does not: everything in the window competes
for attention, and retrieval accuracy degrades as the irrelevant surround
grows. Loading 4,500 lines to edit 20 makes the agent measurably worse at the
20.

**1.3 — File boundaries are our concurrency primitive.**
`stella fleet` fans out parallel agents into isolated worktrees. Two agents
editing one 4,551-line file collide; two agents editing two 300-line files do
not. File size sets how much of our fan-out actually runs in parallel rather
than serializing into merge conflicts. The proof is in the enforcement layer
itself: `scripts/file-size-baseline.txt` is a single shared file, it conflicted,
and a resolution silently triple-entered `command_deck.rs` (§3.4).

**1.4 — Growth is the default, but *not* because agents append.**
*(Rewritten 2026-08-05. The original claim — "asked to add a feature, an agent
adds code at the end… unbounded growth is the default trajectory" — was the most
confidently stated mechanism in this document and it is **flatly wrong**. See
§2.7 / EXP-2, n = 246,010 added lines.)*

Agents edit **throughout** the file. Mean normalized position of an added line
is 0.480 against a uniform-editing null of 0.500, and only 8.0% of added lines
land in the last 10% of the file — *below* the 10% that random editing would
produce. There is no append bias, and no size interaction (delta −0.009).

What survives, and is measured (EXP-3, 4,752 edits with renames and the crates/
restructure excluded): **files grow relentlessly regardless.** 75–80% of edits
increase a file's length, 8–15% shrink it, net **+24.95 lines per edit**. So the
"only turns one way" premise behind the non-increase rule (§5.3) holds — it just
is not caused by appending. Code is added in the middle, everywhere, always.

The corrected mechanism is in §2.7 and it changes where the budget should bite:
growth is **fastest in the 400–1000 line band (+34.98 lines/edit)** and nearly
stops above 2,000 (+4.64). A limit at 1,500 fires after the acceleration is
over.

**1.5 — A large file is a missing name, and names are how a stranger navigates.**
Every agent session is a stranger. When 4,551 lines are called
`command_deck.rs`, the seams inside it have no names — there is no "the
approval-gate part," because that is not a thing that exists. Module boundaries
are the cheapest architecture documentation we can write and the only kind an
agent reads *for free*, from the file tree, before deciding what to open.

**1.6 — Search stops being an answer.**
Agents locate code by grepping. A hit in a 200-line file named for its job
resolves the question. A hit in a 4,500-line file only names the haystack.

**1.7 — Dead code becomes permanent.**
A 200-line file can be deleted. No agent will confidently prune 40 dead lines
out of 4,551 — the blast radius is unbounded and the callers are not all
visible. We have 398,638 lines of Rust; the large files are where dead code is
buried irreversibly.

**1.8 — A human still reads it.** PR review, and debugging at 2am. Critically:
when an agent edits line 3,200 of a 4,551-line file, a reviewer cannot tell by
inspection whether the other 4,550 lines still mean what they meant. Small
files make the *unchanged* portion reviewable for free.

> **The reframe.** For a human, file size is an ergonomics problem — unpleasant
> but survivable. For an agent the binding constraint moved from "what fits in a
> person's head" to "what fits in a context window alongside everything else the
> task needs," and that constraint is tighter.
>
> **Evidence status of this section, after §2.7.** Measured and holding: the
> navigation cost of a file with no internal names (1.1 as rewritten, 1.5, 1.6),
> and relentless growth concentrated in the 400–1000 band (1.4 as rewritten).
> Measured and **refuted**, now rewritten: whole-file reads (original 1.1),
> append bias (original 1.4). Still **unmeasured** and flagged as such:
> attention degradation (1.2 — plausible from the literature but not measured on
> this codebase), merge-conflict cost (1.3 — §12.2 gives the experiment, and it
> may well kill this one too), and dead-code accumulation (1.7).
>
> The conclusion did not move when two of its reasons died, which is the useful
> thing to know about it. Do not quote an unmeasured line as though it were
> §2.7.

---

## 2. The finding, measured

Measured on `main` at `0731387e`, tracked `*.rs` and `*.py`.

### 2.1 Distribution

| Bucket (raw lines) | Files | Lines |
|---|---:|---:|
| 0–200 | 195 | 23,242 |
| 201–400 | 207 | 61,624 |
| 401–600 | 138 | 67,914 |
| 601–800 | 89 | 61,581 |
| 801–1000 | 54 | 48,858 |
| 1001–1500 | 83 | 103,507 |
| 1501–2500 | 24 | 44,374 |
| 2501–4000 | 7 | 22,378 |
| 4000+ | 4 | 21,246 |
| **Total** | **801** | **454,724** |

**118 files (14.7%) hold 191,505 lines (42.1%) of the code.** Mean file: 568
lines.

### 2.2 The largest files

| File | Raw | Substantive (§5.1) |
|---|---:|---:|
| `bench/terminal_bench_analysis/tb21_analysis.py` | 8,211 | — |
| `crates/stella-cli/src/command_deck.rs` | 4,551 | 3,164 |
| `bench/harbor_adapter/stella_harbor/secure_launcher.py` | 4,277 | — |
| `bench/terminal_bench_analysis/tests/test_tb21_analysis.py` | 4,207 | — |
| `crates/stella-tui/src/deck_ui.rs` | 3,902 | 2,783 |
| `crates/stella-core/src/driver/tests.rs` | 3,660 | 3,062 |
| `crates/stella-pipeline/src/pipeline.rs` | 3,465 | 2,115 |
| `crates/stella-protocol/src/event.rs` | 2,806 | 1,696 |
| `crates/stella-core/src/driver.rs` | 2,725 | 1,423 |

### 2.3 The functions are already fine — this is a *filing* problem

Measuring all 13,518 Rust functions:

| Length | Count | Share |
|---|---:|---:|
| 1–50 | 12,486 | 92.4% |
| 51–100 | 810 | 6.0% |
| 101–200 | 178 | 1.3% |
| 201–400 | 34 | 0.3% |
| 401–800 | 9 | 0.1% |
| 800+ | 1 | 0.0% |

**Only 1.6% of functions exceed 100 lines.** `command_deck.rs` contains 54
top-level functions, 6 types, and 3 impls — it is not a monolith of tangled
logic, it is a filing-cabinet failure. This is the single most important
finding in this document, because it means **remediation is mostly mechanical**:
moving already-clean functions into named modules, not redesigning them.

The one genuine outlier is `run_deck_session` at
`crates/stella-cli/src/command_deck.rs:307` — **1,825 lines in a single function**,
5× the next largest. It needs real surgery; almost nothing else does.

Worst offenders, for the record:

| Lines | Location |
|---:|---|
| 1,825 | `crates/stella-cli/src/command_deck.rs:307` `run_deck_session` |
| 667 | `crates/stella-tui/src/render/entry.rs:210` `entry_body` |
| 573 | `crates/stella-protocol/tests/wire_contract.rs:479` `sample_events` |
| 570 | `crates/stella-pipeline/src/pipeline.rs:2119` `verify_candidate` |
| 509 | `crates/stella-tools/src/registry.rs:712` `execute` |
| 495 | `crates/stella-cli/src/main.rs:471` `run` |

### 2.4 Cost is concentrated in ten files

A large file nobody edits costs nothing. Weighting each file by
`lines × commits-touching-it (last 12 months)` gives the actual read-cost
carried by the tree:

| Read-cost | Lines | Edits | File |
|---:|---:|---:|---|
| 527,916 | 4,551 | 116 | `crates/stella-cli/src/command_deck.rs` |
| 309,029 | 2,359 | 131 | `crates/stella-cli/src/agent.rs` |
| 280,665 | 3,465 | 81 | `crates/stella-pipeline/src/pipeline.rs` |
| 234,120 | 3,902 | 60 | `crates/stella-tui/src/deck_ui.rs` |
| 212,550 | 2,725 | 78 | `crates/stella-core/src/driver.rs` |
| 170,392 | 2,242 | 76 | `crates/stella-tools/src/registry.rs` |
| 160,006 | 2,078 | 77 | `crates/stella-store/src/lib.rs` |
| 151,524 | 2,806 | 54 | `crates/stella-protocol/src/event.rs` |
| 111,370 | 2,590 | 43 | `crates/stella-pipeline/src/pipeline/tests.rs` |
| 102,480 | 1,830 | 56 | `crates/stella-tui/src/deck_render.rs` |

**Ten files out of 800 carry 35% of the total read-cost of this codebase.**
Thirty files carry 53%.

Two conclusions follow, and both shape §9:

- `tb21_analysis.py`, the **largest file in the repo at 8,211 lines**, was
  edited **twice** in a year. It is nearly free. Splitting it first would be
  motion, not progress.
- `crates/stella-cli/src/agent.rs` is only the 8th-largest file but has the **highest
  edit count in the workspace** (131). By cost it is second. Size alone would
  have missed it.

### 2.5 Test placement has no convention at all

| Style | Count |
|---|---:|
| Inline `#[cfg(test)] mod tests { … }` in the same file | 427 files |
| Sibling `tests.rs` under `src/` | 117 files |
| Integration tests under `tests/` | 93 files |

Three conventions coexist with no rule choosing between them. **30 of the 118
files over 1,000 lines are test files**, and `crates/stella-core/src/driver/tests.rs`
is 3,660 lines. Tests are ~22% of the tree and grow with no structure
whatsoever.

### 2.6 The existing gate is red on `main` right now — and the hard block was bypassed too

`./scripts/check-file-size.sh` exits 1 on a clean checkout of `origin/main`:

```
NEWOVER
  crates/stella-tui/src/render/tests.rs is 1703 lines, over the 1500-line limit

GREW
  bench/harbor_adapter/stella_harbor/__init__.py grew to 2264, ceiling 2263 (+1)
  crates/stella-cli/src/fleet_cmd.rs                    grew to 1515, ceiling 1504 (+11)
  crates/stella-core/src/driver.rs                      grew to 2725, ceiling 2589 (+136)
  crates/stella-core/src/driver/tests.rs                grew to 3660, ceiling 3543 (+117)
  crates/stella-pipeline/src/pipeline.rs                grew to 3465, ceiling 3387 (+78)
```

Two distinct violations, and the second one is the alarming one.

**Five files are over their grandfathered ceilings** and merged anyway —
`driver.rs` by 136 lines, `driver/tests.rs` by 117. That is the escape hatch of
§3.3 being used.

**One file, `crates/stella-tui/src/render/tests.rs` at 1,703 lines, is not
grandfathered at all.** It is a file that crossed the limit under the rule
`check-file-size.sh` describes as *"a hard block… the rule that actually stops
the tree getting worse."* It is on `main`. The hard block did not hold either.

The mechanism is that the gate lives in a **pre-push hook that is bypassable by
design** (`SKIP_GATE=1`, `git push --no-verify`), on a repository whose history
shows `main` repeatedly landing broken. A limit whose only enforcement is
advisory is a limit that is negotiated away under deadline, every time. This is
a sixth failure mode (§3.5) and it changes a recommendation: the non-increase
rule must be a **required server-side check**, not a hook, or it inherits
exactly this fate.

Measured a week earlier on a slightly older local `main`, the breach set was
different but the shape identical (seven files over ceiling, including
`event.rs` +22 and `pipeline/tests.rs` +21). The set churns; the redness does
not. Any design that does not explain *how this keeps happening* will be
defeated the same way.

---

### 2.7 The experiments — what was measured, and what it killed

*Added 2026-08-05.* §1 of the first draft argued from mechanism. Three of those
mechanisms were guesses, so they were built into re-runnable experiments under
`scripts/experiments/` and run. Two came back refuted. **The conclusion of this
document survived; two of its stated reasons did not.**

Each script states its claim, its prediction, its method, the confounds it
handles, and refuses a verdict below a minimum sample.

| Experiment | Claim tested | n | Verdict |
|---|---|---:|---|
| **EXP-1** `exp1_read_cost.py` | §1.1 — a big file must be read whole | 557 reads | **REFUTED** — 100% of 2000+ line reads are ranged |
| **EXP-2** `exp2_append_bias.py` | §1.4 — agents append | 246,010 added lines | **REFUTED** — mean position 0.480 vs 0.500 null |
| **EXP-3a** `exp3_growth_ratchet.py` | Growth is effectively one-way | 4,752 edits | **SUPPORTED** — 75–80% of edits grow, net +24.95/edit |
| **EXP-3b** `exp3_growth_ratchet.py` | Large files grow *faster* | 4,752 edits | **REFUTED (reversed)** — they grow ~7× slower |

**The growth curve — the finding that should set the thresholds.** Bucketing
each edit by the file's length *before* that edit (so a file is never credited
to the bucket its own growth created):

| Size before the edit | Edits | Grew | Shrank | **Net lines/edit** |
|---|---:|---:|---:|---:|
| <400 | 1,515 | 77% | 8% | +26.05 |
| 400–1000 | 1,533 | 80% | 8% | **+34.98** |
| 1000–2000 | 1,193 | 79% | 11% | +19.35 |
| 2000+ | 511 | 75% | **15%** | **+4.64** |

Growth **accelerates through the 400–1000 band and then collapses.** A file past
2,000 lines gains 4.64 lines per edit and is the most likely of any bucket to be
actively shrunk. The history contains **64 deliberate reductions of ≥300 lines**
— `deck_ui.rs` −2,958 (#697), `agent.rs` −2,187, `driver.rs` −1,929,
`registry.rs` −1,776. Nobody was forced to do those; they were done because the
files had become unbearable.

Three consequences, all of which change the design rather than decorate it:

1. **A 1,500-line limit is at the wrong end of the curve.** It fires after the
   growth it was meant to prevent has already stopped. This is a better
   explanation for the gate's uselessness than §3.1's "no gradient" and it is
   measured rather than argued.
2. **The 400–1000 band is where the budget must bite** — which independently
   lands on the Green/Yellow boundary (§5.2) proposed for entirely different
   reasons. That is a genuine confirmation, not a restatement.
3. **The team already pays for splits, manually, late, and at maximum cost.**
   The non-increase rule (§5.3) does not introduce new work; it moves 64
   painful reductions earlier, where each one is small.

**What could not be measured, and why.** The cost curve that would settle the
threshold numbers directly — bytes read per line changed, by file size — needs
executions that both read and edit the same file. `files_touched` holds 44 rows
and yields 1–3 samples per bucket. **Not reportable.** EXP-1 prints the table
with its n and a power warning rather than a number anyone could quote. Closing
this needs more logged sessions, not more reasoning; see §12.1.

**Confounds caught during the work, recorded so they are not re-discovered:**

- The first EXP-3 run showed large files *shrinking* (−26.86 lines/edit) and
  reported CLAIM A refuted. Cause: commit `7df3d73f` ("previous version") moved
  303 files under `crates/`, deleting 121,963 lines, which git recorded as
  delete+create and which landed entirely in the 2000+ bucket. Fixed with
  `-M -C` rename detection plus a mass-move exclusion; the script now prints
  what it dropped (10 commits, 1,380 rows, 869 rename rows) instead of dropping
  it silently.
- EXP-1's re-read tax rises 1.49 → 3.00 → 3.39 → 9.06 with size, which looks
  like a strong size effect until you notice `deck_ui.rs` is **75% of all 2000+
  reads**. The bucket is one file that happened to be the task. The script now
  prints that share automatically so the trap cannot be walked into twice.
- EXP-2 weights positions by lines added (capped at 50) so one large commit
  cannot dominate, and excludes file-creation commits, which are 100% "append"
  by construction and would have manufactured the very result being tested.

---

## 3. Why the existing guard did not hold

`check-file-size.sh` is a well-reasoned script — its header comment argues its
own design carefully and most of that argument is right. It failed for six
specific reasons, and each one is a design input.

**3.1 — The limit marks catastrophe, not intent.**
1,500 lines is not a design constraint; it is a "you have already lost" marker.
A 1,499-line file passes cleanly and still costs ~20k tokens to read. Because
there is no gradient between 400 and 1,500, nothing exerts pressure anywhere in
that range — and 431 files live there. The gate only fires after the damage.

**3.2 — Grandfathering has no expiry, so an exemption is a permanent license.**
Thirty-four files are exempt with no owner, no deadline, and no split plan. The
exempt set is precisely the set of files that most need the pressure. An
exemption that outlives the sprint that created it is indistinguishable from a
repeal.

**3.3 — `make file-size-update` is a self-serve repeal.**
The script argues that raising a ceiling "lands as a visible diff to be
justified in review." Empirically it is not: a baseline diff looks like
generated noise and reviewers approve it. §2.6 is the receipt — five ceilings
were breached and nothing stopped the merge. **Any escape hatch an agent can
operate by running a make target will be operated.**

**3.4 — The enforcement artifact is itself conflict-prone shared state.**
`file-size-baseline.txt` is one text file sorted by path, touched by every
branch that changes any large file. It merge-conflicts constantly, and a
resolution that "kept both sides" left this:

```
4551 crates/stella-cli/src/command_deck.rs
4551 crates/stella-cli/src/command_deck.rs
4551 crates/stella-cli/src/command_deck.rs
```

Three entries, from two bad merges. The mechanism corrodes under exactly the
parallel-agent workload it exists to police. **A guard that keeps mutable state
in a tracked file will be corrupted by the fleet.**

**3.5 — The only enforcement is a bypassable local hook.**
`make gate` runs from `.githooks/pre-push`, which is advisory and per-clone:
`SKIP_GATE=1 git push` and `git push --no-verify` both skip it, and neither
leaves a trace in the PR. `file-size` is **not** among the required server-side
checks. That is how a non-grandfathered 1,703-line file reached `main` past a
rule documented as a hard block (§2.6). Nothing in the current design has to
fail for this to happen — the author simply pushes.

Consequence for §5.3: the non-increase rule must be a required status check on
`main`, or it is a suggestion. This is the one part of this proposal that
depends on a repository setting rather than a script, and it is the part
without which the rest does not bind.

**3.6 — The metric taxes the behavior we want.**
Raw line count charges you for doc comments, module docs, and imports.
`crates/stella-core/src/driver.rs` is 2,725 raw lines of which **48% are blanks,
comments, and imports** — 1,423 substantive lines. It is one of the
better-documented files in the tree and the gate punishes it for that. An agent
optimizing against this metric learns to delete documentation.

---

## 4. Design principles

1. **Budget, not a cliff.** Pressure must exist across the whole range, not at
   one threshold.
2. **Count what costs.** Measure substantive lines. Documentation, imports, and
   re-export facades are free — they are what we want more of.
3. **No self-serve escape hatch.** If an agent can widen the limit by running a
   command, the limit is advisory. Removing the hatch is the whole fix for §3.3.
4. **No tracked mutable state.** Derive ceilings from git history, so there is
   nothing to conflict, corrupt, or leave stale (§3.4).
5. **Only-improves.** The rule must make the ratchet turn one way with no human
   in the loop, because the human is not in the loop.
6. **Cheap to obey.** Splitting must be the path of least resistance at the
   moment of writing, or agents will route around the rule.
7. **Fix by cost, not by size.** Ten files, not 118 (§2.4).

---

## 5. The budget

### 5.1 Substantive lines — the unit

A file's size is its count of **substantive lines**: source lines excluding

- blank lines,
- `//`, `///`, `//!`, and `/* … */` comment lines,
- `use` / `pub use` / `pub(crate) use` statements,
- `mod` / `pub mod` declarations,
- bare `#[attr]` lines.

Rationale: this makes documentation, imports, and pure re-export facades cost
nothing. It repeals the tax in §3.6 and it removes the "irreducible growth"
objection that `check-file-size.sh` raises against a hard freeze — adding a
subcommand to an oversized `lib.rs` costs one `mod` line, and one `mod` line is
now free.

Across the workspace, 29% of all lines are non-substantive (404,496 raw →
286,551 substantive), so the budget numbers below are *tighter* than they look
against the raw counts in §2.

### 5.2 The four bands

| Band | Substantive lines | Rule |
|---|---:|---|
| **Green** | ≤ 400 | Target. Nothing to do. |
| **Yellow** | 401–600 | Allowed. File must open with a `//!` doc comment naming its single job in one sentence **without the word "and"** (§6.1). |
| **Orange** | 601–800 | Allowed only for existing files, and only under the non-increase rule (§5.3). CI warns. A **new** file may not be created in this band. |
| **Red** | > 800 | Hard block for any file created or renamed after this policy lands. No baseline, no exemption, no escape. |

Current standing against these bands, in substantive lines: 87 files over 800,
146 over 600, 254 over 400. Of the 87 over 800, 30 are test files (§7).

The chosen numbers are deliberate. 400 substantive lines is roughly 550 raw
lines, or ~7k tokens — a file an agent can load, understand, and edit without
the load dominating the turn. 800 substantive lines (~1,100 raw, ~15k tokens) is
the point at which loading the file costs more than the edit is worth.

### 5.3 The non-increase rule — the durable core

> **A diff may not increase the substantive line count of any file that is
> already over 600 substantive lines.**

Not "may not exceed a recorded ceiling" — **may not increase, at all.** To add
50 lines to `command_deck.rs`, you must first move 50 lines out into a named
module. Properties:

- **It converts feature work into decomposition work at exactly the rate
  features arrive.** The files under the most pressure get split fastest,
  automatically, with no roadmap and no assignee. `agent.rs` is edited 131 times
  a year; under this rule it decomposes itself.
- **It never blocks a fix.** Deletions, net-neutral edits, and refactors pass.
  Only *accretion* into an already-oversized file is blocked, which is precisely
  the failure mode of §1.4.
- **It has no dial.** There is no `--update`, no baseline entry, no make target.
  §3.3 is closed by construction.
- **It only turns one way.** Every over-budget file monotonically shrinks or
  holds. The tree cannot regress, ever, without someone deleting the gate — and
  deleting the gate is a one-line diff a human will notice.

### 5.4 Ceilings derive from the merge base, not a tracked file

Delete `scripts/file-size-baseline.txt`. The comparison point for §5.3 is

```sh
git show "$(git merge-base origin/main HEAD):$path"
```

This is self-healing and stateless:

- Nothing is tracked, so nothing can merge-conflict or be triple-entered (§3.4).
- Nothing goes stale — the ceiling is always what `main` currently says.
- If `main` legitimately grows a file after you branch, merging or rebasing
  advances your merge base and the ceiling follows. No manual reconciliation.
- A deleted or renamed file needs no cleanup entry.

### 5.5 Function budget — enable the lint we already pass

We are at **98.4% compliance with a 100-line function limit** (§2.3). Turn the
lint on rather than writing a script:

```toml
# root Cargo.toml
[workspace.lints.clippy]
too_many_lines     = "warn"   # threshold set in clippy.toml
cognitive_complexity = "warn"
```

```toml
# clippy.toml — appended to the existing disallowed-methods config
too-many-lines-threshold = 100
cognitive-complexity-threshold = 25
```

Each crate adds `[lints] workspace = true`. Two secondary benefits:

- Moving lints into `[workspace.lints]` makes them apply to `cargo check` in an
  editor and to rust-analyzer, not just to the `-D warnings` flags the Makefile
  passes. Today an agent sees clippy failures only at gate time.
- The 222 existing violations each take an `#[allow(clippy::too_many_lines)]`
  with a one-line reason, exactly as `clippy.toml` already requires for
  `Box::leak`. That turns the violation set into a **greppable work queue**
  instead of an invisible backlog.

### 5.6 Item-count budget

Lines are a proxy. The direct signal that a file has stopped having one job is
how many top-level items it declares. `command_deck.rs` declares 63.

> **A file declares at most 15 top-level items** (`fn`, `struct`, `enum`,
> `trait`, `impl`, `const`, `static`, `type`).

This catches the failure mode that line count misses: 40 tidy 30-line functions
in one file is 1,200 lines of perfectly good code with no organizing idea.

---

## 6. Module conventions — making small files the cheap path

### 6.1 One file, one job, stated on line 1

Every file opens with a `//!` module doc comment stating its single
responsibility in one sentence. **If that sentence needs the word "and," the
file needs to be two files.** This is the cheapest available test for "does this
module have one idea," it is applied at the moment of writing rather than at
gate time, and it produces the module documentation as a side effect.

### 6.2 Keep the sibling-file module layout

The workspace already uses the modern layout — `foo.rs` plus a `foo/`
directory — with only 5 `mod.rs` files against 110 module directories. Keep it.
It is the right idiom and it makes splitting a mechanical operation:

```
crates/stella-cli/src/command_deck.rs          4,551 lines, 63 items
    ↓
crates/stella-cli/src/command_deck.rs          ~120 lines: the //! doc, mod decls, run_deck_session's shell
crates/stella-cli/src/command_deck/session.rs  the turn loop extracted from run_deck_session
crates/stella-cli/src/command_deck/pr.rs       PrObservation, spawn_pr_monitor, observe_pr, aggregate_ci
crates/stella-cli/src/command_deck/issues.rs   issue_backend, list_issue_rows, handle_issues_input, *_hit
crates/stella-cli/src/command_deck/services.rs service_registry_action, service_inspect_action, workers
crates/stella-cli/src/command_deck/config.rs   engine_config_inbound, tool_policy_inbound, handlers
crates/stella-cli/src/command_deck/agents.rs   handle_agents_input, save_agent, pin_agent
crates/stella-cli/src/command_deck/commands.rs DeckCommand, ModelsCommand, parsing
```

The seams are already visible in the file's own line ordering (§2.3) — the
functions are grouped by concern, they simply have no names. This is close to a
pure `git mv` of function bodies plus visibility adjustment.

### 6.3 `lib.rs` and `main.rs` hold no logic

A crate root is a **façade**: `//!` crate docs, `mod` declarations, and
`pub use` re-exports naming the crate's public surface. Under §5.1 all three are
free, so a correct crate root has a substantive line count near zero.

`crates/stella-store/src/lib.rs` is 2,078 raw / 1,265 substantive lines and is 7th by
read-cost. It is a crate root doing a module's job.

### 6.4 Extraction is the default response to growth

When a file crosses Yellow, the response is never "add a section comment." It is
to create the submodule the section comment was about to describe. Section
comments (`// ---- PR monitoring ----`) are a **code smell specific to this
failure**: they are a module boundary someone declined to make real.

---

## 7. Test placement conventions

Tests are 22% of the tree, follow three incompatible conventions (§2.5), and
account for 30 of the 118 files over 1,000 lines. They need the same discipline
as production code and one additional rule.

### 7.1 The agent-specific argument for separating tests from code

An agent editing production logic does not need the tests loaded. An agent
adding a test does not need the production logic loaded. Splitting them **halves
the read cost of both jobs** — which is the same argument as §1.1, applied
inside the file. Inline `#[cfg(test)] mod tests { … }` forces every reader of
the production code to also load the tests, on every turn, forever.

This is a stronger argument for agents than it ever was for humans, who could
simply scroll past.

### 7.2 Four tiers, four fixed homes

| Tier | Home | Rule |
|---|---|---|
| **Unit** — needs private access to one module | `src/foo/tests.rs`, declared `#[cfg(test)] mod tests;` in `foo.rs` | **Never inline.** Always a sibling file. |
| **Behavior** — one behavior area of the crate's public API | `tests/<area>.rs`, named for the behavior | One file per behavior. Never `tests/integration.rs`. |
| **Property** — invariants (`proptest`) | `src/foo/props.rs` or `tests/props_<area>.rs` | Kept separate from unit tests so the invariant set is enumerable by listing files. |
| **Fixtures & helpers** | `tests/support/` or `src/testkit.rs` behind `#[cfg(test)]` | Shared setup is defined once, never copied into test files. |

### 7.3 Test files carry the same budget, and split by *behavior*

The §5.2 bands and the §5.3 non-increase rule apply unchanged to test files. The
extra rule is how they split:

> **A test file splits by behavior, never by number.**

`crates/stella-core/src/driver/tests.rs` at 3,660 lines becomes
`driver/tests/compaction.rs`, `driver/tests/retry.rs`,
`driver/tests/budget.rs`, `driver/tests/loop_detect.rs` — **never**
`tests_1.rs` / `tests_2.rs`. Done this way the test directory becomes a
readable table of contents for the module's contract, which is a documentation
win on top of the size win. Done the other way it is the same problem with more
files.

### 7.4 Inline test modules are closed to new code

427 files use inline `#[cfg(test)] mod tests { … }`. For a file comfortably
inside Green this costs little, so existing usage is grandfathered with no
deadline. But:

> **New code does not add an inline `#[cfg(test)] mod tests { … }` block, and a
> file that crosses into Yellow moves its tests to a sibling file as part of
> that change.**

Note the several large files that *already* do this correctly —
`command_deck.rs`, `deck_ui.rs`, `pipeline.rs`, and `registry.rs` all declare
`#[cfg(test)] mod tests;` on their last line. Their production halves are
genuinely 4,551 and 3,902 substantive lines. That is worth knowing: those files
are *worse* than a naive reading of §2.2 suggests, not better.

### 7.5 Witness tests

A witness test (AGENTS.md, "The definition of done") lives in the file named for
the **behavior it witnesses**, not in a `witness.rs` catch-all, and the PR
description names its path. A witness whose home file does not obviously match
its behavior is a sign the behavior does not have a name yet.

---

## 8. AGENTS.md changes

`AGENTS.md` is the file an agent actually reads before writing. The rules must
be there in imperative form; the reasoning stays here.

### 8.1 New section, placed after "Code style and conventions"

```markdown
## File budget — the limit that is not negotiable

Size is measured in **substantive lines**: excluding blanks, comments,
`use`, `mod`, and bare `#[attr]` lines. Documentation and re-exports are free.

| Band | Substantive lines | What it means |
|---|---:|---|
| Green | ≤ 400 | Target. |
| Yellow | 401–600 | Fine, but the file needs a `//!` naming its one job. |
| Orange | 601–800 | Existing files only. A new file may not start here. |
| Red | > 800 | Blocked. Split it. |

**The rule that binds: a diff may not increase the substantive line count of
any file already over 600 lines.** To add 50 lines to an oversized file, move
50 lines out into a named submodule first. There is no baseline, no exemption,
and no make target that widens this — that hatch is what killed the last limit.

Also enforced:
- **≤ 15 top-level items per file** (`fn`/`struct`/`enum`/`trait`/`impl`/
  `const`/`static`/`type`). Forty tidy functions in one file is still a file
  with no organizing idea.
- **≤ 100 lines per function** (`clippy::too_many_lines`). We are already at
  98% compliance; an `#[allow]` needs a reason comment like any other.
- **`lib.rs`/`main.rs` hold no logic** — `//!` docs, `mod`, `pub use`. All free
  under the metric, so a correct crate root scores near zero.

**Write the `//!` first.** One sentence naming the file's single job. If the
sentence needs the word "and", write two files. This is the whole discipline;
everything above is just the part a script can check.

**Never respond to growth with a section comment.** `// ---- PR monitoring ----`
is a module boundary you declined to make real. Create the submodule instead.

Check locally: `make file-budget` (or `make check`, which includes it).
```

### 8.2 Replace the "Testing approach" section

```markdown
## Testing approach — where a test goes

| Tier | Home |
|---|---|
| Unit (needs private access) | `src/foo/tests.rs`, via `#[cfg(test)] mod tests;` in `foo.rs` — **never inline** |
| Behavior (public API) | `tests/<behavior>.rs`, one file per behavior area |
| Property (`proptest`) | `src/foo/props.rs` or `tests/props_<area>.rs` |
| Fixtures / helpers | `tests/support/` or `#[cfg(test)]`-gated `src/testkit.rs` |

Test files carry the same file budget as production code. **A test file splits
by behavior, never by number** — `driver/tests/retry.rs`, not `tests_2.rs`. The
test directory should read as a table of contents for the module's contract.

Do not add a new inline `#[cfg(test)] mod tests { … }` block. When a file
crosses 400 substantive lines, move its tests to a sibling file as part of that
change. An agent editing production logic should not have to load the tests, and
vice versa — that is half the read cost of both jobs.

A witness test lives in the file named for the behavior it witnesses, and the PR
names its path.

(Unchanged: property tests cover loop detection, retry history, skill selection,
the task board, retrieval fusion, fleet planning, witness verification, and
render/scroll. Wiremock for provider adapters. Fixture MCP servers in
`crates/stella-mcp/tests/`. Replay fixtures in `crates/stella-pipeline/tests/`.)
```

### 8.3 Parity-lock the numbers

Three documents once asserted a 1,500-line cap that nothing enforced (§1.4).
Prevent the recurrence structurally: the band thresholds are defined **once**,
as constants in `scripts/check-file-size.sh`, and a test asserts the table in
`AGENTS.md` matches those constants. We already parity-lock the website's
command index against `cli.rs` this way — same mechanism, same reason. A number
that appears in two places will drift; a number that appears in two places
*under a test* cannot.

---

## 9. Remediation — ten files, in cost order

Do **not** work the list in size order. §2.4 shows the 8,211-line
`tb21_analysis.py` was edited twice in a year and is nearly free, while the
2,359-line `agent.rs` was edited 131 times and is second-most expensive.

Ordering by `lines × edits`, and noting that ten files carry 35% of total
read-cost:

| # | File | Cost | Shape of the split |
|---|---|---:|---|
| 1 | `crates/stella-cli/src/command_deck.rs` | 528k | 8 submodules per §6.2. **Includes the real surgery: `run_deck_session` at 1,825 lines.** |
| 2 | `crates/stella-cli/src/agent.rs` | 309k | Highest churn in the repo. `run_pipeline_one_shot` (453) and `run_interactive` (426) extract first. |
| 3 | `crates/stella-pipeline/src/pipeline.rs` | 281k | Stage-per-module; `verify_candidate` (570) is its own file. |
| 4 | `crates/stella-tui/src/deck_ui.rs` | 234k | 15 types + 65 fns; split by panel. `handle_deck_key` (386), `ingest_inbound` (311). |
| 5 | `crates/stella-core/src/driver.rs` | 213k | Only 1,423 substantive of 2,725 — **the cheapest win on the list**. |
| 6 | `crates/stella-tools/src/registry.rs` | 170k | `execute` (509) extracts; registry vs. dispatch separate. |
| 7 | `crates/stella-store/src/lib.rs` | 160k | §6.3 — a crate root doing a module's job. Mostly mechanical. |
| 8 | `crates/stella-protocol/src/event.rs` | 152k | 21 types. Split by event family. Watch the serde round-trip tests (invariant #4). |
| 9 | `crates/stella-pipeline/src/pipeline/tests.rs` | 111k | §7.3, split by behavior alongside #3. |
| 10 | `crates/stella-tui/src/deck_render.rs` | 102k | `render_status_bar` (305); split by rendered region. |

Then, and only then, the low-churn tail: the four `bench/` Python files
(15,900 lines between them, 14 edits in a year) are the **last** thing to touch.

Note that #1, #3, #4, and #6 already declare `#[cfg(test)] mod tests;` on their
final line, so their substantive counts are production code with nowhere to
hide. #5 is 48% documentation and imports — it will drop below Orange with
close to a pure `git mv`.

Each split is one PR, scoped to one file, with no behavior change, so
`cargo test -p <crate>` passing *is* the proof. Per AGENTS.md, a pure refactor
needs no witness test — say so in the PR template.

---

## 10. Rollout

| Phase | Change | Gate posture |
|---|---|---|
| **1** | `make file-budget` implements §5.1–§5.4. `check-file-size.sh` and `file-size-baseline.txt` are deleted. `AGENTS.md` gains §8.1–§8.2. | Report only. Prints every band violation; exit 0. |
| **2** | Non-increase rule (§5.3) turns on **and `file-budget` is added to `main`'s required status checks** (§3.5). `[workspace.lints]` + `too_many_lines` at warn (§5.5). | Non-increase **blocks, server-side**. Bands still report. Parity test (§8.3) blocks. |
| **3** | Remediation #1–#5 (§9). Item-count rule (§5.6) turns on for new files. | Red band blocks for **new** files. |
| **4** | Remediation #6–#10. `too_many_lines` to deny with `#[allow]` + reason at each of the 222 sites. | Red band blocks for all files. |

Phase 2 is the load-bearing one, and it has **two** halves that must ship
together. The non-increase rule makes the tree monotonically improve; the
required status check makes the rule binding. Shipping the rule as a pre-push
hook alone reproduces §3.5 exactly — a 1,703-line file reached `main` past a
documented hard block because the only thing standing there was a hook someone
could skip. With both halves live, the tree cannot get worse regardless of
whether phases 3 and 4 ever ship on schedule, which is the definition of a
durable fix and the property §3 says the old gate never had.

This is the one dependency outside the code: it needs a branch-protection
change on `main`, which is owner-only. Note that `main`'s protection is already
known to block admin merges while requiring review, so adding a required check
is a settings edit, not a new workflow.

Phase 1 makes CI **green** on a tree that is currently red (§2.6), because it
replaces the corrupted baseline with a derived ceiling: the five ceiling
breaches and the one ungrandfathered file are absorbed as the new starting
point rather than needing six separate ceiling bumps. That is a deliberate
one-time amnesty — it is what buys the right to have no escape hatch afterward.

---

## 11. Rejected alternatives

**Tighten the existing ratchet to 800 and regenerate the baseline.** This is the
obvious move and it fails for the reason in §3.3: `make file-size-update`
remains, so the limit remains self-serve. It also keeps the conflict-prone
tracked file of §3.4, the documentation tax of §3.6, and the bypassable
enforcement of §3.5. Tuning the number does
not touch any of the three mechanisms that actually defeated the gate.

**A hard freeze on all oversized files.** The existing script's header argues
against this correctly — adding a subcommand costs an irreducible line in an
oversized `lib.rs`, and a guard that blocks every feature gets deleted. §5.1
dissolves the objection instead of overriding it: `mod` and `use` lines are
free, so the irreducible growth is genuinely zero.

**Enforce a byte or token budget instead of lines.** Closer to the true cost
(§1.1) but not diffable, not stable across formatting, and not something an
agent can reason about while writing. Substantive lines correlate well enough
and are checkable by inspection.

**Let an agent split files automatically on a schedule.** Tempting, and wrong
for now: an automated split is a large, review-hostile diff touching the
highest-churn files in the repo, landing on a `main` that already merges broken
(see the repo's own history). Splits should be human-reviewed one file at a
time until §9 is done. Revisit afterward for maintenance.

**Do nothing, because agents write the code.** §1.

---

## 12. Open questions

1. **Is 400/600/800 right? — PARTIALLY ANSWERED (2026-08-05).** EXP-3 (§2.7)
   independently locates the fastest growth in the **400–1000** band, which is
   where the Green/Yellow boundary sits. That is real corroboration from a
   direction the numbers were not chosen from. What is still **not** measured is
   the cost curve — bytes read per line changed, by file size — which is what
   would justify 800 rather than 700 or 1,000. `files_touched` yields 1–3
   samples per bucket, far too thin.

   **To close it:** the blocker is data, not method. `exp1_read_cost.py` already
   computes the curve and takes a store path as its argument; it needs a store
   with meaningfully more executions that both read and edit the same file.
   Point it at a Terminal-Bench run's store, or at `~/.stella/usage.db`-scale
   traffic, and the table fills in. Until then, treat 400/600/800 as a defensible
   starting point corroborated on one axis, not as a measured optimum — and say
   so wherever they are quoted.
2. **Does file size actually cause merge conflicts (§1.3)?** Still a guess, and
   the one remaining unmeasured claim in §1. It is genuinely uncertain because
   **git merges by hunk, not by file** — two agents editing distant regions of
   one 4,000-line file may never conflict, in which case §1.3 is wrong and the
   concurrency argument for small files collapses. The experiment: for commit
   pairs touching the same file on divergent branches, measure hunk-range
   overlap, bucketed by file size. If overlap does not rise with size, delete
   §1.3 rather than defending it.
3. **Should the item-count rule (§5.6) apply to `enum` variants?**
   `crates/stella-protocol/src/event.rs` is 21 types and legitimately wide. A protocol
   crate may warrant its own band.
4. **Does the non-increase rule need a per-PR escape for a security fix?** The
   current answer is no — a security fix is nearly always net-neutral or a
   deletion. If a real counterexample appears, the escape must be a human
   action (a signed trailer, a named reviewer), never a make target.
5. **`bench/` Python:** these files have no `//!` equivalent enforced and are
   currently the largest in the tree. Do they adopt the same bands, or does
   `bench/` get its own policy on the grounds that it is not shipped?
6. **Migration order vs. `main`'s stability.** The repo's history shows `main`
   repeatedly landing uncompilable. Ten large refactor PRs will conflict with
   in-flight work; §9's one-file-per-PR scoping is the mitigation, but the
   sequencing may need to yield to whatever else is in flight.

---

## Appendix — reproducing the measurements

```sh
# Size distribution and per-file raw counts
git ls-files '*.rs' '*.py' | while read -r f; do
  printf '%s %s\n' "$(wc -l <"$f" | tr -d ' ')" "$f"
done | sort -rn

# Read-cost ordering (§2.4): lines x commits-touching-file, last 12 months
git log --since='12 months ago' --format='' --name-only \
  | rg '\.(rs|py)$' | sort | uniq -c | sort -rn

# Function lengths (§2.3) and substantive-line counts (§5.1) were computed with
# throwaway scripts; both belong in scripts/check-file-size.sh's replacement
# rather than being re-derived by hand.

# Current gate state (§2.6)
./scripts/check-file-size.sh; echo "exit=$?"
```

This document is 792 raw lines, 585 excluding blanks and table rules — Yellow
band, one job, and it says so on line 1.
