---
id: scr/004-residue-becomes-issues
title: Fix what you find; file only what cannot ride the PR
status: living
origin: "north-star requirement: nothing evaporates in a chat transcript — and nothing is logged that does not earn its place"
trigger: the end of every completed task
autonomy: L1
enforcement: AGENTS.md standing-decisions block (imported by CLAUDE.md); end-of-task checklist in SCR-003; PR template "Fix over file" section; target L3 — a sweep agent auditing merged PRs for deferred findings that could have ridden them
---

## Directive

A defect noticed while doing a task is fixed inside the task's own pull
request. Two unrelated fixes in one pull request is fine. A finding is filed
as a GitHub issue only when a fix cannot responsibly ride the pull request:
it needs a decision only the maintainer can make, it needs a rig, a
credential or real spend, or it is larger than the session. And it is filed
only when fixing it would move at least one of six pillars: stability,
reliability, maintainability, innovation, efficiency, or performance. A task
that ends with a deferrable, pillar-moving finding neither fixed nor filed is
not done, even if the code is merged.

## Rationale

The backlog stays alive only if observations survive the session that made
them, and chat transcripts are write-only memory. But an issue is not free:
it is read by the triage agent (SCR-005), ranked, dispatched, and re-read by
the session that picks it up. Between 2026-09-02 and 2026-09-06, 300 fixes
filed about 360 follow-ups, and a read of the result found a large share
were open questions, records of choices already in effect, and measurements
that could not change a decision. None of those moves a pillar, and each one
costs a future session the time to discover that. The cheapest place to
settle a finding is the pull request that found it.

## How an agent complies

- When you notice a defect, fix it now, in the branch you are on. Name each
  extra fix in the pull request description so a reviewer can read them
  apart.
- If a fix genuinely cannot ride the pull request, file one issue with the
  task template (`.github/ISSUE_TEMPLATE/task.yml`): the problem, the file
  paths, how to reproduce, the constraints you found, which of the three
  cases stopped you fixing it, which pillar it moves, and a concrete
  definition of done. Apply ONLY the `triage` label (SCR-005).
- Do not file an open design question (decide it in the pull request, or
  write an ADR under `docs/adr/`), a record of a choice already in effect, a
  measurement that cannot change a decision, a test for a path nothing
  reaches, bookkeeping about the tracker, or a follow-up your own change has
  made moot.

## Scope, and when this rule changes

This directive is written for the period with zero customers, when tracing
a production outage back to the change that caused it is not yet a concern.
It trades a one-fix-per-pull-request history for speed, on purpose. The
maintainer decided that on 2026-09-06 and said the rule will be adjusted
once customers exist: at that point a pull request goes back to carrying
one change, so an outage can be bisected to one merge, and the deferral
cases above narrow. Revisit this section before the first paying customer.

## Exceptions

None. A task with nothing deferred states that explicitly ("nothing was
deferred") in its final report, so silence is never ambiguous.
