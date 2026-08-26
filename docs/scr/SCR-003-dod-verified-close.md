---
id: SCR-003
title: Close issues only against a verified definition of done
status: active
origin: delegation-to-DoD steering pattern, 2025–2026
trigger: declaring any issue or task complete
autonomy: L2
enforcement: AGENTS.md standing-decisions block (imported by CLAUDE.md); .github/ISSUE_TEMPLATE/task.yml with mandatory DoD checklist
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
comment instead of verifying a DoD.
