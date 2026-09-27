---
id: adr/0045-retire-ad-hoc-build-time-and-model-tier-labels
title: "ADR 0045: Retire the ad hoc build-time and model-tier labels"
status: implemented
---

# ADR 0045: Retire the ad hoc build-time and model-tier labels

- Status: accepted
- Date: 2026-09-27
- Decides: `#5631`, `#5669`
- Not part of the Phase 0 series.

## Context

A triage job that runs outside this repo put labels from two families on
issues. No rule here set up either family. `build-time:*` guessed how long the
work would take. `model:tier-*` guessed which model class the work needed. It
had one tier each for Haiku, Sonnet, Opus, and Fable.

Neither family is in the label list in AGENTS.md. Neither is in SCR-005. The
set was not whole. Only 7 of the 8 named `build-time:*` buckets existed.
`model:tier-2` and `model:tier-4` were made on 2026-09-27, while `#5669` was
being worked.

Two labels already carry the same facts. `use-model:*` names the model class.
AGENTS.md lists its four values: `cheap`, `balanced`, `pro`, and `ultra`.
`size/*` names the size. SCR-005 gives it to the triage agent.

The count below was taken on 2026-09-27, before any label came off.

- 32 open issues had a `model:tier-N` label. Of those, 11 had no `use-model:*`
  label. For those 11, the tier label was the only sign of which model to use.
  Five were tier 2: `#6481`, `#6478`, `#6158`, `#6076`, and `#5819`. Six were
  tier 3: `#6507`, `#6490`, `#6480`, `#6141`, `#5796`, and `#5787`.
- 43 open issues had a `build-time:*` label.
- One issue had a `size/*` label: `#6138`, with `size/XS`.
- The oxagen repo has five `size/*` labels. It has no label from either
  family. `gh label list --repo macanderson/oxagen` shows this.
- The two newest open issues, `#6582` and `#6570`, had none of these labels.
  The job's last label write on record was at 2026-09-25T17:18:43Z.
- On 2026-09-02 the repo owner left a comment on `#5631`. It said the two tier
  labels that existed then "don't accurately bracket the work". It said that
  adding one more "would just add more of the drift."

## Decision

Stella retires both label families. It does not fill them in.

`model:tier-*` says the same thing as `use-model:*`. With two labels for one
fact, triage put on one or the other. So 11 issues ended up with only the
label no rule names.

`build-time:*` says the same thing as `size/*`. SCR-005 names `size/*` as the
size label. Five repos share SCR-005, so this repo cannot change it alone. To
keep `build-time:*` next to `size/*` would mean two size labels at once. The
done list in `#5631` rules that out.

The tracker changes that carry out this choice are done.

- The 11 open issues that had a tier label and no `use-model:*` label now have
  the matching `use-model:*` label. Tier 1 maps to `cheap`, tier 2 to
  `balanced`, tier 3 to `pro`, and tier 4 to `ultra`.
- The repo's 11 labels in the two families are gone. That is 7 `build-time:*`
  labels and 4 `model:tier-*` labels. `build-time:5mins` never existed.
- Size stays with `size/*`. Only one issue uses it today. A second size label
  would not change that.
- The triage job is not in this repo, and this record does not change it. If
  it puts a retired label on a new issue, report that as a bug in the job. Cite
  the time above.

## Consequences

- `#5669` is closed as not planned. Its closing comment lists the tracker
  changes above.
- The pull request that adds this record closes `#5631`.
- A retired label found on an issue later is drift from this record. Remove it
  and report where it came from. The choice to adopt or retire is settled.
