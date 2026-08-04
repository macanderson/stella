# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## How this file works

**Every merge to main cuts a release** (see [`RELEASING.md`](RELEASING.md)),
and **CI writes this file, not contributors or coding agents.** Don't add a
bullet under `## [Unreleased]` in your PR — leave the section alone. The
release job drafts the entries itself from the **released diff**
(`scripts/changelog-ai.sh`, the same AI Gateway that writes the GitHub
Release notes) and rolls those beneath the new version heading, replacing
whatever was under `[Unreleased]` (normally nothing). A release whose diff
has nothing user-facing gets a one-line `_Internal: …_` note instead of
bullets.

Changelog authorship used to be split between hand-written PR entries and
this drafter, with hand-written text winning whenever present. That produced
inconsistent, sometimes-mangled entries, so the drafter is now the only
author: one voice, always grounded in the diff rather than in whatever a PR
happened to say about itself. If your change needs context the diff alone
won't convey, put it in the PR description — `changelog-ai.sh` reads commit
bodies (squash-merge PR descriptions) too, not just the code.

Internal refactors, test-only changes, and CI work need no action from
you — the drafter files those under the `_Internal_` line on its own.

Each GitHub Release additionally carries per-release notes generated at publish
time from the same diff. Those are release-note prose; this file is the
durable record, one section per version.

Entries for 0.5.29 through 0.6.42 were reconstructed after the fact
(2026-07-31) from each release's diff — the sections were rolled empty at the
time. Versions missing from the ladder entirely (a rapid-merge race used to
skip the roll) were re-inserted the same way.

## [Unreleased]

## [0.6.96] — 2026-08-04

## [0.6.95] — 2026-08-04

## [0.6.94] — 2026-08-04

## [0.6.93] — 2026-08-04

## [0.6.92] — 2026-08-04

## [0.6.90] — 2026-08-04

## [0.6.89] — 2026-08-04

## [0.6.88] — 2026-08-04

### Changed

- The A/B recall control now runs on **every** surface, not only the
  interactive REPL's plain prompts (#1221). `stella run`, `/goal`, and the
  Command Deck each arm it at turn start, and its schedule is counted durably
  per workspace in `context.db` instead of per session — which is what makes a
  one-turn-per-process surface capable of producing a control turn at all. A
  control turn is now frameless end to end: the pipeline's recall port returns
  nothing on it, so the planner and witness author no longer run on recalled
  context while the turn is recorded as suppressed.
- The control's rate is configurable as `context.retrieval.ab_recall_rate`
  (default 10, as the compile-time constant it replaces; `0` disables the
  experiment).

### Fixed

- A control turn's `[ab-control]` episode tag no longer disappears when the
  prompt is longer than 240 characters. The tag was composed into the prompt
  before truncation, so exactly the turns the measurement is made of were filed
  as ordinary ones; it is now appended by the episode writer, after truncation,
  on every surface that records an episode.
## [0.6.87] — 2026-08-04

## [0.6.86] — 2026-08-04

## [0.6.85] — 2026-08-04

## [0.6.84] — 2026-08-04

## [0.6.83] — 2026-08-04

## [0.6.82] — 2026-08-03

### Added

- Bedrock is now a first-class provider everywhere the credential chain reaches,
  not only in a shell with the standard AWS variables exported. `stella auth set
  bedrock` stores the whole set — access key id, secret access key, optional
  session token, and region — in `~/.stella/credentials.toml` (new
  `[credential_fields.<provider>]` section), prompting for each or accepting
  `--field NAME=VALUE`. `stella auth list` shows which companion values a
  provider has stored, by name; `stella auth remove` takes them with it.
- Secure benchmark runs can use Bedrock (#1301). The launcher's credential
  handover carries a set of secrets instead of exactly one, still over a single
  unlinked owner-only descriptor that never touches disk or the environment, and
  the region rides beside it as disclosed routing rather than as a secret. Which
  AWS credential sources are supported — and which are deliberately excluded
  (profile files, SSO, IMDS/container roles, web-identity tokens) — is documented
  in `bench/harbor_adapter/README.md`.

### Changed

- When a model judge passes a turn and **nothing deterministic backs it up** —
  no fail→pass flip, no test that ran green — the pipeline now spends one
  revision asking the worker for that evidence instead of recording the pass as
  `UNVERIFIED` on the spot (#1295). The ask is raised **only where a tracked
  command exists to answer it**: both facts that would settle the question are
  observations of that command, so without one the request is unanswerable
  however well the worker responds — which is why the same feature was measured
  as a loss and switched off the first time. Bounded to one ask per candidate,
  paid from the existing `max_revisions` budget. Turn it off with
  `agent_engine_config.pipeline_judge_evidence_demand = "off"`; the measurement
  and its limits are in `bench/evidence/judge-evidence-demand-1295/`.

### Fixed

- Auto-detection no longer selects Bedrock when only `AWS_ACCESS_KEY_ID` is set.
  A shell carrying an unrelated AWS key used to launch a provider that then died
  with a SigV4 error; it now falls through to the first-run message naming
  providers that can actually run. `--model bedrock/…` still pins it explicitly
  and reports the named missing-credential error.

## [0.6.81] — 2026-08-03
### Removed

- **The `bash` sandbox is gone (#1300).** `STELLA_BASH_SANDBOX` —
  `workspace-write` / `restricted`, backed by `sandbox-exec` (Seatbelt) on
  macOS and `bwrap` (bubblewrap) on Linux — has been removed along with
  `stella-tools/src/sandbox.rs`. **The variable is now inert:** it is read
  nowhere, so a value left in a shell profile, CI job, or service unit does
  nothing and warns about nothing. Setting it no longer fails a tool call
  either — an unrecognized value used to be an error, and now it is ignored
  like any other unused variable.

  It confined the `bash` tool and nothing else. `build_project` and
  `run_tests` (via `command`), `verify_done` (via `test_cmd`), `run_script`,
  `start_process`, the `repo_*` and `ci_status` `git`/`gh` invocations, custom
  manifest tools, and hook actions all spawned around it and always ran
  unconfined — so "sandbox: on" read as a bound on the session while
  delivering a bound on one tool. A half-boundary people rely on is worse than
  a clearly absent one, which is the whole reason this is a removal and not an
  extension.

  **If you were relying on it:** run the whole `stella` process inside a
  container (Docker, Podman, or a remote sandbox) and mount only the
  repository you want the agent to touch. That boundary sits outside every
  spawn path instead of on one of them, so no tool can route around it. Add
  `--network none` for the equivalent of `restricted`, with the same cost it
  always had — no model provider, no dependency fetches, no `git push`. See
  [Permissions](https://stella.oxagen.sh/docs/agent-tools/permissions) and
  `docs/design/remote-sandboxes.md`. Nothing else changes: `bash` already ran
  unconfined by default, and the `command.started` policy chain still gates
  every model-authored command line before it spawns.

## [0.6.80] — 2026-08-03

### Added

- The code graph now records **call sites** (#335, B1): `graph_query` and
  `stella graph` gain two ops — `callees <symbol>` lists the recorded calls
  inside a definition's span, and `callers <symbol>` reverse-looks-up call
  sites by name, labeled best-effort in every answer (same-name methods
  conflate; function-pointer and macro calls are not seen). Structural, so
  unlike `references` it never matches comments or strings. Index-time only,
  no new dependencies; the `code_graph_calls` table converges into existing
  stores on the next open.

- `--accessible` (env `STELLA_ACCESSIBLE=1`) runs the **Command Deck itself** so
  a screen reader can read it — a mode on the real product, not a lesser surface
  beside it. Every tab, gate, key, sub-agent and resume is unchanged. The deck
  draws inline on your own screen instead of the alternate one, each completed
  message moves into normal scrollback exactly once (and is still there after you
  quit), animation is frozen, the GRAPH/SKILLS panes and the session work rail
  stack into one column, the grid views (TRACES, ISSUES, TOOLS, AGENTS,
  INSTALLED) render as labelled rows instead of aligned columns, and tab,
  overlay and focus changes are announced. A terminal that never answers a
  cursor-position request still gets a deck — it degrades to a full-screen draw
  on your own screen, and says so. (#1258)

- The engine now ages old tool results **during** a long turn, not only near
  the context ceiling: results older than 8 tool-bearing steps are middle-out
  truncated to head+tail (batched, so the provider prompt-cache prefix is
  rewritten once per several steps, never per step). On the measured
  head-to-head this growth pattern was the dominant share of a 6.4× input-token
  gap. Tune or disable it with `agent_engine_config.tool_result_horizon_steps`
  (`0` disables); the compaction trigger itself is now also settable as
  `agent_engine_config.compaction_budget_tokens`, and both ride
  `stella serve`'s per-turn `engine` object too. (#1285)
- `stella usage report` now shows cache **writes** beside cache reads (the
  cost side of the cache ledger), the Observatory's models table gained a
  `cache %` column, and the benchmark live feed carries each trial's
  cached/uncached input split instead of dropping it. (#1285)

### Fixed

- Messages the deck itself speaks are now marked `▸`, so the transcript no longer
  reads as though the model said "conversation cleared". The rail glyph that used
  to carry that distinction is visual, and visual distinctions do not survive
  being read aloud. (#1258)

- A turn that ends (or proceeds) holding a length-truncated partial no longer
  retains the whole cut-off scratchpad in its transcript: the spent-allowance,
  out-of-time, budget-abort, and truncated-with-tool-calls paths now keep the
  same middle-elided form the continuation path always kept. What you see is
  unchanged — only what the model re-reads (and re-pays for) every later step
  shrinks. (#1285)
- Prompt-cache opt-ins across every provider that needs one: the Anthropic
  adapter's conversation-tail cache breakpoint no longer silently no-ops when
  the last content block is an image or document (it now stamps the newest
  stampable block); a settings-defined custom provider whose base URL points
  at openrouter.ai now gets OpenRouter's `cache_control` + `session_id`
  markers instead of silently running Claude with zero caching; and Bedrock's
  cache-point gate recognizes any Anthropic-vendored model id, not only ones
  spelling "claude". Implicit-cache providers (OpenAI, Gemini/Vertex, Z.ai,
  xAI, DeepSeek) need no marker — their cached-token telemetry is parsed and
  now witnessed per identity in the parity matrix. (#1285)
## [0.6.79] — 2026-08-03

## [0.6.78] — 2026-08-03

## [0.6.77] — 2026-08-03

## [0.6.76] — 2026-08-03

## [0.6.75] — 2026-08-03

## [0.6.74] — 2026-08-03

## [0.6.73] — 2026-08-03

### Fixed

- `delete_file` now removes a **symlink itself** instead of the file it points
  at. The path resolver canonicalizes, so deleting an in-tree symlink (a
  vendored config, a `node_modules/.bin` entry) silently destroyed the target
  and left the link dangling, reported as an ordinary deletion. A dangling
  symlink is now deletable too, and the tool says which of the two it removed.
- `apply_edits` no longer reverts a write that arrived from outside the batch.
  When a mid-batch write failed, the rollback restored the bytes captured
  during validation unconditionally — destroying any change a formatter,
  watcher, or editor had made in between while still reporting a clean
  all-or-nothing abort. Files whose bytes are no longer the batch's are left
  alone and named in the error.
- `RIPGREP_CONFIG_PATH` is scrubbed from tool subprocesses, alongside the
  `GIT_CONFIG_*` family it matches. A planted `rg` config (`--max-count=1`, a
  hiding `--glob`) silently truncated what the `grep` tool returned, and the
  agent read the shortfall as fact.
- `Retry-After` is honored in its HTTP-date form, not only as delta-seconds.
  A provider behind a CDN — the deployments that actually rate-limit — had its
  stated backoff window dropped, and the retry landed back inside it.

### Changed

- `stella init`'s code-graph summary reports files skipped for exceeding the
  indexer's size ceiling, beside the existing generated-file count. The
  exclusion was counted and surfaced nowhere, so a large file leaving the index
  looked like a file with no symbols in it.

## [0.6.72] — 2026-08-03

## [0.6.71] — 2026-08-02

## [0.6.70] — 2026-08-02

## [0.6.69] — 2026-08-02

## [0.6.68] — 2026-08-02

## [0.6.67] — 2026-08-02

## [0.6.66] — 2026-08-02

## [0.6.65] — 2026-08-02

## [0.6.64] — 2026-08-02

### Added

- **`stella` writes a log you can attach to a bug report.** When a run panics or
  exits non-zero, diagnostics land in `.stella/private/crash-*.jsonl` (owner-only)
  and the path is printed. `stella doctor --last-failure` prints the newest one.
  The file is safe to send as-is: the record type physically cannot hold a
  prompt, a path, or model output, so there is nothing in it to review first.
- **`-v` / `-vv` / `-vvv`** turn on diagnostics at `info` / `debug` / `trace`.
  Default is `warn`, human-readable on a terminal and JSONL when redirected.
- **`--log-level <spec>`** filters per crate — `warn,stella_store=debug` is quiet
  everywhere except where you are looking. Also settable as `STELLA_LOG`; the
  flag wins over the variable, and both win over nothing. An unrecognised clause
  is reported and skipped rather than being fatal. `STELLA_SERVE_LOG` keeps
  working and now accepts the same richer grammar.
- **`--log-file <path>`** writes JSONL to a file, created `0600`, defaulting to
  `info`. An unwritable path is reported and the run continues.
### Changed

- **Stella now asks each model for what that model can actually emit.** The
  engine carried one output cap (16,384 tokens) for every model on every
  provider, and fell back to it even though each model's real ceiling was
  already being read from models.dev, written to the `max_output_tokens`
  column of the local model card, and read back out — then dropped when the
  runtime catalog was assembled. `CatalogEntry` now carries the ceiling and
  the engine uses it, in both directions: a model that can emit 64,000 gets
  asked for 64,000, and one whose ceiling is below 16,384 stops being asked
  for more than its provider will serve. An explicit `params.max_tokens` in
  settings still wins, and a model the catalog has no ceiling for keeps the
  previous default.

  The symptom this fixes is truncation that looks like failure: a step that
  spends its whole budget reasoning and is cut off before it can emit a tool
  call does no work and reports nothing useful, and on a benchmark that is
  scored identically to getting the answer wrong.

- **`model_timeout` is 816s, up from 600s.** The old value assumed no
  generation a caller still wants runs past ten minutes. At high effort
  against a large output ceiling that is false — single steps producing
  correct, complete work were measured at 624s and 756s, and a 600s bound
  killed them after paying for them. Raising the output ceiling without this
  only relocates the cliff, which is why earlier attempts to fix truncation
  by raising the cap alone did not.

### Fixed

- **The benchmark adapter passes the turn deadline it is running under.** The
  engine could already decline to start a length continuation it could not
  finish, and end the turn with a truthful partial rather than being killed
  mid-flight — but nothing told it the deadline, so the policy was inert in
  the one place it was written for. The Harbor adapter now derives
  `--turn-budget` per trial from Harbor's own agent timeout (via
  `--agent-kwarg agent_timeout_sec=<seconds>`, the seam Harbor's Cline adapter
  uses), holding back a fixed 60s so the turn ends as a result instead of a
  kill. `STELLA_TURN_BUDGET` is accepted as a fallback and is now registered:
  before this, exporting it did not enable the policy, it refused the run.

## [0.6.62] — 2026-08-01

## [0.6.61] — 2026-08-01

## [0.6.60] — 2026-08-01

## [0.6.59] — 2026-08-01

## [0.6.58] — 2026-08-01
- The four Context Graph Protocol crates (`contextgraph-types`,
  `contextgraph-host`, `contextgraph-trace`, `contextgraph-conformance`) are now
  ordinary crates.io dependencies at `=0.1.2`, declared once in the root
  manifest, instead of git dependencies pinned by commit rev (#819). Building
  stella from source no longer reaches out to the protocol repository, the
  lockfile carries a checksum for each of them, and `cargo audit` / `cargo vet`
  can see them — none of which was true of a git rev. `deny.toml`'s `allow-git`
  exemption is removed, so the workspace now has no vetted git sources at all.

## [0.6.57] — 2026-08-01

## [0.6.56] — 2026-08-01

## [0.6.55] — 2026-08-01

## [0.6.54] — 2026-08-01

## [0.6.53] — 2026-08-01

## [0.6.52] — 2026-08-01

## [0.6.51] — 2026-08-01

## [0.6.50] — 2026-08-01

## [0.6.49] — 2026-08-01

## [0.6.48] — 2026-08-01

## [0.6.47] — 2026-08-01

- **`/profile fast|balanced|pro|ultra` retunes every engine role from one
  word.** The deck now has a posture command that picks a model *and* a
  reasoning effort for the default, worker, judge, and triage roles at once,
  choosing only from models your configured API keys can actually reach. Models
  are ranked by list output price — the same capability proxy `auto_mode`
  already uses — so the choice follows catalog refreshes instead of a hardcoded
  table, and rows the catalog has no price for are excluded rather than read as
  free.

  Triage is held below the worker. The judge resolves two competing goals in
  the order that gives up least: independent *and* at least as capable wins
  outright; if nothing clears that bar, an independent model one rung down is
  still preferred, because losing bias resistance over a single rung is
  overcorrection; only past that does capability take over, and then the
  confirmation reports that review is correlated with the work.

  Effort is clamped to the rungs each provider actually exposes (Gemini stops
  at `high`, Z.ai has no effort knob at all). Because clamping alone would
  flatten the ladder — `fast` and `balanced` both reaching `low` on a two-rung
  provider — the missing rung is bought back by preferring, *among models at
  the same price*, one whose provider can express the requested level. It never
  leaves the price band, so a profile cannot overspend its tier to buy a knob.

  Applying a profile turns `effort_auto`, `reasoning_auto` and `auto_mode` off
  so the levels it prints are the levels that run, and **`/profile auto`** is
  the way back: it restores all three and drops the per-role pins. Custom
  per-agent prompts, providers, temperatures, model pins and your
  `allowed_models` list are left untouched throughout. Bare `/profile` shows
  what is set and what each profile would choose right now.

### Fixed

- **`/model` no longer copies your project's settings into your user file.**
  Setting the default model read the *merged* view of user + org + project
  settings and saved the whole `agent_engine_config` block to user scope, so a
  repo-local judge pin — or an org-managed effort ceiling — was promoted to a
  machine-wide default that outlived the repo it came from. Both `/model` and
  `/profile` now read the file they are about to rewrite, matching the rule
  the tool switches already followed.

- **A malformed settings file no longer costs you your engine config.** The
  same edit path treated an unparseable file as an empty one, and then wrote
  the whole block back — so a single key of the wrong type anywhere in
  `settings.json` (`"enable_recap": true`, where a `Toggle` was expected)
  silently discarded every model pin and per-agent prompt in it. A file that
  exists but cannot be parsed is now a named error, and nothing is written.

## [0.6.45] — 2026-08-01

## [0.6.44] — 2026-08-01

### Fixed

- **A renamed witness test file now counts as tampering.** Witness artifact
  identity used to be computed from whatever path the lookup happened to
  reach, so a witness test renamed after sealing — but still reachable
  through an aliased lookup — fingerprinted as the same file and kept its
  verified standing. Identity now records the canonical location the artifact
  was actually observed at inside the workspace, and the pinned-path equality
  rejects a moved file as tampering. (#1077)

## [0.6.43] — 2026-08-01

### Changed

- **Changelog entries are now drafted from the released diff.** A release
  arriving with `[Unreleased]` empty gets its section written by
  `scripts/changelog-ai.sh` from the diff it ships (same AI Gateway key and
  model as the GitHub-release notes; degrade-open, hand-written entries
  always win), and the 86 empty or missing version sections from 0.5.28
  through 0.6.42 were reconstructed the same way. (#1076)
- **`stella observe` wears brand kit v1.0 ("the comet").** The Observatory
  dashboard moves to Phosphor Gold on Ink with JetBrains Mono, matching the
  docs site's rebrand. (#1078)
- **The docs site's front page centers on installing.** White sidebar, a
  hero focused on the install command, and install paths that are actually
  counted — the letter counters stay inside their glyphs. (#1074, #1075)

### Fixed

- **A locally-cut release's tag now points at a tree stamped with its own
  version.** `scripts/release.sh` (the degraded, Actions-down release path)
  reverted the version stamp before tagging, so `cargo install --git … --tag`
  and any other from-tag source build produced a binary whose `--version`
  reported the *previous* release — only the prebuilt tarballs were stamped
  right. The script now cuts a release commit with `scripts/sync-versions.sh`
  and tags that, exactly like `auto-tag.yml` (#822, #1072).

## [0.6.42] — 2026-08-01

### Changed

- **The docs sidebar now mirrors the CLI's own help groups.** Navigation is
  regrouped into five sections — Start / Do / Understand / Configure /
  Reference, plus Project — and the command index adopts the six groups
  `stella --help` prints, with summaries kept byte-identical to the CLI's.
  No page URL changed. (#1070)

## [0.6.41] — 2026-08-01

### Changed

- **The docs site was rebuilt on brand kit v1.0 ("the comet").**
  stella.oxagen.sh moves from electric blue on deep space to Phosphor Gold
  on Ink with a warm paper light mode, self-hosted JetBrains Mono as the
  single typeface, and regenerated PWA icons, manifest, and social cards.
  (#1063)

## [0.6.40] — 2026-08-01

### Fixed

- **File changes made through the shell now reach the file ledger.** `bash`
  commands like `make`, `patch`, or `echo > f` changed the tree without
  appearing in the Files tab or the change events verification reads — in a
  measured run, 71% of the agent's activity was invisible. Stella now
  fingerprints the workspace around such calls and records what actually
  changed, even when the command exits non-zero. (#1062)
- **The judge can no longer end a run on its own word.** A model-judge
  "done" now needs corroboration that is not another model's opinion — a
  fail-to-pass flip or a test that ran green. Without it the turn is scored
  UNVERIFIED, never failed. (#1062)

## [0.6.39] — 2026-08-01

### Added

- **`stella serve` gains server-owned sessions with mid-turn controls.**
  `POST /v1/sessions` creates a conversation the server keeps between turns
  — one stable system prompt, shared history, and one budget that binds
  across the whole session — and running turns can now be steered, paused,
  and resumed through the API. An aborted turn leaves the session's history
  exactly as it was. (#1056)
- **`stella context promote` and `stella context govern`.** Promoting a
  context rule to advisory or blocking now writes an approval — approver,
  reason, and policy version — into a hash-chained, append-only ledger at
  `.stella/rules/promotions.jsonl`. A tampered ledger grants nothing, and
  `stella context validate` fails on it. `govern` shows or changes the
  governance mode, including author/approver separation. (#1059)
- **`stella calibration` reports the judge's measured false-positive
  rate.** The new command replays the event journal, reconciles each
  model-judge pass against the next CI verdict in the same session, and
  prints the rate — reporting "unmeasured" rather than 0% when nothing has
  been reconciled yet. (#1055)
- **Trajectory trace capture behind the new `trace_capture` setting.** Off
  by default; when on, every finished execution writes one training-ready
  record — the exact model inputs, stages, tool activity, and cost, with
  secrets redacted — to `.stella/private/traces.jsonl`. (#1057)

### Changed

- **Read-only no longer implies safe-to-run-twice.** Tool schemas carry a
  new `speculation_safe` flag, and Stella only runs a tool ahead of time
  when it is both read-only and marked safe — so web fetches, rate-limited
  issue and CI reads, and MCP tools are never speculatively re-run on a
  retry. (#1054)

## [0.6.38] — 2026-08-01

### Added

- **A test that reacts to nothing no longer earns a fast pass.** Before
  granting a deterministic pass on a test the agent wrote itself, Stella now
  breaks the candidate's changed lines one at a time (up to three
  single-line mutants) and requires the test to catch at least one. A test
  that stays green under every mutant sends the decision to the judge
  instead of ending the run. (#1052)

## [0.6.37] — 2026-08-01

### Fixed

- **Colored tool output no longer leaks escape codes into the transcript.**
  A colorized `cargo build` failure used to arrive as rows of `[0;32m`
  residue that ate the transcript's line budget. ANSI color, hyperlink, and
  charset sequences are now stripped once, when tool output is folded into
  the transcript. (#1051)

## [0.6.36] — 2026-08-01

### Changed

- **A green test run only counts if it fixes the failure that was actually
  failing.** Verification now parses test names from the runner's output; a
  passing run whose complete test list no longer contains the baseline's
  failing tests — the delete-the-failing-test trick — earns no verified
  flip, and the refusal is recorded in the judge evidence. Output it cannot
  parse falls back to the old exit-code behavior. (#1047)
- **Candidate selection prefers the best-verified attempt, not just the
  smallest.** When several candidates tie on verification rank, Stella now
  picks the one that introduced the fewest new warnings before comparing
  diff size. (#1047)

## [0.6.35] — 2026-08-01

### Added

- **Terminal-Bench head-to-head results published on the docs site.** A new
  benchmarks section reports Stella against Claude Code on the same 89
  Terminal-Bench 2.1 tasks, same model, run concurrently on one machine:
  Stella solved 58, Claude Code 44. Every task is on the page with per-run
  cost, tokens, and timing, and the raw trial records ship alongside as
  JSON. (#1037)

## [0.6.34] — 2026-08-01

### Added

- **A fail-then-pass test flip is confirmed before it earns fast credit.**
  When the tracked test suite flips from failing to passing, verification now
  reruns it once just before fast-submit; a flip that does not reproduce is
  demoted and escalates to the judge with `unstable_flip=true` instead of
  passing deterministically. (#1033)
- **New lint or type errors now veto a fast submit.** The pre-submit audit
  runs the workspace's own diagnostics (cargo check, tsc, eslint, ruff) and
  diffs against the pre-run baseline; a candidate that introduces new errors
  goes to the judge with a three-line sample instead of submitting. Warnings
  can join the veto via `diagnostics_veto_warnings`. (#1033)
- **Every verdict now records why it was reached.** The ladder inputs are
  frozen into the verdict at decision time — the oracle runs in order, flip
  state, diff size, diagnostics delta, tamper check — and replay can render
  exactly why a run fast-submitted, revised, or went to the judge. Judge
  escalations carry a compact oracle trace so the judge sees why the ladder
  was inconclusive. Older recordings report "not recorded" rather than
  guessing. (#1035)

### Changed

- **A timed-out test run is infrastructure noise, not a verdict.** Command
  outcomes are now typed (completed, timed out, infra): a timed-out baseline
  no longer locks the oracle onto a phantom failure, and a timed-out candidate
  suite escalates to the judge instead of burning a revision on infra noise.
  (#1033)

## [0.6.33] — 2026-07-31

_Internal: new Frontier-Bench benchmark lane with its own pinned harness; no
user-facing changes._

## [0.6.32] — 2026-07-31

### Fixed

- **The model timeout now bounds silence, not how long an answer takes.** A
  provider that streamed steadily past the deadline used to be killed
  mid-answer and reported as a provider fault — routine for long reasoning
  calls at high effort. The deadline now re-arms on every streamed fragment
  and trips only when a full window passes with nothing arriving, so a
  wedged, silent provider is still cut off exactly as before. (#1032)

## [0.6.31] — 2026-07-31

### Fixed

- **OpenRouter-style providers now get a reasoning-aware output ceiling.** The
  shared Chat Completions adapter (OpenRouter, Z.ai, xAI, DeepSeek, local)
  used to forward no output cap and let the provider's default decide, which
  cut reasoning models off mid-thought with no tool call. With reasoning on it
  now defaults to the same larger ceiling the Anthropic adapter picks; an
  explicit cap is still honored verbatim, and requests with reasoning off are
  byte-identical to before. (#1029)

## [0.6.30] — 2026-07-31

_Internal: live side-by-side dashboard for two-arm benchmark runs; no
user-facing changes._

## [0.6.29] — 2026-07-31

### Fixed

- **Running out of output tokens mid-thought no longer ends the turn.** When a
  reasoning model spent its whole output budget thinking and was cut off
  before calling any tool, the engine treated the truncated text as a finished
  answer. It now keeps the partial in context, nudges the model that the turn
  is not over, and continues — up to twice per turn — so the model acts on its
  own reasoning instead of handing in chain-of-thought as the result. (#1024)

## [0.6.28] — 2026-07-31

_Internal: benchmark harness fix so an empty budget means no cap; no
user-facing changes._

## [0.6.27] — 2026-07-31

_Internal: benchmark harness preflight rejecting non-portable binaries before
a run starts; no user-facing changes._

## [0.6.26] — 2026-07-31

### Fixed

- **A turn that called no tool can no longer report success.** When every
  evidence channel was dark, the verification ladder's abstain rung answered
  `passed: true` — so a run that did nothing at all was reported as a pass.
  A new `NothingAttempted` rung now counts the engine's own mutating tool
  dispatches: zero dispatches means the turn fails and a revision is spent
  telling the worker that describing the work is not doing it. Turns that
  acted but left nothing observable still abstain exactly as before. (#1017)

## [0.6.25] — 2026-07-31

### Fixed

- **`migrate config` no longer copies secrets into the committed file.** The
  migration used to inline an API key from `.stella/settings.json` into the
  repo-root `stella.toml` — a file meant to be committed — and the loader then
  refused to start. It now refuses before writing, leaves nothing on disk,
  names the JSON file the key lives in, and never echoes the secret. Private
  user-scope keys still migrate as before, and `--dry-run` refuses the same
  way the real run does. (#1010)

## [0.6.24] — 2026-07-31

_Internal: benchmark harness gained a witness-author arm and tier disclosure; no
user-facing changes._

## [0.6.23] — 2026-07-30

### Added

- **MCP servers show who they are, and ctrl+o opens an inspector.** A
  configured server used to render only as its config alias ("mcp" — even
  for a Stripe server). The tab now shows title, description, and endpoint
  drawn from the install card and the live handshake, with a full detail
  view on ctrl+o. (#1006)

### Changed

- **SETTINGS navigates like AGENTS.** Instead of two half-width editors that
  truncated their content at ordinary widths, ←/→ switches between
  full-width AGENTS and TOOLS panes, and `e` edits whichever pane you are
  on. (#1004)

### Fixed

- **`stella serve`: a failed write is a disconnect, not a dead turn.** When
  a client's connection dropped mid-stream the whole turn was torn down, so
  a reconnect got "unknown turn"; the turn now survives and the reconnect
  replays the frames it missed. (#1005)

## [0.6.22] — 2026-07-30

### Changed

- **The SETTINGS tab navigates like the AGENTS tab.** It used to draw both
  config editors side by side, splitting the terminal in half: on any ordinary
  width the agents editor truncated its values (`allowed_models` ended in an
  ellipsis after two entries) and the tools editor truncated the reason a tool
  was switched off — and the tab read as two things at once. The two editors
  are now the two panes of a secondary nav, **AGENTS | TOOLS**, walked with
  **←/→** exactly like EXECUTIONS | INSTALLED AGENTS. One pane is on screen at
  a time and it fills the tab. **`e` edits whichever pane you are on**; the
  editor it opens is unchanged, down to the last key, and its Esc still hands
  the keyboard back. `t` still jumps straight to the tool switches from either
  pane. Because a focused editor owns the keyboard, ←/→ can only move the nav
  from browse state — never out from under an open edit.

## [0.6.21] — 2026-07-30

### Added

- **`stella config` prints the wiring for all four engine roles.** It used
  to report only the session's own model; it now shows driver, worker,
  judge, and triage — provider, model, effort, thinking — plus the exact
  settings key that decided each, so "I changed the key and nothing
  happened" becomes visible instead of mysterious. (#997)

### Changed

- **`stella --help` is grouped by what a command is for.** The front page
  used to open with fifteen lines of internal notes and 33 paragraph-length
  summaries; it now opens with the usage line and a one-line-per-command
  index under six purpose headings. (#999)

## [0.6.20] — 2026-07-30

_Internal: docs pages for arena/telemetry commands plus lockfile sync; no
user-facing changes._

## [0.6.19] — 2026-07-30

### Added

- **`stella serve` streams are resumable, and the wire format is published.**
  A client that loses its SSE connection mid-turn can reconnect and replay
  the frames it missed; the event format now ships as committed JSON Schema
  and TypeScript definitions kept in lockstep with the code. (#987)

### Fixed

- **A prompt typed right after "done" stays in this conversation.** The deck
  painted "done" before the turn had fully settled, so a follow-up typed in
  that window spawned a separate agent that could not see the conversation.
  Pasting a screenshot at a text-only model also no longer kills the turn.
  (#986)
- **Cancelling a turn stops its sub-agents — and still counts their spend.**
  Children of a cancelled parent used to keep running unnoticed. Worker
  lanes and fleet workers also advertised the `task` tool while always
  refusing it; delegation now works there. (#988)

## [0.6.18] — 2026-07-30

### Added

- **Sub-agents: the model can delegate with the new `task` tool.** A child
  turn runs with a budget carved from the parent's real headroom, a capped
  report size, and a recursion limit; its spend moves the HUD and can abort
  the parent at the next step boundary. Pause and stop reach the children
  too, so Esc no longer ends the parent while its children keep spending.
  (#976, #983, #984)
- **`stella inspect --diff` shows what the context engine actually added.**
  Instead of re-reading a full reconstructed prompt, you get a unified diff
  of a step against the previous call, the first call of the session, or
  your prompt exactly as submitted. (#982)

### Changed

- **Startup notices are a transient dialog, not transcript rows.** Trust
  warnings and code-graph progress no longer land in the transcript looking
  like the agent said them; a formatted dialog shows them and dismisses on
  any key or on its own after 3 seconds. (#979)

### Fixed

- **The verifier abstains when it cannot see, instead of failing you.** When
  its evidence channels went dark it reported "the file likely does not
  exist" about files that were there; it now says it could not verify, and
  the file-change count it reads matches what the recorder counted. (#981)

### Security

- **A repository's rule file can no longer disarm your guards.** A cloned
  repo could ship a record reusing the lineage id of your user-level hard
  guard and silently replace it, or self-attest its way to an armed deny.
  User-tier guards now stay armed, and your approvals no longer transfer to
  whatever record later claims that lineage. (#978)

## [0.6.17] — 2026-07-30

### Fixed

- **Receipts count tokens one way, and old runs are corrected to match.** A
  context block's `token_cost` was counted in characters while the same
  receipt's `estimated_input_tokens` was counted in UTF-8 bytes, so a single
  receipt held two numbers for the same content that only agreed for ASCII. If
  you work in Japanese, Chinese, or any script that is not one byte per
  character — or your tool output contains emoji — a manifest's summed
  `token_cost` read up to 4x below the step it belonged to, and the size of the
  error depended on your language. Both numbers now come from one shared
  function counting UTF-8 bytes, the unit the Context Graph Protocol already
  specifies. Opening a workspace migrates its store (schema v19): every block
  already recorded is **recounted from its own preimage**, so past runs are
  corrected rather than reinterpreted, and `stella inspect` needs no knowledge
  that the old rule ever existed. Blocks whose preimage the store no longer
  holds — the reconstruction gaps `stella inspect` already declines to vouch
  for — record no cost at all rather than keeping a number in the retired unit.
  The compiled frame's schema version moves to `1.1` accordingly, so frame
  hashes minted under the two rules cannot collide. ([#925])

[#925]: https://github.com/macanderson/stella/issues/925

## [0.6.16] — 2026-07-30

_Internal: dependabot bump of a CodeQL CI action; no
user-facing changes._

## [0.6.15] — 2026-07-30

### Fixed

- **`stella run` exits when the turn ends.** Registry-held clones of the
  event channel kept it open, so the process printed its final answer and
  then hung instead of returning to the shell. (#970)

## [0.6.14] — 2026-07-30

### Added

- **The `stella context` suite: kept rules become governed steering.**
  Proposals extracted by `stella ingest` can now be reviewed (`context
  review`), decided (`keep`/`edit`/`ignore`), listed, explained (`context
  explain <handle>`), and turned into a PR (`context propose`); kept records
  render into the prompt, and a record whose truth probe is refuted stops
  steering. Works offline end to end, no API key. (#957)

### Fixed

- **Mid-turn steering reaches every best-of-N candidate.** Typed guidance
  used to be consumed by candidate 1 alone, so when the judge picked a
  different candidate your words were missing from the winning transcript.
  Every candidate now sees every steer, and the fan-out is narrated live
  ("candidate 2/3 won"). (#959)

## [0.6.13] — 2026-07-30

### Added

- **An OpenRouter key now sets up a whole default posture.** Instances with
  an OpenRouter key default to Kimi K3 at `xhigh` effort as the driver, Opus
  5 as judge, and GLM 5.2 as triage — layered underneath your settings, so
  anything you set yourself still wins. OpenRouter's `xhigh`/`max` effort
  levels also stopped being silently downgraded to `high`. (#952)
- **`stella context validate` re-checks kept rules against the tree.** It
  re-runs each context record's truth probe on demand and reports which
  claims went stale (CLAUDE.md says Node 20, `.nvmrc` moved to 22);
  `--strict` exits non-zero so a stale rule can fail CI. (#947)

### Changed

- **No single keystroke approves a plan anymore.** On the plan scope card,
  `a`/`t`/`x` committed on the first letter typed into an empty composer, so
  a note starting "also do X" silently approved the plan. Every answer is now
  typed and sent with Enter, and Esc closes the card even mid-note. (#951)

### Fixed

- **The SELF-IMPROVE tab shows self-reviews and learned skills.** Self
  ratings were never produced (the only writer hardcoded them to null), and
  the learned-skills counter watched a directory nothing writes to — both
  panels could only ever show zero or a dash. (#945)

## [0.6.12] — 2026-07-30

### Changed

- **Memory only records what a fresh look at the repo would not reveal.** The
  reflection prompt used to invite facts like file locations and module names
  — cheaper to grep than to carry. It now asks what surprised the agent,
  names the worthless categories, and makes "nothing worth keeping" a valid
  answer. (#944)

### Fixed

- **Restated lessons no longer pile up.** Paraphrases of a fact the store
  already held were kept without limit — one live store held 23 memories for
  six distinct facts — crowding other lessons out of the recall budget.
  Near-duplicates are now collapsed before they are written. (#944)

## [0.6.11] — 2026-07-30

### Added

- **`stella observe` now updates live and survives crashes.** Tool calls used
  to appear only after a turn finished, and were lost forever if the process
  died mid-turn. Counts now land as each event is written, unfinished turns
  are repaired on the next open, and the dashboard streams changes over an
  SSE endpoint (`/api/v1/live`) at ~250 ms latency instead of polling every
  5 seconds. (#908)

### Changed

- **One visual identity across every surface.** The terminal palette, docs
  site, and Observatory dashboard now share the electric blue + gold scheme
  from a single normative brand spec; the vermilion cursor, terminal-green
  theme, and the launch cinematic are retired. (#912, #937)

### Fixed

- **Multi-line edits on CRLF files work now.** `read_file` stripped `\r` from
  what it showed the model, so a needle copied from that render never matched
  the file on disk and the error blamed whitespace. `glob` and `grep` also
  stopped silently skipping dotfiles like `.github/`. (#937)
- **The Files tab now shows `apply_edits` batch edits.** Batch edits emitted
  no file-change events, so any turn that used the recommended batch tool
  showed an empty Files tab and no inline diffs. (#846)
- **`stella inspect` prints tool results as readable text.** Results were
  serialized as escaped JSON — file contents and command output became one
  long line of `\n` and `\"` — and are now printed directly, with errors
  prefixed `error:`.

## [0.6.10] — 2026-07-30

### Added

- **Witness tests that could not have failed meaningfully are rejected.** A
  witness with no assertions, one asserting only over constants, or a bare
  `#[should_panic]` is refused at authoring time with a named reason, so the
  author revises immediately instead of a vacuous test flipping the verdict
  green. (#906)

### Fixed

- **Typing at a scope review card answers the review instead of spawning a
  side agent.** A typed reply like "ok" used to become a parallel session
  while the review sat unanswered, and Esc then aborted the turn. A pending
  gate now owns the next submission, with plain words mapped to approve,
  trim, or abort — in both the deck and the single-session REPL. (#907)
- **`/files` and the diff pane no longer show `+0 -0` for edited files.**
  File-change events now come from the one recorder that measures the real
  before/after diff, so bulk edits via `apply_edits` and changes made in
  worker lanes are counted instead of being invisible or zeroed. (#903)

## [0.6.9] — 2026-07-30

### Added

- **A live PROOF rail shows verification beside the work.** A pinned card
  under the transcript tracks warrant, witness, oracle, tamper, and verdict
  while the turn runs, and every row resolves on every path — a waived
  witness, an aborted turn, or a run that never reached verification says so
  explicitly instead of hanging on "pending". (#880, #901)
- **Custom commands can be written in TOML, with Claude Code parity.**
  `.stella/commands/*.toml` loads beside markdown, both formats honor
  `argument-hint`, `allowed-tools`, `model`, `disable-model-invocation`, and
  `$1`–`$9` positional arguments, subdirectories become `/namespace:name`
  commands, and `stella commands convert` translates markdown to TOML. (#882)
- **`stella ingest <doc>` now actually extracts documents.** It used to print
  a placeholder; a markdown file is now split into atomic claims, each
  written as a reviewable proposal under `.stella/proposals/` — never
  published directly, with embedded commands quarantined and command or
  network probes refused for imported content. (#881)
- **A mid-turn prompt announces the agent it became.** Typing while a turn is
  running spawns a parallel `req:<n>` worker, but the transcript used to say
  nothing about it. Each spawn now prints a notice naming the lane, echoing
  the prompt, confirming the lead turn keeps running, and explaining how to
  navigate to it and back. (#898)

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

### Fixed

- **A diff probe that fails can no longer pass verification as "nothing
  changed".** When the machinery could not read the working tree — for
  example a candidate outside a git repository — the empty diff used to
  self-certify a passing "no behavior changed" verdict with no witness test.
  A failed probe is now treated as a failure and escalates to review. (#857)

## [0.6.6] — 2026-07-30

### Added

- **`stella-serve` answers `--version` and `--help`.** Both used to exit with
  "unknown argument", so a host embedding the server could not record which
  release it verified its wire contract against. `--help` also documents
  every `STELLA_SERVE_*` environment variable the binary reads. (#856)

## [0.6.5] — 2026-07-29

### Fixed

- **Creating an agent or skill from a prompt now reports what it made.** The
  create dialog used to close the instant it dispatched, leaving no sign the
  draft was running, finished, or failed. It now shows a spinner while the
  draft runs, opens the new definition's detail view on success, and shows
  the error on failure; Esc hides the dialog without losing the run. (#854)

## [0.6.4] — 2026-07-29

### Changed

- **A collapsed live thought follows its newest text instead of freezing on
  its first line.** While the model is still thinking, the preview window
  tracks the tail of the thought; once it settles, it reverts to showing the
  head. The preview is also budgeted in screen rows, so one long wrapped
  paragraph no longer fills the pane. (#851)

## [0.6.3] — 2026-07-29

### Added

- **The ultra-audit skill now ships in the repo.** Previously a machine-local
  skill, it lives at `.stella/skills/ultra-audit/` and is renamed from
  `/ultraudit` to `/ultra-audit`. Audit score history keeps its old keys, so
  past and future audit rounds stay comparable. (#850)

## [0.6.2] — 2026-07-29

### Added

- **New `stella tune` command A/B tests the worker effort setting.** `stella
  tune effort` compares a baseline and a candidate loop-bench run and reports
  which effort level won; with `--promote` it writes the winner to settings
  only on a statistically confident win. `stella tune rollback` reverts the
  last promotion exactly, and `stella tune status` shows the ledger. (#848)

## [0.6.1] — 2026-07-29

### Added

- **Stella now notices repeated shell patterns and proposes a tool for
  them.** When the same command shape keeps being rebuilt by hand with
  different values, a suggestion with a parameterized command template
  appears in `/inbox`, at most once per shape. It is a proposal only —
  nothing is authored or installed. (#845)

## [0.6.0] — 2026-07-29

_Internal: minor-version cut vehicle isolating flaky grep temp-dir tests; no
user-facing changes._

## [0.5.77] — 2026-07-29

### Added

- **New `/theme` command switches the deck between light and dark.** Two
  themes ship — `stella-dark` (terminal green on black, the default) and
  `stella-light` (ember red-orange on paper white). The switch applies on the
  next frame and persists to `ui.theme` in user settings, so the deck starts
  already themed. (#839)

### Changed

- **The deck's accent color is now terminal green instead of sky blue.**
  Orange is retired from the dark theme: code spans render in a calm sage and
  warnings in amber. Light mode also fills the base foreground, so prose
  renders as ink on paper instead of relying on the terminal's default. (#839)

## [0.5.76] — 2026-07-29

### Added

- **`/model` slash command.** Set the persisted default model straight from
  the prompt: `/model` shows the saved default, the live session model, and
  the pickable list; `/model <provider/slug>` validates the spec and saves
  it, with the same semantics as the SETTINGS tab. (#829)
- **The prompt animates while a turn is in flight.** The `>>>` chevrons chase
  left to right while the agent is working and sit still when idle or under
  `--no-anim`. (#829)

### Fixed

- **New lessons no longer vanish once the memory ledger grows.** Standings,
  retirements, decisions, and observation mining read the oldest 5,000 ledger
  rows, so past that bound every new promotion, retirement, or lesson was
  invisible forever; the folds now read the newest window. (#838)

### Security

- **Session exports redact secrets.** `stella export` wrote prompts, tool
  arguments, and touched-file telemetry verbatim into the ZIP and dashboard,
  so a key that ever reached telemetry left the machine in plaintext; every
  string value is now passed through secret redaction before export. (#840)

## [0.5.75] — 2026-07-29

### Fixed

- **Provider 5xx responses are retried instead of failing the turn.** A
  server-side error from the model provider used to be a hard failure; it is
  now treated as a transport error eligible for retry. (#815)
- **Code fences and wide characters render correctly in the TUI.** Fenced
  code blocks now follow CommonMark fence-length and closing rules (nested
  fences render right), and double-width glyphs no longer overflow
  fixed-width layouts. (#815)
- **Cancelled tool subprocesses are actually killed.** Kill-on-drop is now
  propagated through process execution, so cancelling a run no longer leaks
  child processes. (#815)

## [0.5.74] — 2026-07-29

### Fixed

- **`!` shell commands show their output in the transcript.** Typing `! pwd`
  ran the command but sent the output to a hidden lane the screen never
  draws, so it looked like nothing happened. Output now appears inline in the
  focused transcript, and a `!` command no longer flips an idle agent's
  status or counters. (#814)

## [0.5.73] — 2026-07-28

### Added

- **The context graph resolves Rust `use` paths to files.** Asking
  `graph_query` for a Rust file's importers now returns the real dependent
  set instead of a canned apology, and `run_tests` with scope "impacted"
  narrows a Rust change to the owning crates (`cargo test -p ...`) instead of
  loudly running the full suite. (#810)

### Changed

- **`apply_edits` batches pass the storage gate instead of being refused.**
  The gate used to blanket-refuse schema files inside a batch; it now
  simulates each touched file with the batch's composed edits and judges
  exactly the bytes that would be written, so legitimate multi-hunk schema
  work keeps the transactional path. (#810)

## [0.5.72] — 2026-07-28

### Added

- **New `stella scoreboard` command.** Shows what each unit of work cost and
  whether anyone said it was good: model calls, characters you typed,
  follow-ups and interrupts, plus the verdict a merged or closed pull request
  implies. Unrated work is counted but never averaged in as good. Reads local
  state only; no model judges anything. (#811)

## [0.5.71] — 2026-07-28

_Internal: contributor dev-env scripts de-branded and renamed; agent wiring
made opt-in; no user-facing changes._

## [0.5.70] — 2026-07-28

### Added

- **New `stella ingest` command.** Scans workspace markdown (AGENTS.md,
  CLAUDE.md, or any file you name) and tiers it for turning into steering
  records: offer by default, offer with a look, find but don't offer, or
  skip. Superseded and retired docs are surfaced but never offered as live
  guidance. Local files only; no API key needed. (#805)

### Fixed

- **Git chatter on stderr no longer corrupts parsed values.** Branch names
  and status were read from a merged stdout+stderr stream, so an advice hint
  or GIT_TRACE line could turn a push into "fatal: invalid refspec". The
  parsing reads now use stdout alone; displayed output keeps both streams.
  (#801)
- **A malformed zai stream frame can no longer pass as a successful empty
  turn.** The decoder dropped any frame it could not parse, which ended the
  turn as if the model had nothing left to do. A type-mismatched frame now
  fails loudly, and frames with a missing tool-call index or an explicit
  null tool_calls are tolerated instead of dropped. (#802)

## [0.5.69] — 2026-07-28

### Added

- **Opt-in cloud telemetry sync.** `stella cloud sync` drains usage telemetry
  over HTTPS to a configured hub, a status view reports the drain state, and
  logout deregisters the machine. Nothing is sent unless a hub is configured.
  (#800)

### Changed

- **Memories whose file anchors vanished retire themselves.** Validation now
  detects a memory whose anchor files are gone and retires it without waiting
  for a human ruling; validate reports the status as `anchors-missing`. (#800)
- **Recalled context and tool output are bounded.** Recalled frames cap at
  4k characters each and 20k total with visible truncation markers,
  `web_fetch` output clamps at 120k instead of 400k, and clipboard pastes are
  written private (0600) and pruned to the newest 100. (#800)

### Security

- **Command approval now covers every command-composing tool.** run_lint,
  format_code, diagnostics, the repo_* family, screenshot, ci_status, and
  start_work_on_issue used to run without passing the `command.started` gate;
  each now reports the exact line it would run, and a `run_script`
  approve-then-swap race is closed. (#800)

## [0.5.68] — 2026-07-28

_Internal: contributor worktree environment scripts plus demo-video and docs
refresh; no user-facing changes._

## [0.5.67] — 2026-07-28

_Internal: tag-only release; no commits between the two tags; no
user-facing changes._

## [0.5.66] — 2026-07-28

### Fixed

- **A "just chat" misread no longer swallows real work.** Requests like
  "organize my documents folder" were classified as small talk and answered
  with a tool-less reply that reported the task complete having done nothing.
  Any message asking to explore, organize, rename, or sort files now enters
  the work pipeline. (#794)
- **`stella serve` can no longer deadlock or cancel a live turn during
  cleanup.** Reclaiming abandoned turns took locks in an order nothing
  enforced, and a turn-id collision silently replaced — and cancelled — a
  turn still in use. Reclamation now skips contended entries, and a colliding
  registration is refused instead of overwriting. (#793)

## [0.5.65] — 2026-07-28

### Added

- **Claude extended thinking works on Bedrock.** The Bedrock adapter now
  passes the thinking switch and budget through Converse's model-specific
  passthrough field, so a reasoning budget set on a Claude-on-Bedrock model
  actually reaches it; with reasoning off the request body is unchanged.
  (#780)

### Changed

- **The default look is now electric blue on jet black.** Navy-tinted
  surfaces and the last gold accent are retired everywhere — TUI palette and
  theme, splash and init cinematics, the HTML session export, and the
  observatory dashboard. The `/color` default is `electric`; `sky`, `azure`,
  and `cyan` still resolve as aliases. (#780)

### Fixed

- **The serve live-turn cap is no longer a one-way latch.** A turn that
  finished but was never streamed held its slot forever, so a busy host
  drifted toward answering everything with `429`. Finished-but-unstreamed
  turns are now reclaimed under pressure, and the two colliding rewrites of
  the serve and observatory code are reconciled so both keep working. (#790)

## [0.5.64] — 2026-07-28

_Internal: CI workflow now runs every gate despite earlier failures; no
user-facing changes._

## [0.5.63] — 2026-07-28

_Internal: demo-recording scripts, a CI workflow, and a demo video; no
user-facing changes._

## [0.5.62] — 2026-07-28

### Changed

- **`stella serve` now bounds what one host will absorb.** At most 32 live
  turns — the 33rd gets `429` with `Retry-After: 5` — a 30-second read
  timeout on requests, and a 64 KiB header / 8 MiB body cap. An oversized or
  malformed request now gets a clear `413`/`400`/`408` instead of silently
  parking the engine step it would have answered. (#779)

### Security

- **Serve turn ids are no longer guessable.** Ids were sequential (`turn-0`,
  `turn-1`, …), so a single id leaked in a log or proxy trace made every
  other live turn addressable. Turn ids are now 128 random bits. (#779)

## [0.5.61] — 2026-07-27

### Added

- **Memories anchor to the files they are about — and a deleted file ends the
  anchor.** Reflection now records which files a lesson concerns, and `stella
  memory validate` reports anchors pointing at files that no longer exist;
  `--end-stale` marks them as no longer holding, so recall stops surfacing
  memories about files that are gone. History is kept: the memory was true,
  then the world changed. (#777)

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

## [0.5.59] — 2026-07-27

### Added

- **Verification failures now cross a feedback airlock before reaching the
  worker.** The raw test-runner tail — exact assertions, runtime values, the
  failing test's identity — used to be replayed into every revision prompt,
  making special-casing cheaper than fixing. The worker now gets a redacted
  brief that steps down in detail as the same failure repeats; the operator's
  verdict keeps the full runner output. (#765)

### Changed

- **A witness test is only written when the diff warrants one.** Authoring
  moved after execution and behind the warrant, so a change with nothing to
  prove — a docs edit, say — no longer pays a model turn to write a test.
  The author still sees the pre-execution tree, so the test cannot simply
  restate the implementation. (#770)
- **Reflection learns your codebase, not itself.** The old prompts asked what
  the agent should do differently and reliably got self-critique back — 8 of
  10 stored memories were about the agent. Both prompts now lead with what
  was learned about the codebase, success counts as a learning signal, and
  domain facts are ranked apart from process notes at recall. (#768)

### Fixed

- **The adaptive-context lifecycle no longer runs to nothing on headless or
  untrusted workspaces.** A workspace without project trust skipped all
  learning, not just skill-file writes; lesson parsing broke on models that
  narrate before answering, recording zero lessons; and an unreadable
  reflection looked identical to "nothing to learn". All three are fixed, and
  the reflection output cap rises from 512 to 2048 tokens. (#766)
- **`--model` now outranks `pipeline_worker_model`.** The config file's
  worker model silently overrode the flag, while the JSON envelope reported
  the model you asked for rather than the one that ran and billed. The flag
  now wins, the run says what it suppressed, and the envelope reports what
  actually ran. (#769)

## [0.5.58] — 2026-07-27

### Added

- **`stella doctor` gained a fleet-ledger check.** It now reports rows in
  `fleet.db` that name a run no longer on record. Report-only by design:
  `--repair` will not delete fleet history. (#764)

### Fixed

- **Concurrent workspace opens no longer collide.** Several `stella`
  processes hitting one fresh workspace could fail with "database is locked"
  or a UNIQUE-constraint error; four races on the store-open path are closed,
  and five parallel `stella init` runs now all succeed. (#764)
- **The docs and the system prompt stopped claiming bash is opt-in.** The
  shell and key-free web tools have been registered by default for several
  releases, but the README, the threat model, and the prompt the model reads
  every turn still said the opposite; all now describe the real tool surface.
  (#764)

## [0.5.57] — 2026-07-27

### Added

- **Dropped MCP tools are now visible.** A server advertising more than 256
  tools silently lost everything past the cap, and a missing tool looks
  exactly like one the model chose not to call. The deck's MCP tab now shows
  `N dropped past cap` per server plus a session total in the tab title, and
  text mode prints the same notice. (#760)

## [0.5.56] — 2026-07-27

### Changed

- **The transcript is now bright sky on true black.** Gold on navy is gone;
  the accent is spent on exactly one thing — the name of the tool being
  called — stage markers render as section rules instead of one-word rows,
  and system notes split into loud (errors, gates, questions) and quiet
  (bookkeeping). (#752)
- **Cost prints once per turn.** The budget gauge used to print a fresh spend
  row after every model call, and the figure was a session running total, not
  the turn's cost. The live turn cost now updates in place under the
  composer, and the settled cost lands once at completion with the model that
  ran. (#752)

### Fixed

- **The SESSION tab no longer renders raw ANSI color codes.** Its HUD and
  scope card carried 312 cells of literal cyan escape codes, and the
  scope-review card — the one card that halts the session — was bordered in
  the brand color like decoration; it is now warning-colored. (#752)

## [0.5.55] — 2026-07-27

### Added

- **Chain-of-thought now streams into the transcript.** An earlier fix
  stopped reasoning from being pasted into the answer text, which also meant
  thinking on tool-calling turns was dropped entirely. It now streams on its
  own dimmed channel, collapsed to a one-line live tail, with `ctrl+r` to
  expand — for OpenRouter/GLM reasoning and for direct-Anthropic thinking
  deltas, which were previously discarded. (#756)

## [0.5.54] — 2026-07-27

### Added

- **Memories that stop helping are now retired — reversibly.** Each turn's
  recalled context is recorded together with whether it was actually cited
  and helped, and a memory that repeatedly fails drops out of automatic
  selection with its reasons on record. New commands `stella memory retired`,
  `stella memory retire <id> --reason`, and `stella memory reaffirm <id>`
  list, retire, and restore them — nothing is ever deleted. (#751)

## [0.5.53] — 2026-07-27

### Fixed

- **Large plans no longer dead-end the Command Deck.** Scope review ran
  headless inside the deck, so any plan past the thresholds (over 5 steps, 8
  files, or $1.00) aborted with advice naming a setting the deck never reads.
  The approval card now actually raises: approve, trim to the largest safe
  prefix, or cancel, and the turn waits for your answer. (#750)
- **Chain-of-thought no longer replaces the answer on OpenRouter.** Anthropic
  models routed through OpenRouter stream empty content beside reasoning on
  every tool-calling turn, and the blank-turn fallback published the model's
  private deliberation as the reply. A turn that called a tool is no longer
  treated as blank. (#750)
- **Transient gateway errors are retried again.** OpenRouter reports the
  upstream status only in a numeric `code` field the error parser never read,
  so 502s classified as fatal and the configured retries never ran; 429 now
  classifies as rate limiting too. A stream that died before its usage frame
  also no longer misreports as "store write failed". (#750)

## [0.5.52] — 2026-07-26

### Changed

- **The adaptive-context lifecycle is now on by default.** Decomposed recall,
  frame identity, recall telemetry, the typed learning loop, and the proposals
  ledger now run with no configuration at all — a workspace with no settings
  file gets them too. An explicit `"enabled": false` under `context.lifecycle`
  restores the old behavior wholesale, and an unreadable or malformed settings
  file leaves the lifecycle off rather than overriding a possible opt-out.
  (#746)

## [0.5.51] — 2026-07-26

### Added

- **The accountable context frame, behind `context.lifecycle.enabled`
  (default off).** Each step's manifest gains a deterministic frame identity
  hash, recall decomposes into one labeled block per recalled memory instead
  of one blob, recall telemetry fires from every entry point (one-shot, REPL,
  `/goal`, and the deck — previously only the pipeline reported), and `stella
  inspect` grows a FRAME column. (#738)

## [0.5.50] — 2026-07-26

### Added

- **`stella proposals` — review what the learning loop wants to keep.** The
  new typed adaptive loop (behind `context.lifecycle.enabled`, default off)
  records observations in an append-only ledger and induces skill proposals;
  the command lists each with its supporting evidence and takes Keep, Edit, or
  Ignore, every decision recorded as a replayable event. Works offline with no
  API key. (#736)
- **The whole documentation as one pasteable file.** `docs/llms.txt` carries
  all 78 doc pages as a single Markdown file, generated from the docs site so
  it cannot drift, for dropping into a model's context whole. (#741)

### Fixed

- **Auto-created skills can no longer overwrite a hand-edited skill file.**
  The no-clobber guard checked the list of loaded skills instead of the files
  on disk, so a skill disabled from the SKILLS tab could be silently replaced
  by a freshly mined one — with no backup. The guard now checks the directory
  itself. (#742)

## [0.5.49] — 2026-07-26

### Added

- **`stella memory compact` reclaims the space old edits strand.** Every
  memory edit or forget left its old vector (about 1 KiB each) behind forever;
  compact deletes orphaned vectors and stale junctions while keeping edges and
  revisions, so point-in-time queries and `memory restore` still answer
  identically. (#735)
- **An optional similarity index speeds up recall on large stores.** `stella
  memory index` builds a deterministic IVF index over the workspace's vectors;
  with `context.retrieval.ann_enabled` on, recall probes clusters instead of
  scoring every vector, and every result reports which scan actually ran. Off
  by default, and recall falls back to the exact scan when the index drifts.
  (#735)

## [0.5.48] — 2026-07-26

### Fixed

- **The `enable_recap` setting was silently ignored.** The settings merge
  dropped the key while copying every other one, so `"enable_recap": "on"`
  parsed cleanly and then did nothing; it now reaches the runtime. Found in a
  docs-against-source audit that also fixed the documented (and broken)
  `--model openrouter/auto` form — the working spelling is
  `openrouter/openrouter/auto`. (#734)

## [0.5.47] — 2026-07-26

### Performance

- **Context blocks are hashed once per turn, not once per step.** Block ids
  and digests are now memoized and invalidated only when compaction or the
  overflow summarizer rewrites the transcript, so hashing grows linearly with
  a turn instead of with its square — an 8-step turn drops from 188 hash
  passes to 44, and the gap widens on longer turns. (#732)

## [0.5.46] — 2026-07-26

### Added

- **`stella memory edit <id> <text>` revises a memory instead of forking it.**
  Editing used to create a second live memory while the old one stayed
  citable; memories now have a durable lineage, so an edit updates one record,
  recall serves only the new text, and the old wording stays readable as
  history. `stella memory validate` reports duplicate pairs old edits left
  behind. (#729)
- **The retrieval tuning knobs are now real settings.** The
  `context.retrieval` block in settings.json configures the ranking constants
  and per-query budgets that were hard-coded; defaults are exactly the shipped
  values, and out-of-range values are clamped rather than failing the turn.
  (#729)

### Fixed

- **Forgetting a memory now actually keeps it out of the model's context.**
  `stella memory forget` only filtered at the final projection step, after the
  budget was already spent on the forgotten memory — and workspace memory
  files pasted into the system prompt were never filtered at all. Suppression
  now applies in the retrieval plane and on the system prompt. (#729)
- **The overflow error no longer recommends a command that does not exist.**
  Hitting the output-token limit advised "run /compact to shrink the context";
  `/compact` was never a command, and compaction already runs automatically
  every step. The message is corrected. (#729)

## [0.5.45] — 2026-07-26

### Performance

- **Memory recall no longer reads the whole corpus every turn.** Ranking now
  runs over metadata, with bodies and vectors fetched only for the few
  candidates that can become frames, and the pass runs on a blocking thread —
  recall stops getting slower as a workspace ages, and the recall timeouts can
  actually fire. (#725)
- **Fewer repeated passes over the transcript each step.** Redundant token
  walks and transcript copies inside the agent loop were removed, cutting
  per-step work that grew with the length of a turn. (#725)

## [0.5.44] — 2026-07-26

### Added

- **Find-in-page search for the session transcript.** `ctrl+f` opens an
  incremental, case-insensitive search bar that jumps between matches, and it
  searches full tool output rather than the drawn preview — a hit inside
  ninety collapsed lines is still found, and landing on it unfolds it. (#722)
- **Jump to failures and fold finished turns.** `ctrl+n`/`ctrl+p` cycle
  through failed steps (including rejected judge verdicts and unmet goals),
  and `ctrl+z` folds a finished turn to one summary line with a red `N failed`
  marker when something inside it broke. (#722)

## [0.5.43] — 2026-07-26

### Added

- **Every tool now ships turned on, switchable from one settings map.** Bash
  and the web tools no longer need per-capability flags; a `"tools"` map in
  settings.json takes any tool name, group name, or `*` mapped to on/off, and
  covers MCP and custom tools too. `stella tools` now names the exact key that
  switched a tool off. (#710)
- **A tools on/off editor in the TUI SETTINGS tab.** Every tool in the live
  session — including MCP and custom tools — is listed by group; toggle with
  space, save to user or project scope, and org-managed denials render locked
  instead of offering a switch that would not work. (#710)

### Fixed

- **Disabled tools could still run in process-free sessions.** The interactive
  REPL and `--no-pipeline` one-shot skipped the tool-policy gate when
  process-free authority was active, so a tool switched off in settings could
  still be called there. The policy is now enforced on that path like every
  other. (#710)

## [0.5.42] — 2026-07-26

_Internal: version-sync release re-tagging an already-shipped docs commit; no
user-facing changes._

## [0.5.41] — 2026-07-26

### Changed

- **The session transcript was redesigned for reading.** Labels used to be
  right-aligned into column 22, so the scannable left edge jittered on every
  row and a fifth of the pane went to chrome. Rows now open on a fixed
  left-edge rail of status glyphs, spacing follows block structure, collapsed
  output previews the first failing line instead of line 1, and diff
  truncation is hunk-aware with one-token edits highlighted. (#719)

### Fixed

- **Every edit to a file keeps its diff in the transcript.** Only the latest
  diff per file was retained, so a session that touched one file five times
  showed four edit rows with nothing under them; each path now remembers its
  recent diffs per edit. (#719)

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

## [0.5.39] — 2026-07-26

### Changed

- **Agent edits keep your file's identity.** `write_file`, `edit_file`,
  `apply_edits` and `save_memory` used to rename a temp file over the target,
  which replaced the inode and dropped the mode, the owner, hard links, and
  any editor watching the file. They now write in place, and Stella's own
  state files all go through one atomic, fsync-backed write helper. (#699)

### Fixed

- **Rule guards now cover the edit path the model is steered toward.**
  `guard-tool: Edit` guards `apply_edits` (previously only `edit_file`), a
  guard key with a blank value no longer counts as an enforced guard, and
  `guard-deny-path` checks every path in a multi-file edit instead of letting
  one call walk past it. (#700)
- **Interrupted runs clean up after themselves.** A dropped or cancelled turn
  used to leave `/tmp` shadow directories, stale `.git/worktrees`
  registrations, leaked MCP request slots and orphaned process groups behind.
  Each now has a synchronous guard or a start-of-run prune that reclaims
  it. (#698)

## [0.5.38] — 2026-07-26

### Added

- **`stella serve` can read its bearer token from a file.**
  `STELLA_SERVE_TOKEN_FILE` is supported and wins over the inline variable —
  an unreadable or empty file is an error, never a silent fallback — a token
  under 32 characters warns at startup, and failed auth responses are
  rate-limited. (#696)

### Fixed

- **The MCP OAuth callback no longer decides CSRF on a partial read.** The
  `state` check used to run against whatever a single 8 KiB read happened to
  contain, so a request split mid-`state` was judged on half its input. The
  request head is now read to completion, with oversized heads refused. (#696)

### Security

- **Secrets are wiped from memory when dropped.** API keys, auth prompts,
  signing seeds and the in-memory credentials table now zero their plaintext
  on drop, files Stella creates are written owner-only, and a group- or
  world-readable `credentials.toml` earns a launch warning — never a silent
  chmod of a file Stella did not create. (#696)

## [0.5.37] — 2026-07-26

### Security

- **More credentials are scrubbed from subprocess environments.** Variables
  ending in `_SECRET_KEY`, `_PRIVATE_KEY`, `_ACCESS_KEY`, `_APIKEY`,
  `_CREDENTIALS`, `_CREDENTIAL` and `_PAT` no longer reach model-controlled
  subprocesses, and git-behavior variables (`GIT_SSH_COMMAND`,
  `GIT_CONFIG_*`, `SSH_AUTH_SOCK`, ...) are stripped from every git spawn. A
  new `STELLA_SUBPROCESS_ENV_ALLOW` list re-admits exact names when a tool
  genuinely needs one. (#695)
- **The SVG sanitizer closes the `data:` URI gap.** The old check looked for
  `//` in attribute values, so `fill="url(data:...)"` sailed through and the
  `style` attribute was never inspected. URL-capable attributes (including
  `cursor` and the CSS image-valued families) now get a real scheme test,
  and the `style` attribute is dropped outright. (#695)

## [0.5.36] — 2026-07-26

### Added

- **Pause, resume and stop tasks from the fleet dashboard.** With a task
  focused, `[p]` pauses it, `[r]` resumes it, and `[x]` pressed twice stops
  it (any other key disarms, so the stop can never land on the wrong task).
  Only tasks with a live worker accept the verbs, and the grid marks paused
  and stopping states. (#691)

## [0.5.35] — 2026-07-26

### Added

- **`stella doctor` diagnoses and repairs a corrupt local store.** A corrupt
  `.stella/private/store.db` used to produce an error and nothing else. The
  new command runs a named integrity check with a clear pass/fail, and
  `--repair` quarantines the corrupt file (rename plus salvage copy, never a
  delete) so the next session starts on a fresh store. (#685)
- **`stella serve` turns can be cancelled and no longer hang forever.** A new
  `POST /v1/turns/{id}/cancel` endpoint unwinds a running turn while still
  delivering its terminal frame, and reverse requests to the client get a
  deadline (300s default, per-turn overridable, clamped to 1h) instead of an
  unbounded wait on a silent client. (#685)

### Fixed

- **An MCP server that fails every call no longer reports as Live.** Connect
  health and call health are now tracked separately, so a server that accepts
  connections but drops every tools/call backs off and escalates instead of
  looking healthy forever — and a crashed server's last stderr lines now ride
  the error message instead of being thrown away. (#685)
- **Code-graph index failures stop printing over the TUI frame.** The warning
  used to go straight to stderr, painting raw text across the rendered
  screen. It now returns inside the tool output itself, and impact analysis
  stands down to running the full suite rather than silently narrowing off a
  possibly stale graph. (#685)

## [0.5.34] — 2026-07-26

### Changed

- Every `--output-format json|stream-json` summary now leads with
  `schema_version` instead of burying it mid-object. Two of the three envelopes
  were built with `serde_json::json!`, which emits a sorted map; they are now
  structs, so the version is the first key a reader sees. Key order remains
  outside the contract — consumers must keep reading by key, not position.

## [0.5.33] — 2026-07-25

### Added

- **Machine-readable summaries declare a `schema_version`.** Every envelope
  `--output-format json|stream-json` can emit — the pipeline summary, the
  `--no-pipeline` summary, and the pre-flight error — now carries
  `schema_version: 1`, bumped only on breaking changes, so scripts can pin
  the shape safely. (#679)

### Fixed

- **Unsupported attachments degrade instead of crashing the process.** In the
  zai, anthropic, openai and bedrock adapters, an attachment type outside a
  provider's capabilities could hit an `unreachable!()` that aborts the run
  mid-turn. It now becomes a text note naming what arrived and which dialect
  could not carry it. (#676)
- **One bad timestamp no longer blanks the dashboard's activity tab.** A run
  whose start time SQLite could not parse failed the whole daily rollup.
  Such runs are now reported in a visible "undated" bucket instead of being
  dropped or faked onto the earliest day. (#682)

### Performance

- **Roughly 1,000 tokens shaved off every model call.** Both static system
  prompts opened with a hand-maintained tool catalogue that restated what the
  tool schemas in the same cached prefix already say; the duplicate lines are
  gone, keeping only the cross-tool guidance schemas cannot carry. (#678)

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

### Added

- **`stella memory forget` and `stella memory restore`.** The only way to
  remove a memory used to be citing it untruthful twice to trip quarantine.
  Forget is now one command, backed by a tombstone that also stops the same
  lesson from being re-recorded or re-entering as a mined skill; restore
  takes it back. (#671)

### Changed

- **The event stream tolerates events from newer versions of Stella.** An
  unrecognized event `"type"` used to be a hard deserialization error, so the
  vocabulary could never grow without breaking readers. Unknown types now
  decode with their payload preserved: older binaries skip them, and proxies
  or recorders can forward them intact. (#672)

### Fixed

- **Authored witness tests stay out of your working tree.** Adopting a
  winning candidate used to copy its one-run witness test into the project's
  real test suite, where the runner picked it up forever and nobody ever
  reviewed it. The witness is now withheld at adoption unless you explicitly
  pass `stella run --keep-witness`. (#670)

## [0.5.30] — 2026-07-26

### Fixed

- **`stella memory validate` now catches memories that name bare filenames.**
  A memory saying "in foo_test.rs" with no directory was reported as having no
  file anchors and never checked against the tree, so lore about deleted files
  kept being recalled on every turn. Bare filenames now resolve against a
  workspace filename index, and the rotten ones get flagged as stale. (#669)

## [0.5.29] — 2026-07-26

### Fixed

- **Asking Stella to delete a file no longer plants a failing test.** A plain
  deletion request ("remove foo.rs") was routed through witness authoring,
  which invented a test whose whole body was a panic and left it in the test
  tree, where later runs burned turns trying to satisfy it. Bare deletions now
  skip the authored witness; the verify ladder and the judge still run. (#668)

## [0.5.28] — 2026-07-25

### Added

- **Release artifacts carry build provenance, and the installer checks it.**
  Every release tarball and its `SHA256SUMS` are attested with a Sigstore
  bundle bound to the CI workflow that built them; `install.sh` verifies the
  attestation when `gh` is available, and `STELLA_REQUIRE_PROVENANCE=1` makes
  a missing or failed attestation a hard refusal instead of an info line.
  (#657)
- **This changelog.** `CHANGELOG.md` starts here, rolled on every release.
  (#656)

### Fixed

- **Nine correctness P0s from the backlog triage.** Among them: the
  runaway-loop guard was silently off for Gemini and Vertex (their recycled
  tool-call ids poisoned the detector's identity key), deeply nested SQL
  could overflow the stack and abort the process mid-turn, and the
  config-redirect route around the dotenv deny-list is closed. (#658, #647)

## Before 0.5.27

This file was introduced at 0.5.27. Earlier releases are recorded only in their
generated GitHub Release notes, at
<https://github.com/macanderson/stella/releases>. No attempt has been made to
reconstruct them here — a hand-written history of releases nobody curated at the
time would be a guess presented as a record.
