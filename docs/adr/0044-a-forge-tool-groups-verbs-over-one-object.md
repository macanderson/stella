---
id: adr/0044-a-forge-tool-groups-verbs-over-one-object
title: "ADR 0044: A forge tool groups verbs over one object"
status: implemented
---

# ADR 0044: A forge tool groups verbs over one object

- Status: accepted
- Date: 2026-09-21
- Decides: `#6545`
- Not part of the Phase 0 series.

## Context

The forge plane was asked for as eleven verbs. `create_pr`, `update_pr`,
`close_pr`, `merge_pr`, `create_issue`, `comment_on_issue`,
`comment_on_pr`, `edit_issue`, `edit_issue_comment`, `edit_pr_comment`, and
a CI watch.

It ships as three tools. `pull_request`, `issue` and `watch_ci`. The first
two each take an `action` parameter naming the verb.

`AGENTS.md invariant 9` says a parameter may scope an operation. It may
never select one. `update_task(delete=true)` is the example it gives, and
it gets split. Read as written, that forbids what shipped. So the grouping
is a decision, and it needs a record.

`AGENTS.md invariant 7` pushes the other way. A tool schema rides the
byte-stable prompt prefix. It rides it on every model call of every
session, forge attached or not. Eleven schemas is eleven descriptions and
eleven property tables in that prefix, against three.

That rule gives three reasons. They do not all apply here.

**The model picks on the description alone.** This one is real, and it is a
cost. `pull_request`'s description teaches six verbs. `merge_pr`'s would
teach one. It is weighed against the prefix budget above. The schema
answers it as far as a schema can: the `action` enum's own description
names each verb and the arguments it takes.

**Policy must withhold the destructive verb and keep the benign one.** This
one was broken. The session's gate above the tool stack answers for a whole
tool. So `"pull_request": "off"` took `merge` and `comment` together. An
operator who wanted the agent to comment but never merge had no way to say
it.

**A read tool with a mutating arm misdeclares `read_only`.** This one does
not apply. Neither grouping tool has a read action. `pull_request` and
`issue` both declare `read_only: false`, and every action under them
writes. Reading CI is the separate `watch_ci`. It declares
`read_only: true` and has one action.

## Decision

**A tool may group verbs when all four of these hold.**

1. Every action acts on one object, named by one key. Here that is a pull
   request or an issue, named by `key`.
2. No action is read-only. A grouping tool declares `read_only: false`, and
   that describes every arm. The engine's concurrency contract is not asked
   to guess.
3. The actions are one list in the source. The schema's enum and the
   unknown-action error both read it. `ACTIONS` in `forge/pr.rs` and
   `forge/issue.rs` is that list. A verb cannot be offered and then
   missing, or handled and never offered.
4. The tool asks the operator's policy per action, before it does anything
   else.

**The fourth is the new part.** It is how the second reason above is met
rather than waived. `ToolPolicy` takes a dotted key, `"<tool>.<action>"`,
and `allows_action` answers it. `"pull_request.merge": "off"` withholds
that one verb. The other five keep running.

**An action key can only narrow.** `allows_action` asks `allows` first. A
tool switched off by its own name, by its catalog group, or by the wildcard
stays off, whatever an action key says. That keeps `deny_all_from`'s rule
true of actions too: a lower scope narrows and never widens.

**The tools read the same policy the gate reads.** It is the session's
resolved `ToolPolicy`, with the org-managed ceiling already folded in. It
reaches them at one assembly point: `write_dirs::registry_rooted_at`, then
`forge_install::attach`, then `ToolRegistry::attach_forge`. Loading
settings a second time inside the tools is how the two would come to
disagree.

**The refusal is `ErrorClass::RefusedByPolicy`, and it names the key.** A
model that reads "`merge` is switched off for `pull_request`" stops asking.
One that reads a bare failure retries.

**This is not a licence to group.** The one-tool-per-job rule holds
everywhere else. `bash`, `search`, the file CRUD quartet and the scratch
state plane each stay as they are. Each fails one of the four conditions.
The state plane's verbs take different arguments, and `get_state` is a
read. A file read beside a file write is the `read_only` misdeclaration the
third reason names. When a candidate's actions take different objects, it
is two tools.

## Consequences

The operator gains a switch, and can see it. A tool that is on with one
action withheld would otherwise print as plainly on. `denied_builtins`
resolves catalog names, and a dotted action key names no catalog row. So
`stella tools` names the withheld actions beside the tool. Without that
line, an operator sets a switch that is invisible until a session hits the
refusal.

Two witnesses hold the gate, one per tool. One gate serves both tools, so
each of them can lose its call site on its own. They are
`a_switched_off_merge_refuses_while_comment_still_runs` and
`a_switched_off_close_refuses_while_comment_still_runs`. Each asserts four
things: the class, the key named in the message, that the provider recorded
nothing, and that a second action still runs.

The class assertion is what makes the first witness bite. Remove the gate,
and `merge` falls through to the `confirm: true` check. That refuses it as
`InvalidInput`, and it never reaches the forge. So the "nothing reached the
provider" assertion passes either way. Only the class tells the two
refusals apart.

`AGENTS.md invariant 9`'s text is amended in the same change to name this
mechanism. The numbering is an address. So the text is amended in place,
and nothing is renumbered.
