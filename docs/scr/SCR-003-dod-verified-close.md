---
id: SCR-003
title: Close issues only against a verified definition of done
status: active
origin: delegation-to-DoD steering pattern, 2025–2026
trigger: declaring any issue or task complete
autonomy: L2
enforcement: AGENTS.md standing-decisions block (imported by CLAUDE.md); .github/ISSUE_TEMPLATE/task.yml with mandatory DoD checklist; dod-check merge gate — a PR must link the issue it closes and every linked DoD item must be ticked before it can merge; dod-close-guard reopens an issue closed as completed while DoD items remain unchecked. Both are reusable workflows implemented once in oxagen and called by the other four repos (ADR-039).
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

A pull request that closes no issue is waived from the merge gate by the
`no-issue` label. It exists for changes with no meaningful DoD — a typo, a
pinned-digest bump, a revert. It is a label rather than a phrase in the
description so that every use is enumerable (`is:pr label:no-issue`): an
escape hatch nobody can audit becomes the default path. Reach for it when
filing an issue would be pure ceremony, never to skip a DoD that should have
been written.

Issues predating the task template have no DoD section at all. The close
guard notes them and leaves them closed rather than reopening the historical
backlog; the merge gate still refuses a PR that *claims* to close one, since
a close with nothing to verify is exactly the guess this SCR exists to
prevent.
