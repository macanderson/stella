---
id: adr/0027-a-fleet-worker-gets-its-own-worktree
title: "ADR 0027: A fleet worker gets its own worktree"
status: implemented
---

# ADR 0027: A fleet worker gets its own worktree

- Status: accepted
- Date: 2026-09-04
- Decides: `#5287`

## Context

Two workers in one checkout can wipe out each other's work. One of them runs
`git checkout`. Git then puts every tracked file back to what the new ref
says. It does that for the whole tree, for everyone in it. The other worker's
uncommitted edits are gone.

Nothing prints an error.

The loss is lopsided too. A switch restores tracked files. It leaves untracked
ones alone. So a worker that added a file and edited an old one keeps the new
file and loses the edit. What is left is a module nothing declares. The code
cannot build, and no message says why. The tree looks fuller after the loss,
not emptier. The usual "something is missing" hunch never fires.

`#5287` records it happening. Three sessions shared one checkout. They
switched branches four times in six minutes. One session lost three tracked
edits.

That session's own report is the rest of the case. A session that spots the
clash has no move but to stop. The working directory is picked at launch. It
cannot be taken later.

`stella fleet` used the shared root by default. File claims coordinated the
writers. That is a good mechanism: a clash fails in under a second and names
the rival, commits interleave on one branch, and the build cache stays warm.
But a claim guards one path. `git checkout` rewrites them all, and asks no
claim.

## Decision

`Isolation::Isolated` is the default. Every fleet task gets a `git worktree`
of its own. A plan that wants the shared root says `isolation =
"shared_tree"`.

The default lives in one place: the `#[default]` on `Isolation`, in
`crates/stella-fleet/src/plan.rs`. `Task::new` reads it through
`Isolation::default()`. So a plan file that names nothing and a builder that
says nothing cannot disagree.

The shared root stays. It is right for a plan whose tasks touch different
files and never switch branches. It is what `claims` is for. It is now
something a plan says out loud.

`stella fleet` prints which mode a run uses in its header
(`crates/stella-cli/src/fleet_cmd/isolation_notice.rs`). Reading the plan file
is not the same. A plan may name nothing, and then a default picks. Nobody
reads a default.

This is option 1 of the three the issue lists, and the one its author picked.
Option 2 refuses the second session. Option 3 warns on entry. Both leave the
session that spots the clash with nowhere to go. Only handing out the tree
removes the failure.

## Consequences

A run costs one worktree per task. That is disk, plus a cold build cache in
each tree. `stella doctor` reports the fleet worktree count and what can be
reclaimed (`#1655`). `stella fleet clean` reclaims the finished ones.

Clashes move to merge time. Two workers editing one file both succeed, and
the merge settles it. A plan that wants the one-second failure with the rival
named asks for the shared root.

A plan that declares `claims` and names no isolation changes. It gets
worktrees now, and its claims coordinate nothing anybody shares. Such a plan
wants `isolation = "shared_tree"` beside them.

`scripts/test-session-isolation.sh` is the witness. Its first case dispatches
two workers with no isolation named. It leaves an uncommitted edit in one
tree. It forces a branch switch in the other. Then it asks whether the edit
lived. Before this record that case found no worker trees, and the edit was
gone. The last case runs the same test on a plan that names `shared_tree`, and
still reports the loss. That is what keeps the suite able to fail.

One thing this does not cover. A person, or an outside tool, can still start
two decks in one tree. Nothing here picks where those launch. A deck that
finds a live peer in its checkout prints one line naming it
(`crates/stella-cli/src/command_deck/shared_checkout.rs`). It never refuses. A
session told to stop has nowhere to go.
