---
id: adr/0036-a-driver-takes-a-pull-request-out-of-draft
title: "ADR 0036: A driver takes a pull request out of draft"
status: implemented
---

# ADR 0036: A driver takes a pull request out of draft

- Status: accepted
- Date: 2026-09-07
- Decides: `#6363`
- Not part of the Phase 0 series.

## Context

The `deliver` family served four verbs. `deliver_open`, `deliver_observe`,
`deliver_next` and `deliver_merge`. A driver could open a pull request for the
unit it worked. It could read the forge, decide, and merge.

It could not get from a green build to a merge on its own.

`deliver_open` opens the pull request as a draft. It says why. A pull request
that never goes green never asks a human to look at it.

The pure machine agrees. `stella_autonomy::deliver_next`'s green arm answers
`Action::MarkReady` while `draft` is set.

Nothing on the channel did that. So a driver read `action: "mark_ready"` and had
no ask to follow it with. It stopped there. A person took every pull request the
loop opened out of draft by hand.

That is a safe place to stop. It is not the loop `doc:backlog-self-driving` §3.3
describes. It spends a whole cycle and hands the work back.

## Decision

**A fifth verb joins the `deliver` family.** `deliver_ready` takes the pull
request out of draft.

**The host reads the forge first, the way `deliver_merge` does.**
`DeliverDesk::ready` reads it. It runs the machine over what it saw. It acts
only if that answers `Action::MarkReady`. A driver that reported a green build
nobody saw gets a refusal. The refusal names the state the host found.

That is what keeps the draft worth opening. A draft holds back a pull request
that never goes green. Marking one ready on a plugin's word would spend that.

**The consent sentence grows to match.** `DriverFamily::Deliver` now reads
"pushes branches, opens pull requests, reads CI, takes them out of draft, and
merges". A verb that calls in a reviewer belongs in the sentence a person reads
before install.

### What was given up

**Opening the pull request ready when the operator asks** was the second shape.
It moves the choice to open time. A pull request whose checks then go red has
already called in a human. That is the property the draft buys.

It also needs an operator flag on the driver channel. The channel offers a
plugin no way to waive a human safeguard. A plugin asking for one over a socket
is the loop granting itself the consent.

**A person marks every pull request ready** was the third. It is what the tree
does today, written down. It caps the loop one step short of its goal for good.
`Action::MarkReady` becomes a decision nothing acts on. AGENTS.md asks every
emitted signal to name what reads it.

The cost: `deliver_ready` reads the forge. It spends a `gh` call `deliver_next`
does not. A cycle that marks a pull request ready and merges it pays for three
reads, not one. That is the price of a host that acts on nothing only the driver
saw.

## Consequences

- A green, approved draft reaches a merge with no human step. The witness is
  `driver_plugin::tests::deliver::a_green_draft_reaches_a_merge_with_no_human_step`.
  It drives the shipping desk over a fixture forge that starts as a draft. It
  asserts the merge. A mark-ready that never reached the forge leaves `draft`
  set, and the machine answers `mark_ready` again rather than merging.
- A draft whose checks have not finished stays a draft. The witness is
  `driver_plugin::tests::deliver::a_draft_that_is_not_green_is_not_taken_out_of_draft`.
  It asks one host the question above and gets the other answer. So neither test
  passes by acting on everything, or on nothing.
- The shipped program does it end to end.
  `driver_plugin::tests::deliver::the_shipped_program_carries_a_green_draft_to_a_merge`
  runs `plugins/stella-selfdriving/main.py` over the real transport. It asserts
  both asks reached the forge, in order.
- `plugins/stella-selfdriving/plugin.toml` declares `deliver_ready`. So an
  install is consented again before the loop uses it. A driver that does not
  declare it is refused `undeclared`. It stops where it stopped before.
- `docs/wire/wrapper.wire.json` gains a `deliver_ready` ask, an `ok/ready`
  answer, and a `driver_call` entry. A driver written against the old bytes
  reads and writes what it always did.
