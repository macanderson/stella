---
id: scr/006-schema-changes-are-labelled
title: A schema change is labelled, and its migration is applied before its deploy
status: living
origin: "four repeats of one production failure in oxagen (#1275, #2796, #3449, #3692): a schema change merged, deployed, and its migration never reached production"
trigger: opening or updating a pull request that changes a schema
autonomy: L1
enforcement: "oxagen: .github/workflows/migration-label.yml applies the label from the diff and re-applies it on removal; migration-gate in pipeline.yml blocks the deploy until production carries the schema. Other repos: the directive stands, the automation is per-repo."
---

## Directive

A pull request that changes a schema carries the label
`migration-required`, and the change is not considered deployed until the
migration has been applied to production.

A schema change is any edit that makes production's stored shape differ from
the shape the branch assumes: a migration file, a schema definition a
migration is generated from, or a store's declared constraints and indexes.
Adding the label is not the author's memory exercise — where a repo can read
it from the diff, the repo applies it.

Applying the migration stays a separate, deliberate act. This record does not
authorise a deploy pipeline to apply migrations on its own.

## Rationale

Between 2026-08-25 and 2026-09-21, oxagen shipped the same production failure
four times: #1275, #2796, #3449 and #3692. Each one was a schema change that
merged green, deployed, and left its migration unapplied. Nothing went red,
because nothing was asking.

The ordering is the whole problem. Code that reads a column and a database
that lacks it fail only when a request arrives, so the deploy succeeds and the
breakage surfaces later, to a user, as something that looks unrelated. By then
the author has moved on and the merge that caused it is several merges back.

A checklist had already been tried. The instruction to apply migrations before
deploying was written down, and it was followed most of the time, which is the
characteristic failure of instructions: they work until the one time attention
is elsewhere, and that one time is indistinguishable from the others until
production is down. Four occurrences is enough evidence that the control
cannot be attention.

So the directive splits into two halves that fail in different ways. The label
acts while the pull request is open and its author is still holding the
context, which is the cheapest moment to sequence a migration. A deploy gate
acts at rollout, when nobody is holding anything, and catches what the first
half missed. Neither is sufficient: a label that only informs can be ignored,
and a gate that only blocks arrives after the knowledge has evaporated.

The label is also what makes the failure countable. Four incidents took four
separate investigations to connect, because no field recorded that a pull
request had a migration in it. A label turns that into a query.

## How an agent complies

- Check whether your branch changes a schema before opening the pull request.
  If it does, expect `migration-required`, and say in the description which
  store and what has to be applied.
- Do not remove the label to get a cleaner pull request. Where automation
  applies it, removing it re-applies it; where no automation exists, removing
  it is a false statement about the diff.
- Sequence the apply against the merge. The migration reaches production
  before, or together with, the code that assumes it — never after.
- If the migration cannot be applied yet, say so in the pull request and why.
  A blocked apply is a fact a reviewer needs, not a detail to resolve later.
- Do not add an automatic apply to a deploy pipeline under this record. That
  is a separate decision, made per repo, with its own record.

## Exceptions

- A change that touches a schema file without altering the stored shape — a
  comment, a formatting pass, a rename with no generated migration — still
  attracts the label where automation reads paths rather than semantics.
  Remove it with a note in the pull request saying why the diff is inert.
  This is the one legitimate removal.
- A repo with no persistent store has nothing to label, and this record is
  inert there rather than waived.
