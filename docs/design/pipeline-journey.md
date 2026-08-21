---
id: pipeline-journey
title: "The journey of a prompt — the raw loop, and what a wrapper plugin adds"
status: living
---

# The journey of a prompt — the raw loop, and what a wrapper plugin adds

**Status:** living. Part I describes what this workspace runs today. Part II is
an **archived record** of the built-in staged pipeline, kept in the past tense
because it is the shape `doc:pipeline-as-plugins` §8 asks a verification plugin
to port — not code you can step through here.

> **What changed.** `crates/stella-pipeline` was deleted from the workspace
> (#3865) and `--pipeline classic` is refused outright (#3867). Host-run
> verification no longer exists here at all. The only verification path left is
> an **installed wrapper plugin**, whose evidence is self-reported and which
> Stella evaluates against the plugin's own declared rule without re-running it
> (#3511). This page was rewritten to say so; the sweep of the rest of the tree
> is #3901.

---

# Part I — what actually runs

## 0. Two ways a prompt is served

Every door — `run`, `arena`, `goal`, `fleet`, the Command Deck — resolves one of
two choices, in `stella_cli::wrapper_plugin::PipelineChoice::resolve`:

- **`Raw` — the default, and what you get with no flag at all.**
  `stella_core::Engine::run_turn`: the model proposes tool calls, tools run,
  results feed back, repeat until the model stops or a budget/backstop fires.
  No stages, no verification, no second opinion. `--no-pipeline` is a
  deprecated hidden no-op that names this default explicitly.
- **`Plugin(variant)` — `stella run --pipeline <plugin-id>`, opt-in.** The
  named plugin must be installed (`stella plugin install`). The host drives it
  over the **wrapper socket**, and the turn is still the same
  `Engine::run_turn` underneath — the plugin gets to speak before it and after
  it, and to say *not yet*.

There is no third choice. `--pipeline classic` names the deleted built-in path
and is refused with a message pointing at `stella plugin install`; the
verification-only flags `--test-command`, `--keep-witness` and
`--require-verified` are refused unconditionally on every resolution
(`reject_verification_flags_without_pipeline`), because the host no longer owns
a verification stage for them to configure.

## 1. The wrapper socket — four points

`doc:wrapper-socket` is the design and `stella_runtime::wrapper` is the
implementation. A wrapper is **two async calls it makes, plus two pure
functions the host runs on its behalf**, and that asymmetry is the whole
contract:

| Point | Shape | Owner |
|---|---|---|
| `before_turn` | async, may be remoted | the plugin |
| `after_turn` | async, may be remoted | the plugin |
| `judge` | synchronous, host-run, total | the host |
| `again?` | synchronous, host-run, total | the host |

The split is what keeps a plugin from grading its own work with a model. It may
gather evidence in `after_turn` — run its oracle, report a flip, report
measurements — but the *verdict* is `judge`, a total synchronous function the
host evaluates against the rule the plugin declared in its manifest at install
time. A plugin cannot buy a model call to decide whether it is done, and it
cannot change the rule after consent.

`stella_runtime::wrapper::WrapperDispatch` is the host sequence that drives the
four points. It lives in `stella-runtime` rather than in `stella-cli` because
the same plugin must run under three drivers, and a sequence living in the
binary is one the other two cannot reach.

`admissible` sits **on** that path, not beside it: it consumes the plugin's
response and yields an `AdmittedContribution`, which is the only value
`WrapperDispatch` will apply. A role intent the manifest never declared is
refused, and a signal published at the wrong type is refused — the same rules
`stella-plugin` enforces on *declarations* at load, enforced again on *values*
at dispatch, so the manifest's guarantees do not stop at the process boundary.

Two transports carry the same owned types: `SubprocessWrapper` speaks the wire
contract over stdio in any language, and `InProcessWrapper` is a permitted Rust
fast path. Nothing is reachable through one that is not reachable through the
other — if Rust ever gained a capability Python could not ask for, the wire
contract would have become second-class.

## 2. A plugin may ask, never reach

A point is a bounded conversation, not a one-shot (#3540). A plugin may ask the
host for a capability; it may never reach for one. `HostCallGate` is the host
half: every call is performed by the host, behind
`LoopGrant::permits_call` and a clamped per-point allowance, so what the plugin
gets is what a human consented to at install and nothing more. Host calls are
available during `before_turn` and `after_turn` only, which is what keeps
`judge` and `again` synchronous, I/O-free and total.

## 3. What a manifest declares

`plugin.toml`, parsed and validated by `stella-plugin`. Participation is
declared, never inferred, and **undeclared means none**:

| Block | What it grants |
|---|---|
| `[loop] participation` | `none` / `observer` / `steering` / `arbiter` — a monotone ladder |
| `[loop] hooks` | the hook points this plugin answers, exhaustively |
| `[loop] points` | the wrapper socket points it answers, exhaustively |
| `[requirements]` | arbiter only: the enumerable definition of done |
| `[oracle]` | arbiter only: the evidence shape and the verdict rule as data |
| `[subloop]` / `[roles]` | stages run as bounded child turns, and their routing intents |
| `[wrapper]` | the stage order and the conditions under which each stage runs |
| `[runtime]` | the process it runs as, and the exact environment slice it inherits |
| `[capabilities]` | what it asks to reach outside the turn, each risk-graded |
| `[[configure]]` | config it sets for as long as it is installed |

An undeclared hook is never invoked and an undeclared point is never dispatched,
even if the plugin's process would happily answer. Unknown keys are a load
error, so a typo'd grant fails loudly at install instead of silently granting
nothing.

`[oracle]` is where a verification plugin puts what used to be host machinery.
`flip = "required"` says the host credits the requirement only on a fail→pass
flip the plugin **reports having seen**; `flip = "not-applicable"` says this
oracle's evidence is measurements rather than a flip, and is not a weaker
contract — with no flip to decide anything, every requirement must be decided by
a declared check or the manifest is refused. `tamper` names what the *host*
does: it snapshots witness-artifact identity and refuses the flip if it changed,
which is why tamper findings are not something a plugin may report.

## 4. Where the seats come from

A plugin declares a bare role name; the host applies the `<plugin-id>/` prefix,
so the namespace cannot be forged by a plugin claiming another's. The operator
assigns each seat a model in `stella.toml`:

```toml
[seats]
"vera/verifier" = "anthropic/claude-opus-5"
"stella-plan/planner" = "openrouter/openai/gpt-5.5"
```

`[seats]` is its own top-level table rather than part of `[agents]` precisely
because seat names are an **open** set chosen by whatever the operator
installed, and `[agents]`' flattening is only safe because its set is closed.

---

# Part II — archived: the built-in staged pipeline

Everything below is written in the past tense and describes
`crates/stella-pipeline`, **deleted from this workspace in #3865**. No file
named here still exists. It is kept because `doc:pipeline-as-plugins` §8 asks a
verification plugin to *port* this shape rather than reinvent it, and because
several of these rungs exist only as the scar of a specific measured failure —
the reasons are the part worth carrying forward.

The one-line itinerary it ran:

> prompt → triage (+ recall, concurrently) → [conversational fast path?] →
> plan → scope review → execute → diff probe → witness warrant → witness
> authoring (on demand) → evidence ladder → { submit fast | revise | nothing
> attempted | abstain | unverified } → verdict → complete.

A detail worth carrying forward, because older docs had it backwards: **the
witness test was authored *after* execution, not before it.** Authoring waited
until the warrant had read the actual diff, so a docs-only change never bought a
test, and the author was kept *blind* to the diff (it worked in a pristine
snapshot of the pre-execution tree) so it could not write a test that merely
restated the patch.

## A. Intake — triage and recall ran side by side

Two things started at once, because neither depended on the other. **Triage**
was one cheap model call classifying the prompt on two independent axes — *is
this even a software task?* and *how much ceremony does it deserve?* — under a
hard latency ceiling, falling through to the full path rather than waiting. A
**deterministic floor** pattern-matched the goal and could only ever *raise* the
class toward more planning, never lower it: a misclassified complex task had to
still complete, just with less scaffolding, never fail outright.

**Context recall** was advisory memory/graph recall over the goal, bounded by
its own ceiling and degraded to "no frames" on expiry. Recalled frames rode as a
**volatile user message after the byte-stable system prefix** — never mutated
into the system block — so prompt-cache hits survived across turns. That
discipline is invariant 7 and is unaffected by the deletion: it still governs
`stella_cli::agent::build_system_prompt` today.

## B. The conversational fast path

If triage said "this is chat, not work" and the deterministic floor saw no task
signal to overrule it, the pipeline answered in one plain, tool-less completion
and exited. This was the escape hatch that kept a bare `hi` from being planned,
executed and witness-tested.

## C. Plan, then scope review

Only a multi-step class planned. A plan that failed to parse got one bounded
repair attempt, then degraded to a single-step plan — a stubborn planner never
blocked the work.

If the plan's blast radius crossed a configured threshold (more than 5 steps,
more than 8 estimated files, or estimated cost over $1, all strict `>`), the run
paused for **scope review**. Interactively you approved, trimmed to the largest
under-threshold prefix, aborted, or sent the plan back with a note. Headless, an
over-threshold plan ended the run rather than silently auto-approving.

> The `headless_scope_bypass` engine-config key that steered the headless half
> still parses today and does nothing — the stage it steered went with the
> crate. It is kept only because a published benchmark posture hashes it. Do
> not read it as a knob you can still turn.

## D. Deciding where the work happened

Before executing, the pipeline resolved *whose tree* the candidate worked in.
Best-of-N always isolated: each candidate ran in a snapshot of the current tree
(HEAD + uncommitted + untracked, via a detached git worktree), and only the
winner's changes were adopted. An authored witness always required a disposable
candidate, even at N = 1, so authoring could never mutate the session tree.
Otherwise the worktree policy decided, consulted only when the run would
actually change files.

There was a "never choose nothing" backstop: a candidate that aborted in *setup*
degraded to a bare worker run on the working tree — the fancy path being
unavailable is a reason to do less, never a reason to do nothing. Genuine
execution aborts (budget, loop detection, step caps) kept their stop.

## E. Baselines — the flip oracle armed before execution

When the user supplied a test command, the candidate ran it **once before
executing anything** and recorded the result in the **flip oracle**: a state
machine keyed on the normalized command string that locked onto the first
*failure* it observed, and moved to `Flipped` only on a later *pass of that same
command*. A suite that was already green could never produce a flip — which
structurally excluded the "it passed, ship it" false positive.

Two refinements kept it honest, and both are worth porting:

- **Typed outcomes (#860).** A baseline that timed out or never spawned observed
  no assertion and did not lock the oracle — infra noise plus a merely-faster
  candidate must not read as a verified flip.
- **Failure fingerprints (#867).** A failing baseline contributed the *names* of
  its failing tests; a later pass that named its tests without naming the
  baseline's failures was no evidence. This refused fix-by-disappearance —
  delete the failing test, suite exits 0.

## F. Execute, then the diff probe

The worker ran one engine turn for a simple task, or one per plan step for a
multi-step plan. The pipeline counted `FileChange` events and **mutating
actions** (dispatched tool calls whose tool is not advertised read-only; unknown
tools counted as mutating).

The diff probe was **engineered to be incapable of lying**, and its output was
deliberately three-valued:

- a real diff;
- "the tree changed but the diff could not be captured" — `FileChange` events
  positive, diff empty — which downstream readers had to treat as *couldn't
  verify*, never *verified nothing*;
- "the probe could not read the tree at all", with a separately named case for
  "this is not a git repository, so `git diff` can never answer here" — the
  permanent condition of a Terminal-Bench task image (#973).

That three-valued shape exists because an ambiguous empty diff once convinced a
verifier that "no changes were made" — the archetypal verification lie. **This
is the single most portable lesson in Part II.**

## G. The witness warrant, then on-demand authoring

With no test command armed, verification still wanted a deterministic oracle,
but only when the change *warranted* one. The **warrant** read the diff and
answered "does this change need a witness test, and if not, why not":
`NothingChanged`, `DocsOnly`, `TestsOnly`, `ConfigOnly`, `CommentsOnly`,
`PureRemoval` — each a *stated reason*, recorded in the verdict, mirroring the
contributor rule ("ship a witness test, or a stated reason there isn't one").
Anything mixed, unrecognized or unreadable **failed closed to Required**: an
unnecessary witness costs one model call; a missing one ships unverified
behavior.

When required, an independent model wrote a minimal *failing* test, working in a
pristine snapshot of the pre-execution tree, blind to the diff. The authored
test had to fail on the old code first, pass a static assertion-density screen
(no assertion-free tests, no constant-only assertions, no self-comparisons, no
bare `#[should_panic]` — #863), and survive tamper checks every verify
iteration.

The author was skipped, degrading to an unauthored ladder, if it would have been
the same model as the worker — **Stella would not let the worker write the test
that proves the worker.** Under `require_independent_witness` that degradation
became an up-front refusal instead (#1147: a benchmark arm whose manifest claims
an independent author must not silently produce a number without one).

## H. Verify — the evidence ladder

A pure function took one snapshot — flip state, touched-test result, diff size
and availability, file-touch count, mutating actions, new lint diagnostics, and
whether the witness proved tautological — and answered **in this order**:

1. **Touched tests red → revise.** Already a deterministic failure; nothing was
   spent confirming it.
2. **Nothing attempted.** Zero mutating calls and nothing observed a change: the
   model narrated a solution and wrote none of it down. This rung is
   *knowledge*, not abstention — a run that never acted ended `passed: false`.
   Before it existed, eleven untouched Terminal-Bench tasks were reported as
   successes.
3. **Every channel blind → unverifiable.** No flip, no test result, an
   unreadable tree, no recorded touch: the ladder *abstained*. A verifier asked
   to rule on an empty record once answered with a confident `FAIL` naming a
   file that existed. The run scored unverified, never passed or failed.
4. **Flip + green + diff within budget + no new lint errors + witness not
   tautological → submit fast.** Two audits ran before the submit was final: a
   **confirmation run** (#859 — one extra suite run; a flake demoted the oracle
   and dropped through) and the **mutation check** (#870 — break the changed
   lines one at a time; a witness that stayed green under every mutant was
   tautological and lost its fast-submit).
5. **Otherwise → unverified.** Genuinely inconclusive.

**Every rung was terminal — no arm escalated to a model.** That is the property
`doc:pipeline-as-plugins` §8 asks a verification plugin to preserve as it ports
the logic, and it is the reason the ladder cost nothing to run.

## I. Verdict — the ladder's answer was the answer

Rung 5 used to escalate to a **verifier**: a separate model call, by preference
from a different model *family* than the worker, which reviewed the goal, the
honest diff and a structured evidence snapshot. #2584 removed it, along with the
heuristic fallback that covered its outages, and along with **distress
guidance** — a course-correction note that rode with the next revision prompt
after a second deterministic failure.

The reason was measured, not aesthetic. Across an 89-task Terminal-Bench run
where the witness rung could not fire, the verifier agreed with the benchmark's
own grader 46% of the time, and 17 of its false passes cost 5 tasks outright.
The intermediate design was *asymmetric trust* — a "not yet" was actionable, a
"done" standing alone was downgraded to unverified — and the removal is that
asymmetry taken to its limit: if a verdict standing on nothing deterministic
could never be believed, the call that produced it was buying only the half that
could still be wrong.

Distress guidance died on the same principle from the other side: **a claim
appended to a measurement inherits the measurement's authority**, and a worker
receiving both cannot tell them apart. On `fix-git` that narration talked a
worker into resetting `master` and destroying a correctly-recovered commit,
twice. The measurement was already there and already conclusive.

Both removals are structural rather than defaulted, and that survives the
deletion: `Roster::apply` still rejects a `verdict` or `distress_guidance`
assignment as `NotAssignable`, so no configuration restores them.

## J. Revise, select, complete

A revise decision sent the evidence back into a fresh worker turn, up to a
bounded budget. The worker received the sealed, redacted account of the test
that went red and nothing else; repeated identical failures widened what the
prompt disclosed, at coarser or finer grain by repeat count, scrubbed of
secrets. After every revise turn the command was re-run and the ladder
re-decided.

With best-of-N, each candidate ran in its own isolated snapshot and was scored
`DeterministicPass > VerifierPass > Unverified > Failed`, ties broken by
mutation-survival, then fewer new diagnostics, then smaller diff. Only the
winner's changes were adopted — atomically, failing loudly with the conflicting
paths named if you edited the same files mid-run.

The pipeline emitted exactly one terminal signal, and its outcome recorded the
task class, total cost, revision count, how many candidates actually *ran* (not
how many were configured), and `deterministic: true/false` — so a headless
caller could tell a determinate finding from an unproven pass.

## K. Who ran on which model

Six roles, each configured through `agent_engine_config`: triage, worker,
research, plan, verifier, and the witness author resolving from the verifier's
slot.

> **All six are gone.** Core knows exactly one role today, `default` (#3903,
> `doc:roleless-core`), and the `pipeline_triage_model` / `pipeline_worker_model`
> / `pipeline_verifier_model` keys plus the `[agents.worker]`,
> `[agents.verifier]` and `[agents.triage]` tables are recognized, ignored, and
> named on load (#3908). A plugin that wants a second model asks for a **seat**
> instead — see Part I §4.

Two inheritance rules are worth recording because they were both bug fixes.
Research and plan ended their chain at *the worker* rather than `default_model`,
because `--model` re-points the worker for one invocation and deliberately
leaves settings alone: inheriting `default_model` would have split them onto the
model the flag had just overridden, and the run would report one model while
buying two. And a configured role model whose provider had no resolvable key
degraded softly to the worker with a notice — **configuration could never turn a
runnable pipeline into an error**, with `require_independent_witness` the one
deliberate exception.
