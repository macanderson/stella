# stella-research — the pipeline's research stage, as a plugin

Track B's first extraction (`doc:pipeline-as-plugins` §7, #3380). The plan puts
this stage first and says why in one line: *"before_turn only, read-only, no
worktree. The safest possible first real plugin."*

It answers one wrapper socket point. At the `research` stage of a turn that has
not run yet, it reads the candidate workspace the request grants it, finds
which files actually mention the terms the goal names, and contributes what it
found as volatile context. It runs no loop, spawns no worktree, makes no model
call, decides nothing, and reads nothing the request did not hand it.

```
{"point":"before_turn","body":{…BeforeTurnRequest}}   → stdin
{"point":"before_turn","body":{…BeforeTurnResponse}}  ← stdout
```

Python 3, standard library only — `json`, `os`, `re`, `sys`. No SDK, by rule
(`doc:pipeline-as-plugins` §9 rule 3: *"if a plugin cannot be written without an
SDK, the protocol is too complicated"*). `main.py` is the whole program and the
whole protocol.

## What it replaces, and what it does not

The built-in stage is two files:

| Built-in | What it does |
| --- | --- |
| `crates/stella-pipeline/src/research.rs` | the pure half: the finding type, the sub-agent's byte-stable system prompt, the char budget |
| `crates/stella-pipeline/src/pipeline/research_stage.rs` | the I/O half: triage's questions fanned out as parallel **read-only model sub-agents**, budget-carved and latency-capped, findings folded into the planner prompt and the worker's user message |

**This plugin cannot do that, and the difference is the point of the honest
part of this README.** A plugin gets no engine, no provider and no credential
(`doc:wrapper-socket` §7), and nothing on the wire lets it ask the host for a
model call (#3541). It also never receives the questions triage named — the
request carries `Signal::Questions`, a *count* (#3539).

So it does the half that is checkable without a model: a deterministic literal
scan of the granted workspace, reported as file-and-line citations that exist.
Weaker input than a sub-agent's answer, stronger provenance than one. Which
wins on task outcome is an empirical question and it is #3544, not an argument
to have here — which is why **the built-in stage stays** and the plugin is
opt-in behind `--pipeline research-v1` (#3381).

It also does not do recall, the other half of §3's stella-research row: the
context plane (memories, episodes, the code graph) has no wire representation
at all (#3540). It answers `StageName::Recall` with an empty contribution
rather than pretending, and an empty contribution is byte-for-byte the one a
host that never installed it would have used.

## What it contributes

At most four kinds of context, each its own labelled `VolatileContext`, in this
order:

| Label | What it says |
| --- | --- |
| `research:workspace` | the granted root's top-level entries, and what is never scanned |
| `research:<term>` | every file and line matching one term the goal named, capped and cited |
| `research:unmatched` | the terms nothing matched — said plainly, which a scan can claim with more authority than a model can |
| `research:scan-bounded` | that the file cap bound, so absence above is not read as proof of absence |

Every one rides as a **user** message after the byte-stable system prefix.
That is invariant 7 and it is not this plugin's discipline to keep: the only
exit from a `VolatileContext` is `into_message`, which builds a user message,
and the dispatcher spends it there
(`crates/stella-runtime/src/wrapper/dispatch.rs`).

Bounds are structural rather than a character budget: at most 6 terms, 5
matches per term, 200 characters per line, 40 orientation entries, 2000 files
opened. Their product is inside the built-in's own 12,000-character per-turn
budget (`RESEARCH_PROMPT_BUDGET_CHARS`), so the stage cannot displace the work
it grounds — and every cap that binds says so in the contribution, because a
bounded finding that reads as the whole story is worse than no finding.

## Installing it

```bash
stella plugin install ./plugins/stella-research
```

The consent prompt shows the manifest's declarations: the grade (`steering`),
the point (`before_turn`), the argv, the environment allowlist (`PATH`, and
that is the whole list), and the two `low`-risk capabilities it asks for. It
asks for nothing that could hold a turn open — no `Stop` hook, no
`[requirements]`, no `[oracle]`.

## Testing it

Two harnesses, both run by `cargo test --workspace` (the gate's `test` step and
`ci.yml`'s required job), so a change to the wire contract fails the PR that
made it:

| Harness | What it grades |
| --- | --- |
| `crates/stella-runtime/tests/research_plugin_conformance.rs` | the vectors in `testdata/`, through the host's own `SubprocessWrapper`, against goldens decoded by the real `stella_plugin::wire` types |
| `crates/stella-runtime/tests/research_plugin_dispatch.rs` | the whole host sequence: the declared stage program resolves, `before_turn` runs where it says, and the contribution reaches the turn as user messages |

```bash
cargo test -p stella-runtime --test research_plugin_conformance
cargo test -p stella-runtime --test research_plugin_dispatch
```

A vector is a request plus exactly one grading sibling — `.expected.json` for
an answer, `.refusal.txt` for a refusal, never both. The refusal half is not an
afterthought: `BeforeTurnResponse` has no error variant, so a plugin that
cannot answer *fails* (non-zero exit, one line on stderr, nothing on stdout)
and the host runs the turn without it.

`${workspace_root}` and `${bulk_root}` in a request are substituted by the
harness with the fixture trees it materializes — no committed vector can carry
an absolute path, and one of the fixtures needs a `target/` directory this
repository's `.gitignore` would drop.

## Known gaps, all tracked

| Gap | Issue |
| --- | --- |
| It cannot answer triage's questions — the wire carries their count, not their text | #3539 |
| It cannot cause a model call, so the sub-agent fan-out did not come with it | #3541 |
| Recall has no wire representation | #3540 |
| It publishes no signals: `StageName::Research.publishes()` is empty, so there is none it could honestly publish | #3542 |
| Its `[wrapper]` stage order over-declares, because the condition grammar has no conjunction | #3538 |
| It is spawned once per declared stage and contributes at one of them | #3543 |
| Nobody has benchmarked it against the built-in stage | #3544 |
