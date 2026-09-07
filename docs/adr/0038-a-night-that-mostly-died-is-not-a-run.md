---
id: adr/0038-a-night-that-mostly-died-is-not-a-run
title: "ADR 0038: A night that mostly died is not a run"
status: implemented
---

# ADR 0038: A night that mostly died is not a run

- Status: accepted
- Date: 2026-09-07
- Decides: `#6025`

## Context

`nightly-bench.yml` asks `loop-bench` for four trials a night. Two gates read
the result. `loop_broken` is always on. `below_pass_floor` is off. A third
signal is counted, printed, and read by no gate. It is `Tally::crashed`, which
is harbor's own record that a trial raised.

Most nights, most trials raise. Harbor kills them on its agent timeout. It
does so after 750 or 900 seconds of real work. The table already says this, in
`loop-bench`'s own words: "the pass rate is over a run that partly did not
happen." Nothing acted on it.

Take the 29 nights from 2026-08-10 to 2026-09-07 that left a report. Each
asked for four trials. One night lost none. Fourteen lost one. Five lost two.
Nine lost three. No night lost all four.

Nine nights lost most of the run. Eight were red for loop health anyway. The
ninth was green. Run 33306583016, on 2026-08-30, passed the nightly on one
solved trial and three that never finished.

Each number here comes from that run's own `report.json`, read out of the
artifact it uploaded.

## Options

Leave it ungated and say it louder. A harbor timeout is the machine's fact,
not the engine's. That is why `CRASHED` is a row verdict and not an exit code.
It is why `UNREADABLE` and `BUDGET-CAP` gate nothing. One dead trial in four
leaves a smaller sample, not an absent one.

Or add a crash ceiling: red when more than N trials raised. It needs a number.
`#6025` argued that a number wants a few nights of history first. That
argument has kept `LOOP_BENCH_MIN_PASS` at zero since the day it landed.

## Decision

Gate it, as a share rather than a count. `loop_bench::most_trials_crashed` is
red when more than half the asked-for trials raised. `unfinished_exit` in
`bench/loop-bench/src/main.rs` turns that into exit `8`.

The gate makes a third claim. `loop_broken` says the loop went wrong.
`below_pass_floor` says the agent solved too little. This one says the night
saw too little to say either.

It runs after loop health and after the sample check, and before the pass
floor. So a run that trips more than one still names the half a human can act
on. And it never prints a pass rate over a sample this thin.

A share, not a tuned count, is what makes it shippable today. A pass floor is
a bar on a quality number whose normal value nobody knows yet. It does need
history. This is a bar on how much of the night ran. Its value follows from
what the run asked for. Below half, what is left is not the pinned task set
that `nightly-bench.yml` calls its fixed seed. A share also follows `--tasks`,
`--n` and `--trials` when they move the total. A count pinned to four would
come to mean something else.

## Why this is the durable choice

The workflow already refuses to start with no provider key. Its reason is
written down: a green nightly gate that measured nothing is the exact failure
`bench.yml` complains about. A night where three of four trials die reaches
that state by another road. The gate had no way to say so.

CLAUDE.md states the general rule. A green that flatters us is worse than a
loss. A signal that answers the next question over is worse than no signal,
because someone believes it.

The cost is measured, not argued. Over the 29 nights the rule fires nine
times. It flips one verdict. So it does not become the always-red gate that
nobody reads. That risk is the whole case for leaving this alone.

Red here also tells you what to do. It says the task set, the agent timeout,
or the runner cannot finish what the night asks for. That is a real defect in
the measure. Fix one of the three. Never read the same night greener.

## Consequences

- `bench/loop-bench/src/lib.rs` gains a third gate and a crate-doc section
  beside the two already there. The table prints the majority case in the
  words the exit code uses.
- `.github/workflows/nightly-bench.yml` names exit `8` in its header and in
  its summary.
- The nightly may go red on a night the other two gates pass. That is the
  point. The ninth night above is the one it is for.
