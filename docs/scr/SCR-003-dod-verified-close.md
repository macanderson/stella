---
id: scr/003-dod-verified-close
title: Close issues only against a verified definition of done
status: living
origin: delegation-to-DoD steering pattern, 2025–2026
trigger: declaring any issue or task complete
autonomy: L2
enforcement: AGENTS.md standing-decisions block (imported by CLAUDE.md); .github/ISSUE_TEMPLATE/task.yml with mandatory DoD checklist; dod-check merge gate — a PR must claim to close an issue (or reference one with `Refs #N` without claiming to close it) and every issue it claims to close must have its DoD fully ticked before it can merge; dod-close-guard reopens an issue closed as completed while DoD items remain unchecked. Both are reusable workflows implemented once in oxagen and called by the other four repos (ADR-039).
---

## Directive

An issue closes only when every item in its definition of done is satisfied
and **verified** — tests pass, docs updated, CI green, whatever the checklist
says. Reference-grade covers the whole deliverable: implementation, tests,
code comments, docs, CI config. None is optional polish.

## Rationale

The maintainer delegates to the DoD, not to progress reports or
permission-seeking. A close that hasn't been verified line-by-line converts
the backlog from a contract into a guess, and the gap surfaces later as a
regression someone else pays for.

## How an agent complies

Before closing (or declaring done), run the end-of-task checklist:

1. Re-read the issue's DoD; verify each item with evidence — test output, a
   doc diff, a CI link. A sentence of assertion is not evidence.
2. Sweep for residue; file each item as its own issue via the task template,
   `triage` label only (SCR-004, SCR-005).
3. Confirm every architectural choice made during the task has an ADR
   (SCR-002).
4. Report: what shipped, where it lives, which issues were filed.

## Exceptions

Issues closed as wont-fix / superseded — state the reason in a closing
comment instead of verifying a DoD. Mechanically, close these as **not
planned** rather than completed: only a "completed" close claims the DoD was
met, and it is the one `dod-close-guard` verifies.

A pull request that closes no issue is waived by one of two labels. Which
one is itself a claim.

`no-issue` says the change is trivial: a typo, a bump, a revert. There is no
DoD to verify.

`closes-nothing` says the change is large and closes no issue by design. An
audit that files issues rather than closing them. A refactor. A sweep.
Without it, such a pull request had no route through the gate that was not a
false claim. `Closes #N` would name a close that is not happening.
`no-issue` claims a triviality that is not there.

They are labels rather than phrases, so that every use is enumerable
(`is:pr label:no-issue`, `is:pr label:closes-nothing`). An escape hatch
nobody can audit becomes the default path. Two labels, not one, so a
reviewer can tell a small waived change from a large one at a glance. Reach
for either when filing an issue would be pure ceremony. Never to skip a DoD
that should have been written.

A pull request that advances an issue without finishing it links that issue
with `Refs #N` instead of `Closes #N`. `Refs` does not close, so the merge
gate does not hold that PR against `#N`'s DoD — there is nothing here for it
to verify, because the PR is not claiming the work is done. A PR may carry
both: `Closes #A` closes and is gated on `#A`'s DoD; `Refs #B` beside it is
recorded as a reference and never enforced. A PR whose body disclaims a
close in prose ("this PR does not close #N") is read the same way as a bare
mention — it is not a claim to close, and is not gated on `#N` either, even
though GitHub's own closing-keyword parser has no notion of negation and may
still close `#N` on merge regardless of the "not". That is a gap in GitHub's
parser, not something this gate can veto from the PR side.

Issues predating the task template have no DoD section at all. The close
guard notes them and leaves them closed rather than reopening the historical
backlog; the merge gate still refuses a PR that *claims* to close one, since
a close with nothing to verify is exactly the guess this SCR exists to
prevent.
