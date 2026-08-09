---
id: prompts
title: "The effective prompts — one document per agent role"
status: living
---

# The effective prompts

**What a Stella role actually sends to a provider**, role by role: the exact
bytes of its instruction block, the template its per-call payload is rendered
from, where in the tree each half is built, and what an operator can override.

There is one document per variant of `ModelCallRole`
(`crates/stella-protocol/src/event/call_role.rs`), which is the workspace's
only complete role vocabulary — `ModelCallRole::ALL` is derived from the
`model_call_roles!` macro, so a variant cannot escape it without failing an
exhaustive `match` at compile time. If a role exists, it has a page here.

| Role | Page | Cite as | Dispatch | Tools | What it decides |
|---|---|---|---|---|---|
| `triage` | [triage.md](triage.md) | `doc:prompt-triage` | raw | none | class, witness, verifier, research questions |
| `research` | [research.md](research.md) | `doc:prompt-research` | engine (sub-agent) | read-only | one pre-plan question about the workspace |
| `plan` | [plan.md](plan.md) | `doc:prompt-plan` | raw | none | the ordered step list |
| `plan_repair` | [plan-repair.md](plan-repair.md) | `doc:prompt-plan-repair` | raw | none | re-emit an unparseable plan as JSON |
| `witness_author` | [witness-author.md](witness-author.md) | `doc:prompt-witness-author` | engine | witness set | the failing test that arms the flip oracle |
| `witness_repair` | [witness-repair.md](witness-repair.md) | `doc:prompt-witness-repair` | engine (same thread) | witness set | rewrite a witness that passed on old code |
| `worker` | [worker.md](worker.md) | `doc:prompt-worker` | engine / raw | full registry | the change itself |
| `distress_guidance` | [distress-guidance.md](distress-guidance.md) | `doc:prompt-distress-guidance` | raw | none | course-correction for a stuck worker |
| `verdict` | [verdict.md](verdict.md) | `doc:prompt-verdict` | raw | none | PASS/FAIL on inconclusive evidence |
| `agent_author` | [agent-author.md](agent-author.md) | `doc:prompt-agent-author` | raw | none | a generated agent definition |
| `skill_author` | [skill-author.md](skill-author.md) | `doc:prompt-skill-author` | raw | none | a generated `SKILL.md` |
| `domain_inference` | [domain-inference.md](domain-inference.md) | `doc:prompt-domain-inference` | raw | none | the workspace's domain taxonomy |
| `reflection` | [reflection.md](reflection.md) | `doc:prompt-reflection` | raw | none | post-turn lessons and self-review |
| `summarization` | [summarization.md](summarization.md) | `doc:prompt-summarization` | raw | none | the summary replacing an overflowed span |

Cite a page by its id, never its path (`docs/README.md § How to cite a
document`) — these files will move before they stop being true.

`unknown` has no page: it is the `serde(default)` for an *absent* `role` field
on a legacy event, never a call anything dispatches.

## The two dispatch shapes

Every role takes one of two paths, and the difference decides what "the
prompt" even means.

**An engine turn** runs the full tool-calling loop in `stella_core::Engine`.
Its wire prompt is a system message, the tool schemas, and a conversation that
grows with each step — so the prompt on step 7 is not the prompt on step 1.
What these pages give for such a role is the *system message* plus the
*opening user message*; everything after is transcript.

**A raw completion** is one shot with `tools: Vec::new()`. Its whole prompt is
knowable up front, so these pages give it verbatim. In the pipeline these all
funnel through one chokepoint, `Pipeline::metered_raw_call`
(`crates/stella-pipeline/src/pipeline/raw_usage.rs`), which is also where
per-role output caps and the starvation retry live.

## The management-prompt split

Every raw *pipeline* role is built as a `ManagementPrompt`
(`crates/stella-pipeline/src/management_prompt.rs`) — the planner was the last
holdout and joined in #2416:

```rust
pub struct ManagementPrompt {
    pub instructions: &'static str,  // byte-identical on every call
    pub payload: String,             // goal, evidence, rendered diff
}
```

It goes on the wire as `[system(instructions), user(payload)]`. The
`&'static str` is load-bearing rather than stylistic — byte-stability across
calls is the entire point, and a static is the strongest structural guarantee
the type system offers. An edit that tried to interpolate something volatile
into the instruction block would have to change the type to do it.

**The split buys stability, not a guaranteed cache hit, and the distinction is
the honest one.** Measured in #1786: no fixed block clears Anthropic's ~1024
token prefix minimum on its own — verdict instructions estimate ~520 tokens,
triage and guidance less, the witness author's ~620. For the raw roles the
cache win is real only when an `agents.<role>.prompt` override pads the prefix
past the minimum. The witness author is the exception: it runs an engine turn,
where the prefix is system prompt *plus* conversation, which crosses the
minimum inside the first tool round-trip.

## The effective prompt, in order

For a raw pipeline role the wire message list is:

```
[ system(agents.<kind>.prompt)   ← only when set AND wired for that role
  system(instructions)           ← ManagementPrompt.instructions
  user(payload) ]                ← ManagementPrompt.payload
```

The override is *prepended*, never a replacement: the built-in block carries
the output contract (`PASS`/`FAIL`, the triage token shape), so replacing it
outright would break the parser downstream.

For the worker's engine turn the system message is assembled by
`assemble_system_prompt` (`crates/stella-cli/src/agent/prompt.rs`), in this
fixed order:

```
base persona            SYSTEM_PROMPT | PIPELINE_SYSTEM_PROMPT | agents.<kind>.prompt
+ project scripts       package-manager scripts found at the workspace root
+ project orientation
+ workspace memories    .stella/memories/*.md, sorted by filename, ≤16,000 chars
+ exploration index     COMPLETED maps only, metadata only, ≤2,000 chars
+ rules section         .stella/rules/*.toml, Cached channel
```

Everything in that list is loaded **once per session** and concatenated
deterministically. That is the byte-stable prefix discipline (architecture
invariant 7, and L-E8): a memory saved mid-session deliberately does *not*
appear until the next session, because hot-injecting it would invalidate the
cached prefix on every save. Turn-relevant recall rides as a volatile message
*after* the prefix, never interleaved into it.

## Where an operator can intervene

`agent_engine_config.agents.<kind>.prompt` in `.stella/settings.json`, where
`<kind>` is one of the four `EngineAgentKind` variants
(`crates/stella-cli/src/settings.rs`): `default`, `worker`, `verifier`,
`triage`.

The mapping to call roles is not one-to-one, and the gaps are real:

| Setting | Reaches | Does not reach |
|---|---|---|
| `agents.default.prompt` | the interactive worker's base persona | anything in the pipeline |
| `agents.worker.prompt` | the pipeline worker's base persona, `plan`, `plan_repair` | the conversational reply — by design, see below |
| `agents.verifier.prompt` | `verdict`, `distress_guidance` | `witness_author`, `witness_repair` — by design, see below |
| `agents.triage.prompt` | `triage` | — |

The witness row is a decision, not a gap. `apply_role_shaping`
(`crates/stella-pipeline/src/pipeline/witness_stage.rs`) carries every *other*
verifier knob — `effort`, `reasoning`, `temperature`, `max_output_tokens`,
`params` — onto the witness engine config (#1785), and excludes `prompt`
because it is a raw-call concern: that role's system message is
`WITNESS_SYSTEM_PROMPT`, whose hard requirements the create boundary enforces
mechanically.

The `worker` row reaches the plan stage (#2416): the planner writes the
worker's work order, so operator prose constraining the worker has to be
visible to the role that names its steps. It is prepended, never substituted —
the built-in block carries the JSON-array contract `parse_plan` reads.

The conversational reply is the exclusion, and it is a decision. That path
exists to swap the engineering persona for `CONVERSATIONAL_SYSTEM_PROMPT` ("no
tools, no code, no plan, no test"); prepending the worker's prose would re-arm
what it just suppressed, and the worker's `effort` would displace the `Low`
that role is pinned to — deliberation bought for a greeting. The non-prompt
tuning it *does* want still arrives via `PipelineConfig::engine`, which is
already built from the worker's own settings.

Six roles have no override door at all: `research`, `agent_author`,
`skill_author`, `domain_inference`, `reflection`, `summarization`.

## Output caps

Two chokepoints declare these, and they are deliberately the same shape.
`management_bounds` (`crates/stella-pipeline/src/pipeline/raw_usage.rs`) covers
the staged pipeline's management roles; `standalone_bounds`
(`crates/stella-cli/src/accounted_call.rs`) covers the four paid one-shot calls
that are not engine turns. Each pins a **visible-output** contract per role and
adds `REASONING_HEADROOM_TOKENS` (4,096) on top, so the numbers below read
against the prompts that justify them rather than encoding a guess at a
thinking budget. Both matches are exhaustive over `ModelCallRole` on purpose: a
new role has to decide its bounds rather than inherit a ceiling by omission.

| Role | Visible output | Effort | Declared by |
|---|---|---|---|
| `triage` | 512 | `Low` (pinned) | `management_bounds` |
| `worker` (conversational only) | 2,048 | `Low` (pinned) | `management_bounds` |
| `verdict`, `distress_guidance` | 1,024 | inherited | `management_bounds` |
| `plan`, `plan_repair` | 4,096 | inherited | `management_bounds` |
| `agent_author`, `skill_author` | 4,096 | inherited | `standalone_bounds` |
| `domain_inference` | 2,048 | inherited | `standalone_bounds` |
| `reflection` | 2,048 | `Low` (pinned) | `standalone_bounds` |
| everything else | engine base | inherited | — |

The two authoring rows inherit effort rather than pinning it low, which is a
decision: triage writes a three-line classification whose value is the routing
choice, while an authoring role's *product* is the written artifact — buying it
less deliberation is a quality change, not a bounds one.

A cap stated by the caller always wins and is deliberately given *no* headroom:
an operator who pinned 512 asked for 512, and quietly serving 4,608 would make
the setting a suggestion. On the standalone side that channel is narrower —
there is no separate override field, so one `max_output_tokens` carries both
meanings, and `None` is how a call site says "no per-call reason for a number,
use my role's contract". Exactly one standalone call site states its own:
`ingest_cmd/extract.rs` sizes its request to the document and doubles it on
truncation, up to 131,072, which no per-role constant could express.

A call that comes back empty with `finish_reason: length` has provably starved
— the whole budget went to reasoning before the first visible token — and is
retried once at `STARVED_RETRY_CAP` (32,768), loudly, at **both** chokepoints.
The one role still outside either is summarization, which pins 1,200 at `Low`
from the engine's own `run_compaction_pass`.

## This document set is a snapshot, not the source

The prompts here are transcribed from the code, and **nothing yet checks that
the transcription stays true — that guard is #2417.** The code is normative:
when a page and the tree disagree, the tree is right and the page is a bug.
Each page names the exact symbol it was rendered from, so the diff is always
checkable by hand, which is a mitigation rather than a guarantee.

The one prompt property that *is* machine-enforced is cross-prompt contract
parity: `crates/stella-cli/src/agent/prompt/parity.rs` derives the shared
contract set from `prompt.rs`'s own source and fails by name if a contract
reaches one static worker prompt but not the other. That guard exists because
a contract landing in `SYSTEM_PROMPT` alone is invisible to `stella run` and
to every bench measurement, which read `PIPELINE_SYSTEM_PROMPT` (#2231).
