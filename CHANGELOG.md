# Changelog

What each **minor line** of Stella changed, in the words of someone who uses
the CLI. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## How this file works

**One section per minor line — not per release.** Every merge to main cuts a
patch release (see [`RELEASING.md`](RELEASING.md)), which is a fine way to ship
and a terrible way to keep a record: the 0.6 line alone was 127 releases in
nine days. Those releases are not undocumented — [the releases
page](https://github.com/macanderson/stella/releases) carries notes for every
single tag, generated from that tag's own diff. This file is the other half of
the pair: the durable, curated record, written for someone deciding whether to
upgrade rather than someone bisecting a regression.

**A section covers everything since the previous section's version.** Because
only minor and major lines are listed, `[0.7.0]` means "what you get moving
from 0.6.0 to 0.7.0" — the whole 0.6 line, not the single release that opened
it. That is also exactly the range CI drafts it from.

**CI writes this file, not contributors or coding agents.** Leave the
`[Unreleased]` section alone in your PR rather than adding a bullet to it. (The
exception is a release PR that deliberately writes its own section: the roll is
idempotent and never overwrites or duplicates a version that already has one.)
[`scripts/changelog-ai.sh`](scripts/changelog-ai.sh) drafts the section from the
series range when a minor or major release is cut, and
[`scripts/changelog-roll.sh`](scripts/changelog-roll.sh) rolls it into place. If
your change needs context the diff alone will not convey, put it in the PR
description — the drafter reads commit bodies (squash-merge PR descriptions),
not just code.

Internal refactors, test-only changes, and CI work need no action from you; they
are not user-facing and do not appear here.

<!-- Sections below are generated. See scripts/changelog-roll.sh for the rules:
     minor and major lines only, and never a heading with an empty body. -->

## [Unreleased]

## [0.9.0] — 2026-08-11

Everything since 0.8.0 — the whole 0.8 line. Verification stopped buying model
calls and started ending the turn the moment a proof lands; a turn now survives
the provider failing underneath it instead of dying with it; and the tool
surface grew a scratch state plane, environment probes, and an interactive
approval/skill/hook layer a host process can drive.

### Added

- **Verification halts on proof.** Stop-on-proof kills in-flight tools the
  moment the oracle flips, `verify_done` fires the halt rather than merely
  being tallied, and task mode has to earn its declaration (#2662). A confirmed
  `verify_done` now carries a deterministic pass (#2652).
- **A turn survives its provider.** Mid-turn model fallback re-resolves through
  the router when retries are exhausted, repairs the transcript, and continues
  the turn (#2769) — wired into the pipeline in #2826. Alongside it: a
  streaming→non-streaming fallback for hung or empty streams with a first-byte
  deadline (#2686, #2748), reactive recovery from provider context-overflow
  errors (#2680, #2752), a retry ladder that parks under sustained rate
  limiting instead of aborting (#2677, #2744), and circuit-breaker feedback
  wired so provider failover actually trips (#2673, #2734).
- **A session scratch state plane** — `save_state` / `get_state` /
  `list_state` / `delete_state`, the reference shape for the single-purpose
  tool rule (#2696, #2714).
- **Tools that answer what the environment is**: `get_environment` shares the
  system prompt's own env probes (#2758), `probe_capability` does safe PATH
  lookups (#2760), and `clear_output` is split out of `read_output` (#2717).
  `repo_status` and `project_overview` were made worth calling (#2551), with
  the three git readers turned into an explicit ladder (#2576).
- **The interactive surface a host can drive** — approval flow, `invoke_skill`,
  hook expansion, MCP auth and resources, working-set restore, and prompt
  contracts (#2787), on top of four shared prompt contracts and a byte-stable
  session-environment block (#2719).
- **A signal-consumer ledger**: every `AgentEvent` variant declares what reads
  it (#2720), generated from the tag table so totality is a compile error
  rather than a red test (#2737). Diagnostics gained a generated code registry
  with 43 codes documented and gated (#2693).
- **Ingest grew a lifecycle.** Provenance lineages, staleness alerts, and
  per-file dismissal (#2683, #2711); `--refresh` bitemporal retire-and-add with
  supersession at keep and an accountable ledger (#2708, #2728, #2731); and
  auto-promotion tiers for ingested records — pinned / scoped / retrieved
  (#2709, #2762).
- **Roles are configurable, so a stage can be ablated.** Research and plan are
  independently configurable roles (#2553), and responsibility→agent binding is
  configurable end to end (#2381, #2462).
- **`/export` produces a replayable session transcript** in the row grammar the
  page uses (#2606), scoped to one session rather than the whole workspace
  store (#2573).
- **The deck says more with less.** A `PROOF` panel that states its answer
  replaces `DONE VERIFICATION` (#2568), a context recall renders as a table
  instead of a paragraph (#2566), and the clock that will stop the run sits
  beside the money (#2488). The plain surface renders markdown, stages,
  reasoning, and git diffs (#2421, #2449).
- **ArenaBench runs locally and records who served the call.** `arena-local`
  launches a match with credentials resolved and a fresh SUT (#2655), the
  opponent's harness is measured live and its product recorded (#2522), and the
  gateway's upstream is pinned and carried through trial isolation (#2786,
  #2788).
- **A wall-clock journal axis and pipeline rung** (#2437), and tool-foundry
  proposals ranked and gated on reuse ratio (#2441).

### Changed

- **Verification is model-free.** The model verdict and the distress-guidance
  call are gone structurally, not by default — the roster rejects both keys, so
  no configuration restores them (#2588, #2615, #2584). The one remaining
  verifier-tier call authors the witness, because it creates the oracle rather
  than substituting for one (#2637).
- **Context-size accounting is anchored to provider-reported usage** rather
  than estimated locally (#2739).
- **Reflection asks the counterfactual**, not a proxy for it (#2465), and
  selects its digest instead of truncating the tail — reading tool results for
  the first time (#2460, #2494).
- **MCP wire names are injective**, and colliding routes are dropped rather
  than silently shadowed (#2675, #2729).
- **The step loop is written once** — extracted as `Engine::drive` (#2452,
  #2489).
- **No benchmark trial carries a spend or token ceiling, for any agent**
  (#2461), and a declared per-trial ceiling no longer blocks a launch (#2611).
- **Every web surface stella renders is on one instrument palette** (#2597,
  #2454, #2650).
- **The tool-first, single-purpose rule is invariant #9**, written down instead
  of enforced by habit (#2710).

### Fixed

- **A triage outage no longer routes the turn below `SingleTask`** — an
  unavailable triage call downgraded the work it could not read (#2830).
- **The witness stopped failing on the pipeline's own commits** (#2537), and
  the evidence-routing chain that failed a correct git recovery was broken open
  (#2531).
- **A candidate worktree no longer moves the graded tree's refs** (#2643), and
  an ambient `GIT_DIR` no longer retargets `project_overview` (#2561).
  Read-only roles got a truthful route to git state (#2538).
- **One roster answers who authors the witness, and a resume gets it back**
  (#2458, #2467, #2493); a stage call the remaining task clock cannot fit is
  declined (#2480), and the repair gate reads the clock that can actually stop
  the run (#2470).
- **Transcripts decode tool results and name the tool, on every surface**
  (#2528).
- **The observatory's ratings feed no longer lists turns the model never
  graded** (#2443, #2501).
- **A correct long-running service survives the agent's own exit** (#2766).
- **Two open Dependabot alerts closed** (js-yaml, h2) (#2548).

### Removed

- **`STELLA_CONFIG_DIR`**, which named no resolver and quietly declined the
  legacy-layout migration for processes sitting on the defaults (#2442, #2500).
- **`Engine::run_session_start_hooks`** — `SessionStart` has one owner, the
  host (#2674, #2727).
- **The `require_independent_verifier` residue** (#2639) and the two inert
  settings knobs left behind by the verdict removal (#2631).

## [0.8.0] — 2026-08-08

Everything since 0.7.0 — the verification and scheduling gates got sharper,
ArenaBench grew into a cloud-scale benchmarking product, and the context
layer gained one write surface instead of several.

### Added

- **ArenaBench runs on AWS Batch.** `arenabench cloud run|status|fetch` drives
  scale-to-zero cloud infrastructure, a live web app
  (arenabench.org) serves trends and a live scoreboard keyed on SUT commit,
  and a head-to-head against Sonnet 5 with Stella's pipeline off establishes
  a baseline (#2099, #2103, #2106, #2075, #2389).
- **An `outcome_reason` taxonomy** distinguishes a trial that never ran, one
  that solved and errored, and one that was never measured — so an unmeasured
  trial no longer reads as a $0.00 loss and a partial sum no longer reads as
  a seat total (#2095, #2213, #2222).
- **The trace-replay learning harness** runs the whole learning loop from
  recorded traces with zero model calls, and a local-only Claude Code
  transcript adapter feeds it (#2304).
- **A pre-plan step now refuses to start if its deadline cannot fit it**, and
  the deadline itself can say "closing" before it says "exceeded" — a step
  no longer burns budget it was never going to finish in (#2278).
- **Mined rules and promotions publish through one write surface**: both now
  emit TOML context records instead of two divergent paths (ADR 0014, #2295).

### Changed

- **Invariant #5 is enforced, not aspirational**: public errors must be
  typed, and the gate now checks it (#2394).
- **The file-size ratchet judges the change, not the tree**, and a first-time
  crossing is judged against the base too — so a file that was already over
  the line before your PR touched it doesn't get blamed on you, and one that
  crosses it for the first time can't slide through untested (#2004, #2267,
  #2405).
- **The code-graph walk honors the repository's own ignore rules** (#2360).
- **Domain-overlap recall admits again**, scoped to what the query actually
  narrowed, and the context budget now drops the least important record by
  force then precedence instead of force-filling (#2298, #2299).
- **Moving a card no longer bills a model round trip** (#2410).

### Fixed

- **`stella ingest` no longer goes silent for minutes** on a working run that
  only looked wedged (#2409).
- **Every deck lane stops wedging at `forwarder.await`** after its turn ends
  (#2291).
- **Measurement is voided whenever its producing command errored**, closing
  a gap where a verify stage could see a false pass behind an errored
  command (#2126, #2216).
- **A witness flip is measured against a pinned baseline, never a drifted
  HEAD** (#2071), and its author/repair turns are bounded in wall clock so
  one repair can't consume the whole trial's budget (#2151, #2162).
- **Supervision env vars parse as booleans**, not clap literals (#2145).
- **The brand identity reverts to the comet** — the Nebula recolor and its
  three follow-ups are undone (#2226, #2309).

## [0.7.0] — 2026-08-07

Everything since 0.6.0 — 127 patch releases over nine days, plus the release-
notes overhaul below. The verification line: a model's opinion stopped being
enough to end a run, the verifier stopped being allowed to grade its own work,
and the pipeline learned to research before it plans.

### Added

- **Release notes have a home you can actually read:
  [stella.oxagen.sh/releases](https://stella.oxagen.sh/releases).** Every minor
  line as an interactive page — filter by kind (added, changed, fixed, removed,
  security), search the full text of every entry, and expand a line to read it
  in full. Built from this file at build time, so the site and the repository
  cannot drift.
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

## [0.6.0] — 2026-07-29

Everything since 0.5.0 — the context and memory line. Recall became
accountable, memories learned what they were about, and verification stopped
paying for work it did not need.

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

0.5.0 and earlier predate this file. Their notes are on
[the releases page](https://github.com/macanderson/stella/releases), generated
per tag from each release's own diff.
