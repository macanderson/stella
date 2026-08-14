---
id: prompt-summarization
title: "summarization — the effective prompt"
status: living
---

# `summarization`

The overflow summarizer that replaces a history span with a summary. The
engine's last line of defense before a terminal context overflow: rather than
fail the turn, it compacts a span of the transcript into something a coding
agent can resume from.

| | |
|---|---|
| Call role | `ModelCallRole::Summarization` (`"summarization"`) |
| Dispatch | raw completion, `tools: []` |
| System prompt | `SUMMARIZE_SYSTEM`, `crates/stella-core/src/summarize.rs` |
| User message | `render_span_for_summary`, same file |
| Sent from | `Engine::summarize_overflow_span`, `crates/stella-core/src/driver/restore.rs` |
| Output cap | 1,200 visible output + `REASONING_HEADROOM_TOKENS` (4,096) |
| Temperature | 0.0 |
| Effort | `Low`, pinned |
| Retry policy | `RetryPolicy::standard()` |
| Timeout | `config.model_timeout` |
| Override | none |

This role lives in `stella-core`, so it is the one summarizer every surface
shares — CLI, pipeline, serve, fleet.

## Wire shape

```
[ system(SUMMARIZE_SYSTEM)
  user(render_span_for_summary(&messages[start..end])) ]
```

## System message (verbatim)

```text
You are compacting an agent work log. Write a dense summary of the work so far that a coding agent can resume from: the goal, key decisions and why, files touched (exact paths) and what changed in each, commands run with outcomes, errors seen and how they were resolved, and anything explicitly left unresolved. Short bullet lines. No preamble — the summary text only.
```

A byte-stable `const` even though the summarizer's own request is tiny:
stability costs nothing and keeps its prefix cacheable across repeated overflow
events in one session.

## User message — the rendered span

The span is flattened, not forwarded. Full messages are exactly what overflowed
in the first place, so the render keeps the *shape* of the work and drops the
bytes:

```text
{role}: {message text, ≤600 bytes}
{role} → {tool_name}({tool input, ≤300 bytes})
  ← ok: {result, ≤300 bytes}
  ← error: {message, ≤300 bytes}
```

`{role}` is one of `system`, `user`, `assistant`, `tool`.

| Cap | Value | Applies to |
|---|---|---|
| `SUMMARY_TEXT_CAP` | 600 | message text |
| `SUMMARY_RESULT_CAP` | 300 | tool inputs and tool results |
| `SUMMARY_RENDER_CAP` | 60,000 | the whole render |

When the whole-render cap trips, the walk stops and appends:

```text
[span truncated]
```

`SUMMARY_RENDER_CAP` is half a typical small-model context, leaving room for
the summarizer's own output.

Caps are in **UTF-8 bytes**, walked back to a char boundary, with `[…]`
appended when cut. Bytes rather than chars because every cap here is a proxy
for request size — the truncation helper is named `cap_chars` for historical
reasons and the boundary walk is what keeps it safe on multi-byte input.

## Starvation

`max_output_tokens` is one number on the wire and a reasoning model bills its
thinking against it, so the cap is the 1,200-token written contract **plus**
`with_reasoning_headroom`'s thinking room (`stella-core`'s `starvation.rs`,
the shared home — #2503). A response that still comes back empty with
`finish_reason: length` has provably starved and is retried once at
`STARVED_RETRY_CAP` (32,768) through the same accounting seam, with both
attempts' spend reported. The starved first attempt does **not** count toward
the give-up latch below — starvation is recoverable by room, which is exactly
what the latch must not treat as a broken summarizer. Only a retry that also
comes back empty records the failure.

## The give-up latch

Each attempt is a completion and its latency, so a summarizer that keeps
failing must not be re-fired on every remaining step. Once it has failed a
threshold number of steps in a row this turn, `health.is_latched()` short-
circuits the pass and returns. The next model call then overflows and surfaces
**one** clear failure instead of one per remaining step.

The token estimate is computed *after* the latch and span-size guards, not
before: the walk is Θ(transcript) precisely on the steps whose transcript is at
its largest.

The timeout is not optional. `None` left a wedged summarizer provider parking
the whole turn indefinitely — the one unbounded await on the step path — and a
trip now lands in the timeout arm and counts toward the latch like any other
failure.

## Receipts

The summarizer takes **reserved seat 1** at its step (`call_seq: 1`).
Compaction runs at most once per step, so it collides with no other call; a
starved retry re-emits at the same seat with identical messages, overwriting
the first attempt's row with the same context.

The receipt matters more here than elsewhere: the summarizer rewrites the
conversation it is called on, so its own input is the only record of what it
was given to compress — the span it replaces is gone from the history
afterwards.

## Related

- [worker.md](worker.md) — whose transcript this compacts
- `crates/stella-core/src/driver.rs` — the overflow detection that fires it
