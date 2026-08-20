# stella-candidates — best-of-N, as a plugin

Try the work N ways in isolated worktrees, keep the one that does the least
damage, throw the rest away.

`candidate_fanout`, `run_test` and `adopt_candidate` landed with #3844 and
**nothing consumed them** — three capabilities on the wire with no plugin asking
for any of them. This is the consumer, and it is the only thing in the tree that
reaches the `again?` point through a real fan-out.

## Read this before installing

Every candidate is a **writing** worker turn in an isolated worktree, and this
plugin asks for up to `max_fanout_width` of them per round. That is N times a
worker turn's spend, on your key, bought by a plugin rather than by you asking
for it.

The host clamps the width against its own ceiling and reports what it actually
ran, so the number in the manifest is an ask and never an authority — but read
it as "this may spend N turns", because it may.

Stella does not score the candidates. It mints the worktrees, runs the turns and
reports what each did; the choosing is this plugin's job, and the rule it
chooses by is declared in `[oracle]` as data you can read before installing.

## Why everything happens at `after_turn`

`doc:plugin-completion-plan` §4.2 puts the fan-out at `before_turn`. **This
plugin does not**, and the deviation is deliberate rather than an oversight.

A plugin is a fresh process per point — `SubprocessWrapper::exchange` spawns per
call. So a `before_turn` fan-out exits before `after_turn` runs and has nothing
left to report, and `ObservedEvidence` is an `after_turn` value. Nothing carries
across the boundary: `BeforeTurnRequest::published` propagates signals only
within one `before_turn` sweep, and `AfterTurnRequest` has no field for them.

A plugin that fanned out at `before_turn` would therefore have to fan out
**again** at `after_turn` to have anything to report — N writing worker turns
bought twice.

So the host's own turn is attempt 0, and this plugin buys alternatives at
`after_turn` when the outcome warrants it. Same best-of-N, one process, and one
fewer turn: "best of N" costs N-1 extra turns rather than N.

## How it chooses

Mechanical, in this order, and there is no model call anywhere in the program:

1. **A candidate that did not finish is never chosen.** `completed = false` is
   an ordinary outcome — a carve ran out, a step cap hit — and its workspace is
   still readable, but adopting an unfinished attempt lands a half-change on the
   real tree.
2. **A candidate whose tests passed beats one whose tests failed.** With no test
   signal (see below) every candidate is equal here and the tiebreak does the
   work.
3. **The smallest `lines_changed` wins.**
4. **Ties break on the handle**, so the choice is deterministic rather than
   dependent on the order the host happened to mint them in.

A model-scored ranking is **not** expressible in the oracle grammar
(`doc:plugin-completion-plan` §6.1: *"a verdict over an aggregate the oracle
computes, not a quantifier the host evaluates"*) and smuggling one in through
the oracle process is the failure mode §6 exists to refuse. There is no arm in
`main.py` that asks for a model call, which is how that stays true rather than
being promised.

## The test signal, and why it asks anyway

`run_test` is answered `HostCallRefusal::Unsupported` by **every host** today —
the arm in `wrapper/host_call.rs` is unconditional, and #3580 is open.

This plugin asks for it regardless, once per candidate, and degrades to the
mechanical signals when the answer is a refusal. `test-signal-available` reports
which of the two happened, so a reader can tell a run that ranked on tests from
one that ranked on diff size alone.

That is the decision recorded on #4029: **ask, degrade, disclose.** The
alternative — reading the candidate grant's `TestPlan` and running the tests
here, the way `verify-{rs,py,ts}` run theirs — would route around #3580
permanently instead of inheriting its fix. `run_test` exists precisely as the
way to ask.

When #3580 lands, this plugin ranks on tests with **no change to it**.

## What it reports

| Measurement | Meaning |
| --- | --- |
| `candidates-scored` | how many the host actually built and ran |
| `candidate-adopted` | 1 when one landed on the real tree, 0 otherwise |
| `winner-lines-changed` | the winner's diff size, so a reader sees *what* was chosen |
| `test-signal-available` | 1 when some host served `run_test`, 0 while #3580 is open |

One requirement, decided by one check: `candidate-adopted >= 1`. Deliberately
not "the best candidate was chosen" — that is not something a check over
reported numbers can decide, and a requirement no oracle can settle is
`ManifestError::UndecidableRequirement`. The honest bar for a plugin whose job
is to leave exactly one attempt on the tree is that it left one.

## What §4.2's witness cannot prove yet

§4.2 asks for *"two candidates, one of which fails its test command; the plugin
adopts the other"*, then the anti-vacuity half, *"both pass, and the smaller
diff wins"*.

The first half **cannot pass** while `run_test` is unsupported — there is no
test signal to fail on. What ships is the second half, plus the thing that
matters more while #3580 is open: that the plugin asks anyway and reports the
absence as a number. See
`crates/stella-runtime/tests/candidates_plugin_hostcall.rs`.

## Testing it

| Harness | What it grades |
| --- | --- |
| `crates/stella-runtime/tests/candidates_plugin_hostcall.rs` | six scripted host conversations: the smallest finished candidate wins and is adopted; `run_test` is asked per candidate and its refusal becomes a declared measurement; an unfinished candidate is never adopted however small; no finished candidate means nothing is adopted; a host with no fan-out plane is a report of nothing rather than a failure; and the manifest declares a `worker` tier for its fan-out role |

That last one is not bookkeeping. `CandidateFanoutArgs::role` is judged by the
**opposite** rule to `child_turn`'s: a child turn may not resolve to the
worker's seat, because a plugin must not grade work with the model that did it —
but a fan-out candidate is not evidence about the work, it **is** the work, so
it must resolve to the worker's seat and nothing else.
