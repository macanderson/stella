# CLAUDE.md

@AGENTS.md

## Hard rules for every session

- **AGENTS.md is the orientation document.** Commands, architectural
  invariants, workspace routing, testing approach, and gotchas all live there
  (imported above). When this file and AGENTS.md disagree, AGENTS.md wins —
  and the disagreement is itself a bug: fix it in the same PR you noticed it.
- **Nothing left behind.** Every bug, defect, idea, missing test, piece of
  unwired code, or logical next step you notice and do not fix inside your
  current change MUST be filed as a GitHub issue before you finish — written
  as a handoff a fresh agent could execute without your session's context
  (problem, file paths, repro/verify steps, constraints, definition of done).
  Search for an existing issue first; link, don't duplicate. The full policy
  is AGENTS.md § "Nothing left behind — every finding becomes a fix or a
  GitHub issue". Never end a turn with untracked half-finished work.
