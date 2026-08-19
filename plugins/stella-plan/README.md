# stella-plan — the pipeline's plan stage, as a plugin

Track B's second extraction (`doc:pipeline-as-plugins` §7, #3380). #3562
recorded why it could not start the day `stella-research` shipped: a planner
*is* a model call, and until the `child_turn` host call reached a real
dispatcher on some driver, a plan plugin could contribute a prompt and
nothing else. That driver is `stella run`'s wrapper-plugin binding
(`crates/stella-cli/src/wrapper_plugin.rs::bind_installed`, #3576's slice for
this door), and this plugin is built against it.

It answers one wrapper socket point, at one stage of a turn that has not run
yet.

- At **`plan`** it asks the host for one bounded turn at the `planner` role
  intent (`doc:wrapper-socket` §6b's `child_turn` call), sends the same fixed
  planner instructions the built-in stage uses, parses the JSON-array-of-steps
  answer the way `crates/stella-pipeline/src/plan.rs::parse_plan` does, and
  contributes the result as one volatile context block.
- Every other declared stage gets an empty contribution — byte-identical to a
  host with no plugin installed.

It makes no model call itself. It runs no loop, spawns no worktree, opens no
file, and reaches for nothing beyond the one host call its manifest declares.

```
{"point":"before_turn","body":{…BeforeTurnRequest}}                    → stdin
{"call":"child_turn","id":1,"args":{"role":"planner","instruction":…}} ← stdout   (at `plan`)
{"result":1,"ok":{"role":"planner","seat":"plan","report":…,"completed":…}} → stdin
{"point":"before_turn","body":{…BeforeTurnResponse}}                   ← stdout   ends it
```

Python 3, standard library only — `json`, `sys`. No SDK, by rule
(`doc:pipeline-as-plugins` §9 rule 3: *"if a plugin cannot be written without
an SDK, the protocol is too complicated"*). `main.py` is the whole program.
It ships as one self-contained file rather than importing
`plugins/stella-research/main.py`'s wire-framing helpers, on purpose — a
conformance harness diffs one plugin against the wire types without following
an import graph, and `stella-research`'s own `main.py` docstring makes the
same call for the same reason.

## What it replaces, and what it does not

The built-in stage is two files:

| Built-in | What it does |
| --- | --- |
| `crates/stella-pipeline/src/plan.rs` | the pure half: `build_planner_prompt`, `parse_plan`, the bounded JSON-repair and absolute-path-repair prompts |
| `crates/stella-pipeline/src/pipeline/plan_stage.rs` | the I/O half: the `Role::Plan` model call, the repair retries, `resolve_plan_paths`, folding the plan into `crates/stella-pipeline/src/pipeline/plan_steps.rs`'s per-step engine turns |

**What this plugin's planner prompt leaves out, and why.**
`build_planner_prompt(goal, recall, research, repo_structure, revision)` reads
four inputs beside the goal, and none of them cross this wire today:

| Input | Why it is missing here |
| --- | --- |
| `recall` | This plugin declares no `recall` call — asking for both `recall` and `child_turn` in one `before_turn` call would spend two host-call ceilings for one stage's contribution, and `plugins/stella-research` is the sibling that already reads recall. Running the two together under one `--pipeline` selection is not possible today (#3801). |
| `research` | `ResearchFinding{question, answer}` has no wire representation. `stella-research`'s own findings ride as prose `VolatileContext`, not a typed value a second plugin could re-read. |
| `repo_structure` | No `BeforeTurnRequest` field carries it, and this plugin declares no `read_file`/`search` capability to approximate it — that would measure a second change alongside the one this extraction is meant to isolate (#3562 item 2). |
| `revision` | The `--revise` flag's note has no wire representation either (#3562 item 3). |

So the prompt this plugin sends is honestly narrower than the built-in's:
`## Goal` and the `## Plan (JSON array of step strings)` header, nothing else.
The fixed instruction block — the output contract `parse_plan` depends on — is
copied byte-for-byte from `PLANNER_INSTRUCTIONS` in `plan.rs`, because a
plugin that told the model something different would be parsing a contract it
did not actually offer.

**What the parsed plan does not become.** `BeforeTurnResponse` carries no
`Vec<PlanStep>` field and no per-step mechanism — its only fields are
`context`, `role`, `scope` and `publish`
(`crates/stella-plugin/src/wire.rs`). The built-in stage's own
`crates/stella-pipeline/src/pipeline/plan_steps.rs` walks the plan as one
engine turn per step, host-executed; this plugin cannot drive that walk, so
the parsed steps ride as **one prose block the worker reads before its own
turn**, weaker than the built-in's execution loop and said so in the text it
contributes (#3562 item 4).

**`scope` is always empty.** It is "workspace-relative paths the wrapper
believes the turn should stay within" — advisory input to the host's own
scoping. This plugin has no candidate grant and reads no workspace, so it has
nothing structural to put there; and the planner's own fixed instructions
forbid guessing a literal path (#2932's lesson), so scanning the plan's prose
for path-looking tokens would reintroduce exactly the failure those
instructions exist to avoid. Zero scope entries is a decision, not an
omission — see `main.py`'s module doc.

Which contribution wins on task outcome is an empirical question this README
does not settle — the built-in stage stays, and the plugin is opt-in behind
`--pipeline plan-v1`.

## `child_turn`: it asks, it never makes the call

`doc:turn-loop-wrappers` §9.3 named the mechanism: a wrapper is handed a role
*intent*, never a provider, an engine or a credential; the host resolves it,
carves the budget, runs the turn and settles once. `[loop] calls =
["child_turn"]` is the grant a human reads at install,
`LoopGrant::permits_call` is the filter the host applies, and `[roles.planner]
tier = "plan"` is the one role intent this plugin ever names — it resolves to
`ModelCallRole::Plan` under the host's `default_seats()`, the same
responsibility the built-in stage's own model call carries.

`[roles]` requires `[subloop]` in the manifest schema
(`ManifestError::RolesRequireSubloop`) even though this plugin uses none of
`[subloop]`'s own bounded-child-turn dispatch mechanism — its child turn is
spent entirely over the host-call channel. `plugin.toml` declares `[subloop]
stages = ["plan"]` only to satisfy that validator; the host never reads it for
this plugin. That coupling is a real, awkward constraint on any plugin
wanting a role intent purely for `child_turn`, tracked at #3496.

**Every way the ask can fail leaves the prompt exactly as it was.** The host
offers no channel (`unavailable`), the manifest declares no such role intent
(`undeclared`), the role resolves to the worker's own seat (`forbidden` — the
independence rule, not expected for `planner`/`plan` but exercised by a
vector regardless, since the code path is shared with every `child_turn`
asker), the allowance is spent (`allowance-spent`), the host tried and the
turn failed (`failed`), or nobody answered — each one contributes nothing and
says why on stderr. A completed turn whose report simply does not parse as a
plan degrades the same way: with `max_calls = 1` this plugin cannot afford the
built-in stage's own bounded JSON-repair retry, so an unparseable answer is
reported and dropped rather than guessed at. The one thing that **refuses**
rather than degrades is an `ok` payload shaped differently than
`ChildTurnResult` — that is the host disagreeing about the contract, not an
ordinary outcome.

## What it contributes

One kind of context, at most, per turn:

| Label | Stage | What it says |
| --- | --- | --- |
| `plan:steps` | `plan` | the ordered steps a real planner-role child turn proposed, as numbered prose, with a note when the turn did not reach a final answer |

It rides as a **user** message after the byte-stable system prefix — invariant
7, enforced by `VolatileContext::into_message`'s type, not by this plugin's
discipline.

## Installing it

```bash
stella plugin install ./plugins/stella-plan
```

The consent prompt shows the manifest's declarations: the grade (`steering`),
the point (`before_turn`), the host call it may make (`child_turn`, at most
one per point), the role intent it names (`planner`, resolving to the `plan`
tier), the argv, and the environment allowlist (`PATH`, and that is the whole
list). It asks for nothing that could hold a turn open — no `Stop` hook, no
`[requirements]`, no `[oracle]`.

## Testing it

Three harnesses, all run by `cargo test --workspace`:

| Harness | What it grades |
| --- | --- |
| `crates/stella-runtime/tests/plan_plugin_conformance.rs` | the vectors in `testdata/`, through the host's own `SubprocessWrapper`, against goldens decoded by the real `stella_plugin::wire` types |
| `crates/stella-runtime/tests/plan_plugin_hostcall.rs` | the vectors in `testdata/hostcall/`: a whole §6b `child_turn` conversation, with the plugin's call decoded as a `HostCallRequest` and the answer encoded from a `HostCallResponse` |
| `crates/stella-runtime/tests/plan_plugin_dispatch.rs` | the whole host sequence: `WrapperDispatch` resolves the declared stage program, a real `ChildTurns` plane over a fake `SubAgentDispatcher` serves the call, and the parsed plan reaches the turn as a user message |

```bash
cargo test -p stella-runtime --test plan_plugin_conformance
cargo test -p stella-runtime --test plan_plugin_hostcall
cargo test -p stella-runtime --test plan_plugin_dispatch
```

A vector is a request plus exactly one grading sibling — `.expected.json` for
an answer, `.refusal.txt` for a refusal, never both, exactly as
`plugins/stella-research`'s vectors are. A host-call vector adds
`.calls.json` — the conversation its host holds — and an optional
`.stderr.txt` for what a degraded call reported. There is no `BLESS=1`
regeneration path for either plugin's goldens yet (#3548); this plugin's
vectors were produced by running the shipped `main.py` itself and capturing
its output, and the same is true of `stella-research`'s.

## Known gaps, all tracked

| Gap | Issue |
| --- | --- |
| No plan plugin can also read recall or research findings under one `--pipeline` selection — `WrapperDispatch::bind` takes exactly one manifest | #3801 |
| No `repo_structure` wire representation, so the planner prompt omits the section the built-in reads | #3562 (item 2) |
| No `--revise` wire representation, so a rejected plan's revision note cannot reach a re-plan | #3562 (item 3) |
| The parsed plan rides as prose, not the typed `Vec<PlanStep>` the built-in's per-step engine-turn walk needs | #3562 (item 4) |
| `[roles]` requires `[subloop]` even for a role intent used only over the host-call channel | #3496 |
| `stella run`'s driver installs the `ChildTurns` plane but does not yet fold its spend back into the receipt or surface it beside `HostCallGate::refusals()` | #3576 (open items) |
| It is spawned once per declared stage and contributes at exactly one of them | #3543 |
| Nobody has benchmarked it against the built-in stage | (none yet — parallel to #3544 for `stella-research`) |
| The goldens have no `BLESS=1` regeneration path | #3548 |
| `plan-v1` runs on every door that takes `--pipeline` now (`stella run`, `stella goal` per round, `stella fleet` per worker attempt, #3695) — but only `stella run`'s door installs a `ChildTurns` plane, so the planner role intent it asks for is answered `Unavailable` on the other two | #3833 (goal), #3882 (fleet) |
