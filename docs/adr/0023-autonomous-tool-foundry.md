---
id: adr/0023-autonomous-tool-foundry
title: "ADR 0023: Reconnect the gap detector and let the foundry run autonomously behind standing controls"
status: implemented
---

# ADR 0023: Reconnect the gap detector and let the foundry run autonomously behind standing controls

- Status: accepted
- Date: 2026-08-29
- Decides: `#5433` (reconnect vs. retire), `#5453` (the authoring step's shape)

## Context

`stella_core::tool_foundry::detect_tool_gaps` had no production caller: its
only consumer compiled out of the shipped binary, while the live
`stella tools --adopt/--enable/--foundry` verbs governed a staging directory
nothing fed. `#3629` had retired the authoring connector unused. `#5433` put the
question directly: reconnect the detector to the tools verbs, or delete the
1295-line module.

Separately, the foundry's governance was built around a three-step human
ceremony — stage by hand, `--adopt`, `--enable` — that in practice meant the
self-improvement loop never closed: nothing was ever staged, so nothing was
ever adopted.

## Decision

**Reconnect (Option A), and remove the human ceremony from the default
path.** The pipeline is: detect (end-of-turn hook, beside skill mining) →
auto-author (manifest+script from the ledgered gap) → validate (manifest
re-parse, static script lint, and the capability witness's two executions —
which run network-denied, so they are the sandboxed dry-run) → auto-adopt
(digests recorded in the foundry ledger by the same writer the human verb
uses) → auto-enable → execute through `foundry_gate`, whose per-call
re-digest (`recheck_before_launch`) is unchanged and remains the tamper
check.

What replaces the human is a set of standing controls, each enforced where
it cannot be skipped:

1. **Network denied by default** for every foundry-built tool, at spawn, by
   the OS (`stella-tools`' `netdeny`: macOS `sandbox-exec`, Linux
   `unshare -rn`). A platform with no working mechanism degrades autonomy to
   **draft-only** — files staged, nothing adopted — because a control that
   cannot be enforced is not claimed. Per-tool network access is a settings
   allowlist entry (`foundry.network_allowlist`), a reviewable line rather
   than a ceremony.
2. **Versioned rollback**: every adoption appends the exact manifest and
   script bytes to an append-only version history in the store;
   `stella tools --rollback <name> [--to <version>]` restores and
   re-digests. History is never rewritten.
3. **Telemetry**: every launch writes an invocation row (tool, script
   digest, gap-id lineage, duration, exit, timeout, output bytes).
4. **Circuit breaker**: config-driven auto-disable (default 3 consecutive
   failures, or more than half of a full 10-launch window), recorded as a
   reason the gate and `stella tools --status` surface. A new version —
   re-author or rollback — re-enables.
5. **Kill switch**: `foundry.autonomy = "auto" | "draft-only" | "off"`
   (default `auto`), plus `stella tools --disable <name>` for one tool.
   `stella tools --draft <gap-id>` is the manual escape hatch: the same
   author+validate steps, no adoption.

The detector's thresholds are settings (`[foundry]`, `#2471`), so a workspace
tunes what gets *proposed*; what *executes* is still gated by the ledger,
the re-digest, and the network denial.

## Consequences

- The detector has a live caller, so the self-improvement loop `#830`
  sketched actually closes: repeated shell shapes become adopted,
  version-tracked, telemetered tools without a model call.
- The evolution ledger's Tool row changes from "staging is a hand step" to
  the autonomous mechanism above; its witnesses are the end-to-end
  autonomy test (gap → adopted → executed with a real network attempt
  denied), the breaker trip test, and the rollback round-trip, alongside
  the original unreachable-until-proven gate witness.
- `#5453``'s original human-gated `--draft`-only framing is superseded by
  direct instruction from the repository owner; the draft verb survives as
  the escape hatch and as the degraded mode's output.
- A workspace that wants the old posture writes `autonomy = "draft-only"`
  (adoption stays human) or `"off"` (detection only) — one line, either
  scope.
