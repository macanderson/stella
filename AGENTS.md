# AGENTS.md

Guidance for AI agents (and humans) working in this repository. This is a
condensed orientation focused on the non-obvious conventions and invariants
that aren't immediately apparent from reading a single file. The authoritative
sources for the details behind each section are `README.md` and `CONTRIBUTING.md`.

Stella is a fast, BYOK ("bring your own key"), model-agnostic terminal coding
agent, written in Rust. Its defining contract: a task is **done** only when a
**witness test** (a test that fails on the old code and passes on the new code)
proves it — "verified done, not claimed done." It is the open-source reference
implementation of Oxagen's *Engineering Deterministic AI Coding Agents* field
manual.

---

## Essential commands

The repo is a Cargo workspace. Rust is **pinned to a concrete version**
(currently 1.97.0) via `rust-toolchain.toml` (rustup fetches it automatically).
Floating on `channel = "stable"` was tried and reverted — each new stable
release ships a slightly different rustfmt, which silently reformats
previously-clean files and turns the CI fmt gate red with zero code changes.
When bumping the pin for a new Rust release, do it as one dedicated PR that
updates the version in `rust-toolchain.toml` and runs `cargo fmt --all` in the
same commit (or the next one) so drift never accumulates. A **`Makefile`**
wraps the common commands with the correct flags — run `make help` for the
full list.

```bash
make build               # cargo build --workspace
make test                # cargo test --workspace
make format              # cargo fmt
make lint                # cargo clippy --workspace --all-targets -- -D warnings
make smoke               # compile check — runs `stella models` (no API key needed)
make help                # list every target
```

**Iterate on a single crate** (much faster than the whole workspace):

```bash
make test-core           # or: cargo test -p stella-core
make test-model          # or: cargo test -p stella-model
make test-tools          # or: cargo test -p stella-tools
```

**Watch mode** (requires `cargo install cargo-watch`):

```bash
make watch               # re-run workspace tests on every save
make watch-core          # re-test stella-core only (fastest loop)
make watch-lint          # re-run clippy on every save
```

### The gate — run before every push

A red gate is an automatic "not yet":

```bash
make gate                # = no-scratch + doc-citations + fmt --check + clippy -D warnings + test --workspace
```

`/.github/workflows/ci.yml` runs those five in one required job, plus
`scripts/check-action-pins.sh` and a release smoke build (thin LTO).
`normative-home.yml` is the second doc-facing job: it re-runs
`check-doc-citations.sh` and adds `check-normative-home.sh`, which asserts the
CGP revision pinned in `docs/**` still matches the `contextgraph-*` git rev in
`stella-cli/Cargo.toml`. Neither of those two scripts needs a Rust toolchain,
so run them by hand after touching `.github/workflows/` or a `NORMATIVE-HOME:`
document.

`doc-citations` is the guard that keeps a rustdoc comment from citing a
`docs/**.md` path — or a `§N` inside one — that does not exist (#652). Adding
a citation to a document you have not written yet fails the build, by design.

For a faster pre-push sanity check (no tests): `make check`. **Known break:**
that target's prerequisite list names an `action-pins` rule the `Makefile` never
defines, so it dies with *"No rule to make target 'action-pins'"* before running
anything — until it is defined, run `./scripts/check-action-pins.sh` alongside
`make no-scratch format-check lint`.

`no-scratch` runs first because it costs milliseconds: it asserts no tracked
file matches a `.gitignore` rule. **Session scratch must never reach the
remote** (#448) — your reflections, plans, repro trees, and memory files stay
on your disk. Add the ignore rule *and* `git rm -r --cached` the path, because
git honours ignore patterns only for paths it is not already tracking. A
failure can also mean an ignore rule is too broad to accept new files; the
script's output tells you which case you're in.

**Run `make hooks` once per clone.** It installs a `pre-push` git hook
(`core.hooksPath=.githooks`) that runs `make gate` automatically on every push
and aborts the push if it fails. The point is *when* it fails: on your machine,
in thirty seconds, instead of an hour into `ci.yml` and a review round-trip.
It is advisory and per-clone (bypassable with `SKIP_GATE=1 git push` or
`git push --no-verify`), so it complements the required server-side checks
rather than replacing them — with `enforce_admins` off, an admin or auto-merge
can still land gate-failing code, and the hook is what catches that on the
author's push. When Actions is unavailable entirely (an org billing hold has
happened before — see RELEASING.md's local-release path), it is the only gate
running at all.

Supply-chain checks run as a separate CI job: `make supply-chain` (or
`cargo deny check advisories bans sources licenses` + `cargo audit`). All four
are real gates. The license gate matters more than it looks: the workspace is
AGPL-3.0-only and dual-licensed, so a dependency carrying any further
restriction (non-commercial clause, field-of-use limit, or no license at all)
breaks both AGPL redistribution and the commercial track. **If `cargo deny`
rejects a new dependency, drop the dependency — do not widen the allow-list in
`deny.toml` without a licensing decision.**

---

## Architecture: ports, not concretions

The central architectural invariant. Every design decision in the codebase
flows from this. If a PR breaks one of these, it will be asked to restructure
regardless of how good the feature is.

**This is the normative home.** The invariants are stated here and nowhere else;
`CONTRIBUTING.md` and `README.md` point at this section rather than restating it
(they used to carry their own copies, which had already drifted — one dropped
#8 entirely). **The numbering is an address, not decoration:** Rust doc
comments, runtime error strings, and crate READMEs cite these by number, so
inserting or reordering an entry silently repoints every one of those citations.
Append; do not renumber. `scripts/check-invariants.sh` enforces both halves.

1. **Ports, not concretions.** `stella-core` never imports a provider SDK, a
   filesystem API, or a terminal library. Models go through the `Provider`
   trait (`stella-protocol`), tools through `ToolExecutor` (`stella-core::ports`).
   A new vendor or tool is an adapter, never a rewrite.
2. **No I/O in the engine.** Decision logic (compaction, eviction, loop
   detection, budget, skill selection, hook matching) is plain synchronous
   functions over owned data inside `stella-core`. That's what makes it
   property-testable. Anything that spawns processes, reads files, or hits the
   network belongs in `stella-tools`, `stella-model`, `stella-cli`, or
   `stella-store` — injected as a port/trait, not called directly.
3. **Zero telemetry egress by default.** Community/default Stella sends no
   telemetry anywhere; model-provider traffic remains the normal network
   exception selected by the user. The sole additional egress is an explicitly
   enrolled Oxagen Enterprise managed mode: a signed org-managed document may
   authorize a minimal operational rollup to one exact allowlisted HTTPS sink,
   and only while the process-free execution authority is active. Prompts,
   paths, tool payloads/results, reasoning, errors, git state, memories, rules,
   and local identifiers are never exportable. Update checks and anonymous
   analytics remain prohibited.

   This is **enforced, not assumed** — `stella-store/src/content_free.rs` holds
   the reviewed allowlist of hub `telemetry` columns and a sentinel harness
   every egress encoder registers with. Adding a hub column, or a key to an
   encoder, fails `make gate` until the allowlist is edited in the same PR, so
   a human has to answer "is this content?". A new encoder implements
   `ContentFreeEncoder` and joins `registered_encoders()`; an unbuilt drain
   format is a declared gap in `DRAIN_FORMATS`, not a silent omission. A leak
   here is a privacy incident, not a bug.
4. **Serde-first.** Every type crossing a crate boundary round-trips through
   `serde_json` byte-for-byte. Add a round-trip test when you add a type to
   `stella-protocol`.
5. **Typed errors, no panics.** Library code returns typed, named errors —
   never a bare `String`, never `.unwrap()`/`.expect()` on runtime data
   (network payloads, tool arguments, parsed source files are all runtime
   data). `unwrap` is fine in tests.
6. **Budget aborts at safe boundaries only** — never mid-tool. `run_turn`
   consults the budget guard only between model calls, never interrupts a
   tool in flight.
7. **Byte-stable prompts.** Anything that feeds the system prompt must be
   deterministic — prompt-cache hits are a feature, and nondeterminism there
   is a cost regression. Memories are loaded once per session and concatenated
   in sorted filename order; recalled context rides as a volatile message
   *after* the stable prefix (see `stella-cli/src/agent.rs::build_system_prompt`
   and `stella-cli/src/memory.rs` for the L-E8 discipline).
8. **Provider feature parity is declared, not assumed.** Providers diverge
   in sneaky ways, and this is guarded on **two axes** today in
   `stella-model/src/provider_parity.rs`:
   - **`CachePosture`** — how the prompt cache is engaged/observed
     (Anthropic's cache is explicit opt-in; DeepSeek spells its cache-hit
     telemetry differently; OpenRouter needs a request-root `cache_control`
     plus a sticky `session_id`).
   - **`ReasoningPosture`** — how reasoning/thinking is controlled on the
     wire (`Controllable`/`FixedOn`/`FixedOff`/`Unsupported`). Only Z.ai
     (`thinking`), OpenRouter (`reasoning`), Anthropic/Gemini/Vertex
     (thinking budget / `thinkingLevel`), OpenAI and now xAI
     (`reasoning[_]effort`) honor a pinned effort; the shared adapter drops
     it for `Unsupported` providers (bedrock/deepseek/local) — and a pinned
     effort against one surfaces a one-line boot notice, never a silent drop.

   Each provider id declares a posture on **every** axis and, for a
   controllable/opt-in/implicit posture, names the **witness test** proving
   it on the wire. Tests enforce each matrix from both sides: `stella-cli`'s
   config tests fail if a seeded provider lacks a row on either axis, and
   `stella-model`'s parity tests fail if a row's witness test no longer
   exists. Adding a provider — or a new divergent feature axis — means
   updating the matrix in the same PR. Born from a real defect: OpenRouter
   ran Claude models with ZERO prompt caching for months because nothing
   enforced the cache axis; the reasoning axis was added after the same
   silent-drop shape recurred for pinned `effort`.

---

## The definition of done: witness tests

Stella refuses to call a task done until a test **fails on the old code and
passes on the new** — and contributions are held to the same contract.

For a behavior change or feature, a PR should include a **witness test**:

- It **fails** on `main` without your change (the feature is genuinely absent).
- It **passes** with your change (the feature is genuinely present).

Check it the artisanal way (`git stash && cargo test -p <crate>`). Pure
refactors, docs, and CI changes don't need a witness — say so in the PR
template. If a witness is genuinely impractical (e.g. TUI rendering), explain
how you verified the change instead.

The `verify_done` tool (`stella-tools/src/verify.rs`) automates this in a
detached shadow git worktree at `HEAD` — it copies only the test files from the
working tree into the shadow, runs the suite, and expects a failure there.
**The working tree is never mutated** (no stash, no checkout). Path resolution
is derived from the canonical root-relative path, never the raw model-supplied
string (an absolute path would make the shadow copy truncate the real file).

The staged pipeline enforces the same contract at runtime: when no
`--test-command` is configured, its **witness stage** has an independent model
(the judge's resolution, never the worker) author the failing witness test up
front, tracks its fail→pass flip in the flip oracle, and refuses to credit the
flip if the worker modified the witness files (tamper exclusion). The witness
is **scaffolding for that one run**: it lives in the candidate workspace and is
discarded with it, so an already-satisfied test is never left behind in the
project's test tree. `stella run --keep-witness` promotes it instead. See
`website/content/docs/inference-pipeline.mdx` for the full stage flow, the distress-triggered guidance
loop, and the `/pipeline` deck toggle.

---

## Workspace layout — where a change goes

Fifteen crates. The one-sentence rule of thumb below routes you to the right
one; **each crate's own `README.md`** (linked from the table) then covers its
layout, invariants, gotchas, and extension recipe in depth. Read that before
changing code inside a crate you don't already know.

| You want to… | Crate | Notes |
|---|---|---|
| Change the agent loop (plan / retry / compact / budget / loop-detect / hooks / skills / rules) | [`stella-core`](stella-core/README.md) | **No I/O allowed.** Decision logic only. |
| Add/fix a model provider (SSE, tool-call dialect, pricing) | [`stella-model`](stella-model/README.md) | One file per adapter (`anthropic.rs`, `openai.rs`, `gemini.rs`, `vertex.rs`, `bedrock.rs`, `zai.rs`). Copy an existing adapter's shape. |
| Add/fix a built-in tool (`read_file`, `verify_done`, the opt-in `bash`, …) | [`stella-tools`](stella-tools/README.md) | Implement the `Tool` trait, register in `ToolRegistry`. |
| Change CLI commands, flags, or agent wiring | [`stella-cli`](stella-cli/README.md) | This is the shipping binary. |
| Change REPL rendering / panels / keybindings | [`stella-tui`](stella-tui/README.md) | Pure-fold ratatui REPL — the Command Deck, the default interactive shell on a TTY. |
| Touch shared types crossing a crate boundary | [`stella-protocol`](stella-protocol/README.md) | **Zero logic, zero I/O — types only.** |
| Persistence: executions, events, telemetry (SQLite) | [`stella-store`](stella-store/README.md) | |
| Retrieval: graph, embeddings, episodic memory | [`stella-context`](stella-context/README.md) | |
| Tree-sitter code indexing | [`stella-graph`](stella-graph/README.md) | |
| Triage → … → judge orchestration plane | [`stella-pipeline`](stella-pipeline/README.md) | |
| MCP client (external tool servers) | [`stella-mcp`](stella-mcp/README.md) | |
| Multimodal generation | [`stella-media`](stella-media/README.md) | |
| Multi-agent fan-out, worktree isolation | [`stella-fleet`](stella-fleet/README.md) | |
| The Observatory telemetry dashboard (`stella observe`) | [`stella-observatory`](stella-observatory/README.md) | Loopback-only, read-only, embedded HTML. |
| The headless engine server a host process drives over the wire | [`stella-serve`](stella-serve/README.md) | Its **own binary**, not linked into [`stella-cli`](stella-cli/README.md). Every model/tool call is remoted back to the host; the engine holds no ambient authority. Design: [`docs/design/serve-surface.md`](docs/design/serve-surface.md). |
| Context Graph Protocol (wire types / host / conformance) | external repo: [`context-graph-protocol`](https://github.com/macanderson/context-graph-protocol) | Split out of this workspace; Stella depends on it directly via git as `contextgraph-types`/`contextgraph-host` at a pinned rev. Stays dependency-light by contract. |

**Status — what ships.** The live runtime path is
`stella-cli` → `stella-core` → `stella-model` / `stella-tools` / `stella-store` /
`stella-context` (recall only) / `stella-mcp`, and the CLI also drives
`stella-pipeline` (the default `stella run` path), `stella-fleet` (`stella fleet`),
`stella-tui` (the Command Deck, the default interactive shell on a TTY), and
`stella-media` (image generation via the `generate_image` tool). The fuller
`stella-graph` retrieval + context plane (`stella init` builds the code-graph
index; recall fans out through the CGP host) is also wired. `stella-serve` is
the exception: it builds its own binary and nothing in `stella-cli` links it,
so a change there never reaches a `stella` user.

---

## The `.stella/` directory (per-workspace state)

The CLI reads and writes a `.stella/` directory at the workspace root. An agent
editing Stella's own code should know what lives where:

| Path | Purpose |
|---|---|
| `.stella/memories/*.md` | Durable lessons baked into the byte-stable system prompt prefix. Sorted by filename, loaded once per session. (Write side: the `save_memory` tool.) |
| `.stella/skills/<slug>/SKILL.md` | Auto-promoted skills from recurring reflection lessons. Never enforced — selected and injected as volatile context. |
| `.stella/tools/*.toml` | Developer-defined custom script tools. Also scanned at `~/.stella/tools/`. |
| `.stella/settings.json` | Project-scope provider config (overrides built-ins or defines new providers) and tool switches (`tools.bash: "on"` opts the shell tool in — it is off by default in every scope). Merged per-field with org-managed and user scopes. |
| `.stella/mcp.toml` | MCP server config — extra tools merged into the registry at session start. |
| `.stella/domains.toml` | Domain taxonomy for memory/reflection tagging, inferred by `stella init`. |
| `.stella/workspace.json` | Durable per-workspace telemetry identity (`workspace_id`), written by `stella cloud register`. Deliberately **outside** `private/` and safe to commit — sharing it makes every clone/machine report under one `workspace_id` to a cloud org. |
| `.stella/private/` | Owner-only generated local state (`0700`; files `0600`). The generated `.stella/.gitignore` excludes this whole directory. |
| `.stella/private/reflections.jsonl` | Per-turn reflection mining log (one JSON object per line). |
| `.stella/private/store.db` | Canonical local SQLite telemetry (executions, events, cost/tokens). Community/default has zero telemetry egress; an enrolled Enterprise seat may derive only the documented content-free operational rollup. Retention is opt-in via `stella stats prune` (`Store::prune`): dropping an execution explicitly cascades to the 13 tables keyed off `executions.id` — the schema declares no foreign keys — and never destroys telemetry the usage hub has not replicated yet without `--force`. |
| `.stella/private/context.db` | Recallable memories, episodes, facts, and temporal context. |
| `.stella/private/codegraph.db` | Tree-sitter code-graph index, built on `stella init`. |
| `.stella/private/fleet.db` | Fleet run, attempt, commit, and spend ledger. |
| `.stella/private/mcp_oauth.json` | MCP OAuth tokens. Secret local state; never commit it. |

Older releases wrote these private artifacts directly under `.stella/`. Path
resolvers migrate a safe, closed legacy file into `.stella/private/`; unsafe
permissions or live SQLite WAL/SHM sidecars fail closed with an actionable
error and leave the legacy files untouched.

Everything **user-global** lives under `~/.stella` on every platform (like
Claude Code's `~/.claude`) — no OS-specific data dir. `STELLA_HOME` moves the
whole home; the narrower `STELLA_DATA_DIR` / `STELLA_CONFIG_DIR` still win
where they always did. Key entries: `settings.json`, `credentials.toml`,
`skills/`, `agents/`, `rules/`, `tools/` (config); `usage.db` (the
cross-project telemetry hub), `sessions/`, `notifications/`, `catalog.db`,
`enterprise-telemetry.db`, `installation-id`, `cloud.json` (data). On first
run the CLI migrates the legacy split layout (platform data dir +
`~/.config/stella`) into `~/.stella`, per-entry and best-effort — an entry
that already exists at the new home is never overwritten.

| Global path | Purpose |
|---|---|
| `~/.stella/usage.db` | Cross-project **telemetry hub**: full-fidelity per-call rows replicated from every project's `store.db` via a durable per-project cursor, scoped by `org_id`/`workspace_id`/`repo_id`. Reads never touch project stores. |
| `~/.stella/cloud.json` | Stub cloud-account registration: `org_id` (+ a reserved `oauth_token` slot for the future login). `org_id`/`workspace_id` are NULL until `stella cloud register`. |

`stella usage report` reads the hub (per org/provider/model totals); `stella
usage sync [--all]` replicates above the cursor and heals projects whose
best-effort end-of-turn sync failed; `stella cloud status|register` manages
the identity that scopes it.

---

## Glossary — the identifiers that look alike

Five different ids in this workspace can all be read as "one thing the agent
did", and they are genuinely distinct entities owned by different crates. The
join keys are correct today (`stella-observatory/src/db.rs` joins both
`execution_id` and `run_id`), so this is a naming hazard, not a bug — but read
this before assuming two of them mean the same thing:

| Term | Identifier | Owner | What it is |
|---|---|---|---|
| **session** | `SessionRecord::id` | `stella-store/src/sessions.rs` | One run of the CLI, tracked in the cross-process registry under `~/.stella/sessions/`. Stamped onto `executions.session_id` (schema v8) so `Store::session_events` can reassemble a session's whole journal across its turns. |
| **execution** | `execution_id` | `stella-store/src/ddl.rs` | One row in the `executions` table — the store's unit of work (one goal/turn) with its prompt, provider/model, outcome and cost. The foreign key every child telemetry table hangs off. |
| **turn** | `turn_instance` | `stella-protocol/src/event.rs` | One `run_turn` — a prompt through the model/tool loop to an answer. Monotonic per session; groups the steps of that turn in `step_manifest`/`step_receipt`. In the store one turn is one execution. |
| **step** | `(step, call_seq)` | `stella-protocol/src/event.rs` | One iteration inside a turn: one model call plus the tools it requested. `call_seq` disambiguates the several calls that can share a `(turn_instance, step)` — the engine's worker call is 0, the overflow summarizer and the pipeline's triage/judge/plan/guidance roles take 1, 2, … |
| **fleet run** | `run_id` | `stella-fleet/src/ledger.rs` | One multi-agent fan-out, top of the fleet hierarchy: run → task → attempt → commits/spend. **Not** an `execution_id` and **not** a session. |
| **task** | `TaskId` / `tasks` row | `stella-fleet/src/plan.rs`, [`stella-store`](stella-store/README.md) | Two things that share a word: in the fleet ledger, one unit of work dispatched to a worker within a run; in the store, one row of the agent's own task-board snapshot, keyed `(session, task id)` and mirrored from `TaskUpdate` events. |

---

## Code style and conventions

- **`rustfmt` settles all formatting** — default config, no arguments. Don't
  hand-format. CI runs `cargo fmt --check`.
- **Clippy at `-D warnings`** across all targets. Do **not** `#[allow]` your way
  past a lint without a comment saying why the lint is wrong *here*.
- **Name things for what they are, not what they were.** If you rename a
  concept, chase it through comments and docs in the same PR — stale comments
  are treated as bugs in review.
- **Doc comments on public items**, and on any function whose *why* isn't
  obvious from its body. No comments that narrate the next line.
- **No new dependencies casually.** Every new crate in `Cargo.toml` gets a
  sentence in the PR description justifying it.
- **Match the neighborhood.** Every crate has an established idiom — copy the
  patterns around you before inventing new ones. The module-level doc comment
  (`//!`) is the established entry point for each file; study a sibling before
  writing a new one.
- **Edition 2024, MSRV 1.90.** Workspace deps are centralized in the root
  `Cargo.toml` `[workspace.dependencies]` — reference them as
  `serde.workspace = true` in per-crate manifests.

### Commits

[Conventional Commits](https://www.conventionalcommits.org), with the crate or
surface as the scope, matching the existing history:

```text
feat(stella-model): add mistral provider adapter
fix(stella-tui): restore terminal on panic in raw mode
docs(readme): correct provider table
ci(release): sign macOS binaries
```

Commits must be **DCO-signed** (`git commit -s`). One logical change per PR.

### Closing the issue on merge

Referencing an issue as `(#367)` in the **PR title** does not close it. GitHub
never parses the title for closing keywords — only the PR *description* and
*commit messages*. This repo accumulated a backlog of already-shipped issues
that stayed open for exactly that reason, so treat it as a hard rule:

> Put `Closes #N` in the PR description **and** as a trailer on a commit.

Both are required because the two merge paths read different text:

- **Squash** (the default here) composes the commit body from
  `COMMIT_MESSAGES`, *not* the PR body — so a `Closes #N` that exists only in
  the description never reaches the commit.
- **Rebase** (also enabled) replays your commits verbatim; the PR body is
  likewise never turned into a commit message.

The PR description's link closes the issue through GitHub's linked-issue
mechanism, and the commit trailer closes it through commit-message parsing on
the default branch. Belt and braces — either one alone is a silent single point
of failure, and the failure mode is invisible until someone audits the backlog.

```text
fix(stella-core): stop the step loop spinning on a wedged tool

The dispatch timeout never armed for tools that block before their first
poll, so a headless run could hang forever.

Closes #367
Signed-off-by: Ada Lovelace <ada@example.com>
```

Use `Closes` for bugs and completed features, `Refs #N` when a PR advances an
issue without finishing it — `Refs` deliberately does not close. One issue may
be closed by exactly one PR; if a fix spans several, close on the last and
`Refs` the rest.

---

## Testing approach

- **Property tests** for pure engine logic (`proptest`): loop detection,
  retry history, skill selection, and the task board (`stella-core`), plus
  retrieval fusion (`stella-context`), fleet planning (`stella-fleet`),
  witness verification (`stella-pipeline`), and render/scroll (`stella-tui`).
  These run on every `cargo test`. Compaction, eviction, and budget
  arithmetic are covered by unit tests, not properties — a property test for
  them is a welcome contribution.
- **Witness tests** for features — see above.
- **Wiremock-based adapter tests** for provider SSE parsing and HTTP error
  classification (`stella-model`, `stella-mcp`, `stella-media`).
- **Integration tests** with fixture MCP servers (`stella-mcp/tests/`).
- **Replay fixtures** for pipeline stages (`stella-pipeline/tests/`).

When iterating, run a single crate's tests — `cargo test -p stella-core` is
seconds; `cargo test --workspace` rebuilds everything.

---

## Gotchas

- **`Cargo.lock` is tracked.** Stella ships a binary and `install.sh` builds
  with `--locked`, so the lockfile must be committed and reproducible.
- **`.cargo/config.toml` is gitignored** — it holds per-developer cargo aliases
  (`tc` = test stella-core, etc.). It's not committed.
- **Settings 3-scope merge**: user → org-managed (`STELLA_MANAGED_SETTINGS`) →
  project (`.stella/settings.json`). Project wins per-field.
- **`context.db` vs `codegraph.db`**: `stella-context` and `stella-graph` used
  to share `.stella/private/context.db` — they now use separate files
  (`.stella/private/context.db` and `.stella/private/codegraph.db`
  respectively). Don't revert this.
