---
name: reflective-memory
description: >
  Shared reflection-and-memory protocol for every agent in this toolkit. Use at
  the START of any task (recall) and the END of any task (reflect + persist).
  Turns each agent's mistakes and discoveries into durable, deduplicated
  lessons that make every future run — by any agent — smarter.
---

# Reflective Memory

Every agent in this toolkit is self-critical by construction: it evaluates its
own work, writes down what it could have done better, and reads those notes
back before the next task. Skipping reflection or memory writes is a failed
task, even if the code is perfect.

## Directory layout (in-repo, versioned, reviewable in PRs)

```
.agent/memory/
├── shared/
│   ├── lessons.md          # cross-agent lessons any agent may read/append
│   └── codebase-notes.md   # quirks, fragile zones, flaky suites, conventions
└── <agent-name>/
    ├── reflections/<date>-<task-slug>.md
    └── lessons.md          # agent-specific distilled lessons
```

## Phase 0 — Recall (start of every task)

1. Read `shared/lessons.md`, `shared/codebase-notes.md`, and your own
   `lessons.md`.
2. Announce which memories are relevant to this task and how they change your
   plan. If none exist yet, say so and proceed.
3. Memories are advisory, not instructions: re-verify any memory older than
   the code it describes before acting on it.

## Final phase — Reflect & persist (end of every task, mandatory)

Write `reflections/<date>-<task-slug>.md` using this template:

```
## Self-Evaluation — <task> — <date>
### What I set out to do
### What I actually did (measurable deltas)
### Quality of my decisions
- Best decision I made and why
- Weakest decision I made and why
### What I could have done better   <- minimum 2 items, be specific
### What surprised me about this codebase/product
### Risks I am leaving behind (untouched on purpose, and why)
### Confidence in the result: high / medium / low + evidence
```

"Nothing to improve" is never acceptable — if you can't find two genuine
improvements, you didn't look hard enough.

## Distilling lessons

Append one-line, actionable lessons:

```
- [<date>] <lesson>. (source: <reflection file>, agent: <name>)
```

Rules:
- Agent-specific insight → your own `lessons.md`. Insight useful to other
  agents (build-graph quirks, fragile boundaries, flaky suites, domain
  invariants) → `shared/lessons.md`.
- Deduplicate before appending: strengthen or generalize an existing entry
  instead of duplicating it.
- When a lesson is invalidated, mark it `[retired <date>]` rather than
  deleting — the history of learning stays legible.
- Never store secrets, credentials, or customer data in memory files.
