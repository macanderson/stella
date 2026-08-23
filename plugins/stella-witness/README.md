# stella-witness — the flip oracle, in the open

`AGENTS.md`'s first paragraph defines "verified done, not claimed done" as **a
property of the path that produced the evidence**. Since #3865 deleted the
built-in staged pipeline, the only such path is an installed verification
plugin.

If the only one of those were private, the open project's central claim would
be unfalsifiable by anyone who had not bought something. **This plugin is the
referent.** It is the flip oracle — the thing that decides whether a fail→pass
transition actually happened — installable, readable, and checkable by anyone.

`oxageninc/vera` is the commercial superset: verifier independence across a
roster, tamper hardening past artifact identity, multi-oracle composition, the
durable flip record, org policy. **Verification is open; verification at
organisational scale is paid.** That split was `doc:plugin-completion-plan`
§4.1's recommendation and is the maintainer's decision (#4029).

## What it does

Two points, and each says one thing. No host calls at all.

**`before_turn` — name the artifacts the flip will be judged against.** Paths,
never findings: this plugin does not vouch for its own witness, and the host is
the one that snapshots each path before the turn and compares after it (#3499).
It contributes no context, no role and no scope — a verifier that steered the
turn it is about to judge would be grading its own work.

Two sources, and the second is the reason the point exists at all. The files
the invocation itself names (`pytest tests/test_x.py`) the host already derives.
The files a *runner's convention* implies it does not: `cargo test --test flip`
names `flip`, and the artifact is `tests/flip.rs` by cargo's convention and by
nothing in the invocation. The host is deliberately forbidden from guessing that
(`crates/stella-cli/src/wrapper_candidate.rs`) — a host deriving a witness is a
host guessing at one — so the plugin whose flip it is says it instead. A path
that is not really in the granted tree is not declared; a wrong guess would put
a claim on the wire this plugin cannot stand behind.

Declaring nothing is a real answer and stays one: the watch stays empty, the
finding stays `TamperFinding::NotChecked`, and the round is not credited a flip.

**`after_turn` — report the flip.**

1. Reads the candidate grant: the workspace root, and the `TestPlan` naming the
   invocation and **the baseline the host already observed before the turn**
   (`TestPlan::baseline` — the red half, handed over rather than reconstructed).
2. Feeds the baseline to a `FlipOracle`, then runs the invocation in the
   granted root and feeds that too.
3. If the oracle reaches `flipped`, runs the invocation **a second time** on the
   same tree. A pass that does not reproduce is `unstable`, not a flip (#859).
4. Reports `ObservedEvidence` — a flip and three numbers. Never a verdict.

`judge` is the host's own synchronous function over the rule `plugin.toml`
declares as data. This plugin reports what it saw; Stella decides what that
means.

```
{"point":"before_turn","body":{…BeforeTurnRequest}}                        → stdin
{"point":"before_turn","body":{"witness":["tests/flip.rs"]}}               ← stdout

{"point":"after_turn","body":{…AfterTurnRequest}}                          → stdin
{"point":"after_turn","body":{"evidence":{"flip":…,"measurements":{…}}}}   ← stdout
```

Python 3, standard library only — `json`, `os`, `subprocess`, `sys`, `time`.
No SDK, by rule (`doc:pipeline-as-plugins` §9 rule 3). `main.py` is the whole
program.

## The invariant it exists to hold

> **`flipped` is reachable only by passing through `failing` for the same
> normalized command.**

A pass with no prior failing observation proves nothing, and does not even lock
the command. This is what separates "verified" from "the tests pass": a suite
that was green before the turn and is green after it says nothing about the
turn.

The transition table is ported unchanged from
`crates/stella-pipeline/src/verify.rs` as it stood at the commit before
`a6d3db4f6` — the last commit before the built-in path was deleted:

| state | observation | next |
| --- | --- | --- |
| none | pass (any command) | none — *no evidence* |
| none | fail (command C) | failing, tracked = C |
| any | a different command than tracked | unchanged — *ignored* |
| failing | fail (tracked) | failing |
| failing | pass (tracked) | flipped |
| flipped | pass (tracked) | flipped |
| flipped | fail (tracked) | failing — an honest regression |
| unstable | pass (tracked) | flipped |
| unstable | fail (tracked) | unstable — the pass that *was* seen stays on record |

`unstable` is entered only through the confirmation re-run, never through an
observation.

Commands are compared **normalized** — whitespace collapsed — but never
reordered: a pass on `cargo test -p a` must not be credited to a failure of
`cargo test -p b`.

## Read this before installing

Stella does not run this oracle and does not re-check what comes back. The flip
and every measurement are what this plugin's own process reports it saw. Stella
applies the declared rule to those reported claims and will not credit a
requirement they leave undecided — but it cannot tell an earned result from a
typed one.

That is the architecture rather than an apology (#3511): verification is
delivered by the plugin. `stella plugin install plugins/stella-witness` prints
the whole declaration, including this disclosure, and installs nothing until you
accept it.

## What v1 does not carry

Stated here rather than discovered by a run that quietly did less than you
expected. Each is tracked.

| Absent | Why, and what it would take |
| --- | --- |
| **Witness *authoring*** | This plugin observes a flip and names the artifacts it will judge one against; it does not *write* the failing test that makes one possible. Authoring needs a writing capability, and `child_turn` is contractually read-only (`SubAgentSpec::read_only`), so the only writing turn on the socket is `candidate_fanout`. That is a real design slice, not a missing line. |
| **#867's same-failure rule** | The nucleus refuses a flip credit when the passing run names its tests and none of the baseline's failures are among them — the failing test was deleted or renamed and the suite exits 0 around its absence. It needs `fingerprint::parse_test_results`, a per-runner-dialect parser. Until it is ported, the requirement is **not declared**: a check this plugin could only ever report as satisfied would be a vacuous guarantee. |
| **Tamper hardening past artifact identity** | `tamper` is deliberately not declared. The policy is host-side, has one value, and its finding is not a word a plugin can say — `ObservedEvidence` has no field for it (#3499). This plugin's whole part is naming the artifacts at `before_turn`; the snapshot, the comparison and the finding are the host's. `crates/stella-runtime/tests/host_owned_tamper.rs` is that half. |
| **Verifier independence across a roster** | Vera's, and it needs a roster this plugin does not see. |
| **Output parsing of any kind** | `passed` is `exit_code == 0` and nothing cleverer. Every runner in the closed vocabulary honours its exit status; second-guessing it is the fingerprint work above. |

## What it reports

| Measurement | Meaning |
| --- | --- |
| `test-command-exit-code` | the invocation's exit status, reported not inferred |
| `test-duration-ms` | wall clock, declared and deliberately unread by any check |
| `flip-unstable` | 1 when a flip's confirmation re-run did not pass (#859) |

And `flip`, one of `achieved` / `not-achieved` / `unobservable`.

`unobservable` is deliberately distinct from `not-achieved`: the first means
the oracle never had a command to track (no grant, no test plan, a baseline
that was not red, an invocation that could not be started), the second means it
tracked one and the flip did not happen. Collapsing them would let "we could not
measure" read as "it failed".

When there is nothing to observe, the measurement set is **empty** — never
zeroes. A name absent from `measurements` is *missing*; a name present with a 0
is a claim, and `test-command-exit-code: 0` for a test that never ran would
credit a check on no evidence.

## Testing it

| Harness | What it grades |
| --- | --- |
| `crates/stella-runtime/tests/witness_plugin_conformance.rs` | every vector in `testdata/` through the host's own `SubprocessWrapper`, decoded by the real `stella_plugin::wire` types; the refusals; the manifest's own declarations; and `a_pass_with_no_prior_failure_proves_nothing` — `flip_requires_a_prior_failing_observation`, driven through the real plugin process rather than against a Rust copy of its logic |
