---
description: File everything this cycle could not fix as GitHub issues written as handoffs — the phase the loop is not allowed to skip.
argument-hint: "[--dry-run]"
---

# self-driving:tickets — fix over file

The rule from AGENTS.md § "Fix over file", and the one phase of the cycle
that has no skip condition:

> A defect you can fix gets fixed in the PR you are already making. File it
> only when a fix cannot responsibly ride the PR, and only when fixing it
> moves a pillar.

A cycle that fixed twenty defects and filed nothing about the six it truly
could not fix has **destroyed information**. The worktree gets deleted, the
context window resets, and those six findings are gone. A cycle that filed
twenty questions has wasted the next cycle's time instead.

---

## What gets filed

Only a finding that meets both tests:

1. **A fix could not ride the PR** — it needs a decision only the maintainer
   can make, it needs a rig, a credential or real spend, or it is larger than
   the cycle. Say which in the issue. A defect you could have fixed is fixed,
   not filed, even when it is unrelated to the PR's title.
2. **Fixing it moves a pillar** — stability, reliability, maintainability,
   innovation, efficiency, or performance. Name which.

Do not file an open design question (decide it in the PR, or write an ADR), a
record of a choice already in effect, a measurement that cannot change a
decision, a test for a path nothing reaches, tracker bookkeeping, or a
follow-up your own change has made moot.

## The handoff format

Assume the reader is a fresh agent with none of your context. If an issue needs
your memory to interpret, it is a note to yourself, not a handoff.

```bash
gh issue create \
  --title "stella-core: retry counter survives a goal-round boundary" \
  --label bug --label area:core --label P1 \
  --body "$(cat <<'EOF'
## Problem

`RetryHistory` is keyed per turn but never cleared when a goal round ends, so a
turn that retried three times in round 1 starts round 2 already at the ceiling
and refuses its first legitimate retry.

## Where

- `crates/stella-core/src/driver.rs:812` — `retry_history` is read
- `crates/stella-core/src/driver/settlement.rs:140` — where the round boundary
  is observed and the reset is missing

## Reproduce

    cargo test -p stella-core retry_history_survives_round -- --ignored

Fails today with `attempts: 3, allowed: 3`; should be `attempts: 0`.

## Constraints already discovered

- `driver.rs` is a grandfathered god file (`scripts/file-size-baseline.txt`) and
  is closed to growth — the reset belongs in `driver/settlement.rs`.
- Invariant 2: no I/O in the engine. This is pure decision logic, so it stays in
  `stella-core`.
- Related: #1204 moved settlement out of `driver.rs`; #1211 touched the same
  boundary for timeouts.

## Done when

A witness test fails on `main` and passes with the fix: two goal rounds, three
retries in the first, and a fourth retry granted in the second.
EOF
)"
```

The five headings are not decoration. **Problem** (not a task description),
**Where** (paths and lines, never "somewhere in the driver"), **Reproduce**
(a command and what it prints today), **Constraints** (the gates and rules
you already discovered so the next agent does not rediscover them), **Done when**
(the witness test that would prove it).

## Before you file, every time

```bash
gh search issues --repo macanderson/stella "<distinctive terms>" --state all
gh issue list --state open --label area:<crate> --limit 50
```

Duplicates are worse than nothing — they split the discussion and inflate the
queue the next cycle ranks off. Found a near-match? Comment on it with what you
learned and link it instead.

## Labels

| | |
|---|---|
| type | `bug` · `feature` · `epic` · `documentation` |
| priority | `P0` broken now · `P1` next · `P2` polish |
| area | `area:core` `area:cli` `area:model` `area:tools` `area:tui` `area:pipeline` `area:store` `area:context` `area:bench` `area:ci` `area:docs` … |
| special | `self-improvement` (Stella making Stella more capable) · `needs-witness` (PR waiting on its witness test) |

Leave `triage` off — that is for issues arriving from outside without a type.
You know what you found; classify it.

## Then close the loop on the loop

```bash
scripts/self-driving.sh seen --add <digest>…
```

An issue filed but not recorded as seen will be filed again next cycle. Record it
**after** the issue exists, so a failed `gh issue create` does not silently
swallow the finding.

## Reference the tickets from the PR

Every PR the cycle opened lists the issues it filed. That is what makes the
residue of an autonomous cycle auditable by a human afterwards — the PR says both
what it fixed and what it did not.
