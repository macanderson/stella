# stella-cli

The shipping binary. `[[bin]] name = "stella"` over [`src/main.rs`](src/main.rs) —
the clap surface, the credential/settings/provider resolution that has to happen
before a turn can start, and the composition root that hands `stella-core` its
ports and drives `stella-pipeline`, `stella-fleet`, `stella-tui`, and
`stella-media`.

This crate is **wiring, not decisions**. Anything that could be a pure function
over owned data belongs in `stella-core`; a provider's wire dialect in
`stella-model`; a tool's behaviour in `stella-tools`. What stays is the half that
touches the process — the concrete `Clock`/`Sleeper` ([`src/runtime.rs`](src/runtime.rs)),
the real-git `CandidateWorkspacePort` ([`src/candidate_ws.rs`](src/candidate_ws.rs)),
the pipeline's ports ([`src/agent/tools.rs`](src/agent/tools.rs)), the
directory-reading `RuleSource` ([`src/rules.rs`](src/rules.rs)). `rules.rs` and
[`src/extensions.rs`](src/extensions.rs) say it outright in their module docs:
the semantics live in `stella-core`, only the I/O is here.

## Where it sits

Top of the stack, depending on every other workspace crate except `stella-serve`,
plus `contextgraph-types`/`-host`/`-trace`/`-conformance` from the external
[`context-graph-protocol`](https://github.com/macanderson/context-graph-protocol)
repo at a pinned rev. **Nothing depends on stella-cli** — no other `Cargo.toml`
names it, and there is no `lib.rs`, only the binary. A change here reaches users
immediately and reaches no other crate at all.

## Boundary — does this change belong here?

The intro's rule — wiring, not decisions — applied as a test to a planned
change: if the change needs the process (a clap flag or subcommand, an env var
or `.env` file, a TTY, a credential on disk, a spawned subprocess, or the
composition root that hands `stella-core` its ports), it lands here. If any
part of it could be a pure function over owned data, testable without a
filesystem, that part belongs in `stella-core` behind a port (AGENTS.md
invariant #2) with only the I/O half staying in this crate —
[`src/rules.rs`](src/rules.rs) and [`src/extensions.rs`](src/extensions.rs) are
the pattern to copy, and their module docs say so.

What must never be added here, and where it goes instead: decision logic
(compaction, budget, retry, hook/skill/loop policy) is `stella-core`; a
provider's wire dialect, cache posture, or pricing quirk is `stella-model`,
even when the symptom first shows up in [`src/config.rs`](src/config.rs)'s key
resolution; a tool's behaviour is `stella-tools` — this crate only constructs
and gates the registry in [`src/agent/tools.rs`](src/agent/tools.rs); anything
that draws panels or formats deck output is [`stella-tui`](../stella-tui) —
[`src/command_deck.rs`](src/command_deck.rs) bridges engine events into the
fold, it never renders. And because nothing depends on stella-cli and there is
no `lib.rs` to link, logic another crate will ever need must be born lower in
the stack — it cannot be extracted from here later without a move.

A new subcommand is never a new crate — it is a `*_cmd.rs` module here (see
"Extending it"), and the same goes for a new port implementation. A new crate
is justified only in three cases: the functionality sits behind a port/trait
and would otherwise drag heavy new dependencies into a crate that is
deliberately light (never an argument *from* here — stella-cli already links
every workspace crate except `stella-serve`); it needs a dependency direction
the current graph forbids (the reason [`stella-home`](../stella-home) exists:
`stella-store` and `stella-observatory` must share path resolution while the
observatory must not link the store); or it is a genuinely separate deliverable
with its own binary and release cadence ([`stella-serve`](../stella-serve) is
that precedent, and it is deliberately not linked here). Otherwise extend the
existing crate: a new one costs a workspace-table row, an impacted-crates
scope, CI time, and a README, and a wrong split is harder to undo than a wrong
merge. If you do add one, AGENTS.md's workspace table and the root
`Cargo.toml` members list change in the same PR.

## God files — do not add lines

The gate's file-size guard (`scripts/check-file-size.sh`) enforces a 1500-line
ratchet: a new file over the limit is a hard failure with no baseline escape,
and the five files below are grandfathered at recorded ceilings in
`scripts/file-size-baseline.txt`. They are god files — already too big, closed
to growth — and the pressure to grow them is worst in this crate, because
every feature ends in a flag or a subcommand and the path of least resistance
is always one more match arm in the file that already dispatches the command
family. Plan changes so no new line lands in them: new logic goes in a
submodule beside the file, the split this crate has already made four times —
[`src/agent/`](src/agent) (`tools.rs`, `engine.rs`, `goal.rs`, `coverage.rs`, …)
beside [`src/agent.rs`](src/agent.rs), [`src/command_deck/`](src/command_deck)
beside [`src/command_deck.rs`](src/command_deck.rs),
[`src/agent/tests/`](src/agent/tests) beside
[`src/agent/tests.rs`](src/agent/tests.rs), and
[`src/candidate_ws/`](src/candidate_ws) beside
[`src/candidate_ws.rs`](src/candidate_ws.rs) — and code you touch in a god
file is a candidate to extract into one.

| God file | Ceiling (lines) |
|---|---|
| [`src/agent.rs`](src/agent.rs) | 2270 |
| [`src/agent/tests.rs`](src/agent/tests.rs) | 1747 |
| [`src/candidate_ws.rs`](src/candidate_ws.rs) | 1629 |
| [`src/command_deck.rs`](src/command_deck.rs) | 4754 |
| [`src/fleet_cmd.rs`](src/fleet_cmd.rs) | 1506 |

A ceiling can move only via `make file-size-update`, which lands as a
reviewable baseline diff justified like any other change — treat it as an
escape hatch for an irreducible line (a module declaration in an oversized
file), never as a planning assumption.

## Layout

| Path | What it holds |
|---|---|
| [`src/main.rs`](src/main.rs), [`src/tests.rs`](src/tests.rs) | The whole clap surface (`Cli`, `GlobalArgs`, `Command` and its nested subcommand enums) and the two-phase `run()` dispatch — open it to add a command or a global flag — plus the argument-surface fence guarding it. |
| [`src/agent.rs`](src/agent.rs) + [`src/agent/`](src/agent) | Agent wiring: `run_one_shot` / `run_interactive` / `run_init`, and submodules for engine tuning (`engine.rs`), judged rounds (`goal.rs`), the session code-graph (`graph.rs`), pipeline-status projection (`outcome.rs`), headless output (`output.rs`), event persistence (`persistence.rs`), prompt assembly (`prompt.rs`), registry/port construction (`tools.rs`). |
| [`src/config.rs`](src/config.rs), [`src/settings.rs`](src/settings.rs) + [`src/settings/`](src/settings), [`src/engine_config.rs`](src/engine_config.rs), [`src/settings_check.rs`](src/settings_check.rs) | Which provider/model/key this invocation runs on; the three-scope `settings.json` merge behind it; `agent_engine_config` → per-agent resolution; and the launch-time slug validation that turns a typo into a startup warning instead of a provider `400`. |
| [`src/env_files.rs`](src/env_files.rs) | Project-scoped `.env` loading with shell-wins precedence and the execution-hijack refusal list. |
| [`src/memory.rs`](src/memory.rs) + [`src/memory/`](src/memory), [`src/contextgraph.rs`](src/contextgraph.rs) | `SessionMemory` (per-turn recall, post-turn reflection, skill auto-promotion, CGP→pipeline projection) and the session's `contextgraph-host`, which serves the in-tree workspace-memory and code-graph sources as real CGP providers. |
| [`src/rules.rs`](src/rules.rs), [`src/domains.rs`](src/domains.rs), [`src/discovery.rs`](src/discovery.rs) | Workspace-rule wiring (Tier-2 guards armed at the tool boundary), `stella init`'s domain inference, and the `tool_search`/`skill_search`/`mcp_search` tools. |
| [`src/command_deck.rs`](src/command_deck.rs) + [`src/command_deck/`](src/command_deck), [`src/subsession.rs`](src/subsession.rs), [`src/session_persist.rs`](src/session_persist.rs), [`src/claims.rs`](src/claims.rs), [`src/cache_insight.rs`](src/cache_insight.rs) | The deck driver: bridges engine `AgentEvent`s into `stella-tui`'s `Inbound` fold, runs per-prompt sub-sessions, tees every fold-relevant envelope to the resume journal, and coordinates concurrent writers by claim-on-first-write. |
| [`src/tui.rs`](src/tui.rs), [`src/interactive.rs`](src/interactive.rs), [`src/init_fx.rs`](src/init_fx.rs) | The non-deck surfaces: `render_event`'s plain streaming renderer, `ask_user`'s TTY implementation, and the `stella init` animation. |
| [`src/auth_cmd.rs`](src/auth_cmd.rs), [`src/connect_cmd.rs`](src/connect_cmd.rs), [`src/mcp_cmd.rs`](src/mcp_cmd.rs), [`src/memory_cmd.rs`](src/memory_cmd.rs), [`src/usage_cmd.rs`](src/usage_cmd.rs), [`src/fleet_cmd.rs`](src/fleet_cmd.rs), [`src/inspect.rs`](src/inspect.rs), [`src/stats.rs`](src/stats.rs), [`src/export.rs`](src/export.rs) | One module per command family. Everything but `fleet_cmd` runs without a resolved provider. |
| [`src/model_catalog.rs`](src/model_catalog.rs), [`src/credential_handoff.rs`](src/credential_handoff.rs), [`src/credential_status.rs`](src/credential_status.rs), [`src/enterprise_telemetry.rs`](src/enterprise_telemetry.rs) | The only place that knows both models.dev's provider ids and stella's (`bootstrap()` installs the catalog slug validation and pricing resolve against); launcher FD key handoff; the shared "where did this key come from" verdict for `models`/`config`/`auth list`; the managed-only operational spool. |
| [`src/arena.rs`](src/arena.rs), [`src/candidate_ws.rs`](src/candidate_ws.rs) | The arena-bench adapter (`--task-dir/--journal/--state-dir/--resume`) and best-of-N candidate isolation over detached git worktrees. |
| [`src/skill_manager.rs`](src/skill_manager.rs), [`src/agents_installed.rs`](src/agents_installed.rs), [`src/extensions.rs`](src/extensions.rs) | Disk I/O for the deck's SKILLS and INSTALLED AGENTS panes, and the `.claude/`/`.agents/` adoption sync. |
| [`src/paths.rs`](src/paths.rs), [`src/startup.rs`](src/startup.rs) | The two things `main` establishes before anything else runs. `paths` resolves every user-global anchor (home, XDG state home, the user-tier data dir, the filesystem-isolation boundary) **once** and hands it out — nothing else in the crate reads `HOME` from `std::env`, and tests redirect it per-thread instead of mutating the process environment. `startup` mints the one `StartupPhase` token the three environment-writing paths require, and marks the point where the process stops being single-threaded. |
| [`src/attachments.rs`](src/attachments.rs), [`src/accounted_call.rs`](src/accounted_call.rs), [`src/runtime.rs`](src/runtime.rs), [`src/signals.rs`](src/signals.rs) | Prompt-text → multimodal attachments, cost accounting for paid calls outside a turn, the production time ports, SIGINT/SIGTERM handling. |
| [`build.rs`](build.rs), [`tests/inspect_cli.rs`](tests/inspect_cli.rs), [`tests/fixtures/`](tests/fixtures) | `STELLA_BUILD_VERSION` (package version, or `<version>-dev.<sha>` when `STELLA_BUILD_GIT_SHA` is set) and the only integration test. Fixtures are also `include_str!`'d by `settings/context.rs` and `rules.rs`. |

## Key concepts

### The command surface is two dispatches, split by "does this need a key"

`run()` in [`src/main.rs`](src/main.rs) matches `cli.command` **twice**. The first
match returns early for every command that resolves no provider — `models`,
`tools`, `graph`, `scripts`, `storage`, `inspect`, `stats`, `usage`, `cloud`,
`telemetry`, `memory`, `mcp`, `connect`, `auth`, `observe`, `version`,
`resume --list`. Only then does it run `model_catalog::bootstrap()`,
`config::Config::load` (which can prompt for a key) and
`settings_check::validate_at_launch`. The second match handles the rest (`run`,
`arena`, `goal`, `monitor`, `fleet`, `chat`, `resume`, `config`) and lists the
first group under `unreachable!("handled before provider resolution")` — so a new
key-free command needs an arm in both places or it will not compile.

A bare `stella` is `Command::Chat`, which picks the Command Deck when `use_deck()`
holds (no `--plain`, no `STELLA_PLAIN`, both stdin and stdout TTYs); `resume`
without `--list` is deck-only and errors rather than degrading, because durable
session state is a deck feature.

Every field of `GlobalArgs` must carry `global = true`. clap accepts a plain
root-level flag only *before* the subcommand token, so a non-global field is
silently unreachable where users actually type it — `stella fleet … --budget 5`
died with "unexpected argument". `every_root_flag_is_global` in
[`src/tests.rs`](src/tests.rs) fails the suite instead, and its
corollary `no_subcommand_flag_reuses_a_global_name` reserves those names
CLI-wide — which is why `connect linear` takes `--paste-key`, not `--api-key`.

### Byte-stable prompt prefix (L-E8) — the discipline that costs money to break

[`src/agent/prompt.rs`](src/agent/prompt.rs)'s `build_system_prompt` /
`build_pipeline_system_prompt` funnel into `assemble_system_prompt`, which
appends, in fixed order, the project scripts index, the project orientation
block, the workspace memories, the exploration index, and the rendered rules
section. Everything appended must be deterministic for a given workspace state:
that prefix is what the provider's prompt cache keys on, so nondeterminism here
is a re-billing regression, not a cosmetic one. Two consequences the code
enforces on purpose:

- `append_workspace_memories` reads `.stella/memories/*.md`, `files.sort()`s by
  filename, and concatenates under a 16,000-char budget (`MEMORY_PROMPT_BUDGET_CHARS`);
  overflow is dropped with a count, never reordered. The prompt is built **once
  per session** and reused verbatim (including across `/clear`), so a memory
  saved mid-session deliberately does not appear until the next session —
  hot-injecting it would invalidate the cached prefix on every `save_memory`.
- In-progress exploration drafts are excluded from `append_exploration_index`:
  their line names the producing pid and its liveness, which flips mid-session —
  a guaranteed cache miss on every call (#639).

Both ride the volatile channel instead — [`src/memory.rs`](src/memory.rs)'s
`inject_recall_block`, which builds a `MessageRole::User` message prefixed with
`RECALL_MARKER` and inserts it at the conversation *tail* (just before the turn's
prompt when that prompt is already last), skipping insertion when the newest
marked block is byte-identical. The prefix — system prompt and replayed turns
alike — is never rewritten. `SYSTEM_PROMPT` and `PIPELINE_SYSTEM_PROMPT` are
`concat!` of literals, not `format!`, for the same reason.

### The 3-scope settings merge, and the project scope as a trust boundary

[`src/settings.rs`](src/settings.rs) merges per provider id, per field, ascending:
user `~/.stella/settings.json` → org-managed
(`/Library/Application Support/stella/settings.json` on macOS, `/etc/stella/settings.json`
elsewhere, overridable with `STELLA_MANAGED_SETTINGS`) → project
`<workspace>/.stella/settings.json`. Project wins per field. `Settings::load`
reads each scope exactly once into an immutable snapshot and hands all three to
`merge_captured_scopes`, which does **no I/O** — so the bytes that compute the
authority ceilings are the same bytes that produce the merged config.

The project scope is untrusted input from a cloned repo, so it is not a plain
last-wins overlay. Without `STELLA_TRUST_PROJECT=1` (or `STELLA_PROJECT_HOOKS=1`
for hooks alone), project `hooks`, `context_providers`, credential-routing fields
(`providers.*.base_url` / `api_key` / `api_key_env`, `mcp.registry_url`), tool
switches, and replacement prompts are restored from the trusted scopes, and
stderr names what was ignored. Repointing a built-in provider's `base_url` would
exfiltrate the real key on the first model call; an `enabled` stdio
`context_providers` entry spawns its command at admission time. Managed denials
in `ManagedAuthoritySettings` stay ceilings even after explicit trust —
`AuthorityPolicy` is monotonic and `apply_tool_ceiling` runs last.

Not everything merges per field: `context` is whole-block last-wins, and
`context_providers` merges per **entry** — field-level merging there would let a
project inherit the user scope's `egress_consent` while swapping the `url`.

### `env_files.rs` — dotenvy as an iterator, so precedence stays ours

`.env.<mode>.local` → `.env.local` → `.env`, most-specific first, from the
nearest ancestor directory that has one (never crossing out of the git repo,
never treating `$HOME` as a scope). Templates (`.env.example`, `.env.sample`,
`.env.dist`) and committed non-`.local` `.env.<mode>` files are never read.

The load path calls `dotenvy::from_path_iter` — a **non-mutating** iterator —
rather than `dotenvy::from_path`, precisely so the CLI keeps its own precedence:
`plan_assignments` skips any name already in the process environment and any name
an earlier (more specific) file already claimed, and only then does `maybe_load`
`set_var` the survivors. The live shell always wins; a malformed line is skipped
without aborting the file. Names that redirect the loader, command lookup,
interpreter startup, or the git/pager escapes (`LD_*`, `DYLD_*`, `PATH`,
`NODE_OPTIONS`, `BASH_ENV`, `GIT_SSH_COMMAND`, `GIT_CONFIG_*`, …) are refused and
reported by name — applying them would make `git clone && stella` arbitrary code
execution on the first subprocess (#553). `STELLA_NO_ENV_FILE=1` disables it all.

## Gotchas

- **`main` resets SIGPIPE to `SIG_DFL`.** Rust masks it at startup, so
  `stella tools | head` surfaces EPIPE as a `println!` panic — a SIGABRT and a
  panic dump on a routine pipe. Don't tidy away the `unsafe` block.
- **Startup order in `main` is load-bearing**: legacy `~/.stella` migration →
  managed telemetry snapshot + `StartupAuthoritySnapshot::capture` →
  `credential_handoff::consume_at_startup` (before any repo-controlled process
  can read the FD) → `env_files::maybe_load` → `restore_after_project_env` →
  `Cli::parse()`. Env-file loading must precede parsing (clap fields carry
  `env = "STELLA_MODEL"` and friends) and must follow the authority snapshot, so
  a project dotenv cannot redefine a privileged variable.
- **`set_var` is only safe where it is called** — single-threaded startup, before
  the tokio runtime exists. Tests that mutate env must hold
  `crate::test_env::lock()` across the whole mutate-read-cleanup window:
  concurrent `setenv`/`getenv` is UB on POSIX and the harness runs these modules
  on parallel threads.
- **Don't print a second error envelope.** `agent.rs` emits its JSON summary and
  can still return `Err`; `note_json_summary_emitted()` is what stops `main`'s
  catch-all from following it with a duplicate `{"status":"error",…}`. Text
  output never gets an envelope on stdout at all.
- **Adding a provider needs a parity row in the same PR** —
  [`src/config/tests.rs`](src/config/tests.rs) asserts every seeded provider has
  both a `CachePosture` and a `ReasoningPosture` row in
  `crates/stella-model/src/provider_parity.rs`.
- **`registry_options` in [`src/agent/tools.rs`](src/agent/tools.rs) is the only
  translation from settings to `RegistryOptions`**, so no path can quietly
  re-enable the shell. Hand-build `RegistryOptions` elsewhere and you made one.

## Testing

```bash
make test-cli            # = cargo test -p stella-cli
```

Almost all of it is in-crate unit tests, each declared with a plain
`#[cfg(test)] mod tests;` from the module it covers and living at
`<module>/tests.rs`: [`src/tests.rs`](src/tests.rs) (argument surface),
[`src/agent/tests.rs`](src/agent/tests.rs) (prompt assembly, provider routing,
usage completeness), [`src/config/tests.rs`](src/config/tests.rs) (key
resolution, the provider-parity matrix), plus the `settings`/`memory`
private-state and quarantine suites. [`tests/inspect_cli.rs`](tests/inspect_cli.rs)
is the sole integration test — it spawns `env!("CARGO_BIN_EXE_stella")` against a
real store, needing a built binary but no API key. No feature flags, no fixture
server; env-mutating tests must take `crate::test_env::lock()`.

## Extending it

**A subcommand:** add the variant to `Command` in [`src/main.rs`](src/main.rs)
(plus a nested `Subcommand` enum if it has sub-verbs — follow `McpCmd`/`AuthCmd`);
put the implementation in its own `*_cmd.rs` module and `mod` it in `main.rs`;
wire the arm into the **first** match in `run()` *and* the
`unreachable!("handled before provider resolution")` list in the second if it
needs no provider, or the second match alone if it does. Then
`cargo test -p stella-cli` — `clap_command_is_internally_consistent` and the
global-flag invariants run against the real `Command` tree — and document it
under [`../../website/content/docs/commands/`](../../website/content/docs/commands).

**A `settings.json` field:** add it to the type in
[`src/settings.rs`](src/settings.rs), then extend `overlay_scope` in
[`src/settings/merge.rs`](src/settings/merge.rs) with its merge rule (per field,
per entry, or whole-block — decide deliberately). If it can route credentials,
run code, or grant egress, add it to the untrusted-project restoration in
`merge_captured_scopes` and to the stderr notice too. Enums are "loud": an
unrecognized value is a hard parse error, never a silent fallback.

## See also

- [`../../AGENTS.md`](../../AGENTS.md) — "Architecture: ports, not concretions" (#7 is
  the byte-stable-prompt rule this crate owns), "Workspace layout", and the
  `.stella/` directory table. [`../../README.md`](../../README.md) is the user-facing
  command and provider reference.
- [`../../website/content/docs/commands/`](../../website/content/docs/commands),
  [`../../website/content/docs/configuration/settings.mdx`](../../website/content/docs/configuration/settings.mdx),
  [`../../website/content/docs/inference-pipeline.mdx`](../../website/content/docs/inference-pipeline.mdx)
  — the published docs this crate's surfaces must stay honest to.
- [`../../docs/design/scripts-index.md`](../../docs/design/scripts-index.md),
  [`../../docs/design/storage-map.md`](../../docs/design/storage-map.md) — the contracts
  behind `stella scripts` and `stella storage`.
