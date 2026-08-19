# stella-goal — the goal-supervision reference plugin

The `stella-goal` row of `doc:pipeline-as-plugins` §3 ("Replaces: goal mode,
`stella monitor`") and the worked example `doc:turn-loop-wrappers` §9.2 builds
to resolve one contradiction: the wrapper socket's own rule is that `judge`
may never call a model (synchronous, I/O-free, `stella_runtime::wrapper::verdict`),
yet a goal verifier genuinely *is* a model call. §9.2's answer, quoted
verbatim in `plugin.toml`'s header: **"The model call belongs to `after_turn`,
never to `judge`."** This plugin is that answer, built.

**This runs via `stella run --pipeline goal-v1` — never via `stella goal`.**
That is not a naming accident and `stella goal` will refuse to load it: this
plugin declares `participation = "arbiter"`, and `stella goal`'s own
pre-flight rung (`crates/stella-cli/src/wrapper_plugin.rs::
reject_arbiter_wrapper_on_goal`, #3832) refuses every arbiter-grade wrapper on
that door, before the provider is ever built, naming this exact invocation —
`stella run --pipeline goal-v1` — as the remedy. The reason is **one loop, one
arbiter**: `stella goal`'s round loop is *already* this door's completion
arbiter (`stella_core::Engine::assess` decides met/unmet after every round,
whether the working turn came from the raw loop, the classic pipeline, or an
installed wrapper — see `crates/stella-cli/src/agent/goal/goal_wrapped.rs`).
An arbiter-grade wrapper brings its *own* hold loop
(`stella_runtime::wrapper::WrapperDispatch`'s `judge`/`again`), which wants to
run *inside* one already-judged goal round — a second supervisor judging the
same round the first one is already judging. Before #3832 that shape was only
discovered after `WrapperDispatch` had already billed
`1 + DEFAULT_HOST_MAX_HOLDS` worker turns holding the round open; the whole
run was then discarded anyway (`run_goal_wrapped_turn`'s
`DispatchReport::rounds != 1` check). `stella run`'s own door has no such
second arbiter — `WrapperDispatch`'s hold loop is the *only* thing holding a
turn open there — which is exactly why this plugin's designed home is `stella
run --pipeline goal-v1` and not `stella goal --pipeline goal-v1`.
`crates/stella-cli/src/agent/goal/goal_wrapped.rs` (landed for #3695's goal
half in this same branch) is the *other* half of the design: it keeps
`stella_core::Engine::assess` as the one thing that decides met/unmet on
`stella goal`, for steering/observer wrappers, and its own module doc explains
why moving that decision onto `judge`/`again` would itself be a rewrite of
goal's verifier semantics — encoding `GoalVerifierVerdict`'s free-text
feedback into `EvidenceSet`'s flip/measurement vocabulary is a real gap that
slice ruled out of scope. `plugins/stella-goal` is a self-contained
goal-supervision wrapper using nothing but the generic `WrapperDispatch` loop
every arbiter-grade plugin already gets; it never touches `stella goal`'s own
command, `Engine::run_goal`, or `Engine::assess`.

## What it does

Two points, over as many rounds as the declared arbiter grant holds open:

- **`before_turn`**, at the `execute` stage: contributes the goal framing —
  `goal_kickoff_text` from `crates/stella-core/src/goal.rs`, copied
  byte-for-byte — so a worker turn `stella run`'s door starts fresh each round
  sees the same words `stella goal`'s own loop opens with. Every other
  declared stage answers empty.
- **`after_turn`**: asks the host for one bounded turn at the `verifier` role
  intent, with an instruction built from the byte-mirrored
  `VERIFIER_SYSTEM_PROMPT` plus whatever the wire's `TurnOutcome` carries
  about the round that ran (see "What the verifier cannot see" below). Parses
  the strict-JSON `{"met": ..., "reasoning": ..., "feedback": ...}` answer the
  way `crates/stella-core/src/goal.rs::parse_verdict` does — same
  last-`{`-before-final-`}` scan, ported byte-for-byte — and reports the
  result as `ObservedEvidence`: `measurements = {"met": 1}` when the verifier
  said yes, `{"met": 0}` when it said no, and no `"met"` key at all on every
  degradation.

It makes no model call itself, decides nothing, and grades nothing — `judge`
and `again` (`crates/stella-runtime/src/wrapper/verdict.rs`) are host-run,
synchronous functions over `plugin.toml`'s declared `[requirements]`/
`[oracle]`, and this program's only job is to report the one number the
declared check reads.

```
{"point":"before_turn","body":{…BeforeTurnRequest}}                       → stdin   (per declared stage)
{"point":"before_turn","body":{…BeforeTurnResponse}}                      ← stdout  (context, at `execute` only)
                                    …the host runs the turn…
{"point":"after_turn","body":{…AfterTurnRequest}}                         → stdin
{"call":"child_turn","id":1,"args":{"role":"verifier","instruction":…}}   ← stdout
{"result":1,"ok":{"role":"verifier","seat":…,"report":…,"completed":…}}   → stdin
{"point":"after_turn","body":{"evidence":{"flip":"not-attempted","measurements":{"met":…}}}} ← stdout   ends it
```

Python 3, standard library only — `json`, `sys`. No SDK, by rule
(`doc:pipeline-as-plugins` §9 rule 3). `main.py` is the whole program, and
ships as one self-contained file for the same reason `plugins/stella-plan`
and `plugins/stella-research` do: a conformance harness diffs one plugin
against the wire types without following an import graph.

## Why `arbiter`, not `steering`

`steering` is the grade `stella-plan`/`stella-research` ask for, and it is
the lowest rung that may answer a wrapper socket point at all — but only
`arbiter` may hold a completion open past its first turn
(`Participation::Arbiter`'s own doc comment: "the strongest grant";
`again`'s rule 1: "Only an arbiter may hold a completion open"). A
goal-supervision plugin with no weaker way to do its one job — iterate until
an independent verifier says yes — has to ask for the strongest grade. It is
still the *lowest* grade that works for what this plugin does: it asks for no
capability arbiter does not require (`hooks = ["Stop"]` is required alongside
`arbiter`, `ManifestError::ArbiterMustDeclareStop`, even though nothing in
`WrapperDispatch::run`'s `judge`/`again` sequence dispatches a `Stop` hook
directly — a human reading the consent text still learns this plugin binds
the stronger gate).

## `[requirements]`/`[oracle]` as data — the D-1 grammar, not invented syntax

One requirement, decided entirely by one check, using exactly the closed
grammar `crates/stella-plugin/src/evidence.rs` and
`crates/stella-plugin/tests/fixtures/perf-budget.toml` already ship:

```toml
[requirements]
goal-met = "an independent verifier turn assessed the goal as accomplished"

[oracle]
flip = "not-applicable"
measurements = ["met"]

[[oracle.checks]]
requirement = "goal-met"
check = "met >= 1"
```

- `flip = "not-applicable"`: a verifier's judgment is not a fail→pass
  transition a witness test observes, so this oracle's evidence is a number,
  not a flip — the same shape `perf-budget.toml` uses for a benchmark budget.
  With no flip to decide anything, `[oracle].measurements` +
  `[[oracle.checks]]` is the **only** other vocabulary the manifest schema
  has for "done" (`ManifestError::UndecidableRequirement` refuses any other
  shape), so `met >= 1` is not a stylistic choice — it is the one sentence
  this closed grammar can say.
- No `command` line under `[oracle]`: with `[runtime]` declared, the oracle
  is this plugin's own process (`Oracle::command`'s doc comment) — there is
  no separate oracle binary, because the verifier call already happens
  inline in `after_turn`.
- This is **all already wired**, not aspirational: `WrapperDispatch::bind`
  computes `VerdictRule::from_manifest` at bind time and `WrapperDispatch::run`
  passes it into `judge` every round (`crates/stella-runtime/src/wrapper/dispatch.rs`).
  `goal_plugin_dispatch.rs`'s witness exercises the real `judge`/`again` loop
  end to end, holding a round open on an unmet verdict and stopping on a met
  one — nothing about "the declared-verdict-rule plumbing" is a gap for this
  plugin; see "Known gaps" below for what *is*.

## Known gaps, all tracked

Read this before installing — several of these mean the plugin cannot do its
one job on a host that has not made the change named beside it, and that is
stated here rather than discovered by a silent Undecided run.

| Gap | Detail | Issue |
| --- | --- | --- |
| **No shipped host serves the `verifier` role intent** | `ChildTurns::default_seats()` (`crates/stella-runtime/src/wrapper/child_turn.rs`) serves `worker`/`triage`/`research`/`plan` and deliberately not `verifier` — the module's own doc names the reason (attributing a plugin's child turn to `ModelCallRole::Verdict` would put a call on the receipt the pipeline did not make). `stella run`'s own binding (`crates/stella-cli/src/wrapper_plugin.rs::bind_installed`, `ChildTurns::declare(manifest, dispatcher).with_turn_instance(...)`) never calls `.with_seat("verifier", ...)`. **So on `stella run --pipeline goal-v1` as shipped today, every `after_turn` call degrades to `Unavailable`, evidence is always empty, and the loop ends `Undecided` after round 1, every time** — `goal_plugin_dispatch.rs`'s `without_a_bound_verifier_seat_the_loop_ends_undecided_after_one_round` pins exactly this. | #3838 |
| **`[loop] max_calls` is asked to mean two different things** | `HostCallGate` bounds calls per **point conversation** and is fresh on every `before_turn`/`after_turn` dispatch. `ChildTurns` spends the *same manifest number* as a **whole-run** budget that never resets between rounds (`DEFAULT_HOST_MAX_CHILD_TURNS`'s own doc comment). An arbiter plugin that holds N rounds open, spending one verifier call each, needs N child turns for the whole run — declaring `max_calls = 1` (the natural per-point reading, and what `stella-plan` correctly declares since it only ever runs once) silently caps this plugin's *second round's* verifier call to `AllowanceSpent` even though `main.py` only ever asks once per `after_turn`. `plugin.toml` declares `max_calls = 8` (mirroring `GoalConfig::default().max_rounds`) to work around it; the design gap — one field serving two ceilings with different reset semantics — is generic to any arbiter plugin with a per-round host call, not specific to this one. Found and fixed empirically while writing `goal_plugin_dispatch.rs`: the first draft used `max_calls = 1` and round 2 came back `Undecided { MeasurementMissing }` instead of `Met`. | #3839 |
| **The host's default ceilings are lower than goal mode's own defaults** | `stella run`'s door never calls `.with_host_max_holds` (default `DEFAULT_HOST_MAX_HOLDS = 2`) or raises `ChildTurns`' ceiling past `DEFAULT_HOST_MAX_CHILD_TURNS = 4`. `plugin.toml` asks for `max_holds = 7` / `max_calls = 8` (mirroring `GoalConfig::default().max_rounds = 8`) honestly, but a host running this plugin today caps it at 3 rounds (1 + 2 holds), not 8. | #3841 |
| **The verifier's own words never reach the correction** | `ObservedEvidence` (`crates/stella-plugin/src/observed.rs`) has exactly `flip` and `measurements: BTreeMap<String, u64>` — no field for `GoalVerifierVerdict`'s `reasoning`/`feedback` strings. So `again`'s `Correction.guidance` (`crates/stella-runtime/src/wrapper/verdict.rs::correction_text`) is rendered entirely from the static `[requirements]` statement — "an independent verifier turn assessed the goal as accomplished" — never from what the verifier actually wrote. `stella goal`'s own loop does not have this problem: `verifier_feedback_text` carries the verdict's real `feedback` verbatim. This is real fidelity lost against the built-in, not an oversight. | #3840 |
| **The verifier judges from `TurnOutcome`, not the transcript** | `Engine::assess` renders the whole recent conversation, tail-biased, via `render_transcript_tail`. `AfterTurnRequest.turn` carries exactly `completed`, `answer` (final text only), and `tools`/`changed_files` (each `Option`, absent when the host does not report them). `main.py`'s `verifier_instruction` says so honestly in the text it sends rather than pretending to have seen more. | (documented here; not independently filed — same root as the gap below) |
| **`[roles]` requires `[subloop]`** even though this plugin uses none of `[subloop]`'s own bounded-child-turn dispatch mechanism — its child turn is spent entirely over the host-call channel. `stages = ["verify"]` is declared only to satisfy the validator. | shared with `plugins/stella-plan` | #3496 |
| **No `BLESS=1` regeneration path** for either this plugin's goldens or its siblings' — vectors were produced by running the shipped `main.py` and capturing its output. | shared with `plugins/stella-plan`/`plugins/stella-research` | #3548 |
| **`stella fleet` drives a wrapper per worker attempt now** (#3695, fleet half) and applies no arbiter refusal, so `goal-v1` *can* be named there — but a fleet attempt is one turn with no goal of its own beyond the task prompt, which is not the supervision loop this plugin was written for. `stella run --pipeline goal-v1` remains its designed home. | | (documented here; not a gap to close) |
| **Nobody has benchmarked it against goal mode's own loop** | Which shape wins on task outcome is an empirical question this README does not settle. | (none yet — parallel to #3544/#3801-adjacent open questions for the sibling plugins) |

## Installing it

```bash
stella plugin install ./plugins/stella-goal
```

The consent prompt shows the manifest's declarations: the grade (`arbiter`,
the strongest), both points, the `Stop` gate it binds, the one declared
requirement and its non-flip oracle check, the host call it may make
(`child_turn`, at most 8 for the whole run — see "Known gaps" for why that
number is not "8 calls"), the role intent it names (`verifier`, which no
shipped host serves yet), the argv, and the environment allowlist (`PATH`,
and that is the whole list).

## Testing it

Three harnesses, all run by `cargo test --workspace`:

| Harness | What it grades |
| --- | --- |
| `crates/stella-runtime/tests/goal_plugin_conformance.rs` | the vectors in `testdata/`, through the host's own `SubprocessWrapper`, against goldens decoded by the real `stella_plugin::wire` types — every `before_turn` shape, and every malformed/unsupported request either point must refuse |
| `crates/stella-runtime/tests/goal_plugin_hostcall.rs` | the vectors in `testdata/hostcall/`: a whole §6b `child_turn` conversation for `after_turn`, with the plugin's call decoded as a `HostCallRequest` and the answer encoded from a `HostCallResponse` — every degradation named above, plus the met/unmet/unparseable/incomplete cases |
| `crates/stella-runtime/tests/goal_plugin_dispatch.rs` | the whole host sequence against a real `WrapperDispatch`: a real `ChildTurns` plane over a fake `SubAgentDispatcher` proves the host — never the plugin — makes the verifier's model call, that an unmet round 1 holds the turn open for a correction round the host itself renders, that a met round 2 stops the loop, and (the gap made concrete) that a host serving no `verifier` seat ends the run `Undecided` after exactly one round rather than crediting or blaming an assessment that never happened |

```bash
cargo test -p stella-runtime --test goal_plugin_conformance
cargo test -p stella-runtime --test goal_plugin_hostcall
cargo test -p stella-runtime --test goal_plugin_dispatch
```

A vector is a request plus exactly one grading sibling — `.expected.json` for
an answer, `.refusal.txt` for a refusal, never both — exactly as
`plugins/stella-plan`'s and `plugins/stella-research`'s vectors are. A
host-call vector adds `.calls.json` — the conversation its host holds — and
an optional `.stderr.txt` for what a degraded call reported.
