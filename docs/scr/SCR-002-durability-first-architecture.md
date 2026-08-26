---
id: SCR-002
title: "Architecture: durability-first, decide-and-record, never ask"
status: active
origin: 100%-deterministic answer to every architecture question, 2025–2026
trigger: any architectural or design decision arising mid-task
autonomy: L1
enforcement: AGENTS.md standing-decisions block (imported by CLAUDE.md); ADR trail in docs/adr/
---

## Directive

Do not ask the maintainer. Choose the most durable option — the one that
reads as obviously correct in 10 years. Cheap-and-easy wins only when it is
*also* the excellent durable option. Record every such decision as a short
ADR; the ADR replaces the question.

## Rationale

Every architecture question the maintainer has been asked has received the
same answer. Because the answer never varies, the question itself is waste —
it blocks the agent, interrupts the maintainer, and leaves no record. The
ADR moves review to asynchronous time and leaves a trace the 2036 reader
can follow.

## How an agent complies

- Apply the rule: fewest future regrets, standard over clever, boring
  technology, explicit over implicit, designed for the reader in 2036.
- Write an ADR in `docs/adr/` (context, options, choice, why-durable),
  following the repo's existing numbering scheme.
- Keep moving — review of the ADR happens asynchronously, never as a
  blocking question.

## Exceptions

Decisions that change the product's externally observable contract or spend
real money still go to the maintainer — those are scope changes, not
architecture choices.
