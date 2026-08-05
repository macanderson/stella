---
description: One cycle of the perpetual delivery loop — size to the machine, fix a batch, audit, file what you cannot fix, benchmark against Claude Code, ship, recalibrate. Never terminates.
argument-hint: "[--engine=stella|claude] [--bench=auto|loop|h2h|off] [--ship=auto|off] [--force-tier=light|normal|heavy]"
---

# fullauto — one delivery cycle

You are running **one cycle** of Stella's perpetual delivery loop. Loop it:
`/loop 45m /fullauto`.

Two properties define this system, and every phase below serves one of them:

**It does not terminate.** "No more defects" is always a statement about the
*lens*, never about the code. When a lens goes dry the aperture widens; when
every lens is dry the loop drops to `watch` — cheap sentinels, no spend — and
wakes when something changes. It changes duty cycle; it never declares itself
finished.

**It sizes itself.** You do not choose the batch size, the parallelism, or
whether to compile locally. `scripts/fullauto.sh plan` reads the machine's real
disk, memory, CPU and contention, combines it with what previous cycles proved
this box survives, and hands you a plan. Follow it. A cycle that ignores the
governor is how a 16 GiB laptop gets a killed compiler and how a measured
benchmark gets corrupted by a concurrent build.

Each invocation starts with a fresh context, so **everything the next cycle
needs must end up on disk or in GitHub before you finish.**

---

## The contract

Three rules override everything below.

1. **Nothing left behind.** Every defect you notice and do not fix becomes a
   GitHub issue *before this cycle ends*, written as a handoff a fresh agent can
   execute. See `/fullauto:tickets`.
2. **Verified done, not claimed done.** Every behaviour fix ships with a witness
   test that fails on `main` and passes with the change. No witness, no merge —
   label the PR `needs-witness` and say so.
3. **Report what happened, not what you hoped.** A red gate is a red gate. A
   benchmark that did not run is `skipped`, never `passed`. If you fixed 6 of
   the planned 20, the report says 6.

---

## Phase 0 — Plan this cycle against this machine

```bash
eval "$(scripts/fullauto.sh cycle-begin)"
eval "$(scripts/fullauto.sh plan)"
scripts/fullauto.sh plan --explain      # the reasoning, for your report
scripts/fullauto.sh preflight
```

You now have `FULLAUTO_TIER`, `FULLAUTO_BATCH`, `FULLAUTO_PARALLEL`,
`FULLAUTO_LOCAL_BUILD`, `FULLAUTO_SCOPE`, `FULLAUTO_AUDIT`, `FULLAUTO_BENCH`,
and `FULLAUTO_APERTURE`. **These are the cycle's budget. Do not exceed them.**

`FULLAUTO_LOCAL_BUILD=0` is the one that matters most: it means *do not compile
here*. Push the branch and let CI run the gate. The governor sets it when disk
is below the floor, memory is short, or a benchmark match owns the box — and in
every one of those cases a local `cargo build` produces a killed compiler or a
corrupted measurement, not a slow build.

If `preflight` reports **NOT READY**, fix the named tier or stop. A cycle that
cannot file tickets violates rule 1 and must not run.

## Phase 1 — Sync, and refuse to build on red

```bash
git fetch --prune origin
gh run list --branch main --limit 3 --json conclusion,name,url
```

**If `main` is red, fixing `main` IS this cycle's batch.** A red main makes every
PR you open red and you will spend the cycle diagnosing your own change for
someone else's breakage.

## Phase 2 — Fix the batch

```bash
scripts/fullauto.sh queue --limit "$FULLAUTO_BATCH"
```

The ranked queue: open issues labelled `bug` or `triage`, P0 → P1 → P2 →
unlabelled, oldest first inside a rank. Add anything the previous cycle's audit
left unfixed.

Work in coherent groups — **at most `$FULLAUTO_PARALLEL` worktrees at once**, one
PR per group, at most ~5 issues per PR. Twenty fixes in one PR is not reviewable
and will not be reviewed.

**A cycle wants a worktree to itself.** The verify stage observes the WORKING
TREE, and while a run that dispatches no mutating call now disowns foreign
motion (#1553), a cycle that runs shell commands in a tree a human is also
editing cannot tell its own writes from theirs — no verifier can. Do not share
the cycle's worktree; if you must look, look read-only, or take your edits to
another worktree.

For each group:

- Branch from the freshly-fetched `origin/main`.
- **Route the work to the engine.** With `--engine=stella` (default), dispatch
  the fix through Stella itself — this loop is Stella making Stella better, which
  is what the `self-improvement` label is for. Fall back to fixing it yourself
  only if `stella` is absent or its run fails preflight, and say which engine did
  the work.
- Respect the **god files**: `scripts/file-size-baseline.txt` is gate-enforced
  and the files listed in AGENTS.md are closed to growth. New logic lands in a
  sibling submodule.
- Write the witness test first for a behaviour change, and check it the artisanal
  way — it must fail without your fix.
- Gate according to the plan:
  ```bash
  # FULLAUTO_LOCAL_BUILD=1
  make gate CARGO_SCOPE="$(make impacted RANGE=origin/main..HEAD)"
  # FULLAUTO_LOCAL_BUILD=0 — do NOT compile. Push and read CI.
  git push -u origin HEAD && gh pr create --draft …
  ```
- `Closes #N` **in the PR description and as a commit trailer** — both, because
  squash and rebase read different text and either alone silently leaves the
  issue open.

If a defect is **bigger than the plan allows** — a redesign, a missing subsystem,
a decision only Mac can make — do not half-fix it. Leave it and file it in
Phase 4 with what you learned.

## Phase 3 — Audit through the current lens

The open aperture is `$FULLAUTO_APERTURE`. Audit the **post-fix** tree, and audit
it *through that lens specifically* — the point of the ladder is that each lens
sees what the others structurally cannot:

| Aperture | The question it asks |
|---|---|
| `rubric` | the standard engineering audit — `/ultraudit` or `/reaudit` |
| `properties` | what is asserted by example that should be asserted by property |
| `invariants` | where does the code violate AGENTS.md's numbered invariants |
| `concurrency` | races, ordering, cancellation, partial failure |
| `performance` | allocation, cache voids, per-step cost regressions |
| `supply-chain` | `cargo deny`, pinning, licence drift, unvendored risk |
| `security` | untrusted input, path handling, egress, credential surfaces |
| `docs` | where the docs and the code disagree — either one may be the bug |
| `soak` | long-run behaviour: leaks, unbounded growth, wedged loops |

Run `$FULLAUTO_AUDIT` depth (`deep` = `/ultraudit`, `fast` = `/reaudit`). See
`/fullauto:audit`.

## Phase 4 — Triage every finding into a fix or a ticket

For each finding: digest it, check whether it is new, search before filing, file
it as a handoff, record it as seen.

```bash
d=$(scripts/fullauto.sh seen --digest "<file> <one-line claim>")
scripts/fullauto.sh seen --new "$d"        # prints only if unseen
# … file the issue …
scripts/fullauto.sh seen --add "$d"
```

Deduplicate by **digest, not issue number** — a finding closed as `wontfix` would
otherwise reappear every cycle and the aperture would never advance. Full
procedure in `/fullauto:tickets`.

**Count the unseen findings.** That number is `--new` in Phase 7, and it is the
only input to the aperture oracle.

## Phase 5 — Benchmark against Claude Code

Run `$FULLAUTO_BENCH` — the governor already decided what this box can afford.

```bash
scripts/fullauto.sh bench loop        # loop-health gate in CI, well under $1
scripts/fullauto.sh bench h2h --rig   # Claude Code vs Stella, TB2.1, measured
```

`FULLAUTO_BENCH=off` means the box cannot host a valid measurement right now.
Record `skipped`. Do not override it — a benchmark run on a contended box is not
a cheap result, it is a **wrong** one that will be quoted later.

A regression **blocks the ship phase** and gets its own P0. See `/fullauto:bench`
for the three failure shapes that look like a loss but are not.

## Phase 6 — Ship

`--ship=auto` ships only when all of: every PR from this cycle merged, `main`
green, benchmark not regressed, cycle dry.

```bash
/fullauto:upgrade
```

## Phase 7 — Close the cycle and recalibrate

```bash
scripts/fullauto.sh cycle-end \
  --cycle "$FULLAUTO_CYCLE" --tier "$FULLAUTO_TIER" \
  --fixed <n> --filed <n> --new <unseen-findings> \
  --gate green|red|skipped --bench pass|regressed|skipped \
  --prs "1234,1235" --minutes <n> \
  --outcome ok|resource-fail
```

`--outcome` is the **only** input to the controller, so be precise:

| | |
|---|---|
| `ok` | the cycle ran inside its resources — even if the gate went red |
| `resource-fail` | killed compiler, OOM, out of disk, thrash, a run you had to abort for load |

A red gate is **not** a resource failure: the batch was wrong, not too big.
Confusing the two teaches the controller to shrink away from work it could
handle. `cycle-end` recalibrates automatically — additive increase on `ok`,
halved on `resource-fail`.

`cycle-end` also advances the aperture by itself once the dry streak is reached.
Report what it printed.

## Phase 8 — Every fifth cycle, improve the loop itself

```bash
scripts/fullauto.sh metrics
```

If `FULLAUTO_CYCLE % 5 == 0`, or `metrics` printed any `signals for
/fullauto:evolve`, run `/fullauto:evolve`. This loop is part of the
self-improvement loop: its own ledger is evidence about its own performance, and
a pathology it can name it can fix.

## Phase 9 — Decide the next cycle

- **Aperture advanced to `watch`** → run `scripts/fullauto.sh watch`. If it says
  `SLEEP`, stop this iteration having spent almost nothing and report that the
  loop is in watch mode. It will wake on a change. **Do not stop the `/loop`** —
  watch mode is the loop working, not the loop finishing.
- **Otherwise** → report and let the next cycle run.

---

## Rails

Stop and ask Mac when any of these is true.

- A fix requires a **product decision** — an API shape, a default, a user-visible
  behaviour — rather than a repair.
- The gate is red for a reason you did not introduce and cannot localize.
- A benchmark arm scored zero for an **operational** reason (a 401, an exhausted
  balance, a subscription cap). Those are aborts, not losses, and must never land
  in a denominator.
- Spend for this cycle would exceed **$25**, or the rig would run more than
  **2 hours**.
- **Three consecutive cycles ended with `--fixed 0`.** `metrics` flags this as
  `STUCK`. The queue is not the problem; something structural is.
- The controller has been pinned at its floor (`batch_ceiling` 2) for more than
  three cycles — the box can no longer host this work at all.

Never: force-push, push to `main`, merge your own PR without the gate green, or
widen `deny.toml` to make a dependency pass.

---

## Report

End every cycle with this, whatever happened:

```
cycle 12 · stella · light tier · 38 min

plan      batch 5 · parallel 1 · PUSH TO CI · audit deep · bench off
          "only 3GB free of 16GB — a workspace build here swap-thrashes"

fixed     4   (#1493 #1494 · PRs #1546 #1547)
filed     3   (#1548 #1549 #1550)
new       3   findings this lens had not seen before
gate      green (CI, scoped)
bench     skipped (governor: box contended)
ship      skipped (2 PRs still open)

aperture  rubric · dry streak 0 / 2
controller batch<=12 parallel<=1 (1 clean run)

left undone
  #1550  needs a decision on the retry default — Mac
```
