---
id: agent-monitor-protocol
title: "The agent monitor protocol"
status: living
---

# The agent monitor protocol

One line of JSON per detection, from any tool that watches an agent run, to
any process that supervises one.

## Purpose

Every failure mode worth catching in a running agent — an arm that never
authenticated, a confident premature "done", a verdict that arrived too late
to act on, a hung trial burning its allowance — is visible in artifacts the
run is already writing, long before the run ends. What has been missing is a
shared shape for *reporting* it: each watching tool inventing its own output
means each supervising process grows a bespoke parser, and in practice the
supervision never gets built — the failure is diagnosed by hand, hours later
(issue #1480 records a 110-minute match lost exactly this way).

This protocol is that shared shape. It is deliberately **not** specific to
ArenaBench, benchmarks, or Stella: the subject is "an agent run" in the
broadest sense — a benchmark arm, a fleet worker, a CI-driven session, a
pipeline stage — and the emitter is whichever plugin or tool can see that
run's artifacts. ArenaBench's `arenabench watch` is the first emitter; the
self-driving loop and CI are the first consumers. Other plugins are expected to
offer subscriptions by emitting exactly this, and nothing here requires them
to know ArenaBench exists.

## Roles

- An **emitter** observes one run and writes protocol events, one JSON object
  per line, to an append-only byte stream — its stdout, or a file a consumer
  tails. Observation must be read-only with respect to the run: an emitter
  that can perturb what it watches produces numbers nobody can trust.
- A **consumer** subscribes by reading that stream — typically by spawning
  the emitter (`arenabench watch <run> --follow --format jsonl`) and reading
  lines, or by tailing a file the emitter appends to. A consumer acts on
  detections; it never writes to the stream.

There is no handshake, no acknowledgement, and no control channel. A
subscription is "run the emitter and read", which is what lets a shell pipe,
a CI step, a `Monitor`, and the self-driving loop all be consumers without any of
them linking anything.

## Transport and framing

UTF-8, line-delimited JSON: one complete JSON object per `\n`-terminated
line. A consumer reading a live file must tolerate a torn final line (the
emitter may be mid-append) by holding it until the newline arrives. No other
framing exists.

## The envelope, version 1

```json
{"protocol": "agent-monitor", "v": 1, "kind": "detection",
 "ts": "2026-08-04T21:07:12+00:00", "source": "arenabench",
 "run": "dd52a57a6f49", "agent": "claude-code-fable-5", "task": "fix-git",
 "rule": "zero-token", "severity": "critical",
 "evidence": "2 steps, 0 tokens, $0.00 observed — the arm never made a model call",
 "data": {"steps": 2, "failure": "NonZeroAgentExitCodeError"}}
```

Fields on every event:

| field | meaning |
|---|---|
| `protocol` | Always `"agent-monitor"`. What a consumer keys on — never the emitting tool's name. |
| `v` | Envelope version, an integer. Currently `1`. |
| `kind` | `watching`, `detection`, or `end`. |
| `ts` | When the event was *emitted* (ISO 8601). Observation time, not the time the underlying fact occurred — an emitter attaching to an archive emits old facts with new timestamps. |
| `source` | The emitting tool (`"arenabench"`). Attribution and rule namespacing, not routing. |
| `run` | The observed run's identity, opaque to the protocol (a match id, a fleet run id, a session id). |

Fields on a `detection`:

| field | meaning |
|---|---|
| `agent` | Which agent within the run the detection is about. For a single-agent run, the run's one agent. |
| `task` | Optional finer unit within the agent's work (a benchmark task, a fleet attempt). Omitted when the run has no such grain. |
| `rule` | Which rule fired — kebab-case, see the registry below. |
| `severity` | `notice`, `warning`, or `critical` — see below. |
| `evidence` | One human-readable line. Everything it cites numerically must also appear in `data`; a consumer never parses this string. |
| `data` | Structured evidence, rule-specific. |

A `watching` event opens a stream (`rules`: the list armed); an `end` event
closes it (`detections`: counts by severity, `invalidating`: boolean). The
lifecycle events are what make "no detections" and "the watcher died"
distinguishable: a stream that stops without `end` was interrupted, and a
consumer must treat its silence as unknown, not as clean.

## Severity

| severity | contract |
|---|---|
| `notice` | A fact a reader of the run's numbers needs (e.g. totals are a floor, not a measurement). Requires no action. |
| `warning` | The run produced a suspect result on this unit; the number stands but the explanation matters. |
| `critical` | The observed run's result is **invalid and must not be published**. A one-shot emitter exits nonzero when one fired. |

A consumer meeting an unknown severity (from a newer emitter) must treat it
as at least `warning`.

## Exit-code convention for one-shot emitters

An emitter invoked without follow mode scans once and exits: `0` — nothing
invalidating (notices and warnings may have printed); `2` — usage error;
`3` — at least one `critical` detection. CI fails a publish on nonzero
without reading a line.

## The rule registry

A rule named here means the same thing from **every** emitter — that is the
point of a registry: `self-driving` reacts to `zero-token` identically whether
ArenaBench or a fleet watcher emitted it. An emitter adding a rule with
emitter-specific semantics must pick a name not listed here, and should
propose it for this table the moment a second emitter could want it.

| rule | severity | fires when | canonical evidence |
|---|---|---|---|
| `zero-token` | critical | The unit finished with a nonzero step count and zero observed spend (no tokens in or out, nothing billed). The agent ran and never made a model call — a credential/rate-limit failure the agent swallowed, which scores as a loss and inverts the conclusion. Keyed on *observed spend*, never on an exception name, because the exception name is what lied. | steps, the recorded failure name |
| `premature-complete` | warning | The agent itself declared completion, the verifier failed the unit, and the shape was a confident zero (steps or output tokens under a conservative floor). Outcome **plus** shape: a short pass fires nothing. | steps, output tokens, the floors |
| `late-verdict` | warning | The unit's final word was a verification failure — an error no further step followed. The probe caught a false success too late for the agent to act. | the terminal error message |
| `stall` | warning | A running unit has written nothing for longer than the threshold window. Liveness is the freshest write across an allowlist of the unit's incrementally-written artifacts (event stream, harness log, tee'd stdout, session transcripts) — an allowlist, never the whole tree, so the harness's own grading activity cannot read as the agent's pulse. A write-once-at-the-end agent is therefore as observable as a streaming one (#1571). | silence duration, threshold |
| `usage-incomplete` | notice | Model calls whose tokens/cost never reached the unit's totals — every spend figure on it is a floor (issue #1467). | the count |

## Versioning and evolution

`v` bumps only when the meaning of an existing field changes. New fields, new
kinds, new rules, and new severities are additive and do not bump it —
correspondingly, a consumer **must ignore** fields and kinds it does not
recognise. This is the same additive discipline as Stella's serde-first wire
types: the cheapest protocol to keep compatible is one whose consumers were
never allowed to be strict.

## Non-goals

- **Not a control channel.** There is no way to tell an emitter, or the
  observed run, anything. Supervision that wants to *act* (cancel an arm,
  relaunch a seat) does so through the run's own interfaces.
- **Not a metrics feed.** Detections are facts that fire once per unit, not
  gauges. A consumer wanting scoreboard numbers should read the run's
  scoreboard (for ArenaBench, the snapshot API), not accumulate detections.
- **Not Stella's `AgentEvent` stream.** That is one agent's own internal
  telemetry, rich and agent-specific. This protocol is the thin cross-agent
  supervision layer *above* whatever any particular agent writes — an
  emitter's job is precisely to reduce agent-specific artifacts to these
  agent-agnostic events.

## Emitters

| emitter | subscribes to | since |
|---|---|---|
| `arenabench watch <match> --format jsonl [--follow]` | one ArenaBench match: every arm, every trial | #1480 |

Consumers known to act on the stream: the self-driving bench phase (voids an arm
on `zero-token` instead of publishing an inverted scoreboard), CI (fails the
publish step on exit 3).
