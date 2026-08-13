---
id: tool-removal
title: "Removing a built-in tool — the repeatable checklist"
status: living
---

# Removing a built-in tool — the repeatable checklist

The procedure for deleting a dispatchable tool from Stella so that no source,
doc, config, or CLI surface still claims it exists. Applied first in the
2026-08 purge that reduced the built-in surface from 79 tools to the 12
subagent / task-board / scratch-state / environment tools; written so the next
removal — one tool or fifty — is the same mechanical sweep.

The compiler and the gate are the system. A removal is not "find every
mention by memory"; it is: unhook the tool at its registration and declaration
sites, then let `cargo check`, the `tool-docs` gate, and a repo-wide ripgrep
enumerate everything that still refers to it. Every step below names the file
that makes the step checkable.

## Order of operations

Work through the steps in this order — each one surfaces the work of the next.

### 1. Registration — stop advertising it

- `crates/stella-tools/src/registry.rs` (`with_backends_and_options`) — the
  base entries vec and the conditional families (web, media, issues, graph).
- `crates/stella-tools/src/registry/process_tools.rs` (`builtins`) — every
  tool that spawns a process (shell, repo, process group, project runners,
  verify).
- Late registration in `crates/stella-cli` — tools attached after
  construction (`ask_user`, skills, catalog search). `rg 'register_late|late_tools'`
  finds the sites.

### 2. Declaration — shrink the catalog

- `crates/stella-tools/src/catalog.rs` is the declaration list that
  `crates/stella-cli/src/tool_docs.rs` generates `docs/tools/` from. Remove
  the tool's declaration here or the `tool-docs` gate re-creates its page.

### 3. Implementation — delete the module, not just the wiring

- Delete the tool's impl file(s) **and** their `pub mod` lines in
  `crates/stella-tools/src/lib.rs`. An unhooked file with a live `mod` is dead
  code; a deleted `mod` with a live file is invisible to three gates
  (module-reachability exists for exactly this).
- Delete support modules whose only consumers were the deleted tools. The
  test is `rg` for the module's items across the workspace — zero remaining
  consumers means the module goes too.

### 4. Policy plumbing — the by-name enumerations

Tools are named in policy chains, not just registered. Sweep:

- `ToolRegistry::command_line_for` — enumerates every shell-reaching tool for
  the `command.started` gate.
- `registry/classify.rs`, `registry/executor.rs`, `registry/approval.rs`,
  scheduler read-only/parallel tables — any by-name branch.
- Default tool policy and settings docs (`tools.<name>` toggles).

### 5. Cross-crate references — let the compiler enumerate

`cargo check --workspace` after steps 1–4. Every downstream reference —
`stella-cli` wiring, `stella-core` by-name branches (settlement, loop
detection, prompts), `stella-tui` renderers, `stella-pipeline` stage tooling,
`stella-parity` capability rows, `stella-serve`/`stella-runtime` assembly —
either deletes with the tool or generalizes. A by-name branch on a deleted
tool is dead the moment the registry stops advertising it.

### 6. Tests

Delete the tool's own tests; repair shared tests that used it as a fixture.
Name every deleted test in the PR description — the `check-deleted-tests`
guard compares trees on the PR and an unnamed deletion reads as an accident.

### 7. CLI subcommands and flags

A subcommand or flag whose sole purpose is a removed tool (configuring it,
authenticating its backend, rendering its output) goes with it. Update the
command docs in the same change — the `command-docs` gate diffs them.

### 8. Docs — generated first, prose second

- Regenerate `docs/tools/` (`make tool-docs-update`) once the workspace
  builds; the deleted tools' TOML pages vanish and the README count follows.
- Prose sweep: `README.md`, `AGENTS.md`, `CLAUDE.md`, crate `README.md`s,
  `docs/spec/**`, `website/content/**` — every sentence that names the tool
  as present is now a bug. Rewrite honestly; don't leave "Stella can" claims
  pointing at nothing.

### 9. The repo-wide residual check

The final gate, run per removed tool name:

```sh
git ls-files -z | xargs -0 rg -l --fixed-strings '<tool_name>'
```

Every remaining hit is either (a) updated, (b) a deliberate historical
reference (an ADR, an issue postmortem) that names the tool as *removed*, or
(c) a defect. There is no fourth category. Anything discovered that cannot be
fixed in the same change is filed as a GitHub issue before finishing
(AGENTS.md § "Nothing left behind").

### 10. Gates

`make gate` — with particular attention to `tool-docs` (docs/tools parity),
`command-docs`, `doc-links`, `module-reachability`, `file-size` (deletions
retighten nothing, but splits do), and the workspace test suite.
