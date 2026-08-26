---
id: scr/004-residue-becomes-issues
title: File all residue as issues before declaring done
status: living
origin: "north-star requirement: nothing evaporates in a chat transcript"
trigger: the end of every completed task
autonomy: L1
enforcement: AGENTS.md standing-decisions block (imported by CLAUDE.md); end-of-task checklist in SCR-003; target L3 — a sweep agent auditing merged PRs for unfiled residue
---

## Directive

Every completed task ends with a residue sweep: anything noticed but not
done — follow-ups, tech debt, ideas, flaky behavior, doc gaps — is filed as
a GitHub issue before the task is declared complete. A task that ends
without its residue filed is not done, even if the code is merged.

## Rationale

The backlog stays alive only if observations survive the session that made
them. Chat transcripts are write-only memory; issues are the durable store
the triage agent (SCR-005) can order and the next session can pick up.

## How an agent complies

- At end of task, list everything noticed but not done, however small.
- File each item as its own issue using the task template
  (`.github/ISSUE_TEMPLATE/task.yml`), with context linking back to the
  origin task and a concrete DoD.
- Apply ONLY the `triage` label — never a priority or size (SCR-005).

## Exceptions

None. A task with zero residue states that explicitly ("no residue") in its
final report, so silence is never ambiguous.
