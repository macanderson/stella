# Arm B — Claude Code, the contemporaneous comparator

Harbor's own `claude-code` agent on `glm-5.2` via z.ai's Anthropic-compatible
endpoint (`reasoning_effort: max`, `thinking: enabled`), run back-to-back with
Arm A on the same host in the same session, over the same 89 tasks and the same
unmodified verifier.

```
44/89 = 49.44%   95% CI [39.33%, 59.55%] bootstrap · [38.67%, 60.25%] exact
```

This is **not** the public leaderboard row for Claude Code. It is a fresh run on
this host, on this day, on this account, at a model the leaderboard row did not
use — which is the whole point of running it rather than citing one.

## Two fields here that are not what they look like

`usd_total` and `tool_calls` are filled from Stella's own accounting block, which
this agent does not emit.

* **`usd_total: 63.35` is a sum over `usd_reported_by: 71` of 89 trials**, not
  over all 89. It is not comparable to Arm A's complete total, and the two arms
  bill through different providers besides.
* **`trials_with_zero_tool_calls: 0` with `tool_calls_reported_by: 0` means no
  data was recorded**, not that this agent made no tool calls.

The pass count is the comparable quantity. See [`../README.md`](../README.md) for
what the two arms do and do not license a reader to conclude.
