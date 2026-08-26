---
id: SCR-005
title: Creators label triage only; the triage agent owns priority
status: active
origin: separation-of-duties rule on the backlog, 2025–2026
trigger: creating or labeling any GitHub issue
autonomy: L2
enforcement: task template applies only the triage label; .github/workflows/triage-guard.yml strips creator-applied P-labels; target L3 — a scheduled triage agent with its own whitelisted identity
---

## Directive

Issue creators — human or agent — may apply exactly one label: `triage`.
No priority, no size, nothing else. A dedicated triage agent, and only that
agent, sizes the work, assigns exactly one priority label, optionally a
size, removes `triage`, and comments a one-line rationale.

Priority scheme: `P0` drop everything · `P1` this cycle · `P2` next cycle ·
`P3` backlog. Invariant: every open issue carries either a priority label or
`triage` — never neither, never both.

## Rationale

Whoever creates an issue is the worst-placed party to rank it — creators
systematically over-weight their own findings. Separating creation from
prioritization keeps the backlog ordering honest and gives the maintainer
one place (the triage agent's rationale comments) to audit it.

## How an agent complies

- When filing any issue: use the task template; touch no labels beyond the
  `triage` it applies automatically.
- Never add, remove, or change `P0`–`P3` or `size/*` labels — the
  triage-guard workflow strips such labels and re-queues the issue.
- The triage agent (once stood up) never implements anything and never
  closes issues — it only sizes and orders. Mixing roles collapses the
  separation of duties.

## Exceptions

Until the dedicated triage-agent identity exists, the maintainer
(macanderson) acts as the interim triage authority and is whitelisted in
the guard workflow; remove that whitelist entry when the bot stands up.
