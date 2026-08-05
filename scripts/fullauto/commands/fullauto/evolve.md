---
description: The meta-cycle — audit the loop itself from its own ledger and land an improvement to fullauto as a PR.
argument-hint: "[--dry-run]"
---

# fullauto:evolve — the loop improves the loop

Run this every fifth cycle, or whenever `scripts/fullauto.sh metrics` prints a
signal. This is what makes `fullauto` part of the self-improvement loop rather
than a script that merely runs inside it: **the ledger is evidence about the
loop's own performance, and a pathology it can name it can fix.**

```bash
scripts/fullauto.sh metrics
scripts/fullauto.sh calibrate --show
scripts/fullauto.sh aperture --list
```

---

## The signals, and what each one actually means

`metrics` names specific pathologies rather than a vague "performance is bad",
because the fix differs completely between them.

### `STUCK` — three consecutive zero-fix cycles

The loop is spinning. The queue is not the problem; something upstream is.
Diagnose in this order, because the cheap checks rule out the common causes:

1. Is `main` red? Then every cycle is burning itself on someone else's breakage.
2. Is the queue full of issues that all need a product decision? Then the
   *ranking* is wrong — those should sort below actionable work, not above it.
3. Is the engine failing preflight every cycle (no key, no PATH)? Then the fix is
   in `preflight`, which should have refused the cycle instead of running it
   empty.
4. Is the governor pinned to `light` with `batch=1`? Then the box genuinely
   cannot host the work, and the honest fix is to move the loop, not shrink it
   further.

### `STARVED` — the ceiling stayed low through five clean runs

The multiplicative decrease fired and the additive increase is not keeping up.
Either the increment is too small for the cycle rate, or something is reporting
`resource-fail` when it means `red gate`. **Check the ledger's `outcome` field
against the `gate` field first** — a run of `resource-fail` alongside `gate: red`
is a mislabelling bug in the caller, not a controller-tuning problem, and tuning
the controller would bury it.

### `NOISY` — filing far more than it discovers

Dedup is leaking. The same finding is being re-filed under a slightly different
description each cycle, so the digest differs while the defect does not. Look at
the normalization in `seen --digest`: it lowercases, collapses whitespace and
neutralizes line numbers, and that may not be enough for this codebase. Adding a
normalization rule is a real fix; raising the dry-streak target is not.

### `FRAGILE` — a third of cycles end on a red gate

Batches are too large or too *mixed*. Five unrelated issues in one PR means any
one of them can redden the whole group, and the cycle then spends its time
bisecting its own work. The fix is grouping by blast radius — same crate, same
subsystem — not a smaller number.

---

## How to land the change

`fullauto` is code in this repository and gets the same contract as any other
change:

- **A witness.** For `scripts/fullauto.sh`, the witness is a ledger metric: state
  the number you are moving and what it reads today. "Zero-fix cycles were 40%
  over the last ten cycles" is a witness. "This feels better" is not.
- **`make shellcheck` must pass.** It is a gate step, and `scripts/*.sh` is in
  scope.
- **Keep the file under the size gate.** If `fullauto.sh` approaches the ceiling,
  split a subcommand into `scripts/fullauto/` rather than growing it — the same
  rule the workspace's god files live under.
- **A PR, labelled `self-improvement`**, with the metrics output quoted in the
  description as the before-state.
- **Prose changes go in the command files** under `scripts/fullauto/commands/`,
  then `scripts/fullauto.sh install-commands` to pick them up. Editing
  `~/.claude/commands/` directly loses the change on the next install.

## What NOT to change

Three things look like tuning opportunities and are load-bearing:

- **The dry-streak target of 2.** Raising it does not make convergence more
  reliable, it makes the aperture ladder slower to traverse. If findings are
  reappearing, fix the digest.
- **The optimistic controller seed.** Starting at the hard maximum is deliberate;
  it costs at most one degraded cycle and saves a week of under-using a capable
  machine.
- **The aperture order.** It is widest-yield-first on purpose. Reordering to put
  a favourite lens early means the cheap high-yield passes run after the
  expensive ones.

## Filing rather than fixing

If the pathology needs a change bigger than this cycle can carry, file it like
any other finding — `self-improvement` plus the area label — and quote the
metrics output in the issue body. The next `/fullauto:evolve` reads the ledger,
not your session, so the numbers have to be *in* the issue.
