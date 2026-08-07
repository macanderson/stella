# Changelog

What each **minor line** of Stella changed, in the words of someone who uses
the CLI. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## How this file works

**One section per minor line — not per release.** Every merge to main cuts a
patch release (see [`RELEASING.md`](RELEASING.md)), which is a fine way to ship
and a terrible way to keep a record: the 0.6 line alone was 127 releases in
eight days. Those releases are not undocumented — [the releases
page](https://github.com/macanderson/stella/releases) carries notes for every
single tag, generated from that tag's own diff. This file is the other half of
the pair: the durable, curated record of what a line delivered, written for
someone deciding whether to upgrade rather than someone bisecting a regression.

**CI writes this file, not contributors or coding agents.** Don't add a bullet
under `## [Unreleased]` in your PR — leave the section alone.
[`scripts/changelog-ai.sh`](scripts/changelog-ai.sh) drafts the section from the
whole series range when a minor or major release is cut, and
[`scripts/changelog-roll.sh`](scripts/changelog-roll.sh) rolls it into place. If
your change needs context the diff alone won't convey, put it in the PR
description — the drafter reads commit bodies (squash-merge PR descriptions),
not just code.

Internal refactors, test-only changes, and CI work need no action from you; they
are not user-facing and do not appear here.

<!-- Sections below are generated. See scripts/changelog-roll.sh for the rules:
     minor and major lines only, and never a heading with an empty body. -->

## [Unreleased]

## [0.7.0] — 2026-08-07

### Changed

- **The changelog is now one section per minor line, and reads like something a
  person wrote.** This file had grown to 180 sections of which **77 were
  completely empty** — a bare `## [0.6.x] — <date>` heading with nothing under
  it. Nothing was wrong with the drafter; the composition was. A patch release
  was cut on every merge to main and the roll fired on every one of them, while
  the drafter degrades open by contract (no API key, no non-bot commits, or an
  unparseable response all print nothing and succeed). When the second happened
  the first still stamped the heading. Patch releases no longer touch this file
  at all, and a minor release can no longer emit a heading with an empty body
  under any failure. Per-release detail did not disappear — it ships as GitHub
  Release notes, the surface built for "what changed in this exact build".

### Added

- **Release notes have a home you can actually read:
  [stella.oxagen.sh/releases](https://stella.oxagen.sh/releases).** Every minor
  line as an interactive page — filter by kind (added, changed, fixed, removed,
  security), search the full text of every entry, and expand a line to read it
  in full. Built from this file at build time, so the site and the repository
  cannot drift.

## [0.6.0] — 2026-07-29 → 2026-08-06

The verification line. A model's opinion stopped being enough to end a run,
the verifier stopped being allowed to grade its own work, and the pipeline
learned to research before it plans.

### Added

- **The verdict is decided by a model that did not do the work.** Verifier
  independence is enforced before the spend, not audited afterwards, and each
  candidate records a `verifier_independent` fact so a run that could not get
  an independent verifier says so instead of quietly grading itself (#1867).
- **A pre-plan research stage.** Triage names the questions a task actually
  turns on, and parallel read-only sub-agents answer them before the planner
  writes anything — so the plan is built on what the repository says rather
  than on the model's first guess (#1778).
- **Parked waits: the agent can wait on the outside world without burning
  model steps.** A run blocked on a CI job, a deploy, or a long build parks
  instead of spinning, and wakes when the thing it was waiting for changes.
  `TurnParked`/`TurnWoken` are typed wire events, and the deck draws the park
  state rather than looking hung (#1471, #1857).
- **Sub-agents.** The model can delegate with the `task` tool, with a pool
  ceiling, cancellation that closes the bracket on its children, and spend that
  settles on every exit path — panics included.
- **`--accessible` runs the Command Deck itself under a screen reader.**
  A mode on the real product, not a lesser surface beside it: every tab, gate,
  key, sub-agent and resume is unchanged, the deck draws inline so completed
  messages join normal scrollback exactly once, animation is frozen, and tab,
  overlay and focus changes are announced (#1258).
- **Diagnostics you can attach to a bug report.** A panicking or non-zero run
  writes `.stella/private/crash-*.jsonl` (owner-only) and prints the path;
  `stella doctor --last-failure` prints the newest. The record type physically
  cannot hold a prompt, a path, or model output, so there is nothing in it to
  review before sending. `-v`/`-vv`/`-vvv`, `--log-level <spec>` for per-crate
  filtering, and `--log-file <path>` came with it.
- **The Observatory grew a Sessions tab.** Session replay and inspection with
  real per-turn git diffs, a prompt diff between turns, and the
  self-improvement residue that `context.db` had been accumulating unseen
  (#1870, #1871, #1876).
- **`stella context` — kept rules become governed steering.** `keep`,
  `promote`, `govern` and `validate`, with approvals written into a
  hash-chained append-only ledger; a tampered ledger grants nothing.
- **`/profile fast|balanced|pro|ultra` retunes every engine role from one
  word**, choosing only among models your configured keys can actually reach
  and clamping effort to the rungs each provider really exposes.
- **The code graph records call sites.** `callees <symbol>` lists the calls
  inside a definition, `callers <symbol>` reverse-looks-them-up — structural,
  so unlike a text search it never matches a comment or a string (#335).

### Changed

- **A green test run only counts if it fixes the failure that was actually
  reported**, and a model judge's "done" needs corroboration that is not
  another model's opinion — a fail→pass flip or a test that ran green. Without
  it the turn is `UNVERIFIED`, never silently passed. A witness test that
  survives every single-line mutant of the code it covers no longer earns a
  fast pass either.
- **Stella asks each model for what that model can actually emit.** The engine
  carried one 16,384-token output cap for every model everywhere, even though
  each model's real ceiling was already known and stored. The symptom was
  truncation that looked like failure: a step that spends its budget reasoning
  and is cut off before it can emit a tool call does no work. `model_timeout`
  rose to 816s for the same reason — correct, complete single steps were
  measured at 624s and 756s, and a 600s bound killed them after paying for them.
- **Prompt caching actually engages on every provider that needs it.**
  Anthropic's conversation-tail breakpoint no longer no-ops when the last block
  is an image, a settings-defined provider pointed at OpenRouter now gets
  OpenRouter's markers instead of running Claude with zero caching, Bedrock
  recognises any Anthropic-vendored model id, and the cache TTL is configurable
  — one hour for interactive work, five minutes for headless (#1839).
- **Long turns cost less.** Tool results older than eight tool-bearing steps
  are middle-out truncated during the turn rather than only near the context
  ceiling, batched so the cache prefix is rewritten once per several steps.
  On the measured head-to-head this growth pattern was the dominant share of a
  6.4× input-token gap. `read_file`'s payload cap fell from 76% of the context
  budget to 12% (#1285, #1842).
- **`stella --help` is grouped by what a command is for**, and the docs sidebar
  mirrors those groups.

### Fixed

- **`delete_file` removes the symlink, not what it points at.** The resolver
  canonicalized, so deleting an in-tree symlink silently destroyed the target
  and left the link dangling — reported as an ordinary deletion.
- **`apply_edits` no longer reverts a write that arrived from outside the
  batch.** A failed mid-batch edit restored the bytes captured at validation
  unconditionally, destroying whatever a formatter or watcher had done in
  between while reporting a clean all-or-nothing abort.
- **A planted `rg` config can no longer shorten what the agent sees.**
  `RIPGREP_CONFIG_PATH` is scrubbed from tool subprocesses alongside the
  `GIT_CONFIG_*` family; a `--max-count=1` in a config file used to truncate
  the `grep` tool's answer, and the agent read the shortfall as fact.
- **`Retry-After` is honoured in its HTTP-date form**, not only as
  delta-seconds — which is the form the CDN-fronted deployments that actually
  rate-limit tend to send, so the stated backoff was being dropped and the
  retry landed back inside the window.
- **File changes made through the shell reach the file ledger.** `make`,
  `patch`, or `echo > f` changed the tree without appearing in the Files tab or
  in the change events verification reads — in one measured run, 71% of the
  agent's activity was invisible (#1062).
- **A turn that called no tool can no longer report success**, and `stella run`
  exits when the turn ends.

### Removed

- **The `bash` sandbox is gone (#1300).** `STELLA_BASH_SANDBOX` is now inert:
  read nowhere, so a stale value in a shell profile or CI job does nothing and
  fails nothing. It confined the `bash` tool and nothing else — `build_project`,
  `run_tests`, `verify_done`, `run_script`, `start_process`, the `git`/`gh`
  invocations, custom tools and hook actions all spawned around it and always
  ran unconfined. "Sandbox: on" read as a bound on the session while delivering
  a bound on one tool, and a half-boundary people rely on is worse than a
  clearly absent one. To get the real thing, run the whole `stella` process in a
  container and mount only the repository it should touch; add `--network none`
  for the equivalent of the old `restricted`.

## [0.5.0] — 2026-07-23 → 2026-07-29

The context and memory line. Recall became accountable, memories learned what
they were about, and verification stopped paying for work it did not need.

### Added

- **The accountable context frame.** The adaptive-context lifecycle went from
  opt-in to on by default: what the context engine adds to a turn is recorded,
  inspectable with `stella inspect --diff`, and reviewable — `stella proposals`
  shows what the learning loop wants to keep before it keeps it.
- **Memories anchor to the files they are about**, and a deleted file ends the
  memory's life rather than leaving it to steer future turns from a tree that
  no longer exists. `stella memory forget`/`restore`/`edit`/`compact` round out
  the lifecycle, and memories that stop helping retire themselves — reversibly.
- **`stella scoreboard`, `stella ingest`, and a `stella doctor` that repairs.**
  Doctor diagnoses and repairs a corrupt local store and checks the fleet
  ledger; ingest extracts documents into the context plane.
- **Machine-readable output grew a contract.** `--output-format json` and
  `stream-json` summaries declare a `schema_version`, and the event stream
  tolerates events emitted by newer versions of Stella.
- **Release artifacts carry build provenance, and the installer checks it.**
- **This changelog.**

### Changed

- **Verification is demand-driven.** A change with nothing to prove no longer
  buys a review call to be told so, a witness test is only written when the
  diff warrants one, and a greeting no longer pays for a triage model call.
- **Verification failures cross a feedback airlock** before reaching the worker,
  instead of raw test-runner output being replayed into its context.
- **Every built-in tool ships turned on**, switchable from one settings map and
  from a tools editor in the TUI SETTINGS tab.
- **Reflection learns your codebase, not itself**, and memory only records what
  a fresh look at the repository would not reveal.

### Fixed

- **Cancelled tool subprocesses are actually killed**, and interrupted runs
  clean up after themselves.
- **`stella serve` can no longer deadlock or cancel a live turn** during a
  concurrent request, and its live-turn cap stopped being a one-way latch.
- **Chain-of-thought no longer replaces the answer on OpenRouter**, and
  provider 5xx responses are retried instead of failing the turn.
- **Agent edits keep your file's identity** — inode, permissions and all —
  and every edit keeps its diff in the transcript.

### Security

- **Secrets are wiped from memory when dropped**, redacted from session
  exports, and scrubbed more thoroughly from subprocess environments.
- **Command approval covers every command-composing tool**, not only `bash`.
- **`stella serve` turn ids are no longer guessable**, and the SVG sanitizer
  closes the `data:` URI gap.

## Earlier releases

0.4.0 and earlier predate this file. Their notes are on
[the releases page](https://github.com/macanderson/stella/releases), generated
per tag from each release's own diff.
