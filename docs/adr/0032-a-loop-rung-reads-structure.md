---
id: adr/0032-a-loop-rung-reads-structure
title: "ADR 0032: A loop rung reads structure, not progress"
status: implemented
---

# ADR 0032: A loop rung reads structure, not progress

- Status: accepted
- Date: 2026-09-07
- Decides: `#3292`
- Builds on: `doc:adr/0031-a-turn-has-no-step-cap-by-default`

## Context

`crates/stella-core/src/loop_detect.rs` has five rungs. Each one reads a shape
in the calls the turn has made. The same call twice. A short cycle. The same
answer to changing arguments. One call coming back with other work between.
A sweep that ran off the end and started again.

One recorded turn has none of those shapes and still got nowhere. It made 62
shell calls and no other kind of call. 59 of the commands are distinct, and
all 62 answers are. It produced no result. `#3292` opened on it as a gap in
this module. Its own census says 62 distinct commands; the committed fixture
says 59, and the input-keyed rungs stay silent either way, because each of
them also needs an answer to come back unchanged.

Three signals were then measured against the same corpus of recorded trials:
497 of them, 264 that scored zero and 233 that scored one.

**Repeated output lines.** Count the lines in a window of answers that were
already seen earlier in that window. The stuck turn's middle value sits
*below* the middle value of the turns that won, at every window size tried. A
cut-off high enough to never fire on a winner catches none of the stuck turns.
A cut-off drawn from ten winners fires on 68 of the 230.

**No file changed.** Score each call for whether it wrote anything. Three
ways. Only what is certain. Plus commands whose whole job is to write. Plus
every call that hands work to a program. A cut-off that never fires on a
winner is 202 calls in a row, and it catches none of the stuck turns. The
sharper shape is a turn that *ends* without writing. It reaches 94% precision
at 20 calls. It does not fire on this trace at all, because its last call
writes a file.

**The plan not moving.** Compare each stated next step with the one after it.
Over the model's narration the two groups agree to two decimal places. Over
its reasoning text a cut-off at 0.384 has no false alarm and catches 4 turns
in 115 — and this trace is not one of them.

A fourth shape *did* separate, and it was found by accident. A command re-sent
with one more copy of itself glued on. It fires on three turns in 371 and
every one of them is grinding. It also does not fire on this trace. `#5863`
carries it.

The labels are wrong as well, which bounds what this corpus can ever settle. A
turn that scored 1.0 contains 107 grinding calls in a row, made after its
deliverable was already on disk. So "never fires on a winner" asks for a rule
that never fires on a turn that really was grinding. `#5865` carries that.

Read the trace and the reason is plain. Each command is a fresh idea. Each
block of reasoning names a new next step: probe the video, pull frames, read
the text, fix the crop. The model is debugging an approach that will not work.
Nothing in the calls says so. Only the task does.

## Decision

**A rung reads structure.** A rung keys on a property of the recorded calls
and answers. A reader can check it by looking. A repeat. A cycle. A constant
answer. A wrapped sweep. One call that grew out of the one before it. A rung
may not ask whether the turn is getting closer to the goal.

That question is a judgement about the task, and `loop_detect` is a plain
synchronous function over owned data (AGENTS.md #2). Answering it well also
needs a model call, which no verdict here is allowed to buy. Either bar alone
would settle it.

**The shape in `#3292` is out of scope for this module.** Not unmeasured, and
not waiting on a better metric. A turn whose commands vary, whose answers
vary, and whose stated plan varies, that is still getting nowhere, differs
from a turn that is working only in what the work is for. That fact is not in
the call stream, so no rung can read it.

**What bounds it instead is what ADR 0031 already chose.** The budget, the
wall-clock deadline, and the host's goal predicate each measure something the
operator asked for. The deadline also tells the model to hand in what it has
before it is cut off (`driver/deadline_notice.rs`), which is what turns a run
into a partial answer rather than nothing. A run with none of those armed is
bounded by the person watching it, which ADR 0031 states and accepts.

**A new rung's evidence is hand-read windows.** Trial reward is not a label
for this question, because a winning trial can contain grinding. So a rung
ships when every window it fires on across the corpus has been read and is
grinding — not when it clears a false-alarm rate against reward.

**The trace is committed.** `crates/stella-core/src/loop_detect/tests/` holds
its calls and answers, and pins that no rung fires at any step of it. `#3292`
asked for that trace as a fixture proving a new rung fires. It becomes the
fixture proving this module's edge instead.

## Consequences

- `#3292` closes on this record. Its ask — a rung that fires on this trace —
  is refused, with the measurements that refuse it written down here so a
  fifth session does not run them again.
- `#5863` stays open and stays in scope. A command that grows by repeating
  itself is structure, and its evidence is the three turns it fires on, each
  one read.
- A sixth rung that fires on the committed trace reddens
  `loop_detect::tests::grind`. That is the point. Its author has to say in
  review why firing there is right, instead of reaching this shape by
  accident.
- The module doc of `loop_detect.rs` carries the same rule, so an author
  reaching for a new rung reads it in the file rather than here.
- Nothing in the engine changes behaviour. The turn that prompted this still
  runs until a bound the operator set ends it.
