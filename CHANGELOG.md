# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## How this file works

**Every merge to main cuts a release** (see [`RELEASING.md`](RELEASING.md)), so
this file is written by contributors, not by the release job. Add a bullet under
`## [Unreleased]` in the same PR as your change, whenever that change is
something a user would notice — a new flag, a changed default, a fixed bug, a
breaking rename.

On release, `auto-tag.yml` moves whatever is under `## [Unreleased]` beneath a
new version heading and leaves `## [Unreleased]` empty for the next change. It
does not invent entries: a release with nothing under `## [Unreleased]` gets a
version heading with no bullets, which is the honest record of a merge that
changed nothing user-facing.

Internal refactors, test-only changes, and CI work do not need an entry.

Each GitHub Release additionally carries per-release notes generated at publish
time from the commit range. Those are a summary of *commits*; this file is a
record of *changes*, curated by the person who made them.

## [Unreleased]

## [0.6.15] — 2026-07-30

### Fixed

- `stella run` exits when the turn ends. It used to print its terminal event
  and then hang until something killed it, which broke every non-interactive
  use of the primary surface — CI steps, wrapper scripts, and any harness that
  waits on process exit. The turn itself was always fine; the run's event
  channel was held open by sender clones the tool registry never released, so
  the renderer it was waiting on could not finish.
- A `stella run --output-format stream-json` emits exactly one `complete`
  event. The pipeline's own pre-reflection terminal event and the final
  all-calls total both reached the durable JSONL sink, so a consumer that
  stopped at the first terminal event read the wrong cost.

## [0.6.12] — 2026-07-30

## [0.6.10] — 2026-07-30

## [0.6.9] — 2026-07-30

## [0.6.8] — 2026-07-30

### Fixed

- The transcript no longer eats underscores in identifiers. `tool_use_id`
  rendered as "tooluseid" with an italic "use", because the markdown renderer
  accepted `_` as an emphasis delimiter mid-word — so every `snake_case` field
  and API property name in an agent's prose was silently rewritten. `_` now
  only delimits emphasis on a word boundary (CommonMark's intraword rule), and
  `__` is always literal so `__init__` and `__all__` survive too. `_emphasis_`
  and `**bold**` are unchanged.

## [0.6.7] — 2026-07-30

## [0.6.6] — 2026-07-30

## [0.6.5] — 2026-07-29

## [0.6.4] — 2026-07-29

## [0.6.3] — 2026-07-29

## [0.6.2] — 2026-07-29

## [0.6.1] — 2026-07-29

## [0.6.0] — 2026-07-29

## [0.5.77] — 2026-07-29

## [0.5.76] — 2026-07-29

## [0.5.75] — 2026-07-29

## [0.5.74] — 2026-07-29

## [0.5.73] — 2026-07-28

## [0.5.72] — 2026-07-28

## [0.5.71] — 2026-07-28

## [0.5.70] — 2026-07-28

## [0.5.68] — 2026-07-28

## [0.5.67] — 2026-07-28

## [0.5.66] — 2026-07-28

## [0.5.65] — 2026-07-28

## [0.5.64] — 2026-07-28

## [0.5.63] — 2026-07-28

## [0.5.62] — 2026-07-28

## [0.5.61] — 2026-07-27

## [0.5.60] — 2026-07-27

- `--model` now actually pins the model on the pipeline path. A configured
  `pipeline_worker_model` (or `agents.worker.*`) used to be applied on top of
  the flag, so `stella --model z-ai/glm-4.7-flash run "…"` silently executed —
  and billed for — whatever the settings file named instead. The flag now
  outranks those settings for the worker role, and when it suppresses one the
  run says so on stderr rather than dropping it silently. An explicitly
  configured **judge or triage** is unchanged: `--model` says nothing about
  those roles, so cross-family setups keep working.
- The `--output-format json` envelope's `model` key now reports the model that
  **actually ran** the worker turns, not the one that was requested. When those
  differed, the envelope named a model that never ran a turn and never billed a
  cent while `cost_usd` sat right beside it — backwards for the spend
  attribution the key exists to serve. The text cost summary and the
  `stream-json` terminal `Complete` frame carried the same stale value and are
  fixed with it.
- Short work costs less. A greeting no longer pays for a triage model call: the
  conversational route was always resolved deterministically, but the paid
  classification went out first and could not change the answer. `hi` now costs
  one model call instead of two, and can no longer stall behind a wedged triage
  provider.
- A change with nothing to prove no longer buys a review call to be told so.
  Verification now reads the **diff** — docs-only, tests-only, config-only,
  comments-only, or a pure removal completes with a **stated reason** recorded
  on the verdict, rather than escalating because no test flipped. Anything
  mixed or unreadable still buys the test. Removals and test-only changes keep
  their independent reviewer, because deleting the wrong thing is a mistake a
  reader catches and no test would have. Design:
  `docs/design/witness-protocol.md` §7.
- Verification cost is now fully demand-driven: a change with nothing to prove
  no longer pays for a **witness test** either. Authoring runs after execution
  and only when the diff warrants it, so a docs or comment edit dispatches no
  author call at all — previously it bought one before any work existed, then
  discovered the change was prose. The author still never sees the
  implementation (it works in a pristine pre-execution snapshot), so a witness
  proves the same thing it always did. A witness that cannot be produced now
  leaves the completed work alone instead of discarding the candidate and
  re-running the task. Design: `docs/design/witness-protocol.md` §7.3.

- The pipeline no longer replays raw test-runner output into a worker's
  revision prompt. A deterministic verification failure is now disclosed
  through a **feedback airlock** at one of four grains (`L0`–`L3`), tightening
  automatically when the same failure repeats, and model-authored text coming
  back inbound (distress guidance, judge reasoning) is scrubbed against the
  sealed material before it can reach the worker. Operator-facing output is
  unchanged — `stella` still shows you the real failure; only the model's
  prompt is redacted. Design: `docs/design/witness-protocol.md`.

## [0.5.57] — 2026-07-27

## [0.5.56] — 2026-07-27

## [0.5.55] — 2026-07-27

## [0.5.54] — 2026-07-27

## [0.5.53] — 2026-07-27

## [0.5.52] — 2026-07-26

## [0.5.51] — 2026-07-26

## [0.5.50] — 2026-07-26

## [0.5.49] — 2026-07-26

## [0.5.48] — 2026-07-26

## [0.5.46] — 2026-07-26

## [0.5.45] — 2026-07-26

## [0.5.44] — 2026-07-26

## [0.5.43] — 2026-07-26

## [0.5.42] — 2026-07-26

## [0.5.41] — 2026-07-26

## [0.5.40] — 2026-07-26

### Changed

- `stella storage prune` is now an alias for `stella stats prune` — same flags,
  same engine. Both verbs landed in parallel (#704 and #707) as rival
  implementations of #616's `store.db` retention, against two different store
  engines and with different flag spellings. #707's engine won and replaced the
  other's module, which left `storage prune` wired to code that no longer
  existed. The verb is kept and re-pointed at the surviving engine, so
  retention stays discoverable from both `stats` and `storage`.

  Two flag/behaviour changes for anyone who used `storage prune` in the window
  it existed: the ceiling flag is `--max-rows` (was `--max-executions`), and the
  guard is on un-replicated telemetry rather than on in-flight turns and pending
  enterprise exports.

## [0.5.38] — 2026-07-26

## [0.5.37] — 2026-07-26

## [0.5.35] — 2026-07-26

## [0.5.34] — 2026-07-26

### Changed

- Every `--output-format json|stream-json` summary now leads with
  `schema_version` instead of burying it mid-object. Two of the three envelopes
  were built with `serde_json::json!`, which emits a sorted map; they are now
  structs, so the version is the first key a reader sees. Key order remains
  outside the contract — consumers must keep reading by key, not position.

## [0.5.32] — 2026-07-26

### Added

- `--output-format json|stream-json` summaries now declare `schema_version`
  (currently `1`). Every envelope carries it — the pipeline summary, the
  `--no-pipeline` summary, and the pre-flight error envelope — and all three
  always declare the same value. The bump rule is documented in
  [Scripting & automation](https://stella.oxagen.sh/docs/scripting#the-envelope-contract):
  it increments only when a key is removed, renamed, retyped, or changes
  meaning, never when a key is added, so consumers must keep ignoring
  unrecognized keys.

## [0.5.31] — 2026-07-26

## [0.5.30] — 2026-07-26

## [0.5.29] — 2026-07-26

## [0.5.28] — 2026-07-25

## Before 0.5.27

This file was introduced at 0.5.27. Earlier releases are recorded only in their
generated GitHub Release notes, at
<https://github.com/macanderson/stella/releases>. No attempt has been made to
reconstruct them here — a hand-written history of releases nobody curated at the
time would be a guess presented as a record.
