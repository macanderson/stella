---
id: adr/0022-adopt-standing-decisions-scr-corpus
title: "ADR 0022: Adopt org standing decisions as a Steering Context Record corpus"
status: implemented
---

# ADR 0022: Adopt org standing decisions as a Steering Context Record corpus

- Status: accepted
- Date: 2026-08-26

## Context

The maintainer steers coding agents with a small set of directives that never
vary — scoped test builds, durability-first architecture, DoD-verified
closes, residue-to-issues, triage separation of duties. Re-typing them every
session is waste, and knowledge that lives only in one person's habits does
not survive that person's absence. The maintainer/agent collaboration
process (see the process doc in the oxagen org) mandates promoting each
repeated steer up an autonomy ladder: L1 written rule → L2 enforced →
L3 delegated → L4 recorded.

Two placement questions had to be settled for this repository:

1. **Where does the steering context live so that both Claude Code and
   Stella load it?** Options: (a) duplicate the block in CLAUDE.md and
   AGENTS.md; (b) canonical block in AGENTS.md, imported by CLAUDE.md via
   `@AGENTS.md`; (c) CLAUDE.md only.
2. **Where does the SCR corpus live?** Options: (a) a dedicated shared
   steering repo; (b) replicated `docs/scr/` in each org repo.

## Decision

- The standing-decisions block is canonical in `AGENTS.md`; `CLAUDE.md`
  imports it with `@AGENTS.md` (option 1b). One source, two consumers, zero
  drift between them — the pattern the stella repo already proved.
- The five seed SCRs (SCR-001…SCR-005) are replicated identically in
  `docs/scr/` across the five org repos: oxagen, context-graph-protocol,
  cgp-website, arenabench, stella (option 2b). The rollout was scoped to
  exactly these repositories — no new steering repo. Cross-repo drift is a
  known cost, accepted and named in `docs/scr/README.md`; a periodic sync
  check is filed as residue.
- Enforcement shipped alongside the rules: a Claude Code `PreToolUse` hook
  blocking full-suite test builds (SCR-001, L2), an issue template with a
  mandatory DoD that applies only the `triage` label (SCR-003/005, L2), and
  a `triage-guard` workflow stripping creator-applied priority labels
  (SCR-005, L2). The maintainer is whitelisted in the guard as the interim
  triage authority until a dedicated triage-agent identity exists.

## Why durable

Agent-context files come and go with agent products; a numbered, versioned
corpus of plain-markdown records with explicit frontmatter (`status`,
`autonomy`, `enforcement`) is boring technology that any future agent — or
human — can read, diff, and supersede. SCRs are never deleted, only marked
superseded, so the 10-year reader can trace why the process looks the way
it does. Enforcement lives in the harness (hooks, templates, Actions), not
in anyone's memory.
