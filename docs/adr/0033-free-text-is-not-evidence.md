---
id: adr/0033-free-text-is-not-evidence
title: "ADR 0033: Free text is not evidence"
status: implemented
---

# ADR 0033: Free text is not evidence

- Status: accepted
- Date: 2026-09-07
- Decides: `#3911`, and `doc:turn-loop-wrappers` §9.2's open half
- Not part of the Phase 0 series.

## Context

A goal-mode verifier answers in three parts.
`stella_core::goal::GoalVerifierVerdict` carries `met: bool`, `reasoning:
String` and `feedback: String`.

The built-in loop uses all three. `met` ends the loop or does not. `feedback`
goes to the worker word for word next round. When the verifier wrote none, the
`reasoning` goes instead. And on a round that passes, the CLI prints the
`reasoning` to the person who asked: `goal met after 2 rounds: a regression
test now covers the fix`.

A wrapper plugin reports what it saw as `stella_plugin::ObservedEvidence`. The
host merges its own tamper finding in. What `judge` then reads is an
`EvidenceSet`. That type is closed on purpose. It holds three enums and a map
of names to `u64`. `judge` has to be total over it, and totality over closed
enums is something the compiler checks. Totality over a string is something a
reviewer has to promise.

So a plugin doing goal mode's job had a slot for `met` and no slot for the two
strings. The goal door's own wrapped arm said so in its module doc.
`doc:turn-loop-wrappers` §9.2 named the gap and left it open. Slice 7 of the
roleless-core epic (`#3911`) waited on the answer, and moved `stella goal` onto
the wrapper socket once it had one.

## Decision

**The free text is not encoded into `EvidenceSet`. It never will be.** The
verdict is split by what reads each part:

| Field | Where it goes | Who reads it |
| --- | --- | --- |
| `met` | `ObservedEvidence::measurements["met"]`, 1 or 0, under `flip = "not-applicable"` and an `[[oracle.checks]]` rule `met >= 1` | `judge`, and nothing else |
| `feedback`, or `reasoning` when it is empty | `ObservedEvidence::detail`, one advisory string | the round's readers, never `judge` |

`detail` does not cross into `EvidenceSet`. `EvidenceSet::from_observed` drops
it, so the verdict is still read off closed fields alone. The string rejoins
after the verdict. The rule that keeps it safe is this one:

> Free text never enters the vocabulary a verdict is read from. It rides
> beside the verdict, on a channel `judge`'s signature cannot reach.

The first two rows landed in `#3840`. What this record adds is the third fact,
which that change left half done: **the note reaches a reader on every
verdict, not only on an unmet one.**

`Verdict::with_detail` writes the note onto each unmet clause, and
`correction_text` prints it to the next round. On a `Met` or `Undecided`
verdict it returned the verdict untouched and the string was gone. That string
is the sentence `stella goal` prints on success, and `plugins/stella-goal`
already sends it.

So `DispatchReport` gains `note: Option<String>`. It is the last round's
`ObservedEvidence::detail`, carried word for word whatever the verdict was,
and `stella-cli` prints it on its own line. One origin, two readers, picked by
what the round did. The correction, when the round holds open. The report,
always. Neither one decides anything.

## Options rejected

**Give `EvidenceSet` a free-text field.** This is the shape the question
invites. It is the one that cannot be allowed. `judge` would be total over a
string, which is total by promise, not by construction. And a plugin's prose
would sit in the type a verdict is read from. Two rounds of work went into
keeping a plugin from speaking the tamper finding (`#3499`) and into keeping
this note out of the type (`#3840`). This undoes both.

**Put the note in each arm of `Verdict` and `Outcome`.** Symmetric, and wrong
about what the note is. `with_detail`'s own doc says it: the note is one
remark about the round, not a claim about a requirement. Per-arm fields spread
one string across three arms of two wire enums. A reader holding a `Verdict`
would still have to be told which arm to look in. What that choice gives up:
a caller holding only an `Outcome` cannot see the note. It has to hold the
report. Every caller does.

**Leave the passing case silent.** Cheapest, and it makes the extraction a
visible loss. Goal mode as a plugin would stop saying why the verifier passed
it. A replacement that drops a line the user reads is not a replacement.

## Consequences

- A goal plugin can say why it passed. The witness is
  `the_verifiers_own_words_survive_a_met_verdict` in
  `crates/stella-runtime/tests/goal_plugin_dispatch.rs`. It drives the shipped
  `plugins/stella-goal` through a real `WrapperDispatch` and reads the
  verifier's own sentence off a met round. Before this the report had no field
  to read it from.
- `judge` is unchanged. It never saw the note and still cannot. The same test
  checks that a met verdict carries no free text.
- `summary()` stays one line. The note is a second line, so a plugin's prose
  cannot reach `VerdictEvidence::summary`, which is a journal field.
- `DispatchReport` is a host struct, not a wire type. The JSON a plugin speaks
  is the same, and `PROTOCOL_VERSION` stands.
- A plugin with no note leaves the field out. Every surface prints what it
  printed before.
- Slice 7 loses its blocker. The move itself is still open. This record
  settles the encoding and the missing reader, not the work.
