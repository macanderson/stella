---
id: adr/0048-an-appraisal-reads-a-window-of-control-trials
title: "ADR 0048: An appraisal reads a window of control trials"
status: implemented
---

# ADR 0048: An appraisal reads a window of control trials

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
For a skill they are rare. Only two controls write them: the plane control
and the holdout. How often each one fires depends on two settings. At the
shipped rates (`ab_recall_rate` 10, `artifact_holdout_rate` 20), about one
matched turn in nine is a control turn for a skill that matches alone.

A rule can be the other way round. A rule that matches but does not fit the
prompt budget is held back too. A rule that fits one turn in ten is shown on
one turn in ten, so its shown arm is the rare one. The rows carry no time.

The issue asked us to choose. Is the window a count of rows or a span of
time? Does each arm get its own bound?

## Decision

**The window is a count of rows in each arm.** `live_window`, in
`stella_learn::skills::appraisal`, walks back from the newest row. It stops
at the row that gives both arms `AppraisalConfig::window` rows. It keeps
that row and every row after it. `sweep` judges those rows and no older
ones. If either arm has fewer rows than that in the whole ledger, `sweep`
judges every row.

**One number does both jobs.** `window` was already the bar for `Inert`.
That verdict needs a full window in each arm. Now the same number sets the
length of the live window. So a full window always holds the rows `Inert`
needs, in both arms. No second knob can be set below it.

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

**A count of control rows alone.** This only works when the control arm
is the rare one. For a rule shown one turn in ten, 20 control rows hold two
or three shown rows. That is below `min_samples_per_arm`, which is 5. The
rule stays `Insufficient` for good, so a rule that hurts is never retracted.

Filling both arms in one stretch keeps the good part of each. Both arms come
from one stretch of time. The stretch grows or shrinks with the rarer arm.
So each kind always has both arms full to judge by.

## What follows

**Age does not protect a skill.** Once both arms are full, a skill
that stops helping is judged on the turns since it stopped. The good years
before that do not count.

**A rare arm means a long window.** A rule shown one turn in ten is judged
over about 200 turns, not 40. Decay takes longer to show for it. It still
shows, and the rule can still be judged.

**The holdout aim does not change.** `control_arm_counts` still counts the
whole ledger. When the ledger holds fewer than one window of control rows,
the live window holds all of them. When it holds more, the live window holds
at least one window. The holdout's bar is `min_samples_per_arm`, which is
below one window. So both agree on whether an item is short.

The issue expected a trimmed control arm to send a skill back into the
holdout rotation. This window never trims the control arm below a full
window, so that path is not needed. The plane control still reaches every
matched skill. When no skill is short, the holdout rotates over all of
them.

**The window stalls if either arm stops.** Turn both controls off, and no
new control row comes in. The window then keeps every with-skill row since
the last one. A rule that never fits the budget again stalls the same way
from the other side. That item goes back to the old, unbounded read. No new
row of the stalled arm has come in, so nothing newer can be compared.

**The ledger still grows.** `sweep` reads the whole file, then slices it.
This bounds the evidence. It does not bound the cost of the read.

## Evidence

`a_skill_that_helped_for_a_long_time_is_still_demoted_once_it_stops` lives
in `crates/stella-cli/src/memory/learning/skill_lifecycle.rs`. It seeds ten
windows of a skill that helped. It checks that this history alone reads as
`Helps`. Then it runs one window of turns where the skill stopped helping,
through the real turn code. Three sweeps must demote the skill. Before this
change, the first sweep found no fault.

Five tests in `crates/stella-learn/src/skills/appraisal/tests.rs` pin the
slice itself. `the_live_window_judges_a_skill_on_what_it_did_lately` is the
same case without the turn code. `the_live_window_holds_a_full_window_of_each_arm`
pins where the cut falls. `the_live_window_reaches_back_when_the_with_skill_arm_is_rare`
pins it for a rare shown arm, and
`a_rarely_shown_artifact_that_harms_is_still_judged_on_its_live_window`
checks that such a rule is still judged. The property
`the_live_window_is_the_shortest_suffix_holding_both_arms` checks the cut
for any history.
