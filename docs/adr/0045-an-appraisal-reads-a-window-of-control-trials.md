---
id: adr/0045-an-appraisal-reads-a-window-of-control-trials
title: "ADR 0045: An appraisal reads a window of control trials"
status: implemented
---

# ADR 0045: An appraisal reads a window of control trials

- Status: accepted
- Date: 2026-09-23
- Decides: `#6465`
- Not part of the Phase 0 series.

## Context

`appraisals::sweep` judges each skill, memory and rule from the trial
ledger. It read every row the ledger held for the item. Nothing aged out.
Say a skill helped for six months and then stopped. It was judged on six
months of wins and one week of losses. The more good history it had, the
harder it was to retire.

Each row is one turn that matched the item. Either the item was put in
the prompt, or it was held back. The held-back turns are the control arm.
They are rare. Only two controls write them: the plane control and the
holdout. How often each one fires depends on two settings. At the shipped
rates (`ab_recall_rate` 10, `artifact_holdout_rate` 20), about one matched
turn in nine is a control turn for a skill that matches alone. The rows
carry no time.

The issue asked us to choose. Is the window a count of rows or a span of
time? Does each arm get its own bound?

## Decision

**The window is a count of control rows.** `live_window`, in
`stella_learn::skills::appraisal`, walks back from the newest row. It stops
at the row that brings the control arm to `AppraisalConfig::window`. It
keeps that row and every row after it. `sweep` judges those rows and no
older ones.

**One number does both jobs.** `window` was already the bar for `Inert`.
That verdict needs a full window in each arm. Now the same number sets the
length of the live window. So a full window always holds the control rows
`Inert` needs. No second knob can be set below it.

## The other choices

**A span of time.** The rows have no time. We would need a new field, and
the old rows could never be placed. A quiet month would also look like
decay. It is not. No turns means no new facts, not stale ones.

**The newest N rows, both arms mixed.** The control share decides how many
control rows land in N. At a skill's share, 80 rows hold about nine. `Inert`
needs 20, so it could never fire. A lower share gives fewer still. Below
`min_samples_per_arm`, the item can't be judged at all. An N big enough for
the rarest case is too long to catch decay in the common one.

**The newest N rows of each arm.** No arm runs short. But the two arms then
cover different stretches of time. Control rows are rare, so that arm
reaches much further back. Recent turns would be judged against an old
baseline. If the work got harder, the skill would take the blame. A fair
test needs both arms from the same stretch.

Counting control rows keeps the good part of each. Both arms come from one
stretch of time. The stretch grows or shrinks with the control share. So
each kind always has a full control arm to judge by.

## What follows

**Age does not protect a skill.** Once the control arm is full, a skill
that stops helping is judged on the turns since it stopped. The good years
before that do not count.

**The holdout aim does not change.** `control_arm_counts` still counts the
whole ledger. The live window holds the same count, capped at one window.
The holdout's bar is `min_samples_per_arm`, which is below one window. So
both read the same number where the bar looks.

The issue expected a trimmed control arm to send a skill back into the
holdout rotation. This window never trims the control arm below a full
window, so that path is not needed. The plane control still reaches every
matched skill. When no skill is short, the holdout rotates over all of
them.

**The window stalls if control rows stop.** Turn both controls off, and no
new control row comes in. The window then keeps every with-skill row since
the last one. That item goes back to the old, unbounded read. No baseline
has been measured since then, so nothing newer can be compared.

**The ledger still grows.** `sweep` reads the whole file, then slices it.
This bounds the evidence. It does not bound the cost of the read.

## Evidence

`a_skill_that_helped_for_a_long_time_is_still_demoted_once_it_stops` lives
in `crates/stella-cli/src/memory/learning/skill_lifecycle.rs`. It seeds ten
windows of a skill that helped. It checks that this history alone reads as
`Helps`. Then it runs one window of turns where the skill stopped helping,
through the real turn code. Three sweeps must demote the skill. Before this
change, the first sweep found no fault.

Three tests in `crates/stella-learn/src/skills/appraisal/tests.rs` pin the
slice itself. `the_live_window_judges_a_skill_on_what_it_did_lately` is the
same case without the turn code. `the_live_window_is_counted_in_control_trials`
pins where the cut falls. The property
`the_live_window_is_the_suffix_holding_a_full_control_arm` checks it for any
history.
