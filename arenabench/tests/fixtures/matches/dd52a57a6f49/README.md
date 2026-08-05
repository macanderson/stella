# Match `dd52a57a6f49` — the witness artifacts for `arenabench watch`

A real Terminal-Bench 2.1 head-to-head run on 2026-08-04: Claude Code vs the
Stella pipeline, both on Fable 5, six tasks each. The match that motivated
issue #1480 — its Claude Code arm was rate-limited from the first minute
(`api_error_status:429`, swallowed and re-raised as `NonZeroAgentExitCodeError`),
and nobody knew until the 110-minute match was over. Run against these
artifacts, `arenabench watch` must report, with the default rules, exactly:

| arm | task | rule |
|---|---|---|
| claude-code-fable-5 | fix-git | zero-token |
| claude-code-fable-5 | nginx-request-logging | zero-token |
| claude-code-fable-5 | openssl-selfsigned-cert | zero-token |
| stella-fable-5-pipeline | large-scale-text-editing | late-verdict |
| stella-fable-5-pipeline | sqlite-with-gcov | premature-complete |

— and nothing else. The seven healthy trials are committed for exactly that
reason: a watcher that cannot stay silent on them is worse than none.

## What was kept, per trial

- `result.json` — **verbatim.** Harbor's verdict, timestamps, and
  `exception_info`.
- `agent/trajectory.json` — **verbatim** (Claude Code arm only). The ATIF
  fallback is the only telemetry a non-Stella arm publishes, and the
  zero-token rule reads its step count.
- `agent/stella-events.jsonl` — **structural events only** (Stella arm).
  The streaming `text`/`reasoning` fragments and tool payloads — the bulk of
  the 2 MB originals, and consumed by nothing under test — were dropped;
  `tool_start` is reduced to `{id, name}`. Every `stage`, `step_usage`,
  `verdict`, `error`, `usage_incomplete`, and `complete` event is
  byte-identical to the original stream, in its original order, which is the
  entirety of what `MetricsReader` and the monitor rules consume.

Everything else in a trial directory (`config.json`, logs, verifier output,
recordings) is read by nothing in the telemetry or monitor path and was not
committed.
