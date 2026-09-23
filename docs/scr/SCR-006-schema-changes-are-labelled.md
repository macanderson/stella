---
id: scr/006-schema-changes-are-labelled
title: A schema change is labelled, and its migration is applied before its deploy
status: living
origin: "Oxagen hit this production failure four times. A schema change merged and deployed. Its migration never ran."
trigger: opening or updating a pull request that changes a schema
autonomy: L1
enforcement: "In oxagen, `migration-label.yml` adds the label from the diff. It puts the label back if someone removes it. `migration-gate` in `pipeline.yml` blocks the deploy until production has the schema. Other repos keep the rule and apply the label by hand until they add a check."
---

## Directive

A pull request that changes a schema gets the label
`migration-required`.

The change is not deployed until the migration has run in production.

A schema change makes the stored shape differ from the shape the branch
expects.

That edit can be a migration file.

It can be a schema file a migration is built from.

It can be a store's rules or indexes.

The repo adds the label when it can read the change from the diff.

Where the repo cannot read it from the diff, the author adds the label by hand.

Applying the migration is a separate act.

This record does not allow a deploy pipeline to apply migrations on its own.

## Rationale

From 2026-08-25 to 2026-09-21, oxagen shipped this failure four times.

- `https://github.com/macanderson/oxagen/issues/1275`
- `https://github.com/macanderson/oxagen/issues/2796`
- `https://github.com/macanderson/oxagen/issues/3449`
- `https://github.com/macanderson/oxagen/issues/3692`

Each time a schema change merged.

The deploy went out.

The migration did not run.

Nothing went red.

Nothing was checking.

The order is the problem.

Code that reads a new column fails only when a request comes in.

The database does not have that column yet.

The deploy still succeeds.

The break shows up later.

It looks like some other bug.

By then the author has moved on.

The merge that caused it is several merges back.

A checklist was tried.

The rule was written down.

People followed it most of the time.

A written rule fails when no one is looking.

That time looks like every other time until production is down.

Four times is enough.

Attention cannot be the control.

The rule has two parts.

They fail in different ways.

The label acts while the pull request is open.

The author still has the context then.

That is the cheap time to order a migration.

A deploy gate acts at rollout.

No one has the context then.

It catches what the label missed.

A label that only tells people can be ignored.

A gate that only blocks comes too late.

The facts are gone by then.

The label also makes the failure easy to count.

Four breaks took four separate hunts to connect.

No field said the pull request had a migration.

A label makes that a query.

## How an agent complies

- Check whether your branch changes a schema before you open the pull request.
- If it does, the pull request needs the label `migration-required`.
- Where no bot adds it, add it yourself.
- Say which store changed.
- Say what has to be applied.
- Do not remove the label to make the pull request look cleaner.
- Where a bot adds the label, taking it off puts the label back.
- Where no bot exists, taking the label off states a false fact about the diff.
- Run the migration in production before the code that needs it, or with that code, and never after.
- If you cannot apply the migration yet, say so in the pull request and say why.
- A blocked apply is a fact a reviewer needs.
- Do not add an automatic apply to a deploy pipeline under this record; that is a separate decision, made per repo in its own record.

## Exceptions

- A change can touch a schema file and leave the stored shape the same.
- A comment, a format pass, or a rename with no migration is that kind of change.
- A bot that reads paths still adds the label.
- You may remove the label only with a note.
- The note says why the diff changes nothing that is stored.
- That is the one removal this record allows.
- A repo with no stored data has nothing to label.
- This record does nothing there.
