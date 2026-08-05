---
description: Audit phase — find what the fix batch did not cover, deduplicated across cycles so the loop can terminate.
argument-hint: "[deep|fast]"
---

# fullauto:audit — discover what is left

Run the audit against the **post-fix** tree. The question is not "is this code
good", it is "what did this cycle's batch fail to cover, including anything it
introduced".

```
deep   /ultraudit   full-coverage, cross-model refutation, blind scoring panel
fast   /reaudit     17-dimension rubric, comparable score across runs
```

Default `deep`. Use `fast` when the previous cycle was dry and you are checking
whether it stayed dry — a full audit on a converged tree is expensive noise.

---

## Why the digest matters more than the finding

The loop's whole termination story rests on one number: **how many findings this
audit produced that no previous audit had produced.** Get the dedup wrong and the
loop either never stops or stops too early.

Deduplicate by **content digest**, never by issue number:

```bash
scripts/fullauto.sh seen --digest "crates/stella-core/src/driver.rs the retry counter is never reset between goal rounds"
# -> 9f2a1c4e7b0d5a83
```

The digest lowercases, collapses whitespace, and replaces line numbers with `:L`
before hashing — so the same defect re-described after unrelated edits shifted it
down twelve lines is still **one** finding.

Then:

```bash
scripts/fullauto.sh seen --new <d1> <d2> <d3>    # prints only the unseen ones
scripts/fullauto.sh seen --add <d1> <d2> <d3>    # after you have filed them
```

**Dedup against `seen`, not against "issues I filed."** A finding you filed and
then closed as `wontfix` would otherwise reappear every single cycle and reset
the streak forever. `seen` means *triaged*, not *fixed*.

## The oracle — and why it does not end anything

A cycle is **dry** when it produced zero unseen findings. Fixing twenty bugs does
not make a cycle dry; **discovering nothing does.**

Two consecutive dry cycles under the same lens closes the current **aperture**
and opens the next one. One is noise — an audit that happened to look in the
same places twice. The streak is scoped to the lens: a freshly opened aperture
starts at zero, so every lens earns its own two dry audits before it closes.

The aperture is the point. "We found no defects" is only ever a statement about
the lens you were looking through, and a lens that has never run cannot have
found nothing:

```bash
scripts/fullauto.sh aperture --list   # every lens WITH its tool backing
```

```
rubric  properties  invariants  concurrency  performance
supply-chain  security  docs  soak  →  watch
```

Each is a different question, not a deeper pass of the same one. A codebase that
is clean under `rubric` can be full of races under `concurrency`, and no amount
of re-running `rubric` will ever say so.

## What backs each lens (#1549)

Every lens declares, on the record (`stella-core::fullauto::LENSES`, printed by
`aperture --list`), either the concrete command the cycle runs and how to
interpret it, or that it is **model-only**. No lens is silently a no-op:

- **Tool-backed** — `rubric` (`/ultraudit` / `/reaudit`), `properties` (the
  `rg --files-without-match 'proptest'` sweep over the property-tested crates),
  `invariants` (`make invariants` + reading the numbered list against the
  code), `performance` (`bench loop` + the prompt-cache goldens; heavy tier
  only), `supply-chain` (`make supply-chain`), `docs` (`make doc-links`,
  `make doc-report`, `check-command-docs.sh`), `soak` (`bench h2h`; heavy
  tier only).
- **Model-only** — `concurrency` and `security` have no mechanical backing
  yet. You are working unaided there: say so in the report, and treat "the
  tool found nothing" as a claim you cannot make.

A lens whose tool reports nothing can still go dry — dryness is about unseen
findings, not tool output — but a *missing* tool must never read as "clean".
`cycle-begin` emits the declared backing as `$FULLAUTO_APERTURE_TOOL`; record
what you actually ran with `--lens-tool` on `cycle-end`, so every ledger
record says which tool produced that cycle's findings.

When the last lens goes dry the loop enters **watch mode**: cheap sentinels, no
spend, waking when `main` moves, a defect is filed, or CI goes red. It never
declares itself finished — a clean sweep is a statement about a commit, and the
commit changes.

Audit through the lens the aperture names, not through the one you would have
picked. `$FULLAUTO_APERTURE` carries it into the cycle.

## Rank before you file

Not every finding deserves a ticket at the same urgency, and inflating priority
is the fastest way to make the queue useless:

| Label | Reserved for |
|---|---|
| `P0` | broken or embarrassing for users, right now |
| `P1` | important, next in line |
| `P2` | polish, worth doing, not urgent |

Add the crate label (`area:core`, `area:cli`, …) — the queue ranks on priority
but a fresh agent picks up work by area. Add `self-improvement` when the finding
is about Stella's own capability rather than a defect in it.

## What is not a finding

Do not file, and do not count toward the streak:

- Style preferences `rustfmt` and clippy already settle. The gate is the arbiter.
- "Consider adding tests" with no named behaviour that is untested.
- Anything already open — `gh search issues --repo macanderson/stella "<terms>"`
  before you file, every time. Link, do not duplicate.
- Findings in generated or vendored files.
- A god file being large. `scripts/file-size-baseline.txt` grandfathers those
  deliberately; the gate enforces they do not *grow*. "This file is 2400 lines"
  is a known fact, not a discovery.
