---
id: prompt-reflection
title: "reflection — the effective prompt"
status: living
---

# `reflection`

Post-turn self-reflection writing improvement memories. Runs after a turn that
warrants it, mines the transcript for lessons worth keeping, and — in the same
call — produces the self-review the deck shows the user.

| | |
|---|---|
| Call role | `ModelCallRole::Reflection` (`"reflection"`) |
| Dispatch | raw completion, `tools: []` |
| Built in | `crates/stella-cli/src/memory/reflection.rs` |
| Output cap | 4,096 written contract, plus `with_reasoning_headroom` on top |
| Temperature | 0.0 |
| Effort | pinned low, like every bounded management call |
| Lessons per turn | at most `MAX_LESSONS_PER_TURN` (8) |
| Override | none |

## Wire shape

```
[ system("You are a self-reflection module. Respond with only a JSON object.")
  user(prompt) ]
```

## The question it asks

The prompt asks one thing, and everything else in it exists to keep the answer
honest:

> You are about to be handed a task like this one again, with no memory of
> anything that happened here. What do you want to have been told before you
> start?

That is the objective itself rather than a proxy for it, which is the whole
history of this prompt in one sentence. See
[Three failures, one mistake](#three-failures-one-mistake) below.

## User message (template)

The prompt opens with a **task frame** chosen by the turn's outcome, then the
shared body.

Success frame:

```text
This turn SUCCEEDED.
You are about to be handed a task like this one again, in this same repository, with no memory of anything that happened here. What do you want to have been told before you start, so that the next attempt is faster, costs less, and gets more of it right than this one did?
```

Failure frame:

```text
This turn FAILED.
You are about to be handed a task like this one again, in this same repository, with no memory of anything that happened here. A failure is the cheapest evidence there is about what you did not know going in. What do you want to have been told before you start, so that the next attempt gets where this one did not?
```

Full body:

```text
Review this coding-agent turn transcript. {task_frame}

Respond with ONLY a JSON object:
{"lessons": [{"lesson": "...", "trigger": "...", "saves": "...", "kind": "domain", "domains": ["..."]}], "self_review": {"delivered": true, "rating": 7, "went_well": "...", "to_improve": "...", "critique": "..."}}
There is no approved list of topics and no house style. A command, a constraint, an assumption that turned out to be wrong, an ordering, a dead end not worth walking twice, a number, a name, something the user told you they wanted — if you want it, write it down. You are the only one who watched this turn, and nobody has decided in advance what counts.
ONE TEST, applied to every candidate before you write it: WOULD KNOWING THIS HAVE CHANGED WHAT YOU ACTUALLY DID? Not whether it is true, interesting, or hard to find — whether it would have changed an action. A fact you would have looked up in three seconds anyway changes nothing and is worth nothing, however true it is. A fact you COULD have looked up in three seconds and did not, and paid twenty minutes for, is worth everything: cheap to find and cheap to be without are not the same thing, and it is the second one that matters.
`saves` is what knowing it would have bought you HERE, pointing at the moment in this transcript it would have changed — the wrong attempt it prevents, the wait it skips, the wrong answer it stops you shipping. If you cannot name the moment, do not record the lesson: you are guessing at what helps, and guessing is the one thing this is not for.
`trigger` is what has to be true of a future task for this to matter, written so that a future you can tell at a glance whether it applies. A lesson whose trigger you cannot state is one you have not finished learning.
`kind` is "domain" if it will still be true on a DIFFERENT task in this repository, "process" if it only describes how this particular turn went. That is a question about how far it travels, not about what it is about.
Good: "util/amounts.to_cents parses through float and loses a cent on values like 1.15; money.parse_amount is the correct one despite both looking current" — a wrong answer was shipped and then found; nothing short of getting it wrong would have taught it.
Good: "the full suite takes ~20 minutes and the scoped run covering these files takes ~40 seconds" — one grep away, and it still cost three turns of waiting, because nobody greps for what they do not know to ask.
Bad: "commands are registered in registry.py" — you would have grepped that in three seconds. Knowing it in advance changes not one action you took.
Write as many as pass the test and no more. Padding the list with candidates that failed it is how the list stops being read; an empty list is a complete answer when nothing passes, and at most {max} are kept.
`self_review` is your account of THIS turn alone and is never a substitute for a lesson — omit it entirely rather than let it crowd out one. `delivered` is whether you actually did what was asked, `rating` is 0-10 for this turn's work, `to_improve` is the one thing you would do differently. One sentence per field. This is shown to the user as your own assessment, so do not flatter yourself: a turn that produced no output or left the work unfinished did not deliver.
Allowed domain tags (use only these, or []): {domain_names}

Transcript:
{digest}
```

## Three failures, one mistake

Every previous version of this prompt asked for a **proxy** instead of for the
thing itself, and each one got exactly the class of answer its proxy described.

| | The proxy it asked for | What came back |
|---|---|---|
| #768 | "what should change next time to avoid repeating this failure?" | a question about the agent, answered about the agent — eight of ten mined lessons were process self-critique, none a repository convention |
| #944 | "where things live" | 23 memories encoding six facts, every one a single file-read away; "commands are registered in registry.py" held seven times. The proving ground measured their worth as **negative** — hand-delivering those conventions, perfectly worded, did not improve the pass rate and cost steps |
| the repair for #944 | a rediscovery-cost test ("could a competent engineer find this in under a minute? If YES, DISCARD IT"), operationalized as surprise | right about one class, blind to another — see below |

The third is the subtle one. **Surprise measures novelty; what a memory is worth
is savings.** The two agree on most facts and come apart exactly where the money
is: on a fact that is trivial to look up and expensive not to know. "The whole
suite takes twenty minutes; the scoped run takes forty seconds" is one grep into
a Makefile, so the rediscovery test orders it discarded — after it has already
cost three turns of waiting, and while it goes on costing them in every future
session, because nobody greps for what they do not know to ask.

The test is now the counterfactual itself, settled against the only thing that
can settle it: **whether knowing the fact would have changed an action.** That
subsumes the rediscovery rule without inheriting the blind spot — a fact you
would have looked up anyway changes no action and is still discarded — while
admitting the class the rediscovery rule threw away.

## No topics, but a required argument

The prompt no longer tells the model what a lesson may be *about*: the "do NOT
record / DO record" enumerations are gone, and nothing replaced them. Each of
the three failures above was a topic guess made by whoever wrote the prompt, and
the model that just ran the turn is better placed than the prompt's author to
know what the turn cost it.

What is constrained instead is the **shape of the argument**. A lesson arrives
with:

- **`trigger`** — what has to be true of a future task for it to matter. A
  lesson whose trigger the model cannot state is one it has not finished
  learning; requiring the condition prunes those where the transcript is still
  there to check them against. It is also **folded into the memory body recall
  scores on** (#2459), which is the second thing it buys: a trigger is written
  in the register a goal is written in, so it is the half of a lesson that lives
  in the space the retrieval query is asked in. The restatement band still
  compares bodies to bodies — see
  `crates/stella-cli/src/memory/learning/applicability.rs` for the encoding that
  keeps those two facts compatible.
- **`saves`** — what knowing it would have bought in the turn that produced it,
  pointing at the moment it would have changed. This is what makes a lesson
  refutable rather than merely asserted, and it is refutable against the very
  transcript in front of the model.

Freeing the topic without requiring the argument would only move the guess from
the prompt's author to the model. The argument is what makes it checkable.

`kind` survives the topic cull because it is not a topic question: it decides
the recall tier a lesson competes in (`LessonKind::recall_tier`). It used to be
*asked* as subject matter ("a fact about the codebase, or a note about the
agent") and *read* as transfer, and those come apart on the useful cases — "this
repo's integration tests need the fixture server up first" is a note about how
to work and is true on every task here. It is now asked as transfer directly.

## What `{digest}` contains

A **selection** of the turn under a character budget, built by
`crates/stella-cli/src/memory/reflection/digest.rs` — never a window over the
tail. Budget is spent in three tiers:

| Tier | What | Per-message cap |
|---|---|---|
| Pinned | the goal (first user message) and the last 4 messages | 700 chars |
| Friction | every message carrying an errored `ToolResult`, the assistant message that requested it, and anything the event stream flagged as costly or failed | 700 chars |
| Filler | everything else, in transcript order, while budget lasts | 200 chars |

Total budget is 6,000 characters (~1.5k tokens). Anything not admitted is
replaced in place by `… N messages elided …`, and a digest that elided anything
says so in a header, so the model can report that the evidence it needed was
outside the selection rather than reason across a gap it cannot see. `System`
messages are excluded entirely: the prompt prefix is byte-stable by design
(AGENTS.md invariant 7) and identical on every turn.

When the calling surface folded a friction ledger from the turn's `AgentEvent`
stream, the digest opens with a short section naming where the turn spent
itself — costliest steps by wire call-role, slowest tools, every failed tool
call, retries, loop-detector firings. None of that is recoverable from a
transcript at any window size. That ledger is wired on the staged-pipeline
one-shot today; #2483 tracks the other three surfaces.

**Why this replaced a tail window (#2460).** The digest was the last 12 messages
with each `content` truncated to 300 characters. Two things were wrong. The
window was in the wrong place — a turn's expensive part is in the middle, and
the tail of a successful turn is the summary and the sign-off. And a `Tool`
message carries no `content` at all: the engine builds it as
`content: String::new()` with the payload in `tool_results`, so **every tool
result on every surface rendered as the six characters `tool: `**. Reflection
had never seen what a tool returned. Measured on an 82-message tool-heavy turn:
the old digest was 195 characters (~48 tokens); the selection is ~6.2k
characters (~1.55k tokens), taking the whole billed prompt from ~730 to ~2,255
tokens. `memory::reflection::tests::the_billed_prompt_size_is_reported_and_bounded`
prints both numbers under `--nocapture`.

## Why the self-review is asked for last

`self_review` rides along in this same call rather than costing a second one —
the model has the transcript in front of it either way. But it is deliberately
**last**, and named as explicitly not a substitute for a lesson.

This prompt already lost one fight against self-commentary: asking about the
*agent* produced eight process notes and zero codebase facts. A self-review
field is exactly the kind of invitation that can re-open that, so the lesson
instruction keeps the front of the prompt, and the self-review is fenced off as
being about **this turn only** — the one place a note about the agent genuinely
belongs, because it is stored against this execution and never recalled as a
lesson.

The fence matters more now, not less. With no topic list, the pressure that
produced those eight process notes has nothing topical holding it back. What
holds it back instead is `saves`, which a self-critique cannot fill in without
naming a moment, and `kind`, which sends anything that does not travel to a
deferred recall tier rather than into competition with facts that do.

## Why the written contract is 4,096

`max_output_tokens` is **one** number on the wire covering **two** things, and
each was undersized once with the same invisible symptom. The number sent is
`with_reasoning_headroom(LESSONS_OUTPUT_CONTRACT)`: the contract below, plus
`REASONING_HEADROOM_TOKENS` for whatever a reasoning model spends before it
writes anything.

The **contract** is what reflection is asked to write. 512 was enough for a
model that answers with bare JSON and nothing else. A model that narrates first
spends the whole allowance on prose and is cut off before it reaches the array —
so **every lesson from every turn is lost, silently**, because a truncated
response parses to zero lessons exactly like an empty one. That is why 2,048
followed, and why it is no longer the right number: a lesson is now three prose
fields rather than one, and up to `MAX_LESSONS_PER_TURN` of them may be
returned. The extra room is only ever spent by responses that were going to be
truncated, and a truncation here is invisible.

The **headroom** is why the contract is not sent alone. A reasoning model bills
its thinking against the same wire number, so the bare cap came back spent
entirely on reasoning — `finish_reason: length`, empty text, zero lessons — and
froze this workspace's learning plane for nine days with every surface
reporting health (#2174).

The per-turn cap of 8 is a backstop, not a decision. The previous limit of 3 was
below the number of distinct things a busy turn teaches, so on those turns the
cap was what decided — by arithmetic, after the model had already done the work
of finding them. It is not unbounded either: reflection memories ride the recall
channel, where `max_frames` defaults to 5, so every stored lesson competes for
five slots and a turn permitted to file twenty would dilute recall rather than
add to it.

## Partial responses are kept

`SelfReviewJson` marks every field `#[serde(default)]`. A model that offers only
`to_improve` has said the one thing the "what to improve" panel exists to show,
and rejecting the whole object over a missing `rating` would discard it.

## Where the output goes

Lessons are parsed by `parse_lessons_checked` against the workspace's allowed
domain tags, then — for the half `partition_known` judges novel — upserted into
the **context store** as recallable, domain-tagged, anchor-resolved reflection
memories, at the recall tier their `kind` selects. Both halves, novel and
restatement, are appended to the per-turn mining log
`.stella/private/reflections.jsonl` and mirrored into `store.db`'s `reflections`
table, because a re-learned lesson is the recurrence the skill and rule miners
count (#2358).

Reflection does **not** write `.stella/memories/*.md`. That directory is the
byte-stable prompt prefix and its only writer is the `save_memory` tool, called
by the worker in-turn; a lesson mined here reaches a future prompt through
recall, not through the prefix. The distinction is the whole cost model: prefix
memories are paid for on every model call in every session whether relevant or
not, while recalled ones are paid for only when retrieved.

## Known limits

The digest this prompt reasons over is the last 12 messages, each truncated to
300 characters. The expensive part of a turn is usually in its middle, and the
middle is what the tail window drops — so the model is asked what would have
made the turn faster by a witness that cannot see where it was slow. This caps
how good any wording here can be; #2460 tracks selecting the digest from the
event stream instead.

## Related

- [worker.md](worker.md) — whose transcript is mined, and whose `save_memory`
  calls write the prefix this prompt does not
- [domain-inference.md](domain-inference.md) — produces the allowed tag list
