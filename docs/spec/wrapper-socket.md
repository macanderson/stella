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

Two consequences worth writing down because they are easy to erode:

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

**The invariant-7 constraint, and it is load-bearing.** Contributed context
rides as a *volatile* message **after** the byte-stable system-prompt prefix,
never inside it. Prompt-cache hits are a feature, and a wrapper that could
inject into the stable prefix would make every installed plugin a cost
regression for every turn. This is the same discipline
`crates/stella-cli/src/agent.rs::build_system_prompt` and
`crates/stella-cli/src/memory.rs` already hold for recalled context.

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
`ladder_decision` already has (`crates/stella-pipeline/src/verify.rs`), which is
why porting it is a re-home rather than a rewrite.

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
  `CandidateWorkspacePort` + `CandidateWorkspace` are 19 methods returning
  borrowed trait objects (`crates/stella-pipeline/src/ports/workspace.rs:94-335`).
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

### What this does not change, and the reasons are the load-bearing part

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
