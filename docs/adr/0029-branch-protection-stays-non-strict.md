---
id: adr/0029-branch-protection-stays-non-strict
title: "ADR 0029: Branch protection stays non-strict"
status: implemented
---

# ADR 0029: Branch protection stays non-strict

- Status: accepted
- Date: 2026-09-05
- Decides: `#6078`

## Context

`main`'s branch protection has a setting named `required_status_checks.strict`.
Turned on, a pull request cannot merge until its branch has run its checks
against the current tip of `main`. Turned off, a pull request can merge on
a green run from an older `main`, even many commits behind.

`#6078` asked which one this repository has. Read straight from the
protection API, with an admin-scoped token:

```
$ gh api repos/macanderson/stella/branches/main/protection \
    --jq '.required_status_checks | {strict, contexts}'
{
  "contexts": [
    "fmt + clippy + test",
    "cargo deny + cargo audit",
    "harbor_adapter + analyzer pytest",
    "main is not known-broken",
    "gate steps no other workflow runs"
  ],
  "strict": false
}
```

`strict` is `false`. `enforce_admins` is `true`. A red required check still
blocks everyone, admins included. The only thing not required is a fresh run
against the latest `main`.

Two files already argued as if `strict` were on:
`.github/workflows/automerge-nudge.yml` and `scripts/automerge-nudge.sh`.
Both open by saying branch protection sets `required_status_checks.strict`,
and both build their reason for existing on that — a workflow that unsticks
pull requests parked at GitHub's `BEHIND` merge state. Neither file is right
about the setting today. `git log` shows `AGENTS.md` never made this claim,
so the fix belongs in those two files, not there.

Turning `strict` off was not a new decision made here. A repository setting
does not flip itself. But nobody had written the choice down, so nobody could
tell a chosen setting from a forgotten one. Proof the choice matters arrived
within the hour `#6078` was filed: `main` broke at 16:59 UTC (`#6081`, closed
by `#6082`). Two pull requests, each green and each correct on its own, both
touched `Cargo.lock`. Together they made a lock file that would not resolve.
Every pull request merged in that window had run its checks against a `main`
that was 4 to 14 commits old.

## Decision

Leave `strict` off.

Many agent sessions merge into this repository at once. A scheduled sweep
runs. Several other sessions run too. They often overlap. Turning
`strict` on would force every open pull request through `gh pr
update-branch` plus a full ~12-minute CI run each time `main` moves. Under
this load, `main` moves every few minutes. Merging would slow to a handful
of pull requests an hour. The pile-up `automerge-nudge.yml` already exists
to drain (`#1527`) would come back, and worse than before. `strict` is the
exact rule that makes GitHub's `BEHIND` deadlock happen at all.

The risk `strict` would have caught still gets watched, just later. It has a
name in `AGENTS.md`: the shared-cell hazard behind `Cargo.lock` and
`scripts/file-size-baseline.txt`. Two branches, each correct alone, can
compose into a broken tree once both land — and no check on either branch
alone can see that coming. `main-canary.yml` asks the same question again,
against `main` itself, after every push. `main-red-hold.yml` — a required
check since 2026-09-02 — blocks further merges while a `main-red` issue from
the canary stays open. That pair catches a break after the merge, not before
it. This record accepts that gap on purpose, rather than leaving it unnamed.

The lasting fix for the gap is GitHub's merge queue. It tests each candidate
against the true merged result before it lands, so a queued pull request
never merges on a stale green. Turning it on is a separate repository
setting, and a separate decision from this one. `#4998` already asks the
same question: a merge queue, or a cheaper scheduled composer? It is still
open, and neither one is picked yet. This record only answers `#6078`'s
question: is `strict` on. `#4998`'s bigger question stays there. A second
issue asking the same thing would not help.

`automerge-nudge.yml` exists to drain pull requests stuck at GitHub's
`BEHIND` merge state. It cannot fire while `strict` is off. GitHub only
reports `BEHIND` when required status checks are strict. `#6078`'s own
table already showed this: pull requests 4 to 14 commits behind all
reported `clean`, never `behind`. Deleting the workflow and its script is a
bigger change than a documentation record should make on its own. So both
files stay. Their `push`/`schedule` triggers are turned off, and
`workflow_dispatch` is kept. Restarting them is cheap if `strict` ever
comes back. Their headers say what the repository does today.

## Consequences

`AGENTS.md` never claimed `strict` was on, so it needed no fix. The false
claim lived in `automerge-nudge.yml` and `automerge-nudge.sh`. Both are
fixed in the same change that adds this record. Both now say their
selection rule cannot match anything until `strict` returns. Both say how
to turn them back on: run `workflow_dispatch` by hand today, or restore
the `push`/`schedule` triggers this record turns off.

A pull request can still merge on a run against a `main` that has since
moved. `main-canary.yml` and `main-red-hold.yml` are the safety net for
that, and they run after the merge, not before it. This record accepts that
gap; `#4998` is tracking the lasting fix for it.

A later measurement may show the break rate is too high, even with
`main-canary.yml` catching most of them. If so, amend this record. Turn
`strict` back on. Turn `automerge-nudge.yml`'s automatic triggers back on
with it.
