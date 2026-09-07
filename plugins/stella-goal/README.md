# stella-goal — the goal-supervision reference plugin

The `stella-goal` row of `doc:pipeline-as-plugins` §3 ("Replaces: goal mode,
`stella monitor`") and the worked example `doc:turn-loop-wrappers` §9.2 builds
to resolve one contradiction: the wrapper socket's own rule is that `judge`
may never call a model (synchronous, I/O-free, `stella_runtime::wrapper::verdict`),
yet a goal verifier genuinely *is* a model call. §9.2's answer, quoted
verbatim in `plugin.toml`'s header: **"The model call belongs to `after_turn`,
never to `judge`."** This plugin is that answer, built.

**`stella goal` runs this plugin, and so does `stella run --pipeline
goal-v1`.** The verb resolves `goal-v1` when no `--pipeline` names anything
else, binds it, and hands it the turn (#3911); the two invocations reach the
same program with the same job.

That is a reversal. `stella goal` once refused this plugin outright: it
declares `participation = "arbiter"`, and the door's own pre-flight rung
(`reject_arbiter_wrapper_on_goal`, #3832) rejected every arbiter-grade wrapper
before the provider was built. The reason was **one loop, one arbiter**:
`stella goal` carried a round loop of its own, with
`stella_core::Engine::assess` deciding met/unmet after every round, and an
arbiter-grade wrapper brings a *second* hold loop
(`stella_runtime::wrapper::WrapperDispatch`'s `judge`/`again`) that wanted to
run inside one already-judged round. Before #3832 that shape was only
discovered after `WrapperDispatch` had already billed
`1 + DEFAULT_HOST_MAX_HOLDS` worker turns holding the round open.

The built-in loop is gone from that door, so there is no first arbiter left to
double — and arbiter is the grade the verb now needs, because it is the only
one `again` lets hold a completion open past its first turn. What remains of
the old arrangement lives in `stella-serve`, whose `drive_goal` re-expresses
`Engine::run_goal` over its own cancellable turn driver; nothing in the CLI
reaches it.

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
| ~~**No shipped host serves the `verifier` role intent**~~ **CLOSED** | Core held a table of four role words and this was not one of them, so the ask was refused everywhere. `stella run`'s door bound the word by hand (#3838); #3905 deleted the table and the binding with it. The answer now comes from the grant a person consented to. This manifest declares an `[oracle]`, so the plugin judges the turn, and its child turns are booked at `ModelCallRole::Plugin` with the seat it declared beside them on the child's `sub_agent` bracket — so a receipt says a plugin bought the call and which of its jobs bought it. Attribution only. Nothing branches on `ModelCallRole`, so the seat decides what a call is *called*, never what it may *do*. Witnessed against this manifest, read off disk, by `the_shipped_goal_plugins_verifier_intent_resolves_on_this_hosts_plane` in `crates/stella-cli/src/wrapper_plugin/tests.rs`. | #3838, #3905, #3906 |
| ~~**`[loop] max_calls` is asked to mean two different things**~~ **CLOSED** | `max_calls` bounds calls per **point conversation** and is fresh on every `before_turn`/`after_turn` dispatch; the whole-run `ChildTurns` budget, which never resets between rounds, is now `[loop] max_child_turns` (#3839). This plugin declares the honest pair — `max_calls = 1`, because `main.py` asks the host for one thing once per `after_turn`, and `max_child_turns = 8`, because an arbiter holding `max_holds + 1` rounds open needs one verifier turn in each. Before the split it had to declare `max_calls = 8` to buy them, which made the per-point number answer a question it was not asked. Found empirically while writing `goal_plugin_dispatch.rs`: the first draft used `max_calls = 1` and round 2 came back `Undecided { MeasurementMissing }` instead of `Met`. | #3839 |
| **`stella run`'s door still caps this plugin at three rounds** | The ceilings are the host's, never the manifest's ask (#3841). `stella goal` funds `GoalConfig::default().max_rounds` on both of them — the hold loop's and the child-turn plane's — because that is the number its own built-in loop ran before the verb moved onto this socket (#3911), so `plugin.toml`'s `max_holds` / `max_child_turns` are honoured there. `stella run --pipeline goal-v1` funds neither: it is a one-shot door with no round count of its own to promise, so it leaves the defaults (`DEFAULT_HOST_MAX_HOLDS = 2`, `DEFAULT_HOST_MAX_CHILD_TURNS = 4`) standing and announces the narrowing. Run it through `stella goal` for goal-mode parity. | #3841, #3911 |
| ~~**The verifier's own words never reach the correction**~~ **CLOSED** | `ObservedEvidence` grew one advisory string, `detail` (#3840), and `main.py` sends `GoalVerifierVerdict`'s `feedback` there, falling back to `reasoning` — the mirror of `stella_core::goal::verifier_feedback_text`. It is not evidence and cannot decide anything: it never reaches `EvidenceSet`, which is the closed vocabulary `judge` is total over. The host attaches it to the unmet clauses *after* the verdict, and `correction_text` prints it under the static `[requirements]` statement. A round that *stopped* had no reader at all until `doc:adr/0033-free-text-is-not-evidence` gave `DispatchReport` a `note`, so a met verdict's `reasoning` — the sentence `stella goal` prints on success — reached nobody. Witnessed by `a_round_the_verifier_marks_unmet_holds_open_for_one_correction_round` and `the_verifiers_own_words_survive_a_met_verdict` in `crates/stella-runtime/tests/goal_plugin_dispatch.rs`. | #3840, ADR 0033 |
| **The verifier judges from `TurnOutcome`, not the transcript** | `Engine::assess` renders the whole recent conversation, tail-biased, via `render_transcript_tail`. `AfterTurnRequest.turn` carries exactly `completed`, `answer` (final text only), and `tools`/`changed_files` (each `Option`, absent when the host does not report them). `main.py`'s `verifier_instruction` says so honestly in the text it sends rather than pretending to have seen more. | (documented here; not independently filed — same root as the gap below) |
| ~~**`[roles]` requires `[subloop]`**~~ **CLOSED** | The rule is now "a role intent needs something that could resolve it" — a `[subloop]` **or** a `[wrapper]` (`ManifestError::RolesResolveNowhere`). This plugin declares a `[wrapper]`, so the `[subloop] stages = ["verify"]` it never used is gone from `plugin.toml`; `plugins/stella-plan` and the reference fixture in `crates/stella-runtime/tests/wrapper_socket.rs` shed the same workaround. | #3496 |
| ~~**No `BLESS=1` regeneration path**~~ **CLOSED** | `BLESS=1 cargo test -p stella-runtime --test goal_plugin_conformance` rewrites the goldens from the same fixture the assertions run against, so the fixture has exactly one definition. Shared with `plugins/stella-plan` and `plugins/stella-research` through `crates/stella-runtime/tests/common/mod.rs`. `plugins/stella-witness` is deliberately not on it — its harness normalises the golden before comparing, so what it would write is not what it reads. | #3548 |
| **`stella fleet` drives a wrapper per worker attempt now** (#3695, fleet half) and refuses no grade, so `goal-v1` *can* be named there — but a fleet attempt is one turn with no goal of its own beyond the task prompt, which is not the supervision loop this plugin was written for. `stella goal` is its home. | | (documented here; not a gap to close) |
| **Nobody has benchmarked it against the loop it replaced** | Which shape wins on task outcome is an empirical question this README does not settle, and `stella goal` runs this plugin now — so the comparison has to be made against a build that predates #3911 rather than against a flag. | (none yet — parallel to #3544/#3801-adjacent open questions for the sibling plugins) |

## Installing it

```bash
stella plugin install ./plugins/stella-goal
```

The consent prompt shows the manifest's declarations: the grade (`arbiter`,
the strongest), both points, the `Stop` gate it binds, the one declared
requirement and its non-flip oracle check, the host call it may make
(`child_turn`, at most 8 for the whole run — see "Known gaps" for why that
number is not "8 calls"), the role intent it names (`verifier`, which
`stella run`'s door serves as `Verdict` — see "Known gaps" for how that seat
came to be bound), the argv, and the environment allowlist (`PATH`,
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

Regenerate the `.expected.json` goldens from the same fixture the assertions
run against:

```bash
BLESS=1 cargo test -p stella-runtime --test goal_plugin_conformance
```

Then **read the diff** — a golden blessed without looking is a changelog, not a
test. Blessing a vector that carries a `.refusal.txt` sibling is refused.
