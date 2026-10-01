---
id: adr/0046-one-uppercase-label-scheme
title: "ADR 0046: One uppercase label scheme"
status: implemented
---

# ADR 0046: One uppercase label scheme

- Status: accepted
- Date: 2026-09-30
- Deciders: Mac (the repository owner)
- Amends: ADR 0045
- Not part of the Phase 0 series.

## Context

ADR 0045 kept two label families. `use-model:*` named the model tier, and
`size/*` named the size. Most labels in this repo were lowercase, such as
`triage` and `area:core`.

The oxagen repo put the same facts under other names, such as `kind:defect`
for a bug. On 2026-09-30 Mac set one label scheme for `macanderson/oxagen`
and `macanderson/stella`. The manifest is `issue-management/labels.json` in
the oxagen-roadmap repo. It lists each label with its family, color, text,
and the repos that carry it. Its `aliases` field lists the names a label had
before the switch. Its `retired` field lists the names folded into another
label.

## Decision

Every label name is uppercase. A family name ends in a colon, as in
`KIND:BUG`.

A triaged issue's title reads `<Priority> <Tier> <Size> <Kind> (<Area>): <Statement>`.
An example is `P1 T3 XS Bug (CLI): The resume picker drops the newest thread`.

Five families take exactly one label on a triaged issue:

- Priority: one `P` label. SCR-005 names the levels.
- Tier: `MODEL:T1` (Haiku), `MODEL:T2` (Sonnet), `MODEL:T3` (Opus), or
  `MODEL:T4` (Fable).
- Size: the agent minutes it takes to get a pull request ready to merge.
  `SIZE:EXTRA-SMALL` is 30 or fewer. `SIZE:SMALL` is 31 to 90.
  `SIZE:MEDIUM` is 91 to 240. `SIZE:LARGE` is 241 to 480.
  `SIZE:EXTRA-LARGE` is more than 480. The title uses `XS`, `S`, `M`, `L`,
  and `XL`.
- Kind: `KIND:BUG`, `KIND:FEATURE`, `KIND:IMPROVEMENT`, `KIND:CHORE`,
  `KIND:DOCUMENTATION`, or `KIND:DEVOPS`. `EPIC` marks a container issue. It
  is not a kind.
- Area: one `AREA:` label, such as `AREA:CORE`.

Signals take any number. These are `PAIN:*`, `GOAL:*`, `PILLAR:*`, and the
like.

These names change by more than case:

| Old name | New name |
|---|---|
| `use-model:cheap`, `use-model:balanced`, `use-model:pro`, `use-model:ultra` | `MODEL:T1`, `MODEL:T2`, `MODEL:T3`, `MODEL:T4` |
| `size/XS`, `size/S`, `size/M`, `size/L`, `size/XL` | `SIZE:EXTRA-SMALL`, `SIZE:SMALL`, `SIZE:MEDIUM`, `SIZE:LARGE`, `SIZE:EXTRA-LARGE` |
| `bug`, `feature` | `KIND:BUG`, `KIND:FEATURE` |
| `chore`, `tech-debt` | `KIND:CHORE` |
| `documentation`, `docs` | `KIND:DOCUMENTATION` |
| `area:ocp` | `AREA:CGP` |

`KIND:IMPROVEMENT` and `KIND:DEVOPS` are new. Every other label keeps its
name in uppercase. So `triage` is now `TRIAGE`, `area:core` is `AREA:CORE`,
`status:ready` is `STATUS:READY`, and `main-red` is `MAIN-RED`.

This record replaces the label part of ADR 0045. The `MODEL:` tier labels
take the place of `use-model:*`, and the `SIZE:` labels take the place of
`size/*`. ADR 0045 also retired `build-time:*` and `model:tier-*`. That
part stands.

## Consequences

- Code compares label names in any case. It does so through one function,
  `stella_autonomy::labels::same`. GitHub finds a label by name in any case,
  but it hands back the spelling it stores. A plain string test would miss a
  label whose case changed.
- The same function reads each old name in the table above as its new name.
  An issue labelled before the switch still ranks. The loop writes and makes
  only the new names.
- A separate session renames the labels on GitHub once this change merges.
  Until then the tracker holds the old names, and the code still reads them.
- The loop keeps `STATUS:BLOCKED` next to `STATUS:READY`. It lifts
  `STATUS:BLOCKED` once each `Blocked by:` line names a closed issue. It
  never lifts `BLOCKED`, the hold a person sets. `STATUS:BLOCKED` is not in
  the manifest, and Mac decides if it joins. The same goes for
  `RELEASE-RED`, which the loop files on a red release.
- SCR-005 and the other records under `docs/scr/` name the new labels. The
  priority levels and the guard's rule stay as they were.
