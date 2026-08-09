---
id: prompt-triage
title: "triage — the effective prompt"
status: living
---

# `triage`

Prompt classification and tier routing. The first model call of a `stella run`
turn, and the one that decides how much orchestration the rest of the turn
gets: whether this is even a task, whether a witness is warranted, whether a
verifier should be spent, and which questions a research fan-out should answer
before the planner names files.

| | |
|---|---|
| Call role | `ModelCallRole::Triage` (`"triage"`) |
| Router tier | `Role::Triage` |
| Dispatch | raw completion, `tools: []` |
| Instructions | `TRIAGE_INSTRUCTIONS`, `crates/stella-pipeline/src/triage.rs` |
| Payload | `triage_prompt`, same file |
| Sent from | `Pipeline::triage_stage`, `crates/stella-pipeline/src/pipeline/triage_stage.rs` |
| Retry policy | `RetryPolicy::deterministic()` |
| Timeout | `config.triage_latency_ceiling` (default 10s) |
| Output cap | 512 visible + 4,096 reasoning headroom |
| Effort | `Low`, pinned |
| Override | `agents.triage.prompt` — **wired**, prepended as a system message |

## Wire shape

```
[ system(agents.triage.prompt)?   ← config.role_overrides.triage
  system(TRIAGE_INSTRUCTIONS)     ← &'static str, byte-identical every call
  user(payload) ]
```

## System message (verbatim)

```text
Classify the following user message, and decide what assurance its result actually warrants. Do NOT assume it is a software task — it may just be conversation. Answer with EXACTLY these three lines — plus, for `multi` only, an optional fourth — and nothing else:
CLASS: chat|lookup|single|multi
WITNESS: yes|no
VERIFIER: yes|no
RESEARCH: <question> | <question>

RESEARCH (optional, `multi` only): up to 4 self-contained questions about THIS workspace whose answers a planner would need before naming concrete files — e.g. `Which module owns retry policy and where are its tests?`. Each must be answerable by reading files. One line, questions separated by `|`. Omit the line (or write `RESEARCH: none`) when the goal already names its files or the work is simple.

CLASS is what the message needs:
- `chat`    — asks for NOTHING to be done: a greeting (`hi`), thanks, small talk, or a question about you. Reply conversationally; touch no files, write no plan, no test.
- `lookup`  — a read/explain/search/explore question about the workspace that changes no files
- `single`  — one concrete change
- `multi`   — genuinely multi-step work spanning several changes

The workspace is not always code. Exploring, organizing, renaming, sorting, cleaning up or summarizing ANY files — documents, notes, data, photos — is real work: classify it `lookup`, `single` or `multi` like any other task. Use `chat` ONLY when the message asks you to do nothing at all. If it asks you to look at, decide about, or change anything, it is never `chat`.

A truncated listing of the workspace's files may precede the task. It is evidence, not the ask: use it to judge what kind of workspace this is, how far the task could reach, and whether a test could even run here — a workspace with no test files or build manifest usually warrants WITNESS: no. Classify the message, never the listing.

WITNESS is whether a failing test should be written first to pin the intended behavior. Say no when the change is mechanical, when correctness is already obvious from the diff, or when the project has no way to run such a test. Always say no when the ask is to DELETE something — a witness must fail on the old code and pass on the new, and a removal leaves nothing to write that against. Prefer `no` on small, self-evident work — ceremony that proves nothing costs the user time and money.
VERIFIER is whether a separate model should review the result. It is only consulted when no test settled the outcome, so say no ONLY when a test will already prove the change or the result is trivially checkable from its diff; when unsure, say yes.
```

## User message (template)

Two shapes, chosen by whether the `RepoStructurePort` produced anything:

```text
Workspace listing (truncated):
{structure}

Task:
{goal}

Answer:
```

```text
Task:
{goal}

Answer:
```

An empty structure renders the second form **byte-for-byte identical to the
pre-listing payload** — the listing is evidence when it exists, never a
requirement, so a workspace with no git and no port available is not penalised
with an empty heading.

`{structure}` is clamped to `TRIAGE_STRUCTURE_CHARS` (4,000 **chars**, not
bytes, so multi-byte paths are not over-charged), head-kept, with an explicit
marker appended when anything was dropped:

```text
[… listing truncated at the triage budget …]
```

A truncated listing must never read as the whole workspace, which is why the
marker is unconditional on truncation rather than left to the model to infer.

## Why the listing is here at all

Triage decides the highest-leverage questions in the pipeline, and until the
listing section existed it was the only stage deciding anything from the goal
string alone. `WITNESS: no` was guessed with no way to see whether the
workspace even *has* a test harness, and the chat-versus-task call had no
evidence of what the workspace is.

It rides in the volatile payload half deliberately: the listing changes per
workspace, so putting it in the instruction block would destroy the
byte-stability the split exists for.

## Degradation

Triage never fails a run. Three separate soft paths:

- **Provider unresolvable** — falls through to the deterministic floor,
  `resolve_task_class(None, goal)`. The conversational route is still resolved
  deterministically via `resolve_conversational(false, goal)`, because a bare
  greeting must reach the chat path even when no triage provider exists.
- **Timeout or provider error** — same floor. The abandoned in-flight call is
  not silent in accounting: it records a content-free `UsageIncomplete`
  envelope, since its provider-side spend is unknowable once the response
  never lands.
- **Unparseable response** — same floor again; a real assessment keeps its own
  assurance flags, a missing one does not invent them.

An empty response with `finish_reason: length` is the starvation signature and
is retried once at 32,768 tokens with a warning. Before that retry existed, an
empty triage silently collapsed the research, plan, scope and witness stages to
defaults that read like decisions.

## Related

- [research.md](research.md) — consumes the `RESEARCH:` line
- [plan.md](plan.md) — consumes `CLASS`
- [witness-author.md](witness-author.md) — consumes `WITNESS`
- [verdict.md](verdict.md) — consumes `VERIFIER`
- [worker.md](worker.md) — `CLASS: chat` routes to the conversational reply
