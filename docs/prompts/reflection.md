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
| Output cap | 2,048 |
| Temperature | 0.0 |
| Effort | pinned low, like every bounded management call |
| Override | none |

## Wire shape

```
[ system("You are a self-reflection module. Respond with only a JSON object.")
  user(prompt) ]
```

## User message (template)

The prompt opens with a **task frame** chosen by the turn's outcome, then the
shared body.

Success frame:

```text
This turn SUCCEEDED.
What SURPRISED you? Record only what you could NOT have predicted by reading the code — something that contradicted a reasonable assumption, cost you a wrong attempt, or that you only know because you ran it and watched what happened. If nothing surprised you, return an empty list. Most successful turns teach nothing worth keeping, and saying so is the correct answer.
```

Failure frame:

```text
This turn FAILED.
What did the code expect that reading it did not tell you? A failure is the cheapest evidence there is that something was not discoverable by inspection — a helper that looks usable and is not, an ordering that matters and is not stated, a check that fires from somewhere unobvious. Record that, as a flat statement of fact.
If the failure was your own carelessness on something the code stated plainly, there is no lesson: return an empty list.
```

Full body:

```text
Review this coding-agent turn transcript and reflect on the agent's performance. {task_frame}

Respond with ONLY a JSON object:
{"lessons": [{"lesson": "...", "kind": "domain", "domains": ["..."]}], "self_review": {"delivered": true, "rating": 7, "went_well": "...", "to_improve": "...", "critique": "..."}}
`lessons` holds at most 3, most useful first. `kind` is "domain" for a fact about the codebase that holds independent of this turn, or "process" for a note about how you worked. Prefer domain.
THE TEST, applied to every candidate before you write it: could a competent engineer find this in under a minute by reading the code or grepping? If YES, DISCARD IT. It is cheaper to look up than to carry, and every remembered fact costs room in a future prompt.
So do NOT record: where files live, what a module is called, a function's signature, the directory layout, which helper exists, or anything a README or a type definition already states. These are the most tempting lessons and the most worthless.
DO record what inspection cannot reveal: a helper that looks correct and is subtly wrong, an ordering that matters but is not written down, a step that silently does nothing if skipped, a check that fires from somewhere unrelated, a stated rule that the code does not actually follow, or an explicit preference the user expressed.
Good: "util/amounts.to_cents parses through float and loses a cent on values like 1.15; money.parse_amount is the correct one despite both looking current" — you can only know that by getting it wrong.
Bad: "commands are registered in registry.py" — one grep away, worthless to carry.
A lesson that begins "the agent should" is a process lesson, and if you cannot state something that survives the test, return an empty list rather than padding it.
`self_review` is your account of THIS turn alone and is never a substitute for a lesson — omit it entirely rather than let it crowd out a codebase fact. `delivered` is whether you actually did what was asked, `rating` is 0-10 for this turn's work, `to_improve` is the one thing you would do differently. One sentence per field. This is shown to the user as your own assessment, so do not flatter yourself: a turn that produced no output or left the work unfinished did not deliver.
Allowed domain tags (use only these, or []): {domain_names}

Transcript:
{digest}
```

## Why the self-review is asked for last

`self_review` rides along in this same call rather than costing a second one —
the model has the transcript in front of it either way. But it is deliberately
**last**, and named as explicitly not a substitute for a lesson.

This prompt already lost one fight against self-commentary: asking about the
*agent* produced eight process notes and zero codebase facts. A self-review
field is exactly the kind of invitation that can re-open that, so the lesson
instruction keeps the front of the prompt and its "prefer domain" rule intact,
and the self-review is fenced off as being about **this turn only** — the one
place a note about the agent genuinely belongs, because it is stored against
this execution and never recalled as a lesson.

## Why the cap is 2,048

512 was enough for a model that answers with bare JSON and nothing else. A
model that narrates first spends the whole allowance on prose and is cut off
before it reaches the array — so **every lesson from every turn is lost,
silently**, because a truncated response parses to zero lessons exactly like an
empty one. The array itself is at most three short objects; the extra headroom
is only ever spent by models that were going to be cut off.

## Partial responses are kept

`SelfReviewJson` marks every field `#[serde(default)]`. A model that offers
only `to_improve` has said the one thing the "what to improve" panel exists to
show, and rejecting the whole object over a missing `rating` would discard it.

## Where the output goes

Lessons are parsed by `parse_lessons_checked` against the workspace's allowed
domain tags, then written to `.stella/memories/*.md` via the `save_memory`
path — where they join the **next** session's byte-stable prompt prefix, never
this one. The per-turn mining log is `.stella/private/reflections.jsonl`.

## Related

- [worker.md](worker.md) — whose transcript is mined, and whose next-session
  prefix the lessons join
- [domain-inference.md](domain-inference.md) — produces the allowed tag list
