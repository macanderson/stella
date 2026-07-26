# stella-tools

The built-in tool set the agent loop calls. Every tool implements the [`Tool`](src/registry.rs)
trait, takes model-produced JSON in, and returns a `ToolOutput` — success, or an error whose
message names the failure. `ToolRegistry` is the crate's public face and the adapter behind
`stella-core`'s `ToolExecutor` port.

This is the I/O half of the runtime: the complement to `stella-core`'s "no I/O in the engine".
Process spawning, filesystem access, and network egress belong here — and decision logic that
needs none of them (what to run, when to compact, whether to retry) does not. Two boundaries run
the other way. **No tool-specific code lives outside this crate** — the engine only ever sees
`schemas()` and `execute(name, input)`, so a new tool is never an engine edit. And **no authority
is ambient**: every tool is workspace-root-pinned, the shell and the web family are off by
default in every settings scope, and media/issue tools do not register at all without a key or a
configured backend.

## Where it sits

Depends on `stella-protocol` (`ToolOutput`/`ToolSchema`), `stella-core` (the `ToolExecutor`
port, the hook bus, the task board, the MCP-usage ledger), `stella-graph` (code graph and the
storage snapshot), `stella-store`, and `stella-media`. It builds no binary.

`stella-cli` is the real consumer: it constructs the registry, layers custom script tools and
MCP tools around it, and drives `stella connect`. `stella-tui` and `stella-fleet` depend on the
crate for exactly one thing — `subprocess_env::scrub_sensitive_env`, so their own spawns share
the single credential deny-list rather than growing a second one that drifts.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | Module list plus `resolve_within_root` — the one path-confinement primitive every tool resolves through. |
| [`src/registry.rs`](src/registry.rs), [`src/registry/process_tools.rs`](src/registry/process_tools.rs) | The `Tool` trait, `ToolRegistry`, construction (which tools register under which conditions), and the single `execute` path all cross-cutting behaviour hangs off. `process_tools.rs` is the closed list of child-process-spawning built-ins. |
| [`src/catalog.rs`](src/catalog.rs) | The canonical tool table. Open it to add a tool or to answer "is this name taken / is it read-only". |
| [`src/verify.rs`](src/verify.rs) | `verify_done` — the shadow-worktree witness gate. See below. |
| [`src/read.rs`](src/read.rs), [`src/read_symbol.rs`](src/read_symbol.rs), [`src/write.rs`](src/write.rs), [`src/edit.rs`](src/edit.rs), [`src/apply_edits.rs`](src/apply_edits.rs), [`src/delete.rs`](src/delete.rs) | File CRUD. They share one read-state ledger so an `old_string` miss can be attributed to out-of-band drift rather than to the model. |
| [`src/durable_write.rs`](src/durable_write.rs) | Durable in-place file replacement. Read the header before touching any write path. |
| [`src/file_touch.rs`](src/file_touch.rs) | The CRUD event model and per-session ledger behind the "Files Touched" panel and its telemetry. |
| [`src/grep.rs`](src/grep.rs), [`src/glob.rs`](src/glob.rs), [`src/graph.rs`](src/graph.rs), [`src/code_map.rs`](src/code_map.rs) | Text search, file search, code-graph query, and the code-map footer search results carry so the agent's next move is structural. |
| [`src/overview.rs`](src/overview.rs), [`src/gather.rs`](src/gather.rs), [`src/exploration.rs`](src/exploration.rs), [`src/staleness.rs`](src/staleness.rs) | Orientation and reusable context: `project_overview`, the `gather_context` sweep, saved exploration maps, and the per-file sha256 staleness oracle they all share. |
| [`src/exec.rs`](src/exec.rs) | The shared subprocess runner: process-group spawn, hard timeout with group kill, output truncation, and the env-scrub constants. |
| [`src/subprocess_env.rs`](src/subprocess_env.rs) | The credential deny-list applied as the last env mutation before any model- or repo-controlled spawn. Downstream crates use this, never a copy. |
| [`src/project.rs`](src/project.rs), [`src/scripts.rs`](src/scripts.rs), [`src/diagnostics.rs`](src/diagnostics.rs), [`src/impact.rs`](src/impact.rs) | Build/test/lint/format as thin verbs over the scripts index, structured typecheck output, and the importer-edge blast radius behind `run_tests` `scope: "impacted"`. |
| [`src/process.rs`](src/process.rs) | The long-running process group (`start_process`/`read_output`/`send_stdin`/`stop_process`) for servers, REPLs, watchers. |
| [`src/repo.rs`](src/repo.rs), [`src/ci.rs`](src/ci.rs) | Vendor-neutral `repo_*` tools behind the `RepoBackend` port (`GitCli` is the only adapter), and `ci_status` via `gh`. |
| [`src/bash.rs`](src/bash.rs), [`src/sandbox.rs`](src/sandbox.rs) | The opt-in shell and its opt-in OS confinement (`sandbox-exec` / `bwrap`). |
| [`src/web.rs`](src/web.rs), [`src/web_extract.rs`](src/web_extract.rs) | The opt-in web family, and the pure HTML/CSS extraction behind it (no I/O in `web_extract.rs`, so it unit-tests without a network). |
| [`src/issues.rs`](src/issues.rs), [`src/issue_ops.rs`](src/issue_ops.rs), [`src/github_rest.rs`](src/github_rest.rs), [`src/tracker_auth.rs`](src/tracker_auth.rs) | Issue-tracker tools, the backend-dispatching operations shared with the Command Deck, a minimal GitHub REST client, and the `stella connect` OAuth store. |
| [`src/media.rs`](src/media.rs), [`src/media/`](src/media) | `generate_image`/`generate_video`/`poll_video` over `stella-media`'s port, plus the always-on client-side `generate_svg` and the host-attested process-free authority marker. |
| [`src/memory.rs`](src/memory.rs), [`src/tasks.rs`](src/tasks.rs), [`src/agent_use.rs`](src/agent_use.rs) | `save_memory`/`cite_memory`, the six `task_*` tools over the session board, and the per-session agent-invocation ledger. |
| [`src/custom.rs`](src/custom.rs), [`src/validate.rs`](src/validate.rs) | Developer-defined TOML script tools — lenient discovery for a session, strict validation for `stella tools --validate`. |
| [`src/schema_gate.rs`](src/schema_gate.rs) | The pre-write storage gate that makes duplicate or misplaced schema hard to write. |
| [`src/hook_runner.rs`](src/hook_runner.rs) | The real-I/O half of the hooks framework (`stella-core` owns matching and blocking). |
| [`src/screenshot.rs`](src/screenshot.rs) | `screenshot` — capture to `.stella/screenshots/` as visual evidence a judge can demand. |

## Key concepts

**One dispatch path.** `ToolRegistry::execute` ([`src/registry.rs:467`](src/registry.rs)) is the
only way a tool runs, and everything cross-cutting lives in that one function, in order: the
`tool.call.requested` blocking hook chain (a `modify` decision replaces the input, and every
later stage must see the replacement); file-op classification, which happens *before* execution
because create-vs-update depends on whether the file exists now, not after; the storage schema
gate; the per-side-effect hook chains; execution; observer events; then the ledger drains and
the exploration-coverage hint. New cross-cutting behaviour belongs here, not sprinkled into
tools.

**The catalog is the single declaration point.** [`src/catalog.rs`](src/catalog.rs) declares
every dispatchable name once, with its `read_only` flag and what must be true for it to
register. Everything else derives from it: the registry's expected-name pins, the read-only
partition, `custom::RESERVED_NAMES` (aliased straight to `ALL_NAMES`, so a custom manifest
cannot shadow a built-in), and the counts and access markers in the published docs. It exists
because those used to be hand-maintained integers in six places: parallel PRs each bumped the
same number off the same base and squash-merged to a plausible-but-wrong count with no
conflict. The `read_only` flag is load-bearing, not documentation — `stella-core`'s speculation
gate forwards exactly the read-only set and drops everything after the first mutating call, and
`ReadOnlyTools` uses it to give a judge evidence-gathering power without write authority.

**Root pinning.** `resolve_within_root` ([`src/lib.rs:81`](src/lib.rs)) is the only confinement
check. It canonicalises the root and then normalises the join, because `Path::starts_with` is a
*lexical* comparison that never resolves `..`. The existence walk uses `symlink_metadata`
(lstat), not `exists()`: `exists()` follows symlinks and so reports `false` for a *dangling*
link, which let the old code hand back `root/link` as a brand-new in-root file — the OS then
followed the link on write and escaped the workspace. All four cases have witness tests at the
bottom of `lib.rs`.

**`verify_done` — the definition of done, as a tool.** A change is done when a witness test
fails on the previous code and passes on the new code. Either half alone is worthless: a test
that also passed before witnesses nothing. `verify_done` runs both halves and refuses anything
else — `NOT DONE` when the new code fails, `VACUOUS TEST` when the old code passes.

The half that needs care is producing "the previous version" without touching your tree. There
is no stash and no checkout. Instead a **detached shadow git worktree** is created at `HEAD` in
the temp dir, *only* the named test files are copied into it, the test command runs there, and
the worktree is removed on every exit path. Three details are the difference between a correct
verdict and a destroyed working tree:

1. **Destinations come from the canonical root-relative path, never the model-supplied string.**
   An absolute `test_files` entry would make `shadow.join(file)` discard the shadow prefix and
   resolve straight back to the real file — and `fs::copy(src, src)` truncates it, silently
   emptying the user's test file while violating the tool's central contract.
2. **The shadow mirrors the git toplevel, not the workspace root.** Destinations are relative to
   `git rev-parse --show-toplevel` and the shadow run's cwd is the corresponding subdirectory,
   so a relative `test_cmd` resolves the same package it would in the real tree. Assuming
   root == toplevel produced false `WITNESS CONFIRMED` and false `VACUOUS` verdicts whenever
   `verify_done` ran from a repo subdirectory.
3. **Shadow paths carry pid plus a process-wide counter**, because a timestamp alone can collide
   when two `verify_done` calls run concurrently.

Every `git` invocation goes through `git_in`, which strips the repo-targeting env vars and
scrubs credentials, so a surrounding git hook cannot redirect it at the outer repository. The
verdict always includes the previous-code output tail: a *compile* error is a much weaker
witness than an assertion failure, and the reader — model or judge — is told to check which it
got.

## Gotchas

- **`schemas()` is sorted by name deliberately.** The list is serialized verbatim at position 0
  of the prompt prefix and `HashMap` iteration order is per-process randomized. Prompt caching
  is a byte-level prefix match, so an unsorted list means every process writes a divergent cache
  entry.
- **`bash` is off by default in every scope** — user, org-managed, and project. It registers only
  when settings say `tools.bash: "on"`. The `web` family has the same posture, because a fetched
  page is untrusted input *and* an uncontrolled egress channel. Prefer the genuinely
  shell-free executors (`run_lint`, `format_code`, `diagnostics`, `repo_*`, the process group),
  which spawn enumerable argv and never interpret a shell string.
- **Turning `bash` off removes the shell *tool*, not the shell *capability*.** `build_project`
  and `run_tests` take a `command` override, `verify_done` a `test_cmd`, and `run_script` composes
  a line from the scripts index — all four are always-on and all four reach `bash -c` through
  `exec::run`. `STELLA_BASH_SANDBOX` does not cover them either; only the `bash` tool spawns
  through `src/sandbox.rs`. The one fence that spans the whole class is the registry's
  `command.started` policy chain (`ToolRegistry::command_line_for` enumerates every member), so a
  deployment that needs shell execution actually bounded must gate on that chain — not on the
  `tools.bash` switch. Any new tool that can reach `bash -c` must be added to that enumeration.
- **Don't reintroduce `tokio::fs::write` on a write path.** It opens with `O_TRUNC`, and a crash
  in that window left the user's own source file empty. `durable_write` rewrites **in place** —
  write, then truncate, then `fsync` — and never renames over the target, because rename swaps
  the inode and would drop the mode, change the owner, sever hard links, and leave any editor or
  watcher holding the old one (#617). Stella's *own* state files are the opposite case and go
  through `stella_store::durable::write_atomic` instead.
- **Three env hygiene rules before any spawn**, all in [`src/exec.rs`](src/exec.rs) and
  [`src/subprocess_env.rs`](src/subprocess_env.rs). Scrub `GIT_REPO_ENV_VARS`: when Stella runs
  from inside a git hook (the pre-push gate), an inherited `GIT_DIR` aims every git call at the
  *outer* repo, so a scratch `git init` re-inits the host repository — the `verify.rs` tests
  scrub them for exactly this reason. Scrub `FORCED_COLOR_ENV_VARS`: everything here writes to a
  captured pipe, so an inherited `CLICOLOR_FORCE=1` wraps `gh --json` in ANSI escapes and every
  parse dies at column 1. And apply `scrub_sensitive_env` as the *final* env mutation on anything
  that can run model- or repository-controlled code — a second credential list elsewhere drifts.
- **No lock guard may cross an `.await`.** `execute` clones the `Arc<dyn Tool>` out of the
  overlay before awaiting, and the storage-snapshot load awaits *before* the overlay merge takes
  the `std` mutex — otherwise the future stops being `Send`.
- **The opt-in `bash` tool does not go through `exec::run`.** It spawns its own child so the
  sandbox wrapper can replace the program; a change to the shared runner will not reach it.
- **`timeout_secs` is clamped.** `exec::timeout_from` treats 0 as "default" and caps at 600s, so
  a model-supplied `u64::MAX` cannot disable a tool's own hang backstop.
- **Process-free isolation omits a whole class, not a path.** When the host attests
  `HostDataIsolation::ProcessFree`, every built-in that launches a child process is skipped
  wholesale: all of `process_tools::builtins` (including `verify_done` and the `repo_*` tools),
  plus `grep`, `glob`, `gather_context`, the exploration tools, and the issue backend probe. It is
  not a filesystem sandbox and must never be inferred from path checks.

## Testing

```bash
make test-tools          # or: cargo test -p stella-tools
```

Most coverage is inline `#[cfg(test)]` modules next to the code; `registry.rs` alone carries the
hook-chain, schema-gate, and file-touch-ledger suites. Registry tests construct through
`ToolRegistry::with_issue_backend` so tool counts depend on neither the host's `gh` auth nor its
provider env keys nor any opt-in. Four integration suites live in [`tests/`](tests):
`web_integration.rs` and `tracker_integration.rs` drive the full HTTP surface against `wiremock`
(the OAuth "browser" round-trip is simulated by GETting the loopback redirect),
`media_replay.rs` exercises the media authority rules against an in-test `MediaProvider`, and
`docs_in_sync.rs` derives every expectation from `catalog.rs`. That last one skips when the
`website/` tree is absent (a vendored copy of this crate ships without its siblings) but still
fails on a missing file *inside* a present tree — renaming `index.mdx` must not silently disable
the gate.

## Extending it

Adding a built-in tool:

1. Add a module under `src/` with a `//!` header saying what the tool is *for* and why it exists.
   Study a sibling of similar shape first.
2. Implement `Tool`: `schema()` (name, model-facing description, JSON Schema, honest `read_only`)
   and `execute(input, root)`. Resolve every path through `resolve_within_root`. Return
   `ToolOutput::Error` with a message that names the failure — never panic on model input.
3. Register it in `ToolRegistry::with_backends_and_options` ([`src/registry.rs`](src/registry.rs))
   — or in [`src/registry/process_tools.rs`](src/registry/process_tools.rs) if it spawns a child
   process, delegates to another agent, or reaches a process-backed adapter, since that list is
   what a process-free registry omits wholesale.
4. Add exactly one line to the `catalog!` invocation in [`src/catalog.rs`](src/catalog.rs), inside
   the contiguous block for its `Availability`. Nothing else needs a count bumped.
5. Document it in [`../website/content/docs/agent-tools/index.mdx`](../website/content/docs/agent-tools/index.mdx)
   with the matching Read-only/Mutating marker.
6. Write the witness test.

Until steps 4 and 5 are done these fail, by name rather than by an off-by-one:
`registry_advertises_exactly_the_catalog_tool_set`, `an_undeclared_tool_fails_the_catalog_pin_by_name`,
`read_only_flags_partition_the_registry_correctly`, and
`every_registry_tool_is_reserved_against_custom_shadowing` in `registry.rs`, plus
`every_catalog_tool_is_documented_and_vice_versa` and `documented_access_markers_match_the_catalog`
in `tests/docs_in_sync.rs`.

A tool that needs no Rust at all is a **custom script tool** — a TOML manifest next to a script
under `.stella/tools/` or `~/.stella/tools/`, discovered at startup with no registry edit. See
[`src/custom.rs`](src/custom.rs) and
[`../website/content/docs/agent-tools/custom-tools.mdx`](../website/content/docs/agent-tools/custom-tools.mdx).

## See also

- [`../AGENTS.md`](../AGENTS.md) — "Architecture: ports, not concretions" (ports and the no-I/O
  rule) and "The definition of done: witness tests" (the contract `verify_done` automates).
- [`../stella-core/src/ports.rs`](../stella-core/src/ports.rs) — the `ToolExecutor` port this
  crate implements, and the `ReadOnlyTools` view built from `read_only`.
- Specs for the subsystems above: [`scripts-index.md`](../docs/design/scripts-index.md) (verb
  detection), [`exploration-sharing.md`](../docs/design/exploration-sharing.md) (saved maps, the
  staleness oracle, coverage hints), [`storage-map.md`](../docs/design/storage-map.md) (§8 is the
  pre-write schema gate).
- [`../website/content/docs/agent-tools/index.mdx`](../website/content/docs/agent-tools/index.mdx),
  [`hooks.mdx`](../website/content/docs/agent-tools/hooks.mdx),
  [`permissions.mdx`](../website/content/docs/agent-tools/permissions.mdx) — the user-facing tool
  reference, the hook events the registry emits, and the permission model.
