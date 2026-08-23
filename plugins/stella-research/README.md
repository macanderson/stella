# stella-research — the pipeline's research stage, as a plugin

Track B's first extraction (`doc:pipeline-as-plugins` §7, #3380). The plan puts
this stage first and says why in one line: *"before_turn only, read-only, no
worktree. The safest possible first real plugin."*

It answers one wrapper socket point, at two stages of a turn that has not run
yet.

- At **`research`** it reads the candidate workspace the request grants it and
  finds which files actually mention the terms the goal names.
- At **`recall`** it reads nothing at all. It *asks* the host for the context
  plane over the host-call channel (`doc:wrapper-socket` §6b) and contributes
  the frames that come back.

Both ride as volatile context. It runs no loop, spawns no worktree, makes no
model call, decides nothing, and reaches for nothing: every capability is
either handed to it in the request or asked for under the grant its manifest
declares.

```
{"point":"before_turn","body":{…BeforeTurnRequest}}     → stdin
{"call":"recall","id":1,"args":{"goal":…,"limit":8}}    ← stdout   (at `recall`)
{"result":1,"ok":{"frames":[…]}}                        → stdin
{"point":"before_turn","body":{…BeforeTurnResponse}}    ← stdout   ends it
```

Python 3, standard library only — `json`, `os`, `re`, `sys`. No SDK, by rule
(`doc:pipeline-as-plugins` §9 rule 3: *"if a plugin cannot be written without an
SDK, the protocol is too complicated"*). `main.py` is the whole program and the
whole protocol.

## What it replaces, and what it does not

The built-in stage — deleted with `stella-pipeline` in #3865 — was two files:

| Built-in | What it does |
| --- | --- |
| `crates/stella-pipeline/src/research.rs` | the pure half: the finding type, the sub-agent's byte-stable system prompt, the char budget |
| `crates/stella-pipeline/src/pipeline/research_stage.rs` | the I/O half: triage's questions fanned out as parallel **read-only model sub-agents**, budget-carved and latency-capped, findings folded into the planner prompt and the worker's user message |

**This plugin cannot do that, and the difference is the point of the honest
part of this README.** A plugin gets no engine, no provider and no credential
(`doc:wrapper-socket` §7). The host-call channel carries a `child_turn`
capability — the shape that lets a plugin *ask* for one bounded turn at a
declared role intent — and `stella run --pipeline <variant>` now performs it,
over the session's own sub-agent dispatcher (#3576). **This plugin still
cannot use it**: it declares neither the capability nor a `[roles]` entry to
name, so every ask it could make would be refused as undeclared (#3541). It also never receives the questions triage named — the
request carries `Signal::Questions`, a *count* (#3539).

So it does the half that is checkable without a model: a deterministic literal
scan of the granted workspace, reported as file-and-line citations that exist.
Weaker input than a sub-agent's answer, stronger provenance than one. Which
wins on task outcome is an empirical question and it is #3544, not an argument
to have here — which is why **the built-in stage stays** and the plugin is
opt-in behind `--pipeline research-v1` (#3381).

## Recall: it asks, it does not reach

Recall is the other half of §3's stella-research row, and it is the half a
filesystem root cannot serve: the context plane is materialized memories,
episodes, facts and code-graph symbols, behind `.stella/private/context.db` and
`codegraph.db`. This plugin declined it outright until the socket grew the
host-call channel, because there was nothing on the wire it could use (#3540).

Now it asks. `[loop] calls = ["recall"]` is the grant a human reads at install,
`LoopGrant::permits_call` is the filter the host applies, and the host performs
the retrieval — the plugin never touches a database, a path or a query. One
call per point, which is what `max_calls = 1` declares and what it spends.

What comes back is rendered as a single `recall` contribution in the format
`stella_core::receipts::render_recall_line` writes, under the
`[auto-recalled context]` marker `receipts::user_block_kind` reads. That
mirroring is deliberate: without the marker a receipt files these frames as the
*person's* words, which is the misattribution #3243 D4 removed from the
built-in path. It does not mirror the `[id]` half of that line — a `RecallFrame`
carries no record id, and minting one from a label would fabricate the join key
the write→citation loop trusts.

**Every way the ask can fail leaves the prompt exactly as it was.** The host
offers no channel (`unavailable`), the manifest declares no such call
(`undeclared`), this host does not implement it (`unsupported`), the allowance
is spent (`allowance-spent`), the plane failed (`failed`), nothing was relevant,
or nobody answered — each one contributes nothing and says why on stderr. A
fabricated recall would be the one unrecoverable failure here, so the vectors
grade the empty answers as hard as the full one.

## What it contributes

At most five kinds of context, each its own labelled `VolatileContext`:

| Label | Stage | What it says |
| --- | --- | --- |
| `recall` | `recall` | the frames the host recalled for this goal, cited and attributed |
| `research:workspace` | `research` | the granted root's top-level entries, and what is never scanned |
| `research:<term>` | `research` | every file and line matching one term the goal named, capped and cited |
| `research:unmatched` | `research` | the terms nothing matched — said plainly, which a scan can claim with more authority than a model can |
| `research:scan-bounded` | `research` | that the file cap bound, so absence above is not read as proof of absence |

Every one rides as a **user** message after the byte-stable system prefix.
That is invariant 7 and it is not this plugin's discipline to keep: the only
exit from a `VolatileContext` is `into_message`, which builds a user message,
and the dispatcher spends it there
(`crates/stella-runtime/src/wrapper/dispatch.rs`).

The scan's bounds are structural rather than a character budget: at most 6
terms, 5 matches per term, 200 characters per line, 40 orientation entries,
2000 files opened. Their product is inside the built-in's own 12,000-character
per-turn budget (`RESEARCH_PROMPT_BUDGET_CHARS`), so the stage cannot displace
the work it grounds — and every cap that binds says so in the contribution,
because a bounded finding that reads as the whole story is worse than no
finding.

Recall is bounded differently and deliberately so: the plugin asks for 8 frames
and the **host** clamps that against its own ceiling, because the host is the
one that performed the retrieval and knows the budget. The plugin's only bound
is how many it will render, and it names the remainder if a host ever answers
with more.

## Installing it

```bash
stella plugin install ./plugins/stella-research
```

The consent prompt shows the manifest's declarations: the grade (`steering`),
the point (`before_turn`), the host call it may make (`recall`, at most one per
point), the argv, the environment allowlist (`PATH`, and that is the whole
list), and the two `low`-risk capabilities it asks for. It asks for nothing
that could hold a turn open — no `Stop` hook, no `[requirements]`, no
`[oracle]`.

## Testing it

Three harnesses, all run by `cargo test --workspace` (the gate's `test` step
and `ci.yml`'s required job), so a change to the wire contract fails the PR
that made it:

| Harness | What it grades |
| --- | --- |
| `crates/stella-runtime/tests/research_plugin_conformance.rs` | the vectors in `testdata/`, through the host's own `SubprocessWrapper`, against goldens decoded by the real `stella_plugin::wire` types |
| `crates/stella-runtime/tests/research_plugin_recall.rs` | the vectors in `testdata/hostcall/`: a whole §6b conversation, with the plugin's call decoded as a `HostCallRequest` and the answer encoded from a `HostCallResponse` |
| `crates/stella-runtime/tests/research_plugin_dispatch.rs` | the whole host sequence: the declared stage program resolves, `before_turn` runs where it says, and the contribution reaches the turn as user messages |

```bash
cargo test -p stella-runtime --test research_plugin_conformance
cargo test -p stella-runtime --test research_plugin_recall
cargo test -p stella-runtime --test research_plugin_dispatch
```

Regenerate the `.expected.json` goldens from the same fixture the assertions
run against, so the fixture has exactly one definition:

```bash
BLESS=1 cargo test -p stella-runtime --test research_plugin_conformance
```

Then **read the diff** — a golden blessed without looking is a changelog, not a
test. Blessing a vector that carries a `.refusal.txt` sibling is refused: a
refusal is graded by the plugin's stderr and exit status, and a second grading
file is exactly what `every_vector_is_graded_by_exactly_one_sibling` forbids.

A vector is a request plus exactly one grading sibling — `.expected.json` for
an answer, `.refusal.txt` for a refusal, never both. The refusal half is not an
afterthought: `BeforeTurnResponse` has no error variant, so a plugin that
cannot answer *fails* (non-zero exit, one line on stderr, nothing on stdout)
and the host runs the turn without it.

A host-call vector adds a third file, `.calls.json`: the conversation its host
holds, as a list of "what I expect to be asked" and "what I answer" — `[]` for
a stage that must ask nothing, `"answer": null` for a host that dies
mid-conversation, and `answer_raw` for the one case the host's own encoder
*cannot* express (a frame carrying a field `RecallFrame` denies). An optional
`.stderr.txt` grades what a degraded call reported, which is how "a refusal is
reported, never silent" stops being a promise.

`${workspace_root}` and `${bulk_root}` in a request are substituted by the
harness with the fixture trees it materializes — no committed vector can carry
an absolute path, and one of the fixtures needs a `target/` directory this
repository's `.gitignore` would drop.

## Known gaps, all tracked

| Gap | Issue |
| --- | --- |
| ~~Its research half contributes nothing under `stella-cli`~~ — closed. #3553 mints a `CandidateGrant` over the shared work tree, so there is a workspace to read; #3547 took the stage's `if = "questions > 0"` off, since no host runs a triage stage and the condition made this the one stage the plugin was never asked to contribute at | #3547 |
| **And recall does not reach one yet either**: `stella-cli` attaches no `HostCallGate`, so the ask has nowhere to go and the stage is reported as a fault rather than served. The plugin degrades exactly as it does for any refusal; what is missing is the host wiring | #3561 |
| It cannot answer triage's questions — the wire carries their count, not their text. Blocked on a producer, not on a design: nothing publishes `Signal::Questions` today (`pre_turn_signals` sends `0`), and the shape the field takes when a triage plugin does is written down in `doc:wrapper-socket` §5 | #3539 |
| It cannot cause a model call, so the sub-agent fan-out did not come with it | #3541 |
| It publishes no signals: `StageName::Research.publishes()` is empty, so there is none it could honestly publish | #3542 |
| Its `[wrapper]` stage order over-declares, because the condition grammar has no conjunction | #3538 |
| Nobody has benchmarked it against the built-in stage | #3544 |
