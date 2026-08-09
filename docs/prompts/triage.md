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
| Timeout | `config.triage_latency_ceiling` (default 30s — see below) |
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

A truncated listing of the workspace's files may precede the task. It is evidence, not the ask: use it to judge what kind of workspace this is and how far the task could reach. Do NOT read an absent test framework as an absent witness — see WITNESS below. Classify the message, never the listing.

WITNESS is whether a failing check should be written first to pin the intended outcome. It does NOT require a test framework, and a workspace with no test files or build manifest is not a workspace that cannot be checked: a witness is any command that fails before the work and passes after, a POSIX shell is always available to run one, and its exit status is the whole contract. So say yes whenever the goal names an end state something executable can assert — `git merge-base --is-ancestor <commit> master` for a recovery, a config that must parse, a port that must answer, a file that must contain a value. Those tasks need a witness MORE than a library change does, because nothing else in the run can tell whether the end state was reached. Say no when the change is mechanical, when correctness is already obvious from the diff, or when nothing executable could distinguish success from failure. Always say no when the ask is to DELETE something — a witness must fail on the old code and pass on the new, and a removal leaves nothing to write that against. Prefer `no` on small, self-evident work — ceremony that proves nothing costs the user time and money.
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

Triage never fails a run. Four separate soft paths, all landing on the
deterministic keyword floor (`resolve_task_class(None, goal)`), and all four
now **named on the record** — see the next section:

- **Provider unresolvable** — the conversational route is still resolved
  deterministically via `resolve_conversational(false, goal)`, because a bare
  greeting must reach the chat path even when no triage provider exists.
- **Timeout** — the abandoned in-flight call is not silent in accounting: it
  records a content-free `UsageIncomplete` envelope, since its provider-side
  spend is unknowable once the response never lands.
- **Provider error** — kept distinct from a timeout: same outcome, but it
  costs no dead air, and a census that conflated the two could not tell a
  slow ceiling from an outage.
- **Unparseable response** — a real assessment keeps its own assurance flags,
  a missing one does not invent them.

An empty response with `finish_reason: length` is the starvation signature and
is retried once at 32,768 tokens with a warning. Before that retry existed, an
empty triage silently collapsed the research, plan, scope and witness stages to
defaults that read like decisions.

## The floor is reported, not silent (#2414)

Each of the four paths above emits `ProofStep::TriageDegraded { reason }` —
plus the same statement in prose, the both-channels discipline `unproven` and
`unverifiable` use. The record exists because the fallback stopped being rare:
across three Terminal-Bench arm runs, **27 of 34 triage calls burned the full
10,000 ms ceiling and returned nothing**, roughly four and a half minutes of
wall clock purchasing zero bits, with nothing in the summary layer saying so.

Every downstream reader of `CLASS` — whether to plan, whether to author a
witness, whether research questions are asked at all — then looks like a
decision while being a default. A bench conclusion must not be drawable from a
triage that never ran, so the stream carries the fact:

```json
{"type":"proof","kind":"triage_degraded","reason":"the triage call timed out at its 30s ceiling"}
```

Census it the same way the evidence above was gathered:

```bash
jq -rc 'select(.type=="proof" and .kind=="triage_degraded") | .reason' stella-events.jsonl
```

## Why the ceiling is 30s

It was 10s, and that number sat *inside* the distribution it was bounding. The
7 calls that did answer in those runs took **4,684–8,587 ms** — a successful
triage was landing within a couple of seconds of the wall. A ceiling below the
answering distribution does not bound a pathology; it converts slow-but-correct
answers into no answer at all and pays the full ceiling for the privilege.

The never-wedge contract is unchanged: a wedged provider still costs exactly
one bounded wait and the run still proceeds. The honest caveat is that 7
answering samples is a small basis for the new number — which is what the
degradation record above is for, so the next sizing comes from a census rather
than an assumption.

## Related

- [research.md](research.md) — consumes the `RESEARCH:` line
- [plan.md](plan.md) — consumes `CLASS`
- [witness-author.md](witness-author.md) — consumes `WITNESS`
- [verdict.md](verdict.md) — consumes `VERIFIER`
- [worker.md](worker.md) — `CLASS: chat` routes to the conversational reply
