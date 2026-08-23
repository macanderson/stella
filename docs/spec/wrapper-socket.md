---
id: wrapper-socket
title: "The wrapper socket: four points, one wire, no host assumed"
status: implemented
---

# The wrapper socket: four points, one wire, no host assumed

**Status:** implemented in the design's own scope, updated 2026-08-17 (design
dated 2026-08-17, landed the same day, #3479). This is the design A3 of
`doc:pipeline-as-plugins` builds. `doc:turn-loop-wrappers` §4 named the four
points; §9.1 of the same document decided they cannot live in `stella-core`.
This document decides everything else: where the trait lives, where the wire
types live, what each point may say, and what `judge` is now that it is not a
plugin's code. **What "implemented" covers and what it does not, stated
plainly because §6 below is an acceptance test and it has not been run:** §2's
table — the trait in `stella-runtime`, the wire types in `stella-plugin`,
`judge`/`again` as free functions — is real code, proven end-to-end against
one real subprocess plugin by `stella-runtime`'s own test suite
(`tests/wrapper_socket.rs`). §6's acceptance test — the same plugin, unchanged,
driven by `stella-cli`, `stella-serve`, and an embedded `stella-engine` host —
has not been attempted: none of the three drivers calls this socket yet, and
there is no host-sequence type in `stella-runtime` for a driver to call
through. Track B (`doc:pipeline-as-plugins` §7) is where a first real caller,
and with it a candidate for that sequence, is expected to appear.

Everything below about today's tree was read out of it; where a claim is an
inference it says so.

---

## 1. The one sentence

**A wrapper is two async calls it makes, plus two pure functions the host runs
on its behalf** — and the two async calls are defined as serialized
request/response first, with the Rust trait as a typed view of the same shapes.

That asymmetry is the whole design, and §3 argues it rather than assuming it.

---

## 2. Where each half lives, and why

| Half | Crate | Why there and not elsewhere |
|---|---|---|
| The **wire contract** — request/response types for `before_turn` and `after_turn`, the evidence vocabulary, the verdict rule | `stella-plugin` | It is the plugin-facing contract, and `stella-plugin` is the plugin-contract crate: pure types and validation, `stella-protocol` its only workspace dependency. A non-Rust author needs exactly this crate's JSON shapes and nothing else, which is a property worth being able to state. |
| The **trait** and the code that sequences the points | `stella-runtime` | `before_turn` performs recall and `after_turn` spawns processes. That is I/O, and invariant 2 forbids I/O in the engine (`doc:turn-loop-wrappers` §9.1). `stella-runtime` already owns engine assembly and reads no ambient environment by contract (`crates/stella-runtime/tests/no_ambient_reads.rs`). |
| `judge` and `again?` | `stella-runtime`, as free functions | They are synchronous, I/O-free and total. §4 is why they are not trait methods. |

This adds one dependency edge: `stella-runtime → stella-plugin`. It is
acyclic (`stella-plugin`'s only workspace dependency is `stella-protocol`) and
it is the edge that ends "`stella-plugin` has zero consumers".

**`stella-core` gains nothing and loses `goal.rs`'s round loop.** The engine
never learns plugins exist. That is not a style preference: it is what lets
`stella-serve` and an embedded host linking `stella-engine` drive the same
wrapper the CLI does, which §6 makes an acceptance test rather than an
aspiration.

---

## 3. Why the wire contract is primary and the trait is the view

`doc:pipeline-as-plugins` §5 gives the adoption argument — a Rust-only
extension surface is a library with extra steps. There is a second argument
that arrives at the same place from the host side, and it is the one that
makes this non-negotiable:

**A loop driven over HTTP and a plugin spoken to over a pipe need the identical
thing** — the loop's participation points expressed as data rather than as Rust
borrows. `stella-serve` already remotes every model and tool call to its host.
If the wrapper socket is authored as a Rust trait over borrowed types and
serialized afterwards, the serialization is a translation of a shape that was
never designed to cross a process, and the second design — the one for hosts —
gets built separately. Building the wire contract twice is the failure to
avoid.

So the ordering rule is mechanical: **a point that cannot be expressed as
serialized request/response is not a point.** If that is discovered while the
trait is still editable, the trait changes. If it is discovered after, the wire
contract inherits a defect it did not choose.

Easy to erode, so written down here:

1. **The Rust reference plugin uses the wire path in CI.** It may
   *additionally* have an in-process fast path. If only Rust can reach a
   capability, the wire contract is second-class and will rot.
2. **`protocol_version` rides on every message**, and the contract is
   additive-only. `docs/wire/` is generated and gate-checked
   (`scripts/check-wire-schema.sh`) precisely so that a renamed field or a
   re-tagged variant lands on the author's screen instead of in a consumer's
   parser; the wrapper wire contract joins it on the same terms.

---

## 4. `judge` is not a plugin's code, and that is the point

`doc:turn-loop-wrappers` §9.2 sharpened "`judge` may not call a model" into a
property of the signature: `judge` is synchronous and I/O-free over owned data,
so the compiler enforces the rule instead of a reviewer. An out-of-process
`judge` is I/O by construction and destroys that property.

`doc:pipeline-as-plugins` §6 resolves it and this document does not re-argue it
— **plugins declare the verdict rule as data; the host evaluates it.** What
this document adds is the shape that resolution implies for the socket:

> `judge` and `again?` are **not trait methods**. They are host functions over
> the plugin's declared rule and the evidence its `after_turn` returned.

The evidence a plugin returns is `ObservedEvidence` — the flip it watched and
the numbers it measured. `EvidenceSet` is what `judge` reads, and it is the
host's assembly: `EvidenceSet::from_observed(observed, tamper)` merges in the
host's own artifact-identity finding, which no plugin can observe and therefore
may not report (#3499, `doc:pipeline-as-plugins` §4 A10).

```text
judge(rule: &VerdictRule, evidence: &EvidenceSet) -> Verdict
again(verdict: &Verdict, round: &RoundState, grant: &LoopGrant) -> Continuation
```

Both synchronous, both total, both over owned data. A plugin cannot implement
either one, in Rust or in Python, which is why "a verification plugin quietly
calls a model to decide done" stays impossible by construction rather than by
policy.

`VerdictRule` is assembled from what the manifest already declares:
`[requirements]`, the `[oracle]` flip and tamper policy, and the closed
condition grammar over published signals
(`crates/stella-plugin/src/wrapper.rs`). Data has no programming language, so a
Python author and a Rust author write the identical artifact.

The honest cost, stated plainly: a plugin author cannot write a verdict as a
loop. The variation that remains open is what counts as **evidence** and what
**done** means — and both of those are where the interesting variation actually
lives. Where the grammar turns out too narrow to say something real, the answer
is to widen the closed grammar with a named predicate, never to open it into an
expression language: a Turing-complete condition in a manifest is a second
program with no gate on it.

---

## 5. The four points, precisely

### `before_turn` — async, may be remoted

Runs before the loop is asked for a turn, once per declared stage that the
stage program says runs.

**May:** contribute context, narrow scope, name a role intent, publish signals
for later stages to read.

**May not:** run the loop itself, or reach for ambient authority. Every
capability arrives in the request.

**The invariant-7 constraint, and it is required.** Contributed context
rides as a *volatile* message **after** the byte-stable system-prompt prefix,
never inside it. Prompt-cache hits are a feature, and a wrapper that could
inject into the stable prefix would make every installed plugin a cost
regression for every turn. This is the same discipline
`crates/stella-cli/src/agent.rs::build_system_prompt` and
`crates/stella-cli/src/memory.rs` already hold for recalled context.

**A stage receives typed signals, never free text — and the one place that
bites is research questions** (#3539). `published` is a `Vec<PublishedSignal>`
typed by the closed `Signal` vocabulary, whose whole defence is that a stage
cannot invent a fact for a later stage to read. `Signal::Questions` is
therefore a *count*: a research stage learns that two questions were named and
never what they were, so `plugins/stella-research` re-derives search terms from
the goal string, which is strictly weaker than questions produced by a model
call that already read the goal *and* the task class.

That gap is real and it is deliberately not closed yet, because **nothing
produces the questions**. The built-in triage stage that named them
(`stella-pipeline`'s `parse_research_questions`) went with that crate in #3865,
and the shipping door publishes `questions: 0` unconditionally
(`crates/stella-cli/src/wrapper_plugin.rs::pre_turn_signals`). A
`questions: Vec<String>` field on `BeforeTurnRequest` today would be a field
the wire carries, every reference plugin's `BEFORE_TURN_REQUEST_FIELDS` set has
to admit, and the schema publishes — always empty. A wire field that has never
carried a value is a claim about a capability that does not exist.

The shape when a producer arrives is settled, so the next author does not
re-open it: a first-class optional field on `BeforeTurnRequest`
(`#[serde(default, skip_serializing_if = "Vec::is_empty")]`, `PROTOCOL_VERSION`
unchanged), populated by `WrapperDispatch::open_round` from whatever published
the count — **not** a general "stage input" bag, which would reopen exactly
what the closed vocabulary closed. `HostStage::Triage.publishes()` already
includes `Signal::Questions` and `open_round` already carries `published`
forward into each later stage's request, so a composed triage plugin is the
producer this is waiting on.

### `after_turn` — async, may be remoted

Runs once the turn's `Complete` lands.

**May:** gather evidence — run a test, read a diff, author a witness, spend a
declared model role's call and return the parsed assessment as evidence.

**May not:** change the turn that just ran. It receives the outcome; it does
not hold a channel into it. This is #3379's one-directional connection stated
as a socket rule: the pipeline no longer edits the engine's stream, and no
plugin ever gets to.

**The model call belongs here, never to `judge`** (`doc:turn-loop-wrappers`
§9.2). A goal-mode wrapper spends its verifier call in `after_turn` and returns
the assessment as evidence; the spend is then visible on the receipt against a
declared role instead of being described as a `judge`.

### `judge` — synchronous, host-run, total

Evidence in, verdict out. §4. No arm escalates to a model — the same property
`ladder_decision` had (`crates/stella-pipeline/src/verify.rs`, deleted in
#3865), which is why porting it is a re-home rather than a rewrite. The property
is the thing being ported; the code is no longer here to copy, so a plugin
implements it from this contract.

### `again?` — synchronous, host-run, total

Verdict in, continuation out: another turn with a correction, or stop with an
outcome.

**May not fake an ending the engine did not emit.** The engine always finishes
its own turn and always says so; the wrapper's "the whole job is over" is a
different, separately named event, and both appear in the journal.

**Bounded by the host, not by the plugin.** `LoopGrant::max_holds` is the
plugin's *ask*; the host clamps it against its own ceiling. A spent allowance
completes the turn with the unmet requirements reported — never silently
dropped, and never an unbounded loop because a manifest asked for one.

---

## 6. The acceptance test: no host assumed

`doc:pipeline-as-plugins` §0 makes this an acceptance criterion rather than an
aspiration, and it is the criterion most likely to be quietly failed, because
failing it feels like success: every plugin works, and the socket has grown a
dependency on the CLI's process model, so the loop is excellent and embeddable
in exactly one thing.

**The test:** one wrapper plugin, unchanged, runs when driven by

1. `stella-cli`,
2. `stella-serve` over HTTP,
3. a minimal embedded host linking `stella-engine`.

Proven by a test that exercises all three, not by an argument that it should
work.

The design rules that make it passable:

- **No borrowed trait objects in the request or response types.** An
  out-of-process plugin cannot be handed a `&dyn` anything. This is the same
  constraint #3387 answered for `TurnCapabilities` with owned slots, applied
  one layer out.
- **The candidate worktree crosses as a serializable handle, not as a port.**
  `CandidateWorkspacePort` + `CandidateWorkspace` were 19 methods returning
  borrowed trait objects (`crates/stella-pipeline/src/ports/workspace.rs:94-335`,
  as measured before the crate's deletion in #3865).
  The socket takes the minimum serializable subset — create, root path,
  run-test, seal, adopt, remove — and the **host** fences filesystem access.
  Tamper snapshotting stays host-side, which `TamperPolicy::ArtifactIdentity`
  already assumes.
- **No terminal, no git, no cwd in any signature.** A wrapper that only works
  when a TTY or a git workspace is present is not a socket, it is a CLI
  feature.

---

## 6b. The host-call channel — a plugin may ask, never reach

**Added 2026-08-17, correcting a design error in the original socket (#3540).**

§5 defines four points, and every one of them is the *host* asking and the
*plugin* answering. That is one exchange in one direction, and it forecloses an
entire class of plugin: **one that needs something only the host has.**

The gap is not hypothetical and it is not narrow:

- `stella-research` is defined by `doc:pipeline-as-plugins` §3 as replacing
  "research sub-agents, **recall**". Recall is
  `ContextRecallPort::recall(goal) -> Recall { frames, .. }`, fanned out through
  the CGP host over `context.db` and `codegraph.db`. A plugin gets none of it.
- `stella-plan` reads the same frames — `build_planner_prompt` takes them — so
  the second extraction is blocked by the identical gap.
- `doc:turn-loop-wrappers` §9.3 already anticipated the shape and nobody built
  it: *"a wrapper is handed a `ChildTurn` port… it names a role intent; the host
  resolves it, carves the budget, runs the turn and settles once."* That is a
  plugin asking the host to do something, mid-point.

**How the error was made, recorded so it is not repeated.** #3498 offered
exactly this as its option 2 — "a real callback channel… which makes the
transport bidirectional" — and it was declined in favour of the smaller option 1
on the grounds that a callback channel reopened the measured transport decision
in `doc:plugin-transport-spike`. For `run_test` alone that was right. As a
general judgement it was wrong: three separate capabilities need the same shape,
so the question was never "does `run_test` deserve a channel", it was "is the
socket one-directional", and that is an architecture question that was answered
by accident.

### The rule

> **A plugin may ask the host for a capability. It may never reach for one.**

While handling a point, a plugin may emit **host calls** and read their
responses before returning its final point response. The exchange stops being
one request/response and becomes a bounded conversation that *ends* in the point
response.

```text
host  → { "point": "before_turn", "body": { … } }
plugin→ { "call": "recall", "id": 1, "args": { "goal": "…", "limit": 8 } }
host  → { "result": 1, "ok": { "frames": [ … ] } }
plugin→ { "point": "before_turn", "body": { "context": [ … ] } }     ← ends it
```

### What this does not change, and why that part is required

- **No ambient authority.** The plugin does not retrieve; it *asks*, and the
  host performs the retrieval, applies the gate, and returns only what the
  plugin's declared grant permits. This is §0.3 of the plan intact — a wrapper
  is handed its capabilities and never reaches for them. An `ask` is the
  handing, made explicit.
- **`judge` and `again` stay host functions.** A host call is available during
  `before_turn` and `after_turn` only. The two pure functions gain nothing and
  remain synchronous, I/O-free and total, so "a plugin cannot grade its own
  work with a model" survives untouched.
- **The transport decision stands.** `doc:plugin-transport-spike` chose the
  subprocess path on measurement; a framed conversation over the same stdio
  pipes is that path used more than once, not a different one. Nothing about
  the spike's three axes changes.
- **The capability set is closed and declared.** `HostCall` is a closed enum,
  not an RPC surface: a plugin may only make calls its manifest declared, and
  an undeclared call is refused the way an undeclared hook is
  (`LoopGrant::permits_hook` is the precedent). A new capability is a reviewable
  addition to the enum, never a string a plugin invents.

### What it must be bounded by

A conversation can hang where a single exchange could not, so the bounds are
part of the contract rather than an implementation detail:

- the existing per-point timeout covers the **whole** conversation, not each
  turn of it — a plugin cannot buy time by talking;
- a **maximum number of host calls per point**, declared and host-clamped, on
  the `max_holds` precedent — the plugin's number is an ask, never an authority;
- a call whose capability the manifest did not declare is refused with a typed
  error, and the refusal is **reported**, never silent;
- the response to a refused or failed call is delivered to the plugin so it can
  degrade honestly, rather than killing it — the fail-open direction the Stop
  gate already argues for (`user_hooks.rs:55-59`).

### The first three calls

`recall` (the context plane, read-only), `child_turn` (a bounded turn at a
declared role intent — the `ChildTurn` port §9.3 named), and `run_test` (the
candidate's test invocation, which #3498 solved narrowly by putting the plan in
the request; it stays there, and the call is for the re-runs a verification
plugin needs).

**Two of the three are performed; the third is a declared gap.** `recall` and
`child_turn` are served by `stella_runtime::wrapper::HostPlanes` — the latter
through `ChildTurns`, over the host's own `SubAgentDispatcher`, so the budget is
carved by `BudgetGuard::carve`, the child runs behind `ReadOnlyTools`, and every
model call is the host's (#3564). A `child_turn` names a role intent the
manifest declared; the host resolves it to a `ModelCallRole` seat, and **refuses
outright any intent that resolves to the worker's seat** — a plugin may not
spend the model whose work it is judging, which is the independence
`Roster::independence_losses` merely *reports* for an operator's own
configuration. A caller must know rather than discover: the
`verifier` tier binds to no seat by default (a host that wants it says so with
`ChildTurns::with_seat`, because attributing a plugin's call to `verdict` would
put a call on the receipt the pipeline did not make — #2584), and `run_test` is
still `unsupported` from every host in the tree.

**`child_turn` is bounded by its own manifest key, `[loop] max_child_turns`,
clamped against `DEFAULT_HOST_MAX_CHILD_TURNS` (4).** Not `[loop] max_calls`,
which bounds the *point conversation* and is fresh on every dispatch: what this
one bounds is how much of the user's money a plugin may spend, and a bound that
resets per point is no bound at all across N rounds. The two diverge for an
arbiter that holds rounds open and asks once in each — `plugins/stella-goal` is
that shape, and while the two shared a key its honest `max_calls = 1` capped its
second round's verifier turn at `AllowanceSpent` (#3839). A manifest declaring
only `max_calls` still gets that number: the host clamps an ask down and never
widens one nobody made.

### The fourth and fifth: `candidate_fanout` and `adopt_candidate`

`candidate_fanout` asks for **N isolated writable workspaces, each running one
full worker turn**, and returns per-candidate evidence; `adopt_candidate` lands
one of them and discards the rest. They are one plane
(`stella_runtime::wrapper::CandidateFanouts`, over a `CandidateWorkspaces`
substrate) because the second is fenced against the handle table the first
mints, and a host that could install the fence without the table would be
installing nothing.

They exist because `plugins/stella-candidates` (§3 of
`doc:pipeline-as-plugins`, item 4 of its extraction order) could not be written
at all: every earlier capability is read-only or single-tracked, so the
strongest plugin this socket permitted was *bounded retry with correction over
the shared tree* — each attempt mutating the one real work tree in place, no
isolation, no rollback of a loser. Real, and not best-of-N (#3844).

A caller needs to know, not discover, the following:

- **The seat rule is `child_turn`'s, inverted, and it is not a relaxation.** A
  child turn may not resolve to the worker's seat, because a plugin must not
  grade work with the model that did it. A candidate **is** the work, so it
  must resolve to the worker's seat and nothing else — booking a writing turn
  against `triage` would put spend on the receipt under a responsibility that
  wrote nothing. Both compare the **resolved seat, never the spelling**.
- **The width has its own manifest key and its own ceiling.** `[loop]
  max_fanout_width`, clamped against `DEFAULT_HOST_MAX_FANOUT_WIDTH` (3), and
  deliberately *not* `[loop] max_calls`: that key bounds how chatty a plugin may
  be inside one point, and its unit is a cheap read. This one multiplies model
  spend by N. A second ceiling, `DEFAULT_HOST_MAX_FANOUTS` (2), bounds fan-outs
  for the whole run rather than per point, so the product is what a fan-out
  plugin may cost.
- **The clamp is reported, not silently applied.** The answer carries both
  `requested` and the candidates that actually ran, so a plugin can say it
  scored three of the eight it wanted rather than claiming it wanted three.
- **The budget is carved, then divided.** The plane's requested carve is split
  by the *clamped* width before each candidate asks for its share, so each one
  is a slice of a budget the host agreed to rather than of the plugin's ask.
- **Adoption takes a handle, never a path,** resolved against the table the
  host minted — which is what keeps `HOST_TREE_HANDLE` un-adoptable for free,
  since it names no entry in any table and never has. Adopting empties the
  table, so a second adoption in one run is refused rather than layering a
  second diff over the first. A refused *or failed* adoption discards nothing:
  the losers are the only copies of work that might still be wanted.

**`stella run --pipeline <variant>` installs the plane; every other door
declines it.** The substrate is
`crates/stella-cli/src/candidate_workspaces.rs` — one `git worktree` per
candidate under `.stella/private/candidates/`, one writing worker turn inside
each, and an adoption that is `git diff` in the candidate and `git apply` on
the real tree (#3892). A plugin author should know:

- **Adoption applies a patch; it does not commit, merge or rebase.** An
  adopted candidate leaves the same thing the user's own turn leaves —
  working-tree changes — so best-of-N can never put a commit in someone's
  history that they did not write. `git apply` validates every hunk before
  writing any and is passed no `--reject`, so a candidate that no longer
  applies changes nothing at all and is reported as a failed adoption. There
  is no conflict-resolution path: resolving one is a judgement, and a host
  making it silently would be editing the user's tree on its own authority.
- **`stella goal` installs no fan-out plane**, exactly as it installs no
  `child_turn` plane, and for the same reason: that loop's own even/odd
  worker/verifier receipt slots (#3833) own the low `turn_instance` values
  across a run, so a plugin's fixed slot would collide there rather than
  merely crowd.
- **A candidate turn runs behind the operator's `tools.<name>` switches** and
  the session authorization gate. It is the one dispatched child in the tree
  that does; `delegate`'s children run against the bare registry and always
  have, which is #3930 rather than a property of this capability.

What is still not covered: a process **killed** mid-fan-out leaks its
worktrees, since the sweep that discards them runs at the end of a wrapped run
(#2813 is that shape); and `stella fleet gc` deliberately cannot reclaim them,
because the substrate stays out of the `.stella/worktrees/` + `fleet/`
namespace rather than borrow a sweeper that would then delete checkouts it did
not create.

---

## 7. What this design deliberately does not do

- **It does not let a plugin emit a trace.** Plugins emit journal events in
  the `plugin.<id>.*` namespace; the trace is a fold
  (`crates/stella-cli/src/trace.rs`). Contributed facts then inherit
  replayability, `TRACE_SCHEMA_VERSION` skip-on-unknown, redaction, and the
  guarantee that nothing reaches `store.db`. A plugin writing `traces.jsonl`
  directly routes around all four.
- **It does not give a plugin an `Engine`, a provider, or a credential.** A
  wrapper names a role *intent*; the host resolves it against the user's BYOK
  providers, carves the budget, attaches gate/steering/hooks, runs the turn and
  settles once. For an out-of-process wrapper that is a JSON request on stdio
  and **every model call is made by the host** — invariant 3, intact.
- **It does not admit a second granularity.** Self-driving is an outer loop
  over whole runs, not a turn participant, and it becomes a *host* rather than
  a wrapper (`doc:pipeline-as-plugins` §10). Widening this socket to a second
  granularity for a single caller is the generalisation that is cheaper to add
  later, when a second caller exists to shape it.

---

## 8. How you would know it worked

- A wrapper plugin written in Python, with no SDK beyond the standard library
  and a JSON parser, participates in a real turn and its verdict is decided by
  the host from its declared rule.
- The same plugin runs under all three drivers of §6 with no change.
- `judge` remains synchronous and I/O-free, and no configuration restores a
  model verdict.
- A manifest that names a signal the host does not publish still fails at
  **load**, with a reason — the property `crates/stella-plugin` already
  enforces, now enforced against a socket that actually dispatches.
