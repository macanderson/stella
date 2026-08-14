# stella-tools

The built-in tool set the agent loop calls, and the mechanisms every session
tool — built-in, custom, or MCP — runs through. Every tool implements the
[`Tool`](src/registry.rs) trait, takes model-produced JSON in, and returns a
`ToolOutput` — success, or an error whose message names the failure.
`ToolRegistry` is the crate's public face and the adapter behind
`stella-core`'s `ToolExecutor` port.

The dispatchable surface is twelve tools, in three families plus one report:

- **Sub-agent delegation** — `task` ([`src/subagent.rs`](src/subagent.rs)),
  which spawns read-only child turns through a host-attached dispatcher.
- **The session task board** — `task_create` / `task_list` / `task_start` /
  `task_complete` / `task_cancel` / `task_assign`
  ([`src/tasks.rs`](src/tasks.rs)); `task_assign` additionally queues a
  sub-agent spawn request the session driver drains.
- **Session scratch state** — `save_state` / `get_state` / `list_state` /
  `delete_state` ([`src/scratch.rs`](src/scratch.rs)), backed by a
  self-deleting `TempDir` the registry owns.
- **The environment report** — `get_environment`
  ([`src/environment.rs`](src/environment.rs)).

Everything else the model can call reaches it as a **custom script tool**
([`src/custom.rs`](src/custom.rs) — a TOML manifest beside a script, no
registry edit), an **MCP tool** (`stella-mcp`, merged above this registry by
the CLI), or a CLI session-layer tool. This crate is also where the tool
*mechanisms* live regardless of which surface a tool arrives on: operator
tool policy ([`src/policy.rs`](src/policy.rs)), skill `allowed-tools` grants
([`src/skill_grant.rs`](src/skill_grant.rs)), the tool-foundry
authorship/adoption plane ([`src/foundry_author.rs`](src/foundry_author.rs),
[`src/foundry_gate.rs`](src/foundry_gate.rs),
[`src/foundry_witness.rs`](src/foundry_witness.rs)), the extension hook
runner and its approval bridge ([`src/hook_runner.rs`](src/hook_runner.rs),
[`src/hook_bridge.rs`](src/hook_bridge.rs)), and the subprocess hygiene
every spawn path shares ([`src/subprocess_env.rs`](src/subprocess_env.rs),
[`src/exec.rs`](src/exec.rs)).

## Where it sits

Depends on `stella-protocol` (`ToolOutput`/`ToolSchema`), `stella-core` (the
`ToolExecutor` port, the hook bus, the task board, the MCP-usage ledger),
`stella-store` (the foundry adoption ledger), and `stella-home` (the
user-global custom-tools directory). It builds no binary.

`stella-cli` is the real consumer: it constructs the registry and layers
custom script tools and MCP tools around it. `stella-tui` and `stella-fleet`
depend on the crate for exactly one thing —
`subprocess_env::scrub_sensitive_env`, so their own spawns share the single
credential deny-list rather than growing a second one that drifts.

## Boundary — does this change belong here?

This crate owns the built-in tools — everything the model can invoke by name
through `ToolRegistry::execute` — and the mechanisms above. If a planned
change gives the model a new callable capability, changes what an existing
tool does, or must apply to every tool call (a hook stage, a policy check on
the dispatch path), it lands here. A new built-in tool is a new module in
this crate — implement `Tool`, register it, add its catalog line (the recipe
under Extending it) — never a new crate, and never an engine edit, because
the engine sees only `schemas()` and `execute(name, input)`.

Two neighbours take what this crate must not. Deciding *whether* or *when* a
tool runs — speculation, retry, compaction, budget, hook *matching*,
permission policy evaluation — is I/O-free engine logic and belongs in
[`stella-core`](../stella-core) behind a port (AGENTS.md invariant #2): this
crate implements `ToolExecutor`; it never drives it. A tool whose
implementation lives in an external server process is an MCP tool, reached
through [`stella-mcp`](../stella-mcp)'s client — do not teach this crate a
wire protocol just to reach a tool. Cheaper than either, a tool that needs
no Rust is a custom script tool and costs no code here (see Extending it).

## God files

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it
before it crosses: new registry logic goes in a sibling submodule
(`src/registry/approval.rs`, `src/registry/executor.rs`, and
`src/registry/validate.rs` are the precedent), with only the call site
landing in `registry.rs`.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The module list and the crate doc — what is dispatchable and what is mechanism. |
| [`src/registry.rs`](src/registry.rs), [`src/registry/approval.rs`](src/registry/approval.rs), [`src/registry/executor.rs`](src/registry/executor.rs), [`src/registry/validate.rs`](src/registry/validate.rs) | The `Tool` trait, `ToolRegistry`, construction, and the single `execute` path all cross-cutting behaviour hangs off. `approval.rs` is the `tool.call.requested` blocking policy chain plus the #2676 interactive approval flow (a `RequireApproval` parks on an injected responder with a TTL instead of dead-ending); `executor.rs` is the `ToolExecutor` port impl (`schemas`/`execute` plus the drains and aggregations the engine reads); `validate.rs` is dispatch-time input validation against the advertised schema (#3144). |
| [`src/catalog.rs`](src/catalog.rs) | The canonical tool table. Open it to add a tool or to answer "is this name taken / is it read-only". |
| [`src/subagent.rs`](src/subagent.rs) | The `task` tool: sub-agent delegation over a host-attached dispatcher (#922), with turn controls and a spend ledger the engine drains at step boundaries. |
| [`src/tasks.rs`](src/tasks.rs) | The six `task_*` tools over the session board, plus `task_assign`'s spawn queue. |
| [`src/scratch.rs`](src/scratch.rs) | The scratch state plane: `ScratchDir` and the four state tools. |
| [`src/environment.rs`](src/environment.rs) | `get_environment` and the shared environment-identity probes the CLI prompt renders from (#2697). |
| [`src/policy.rs`](src/policy.rs) | `ToolPolicy` — the operator's `"tools"` switches, resolved exact-name-first, then group, then wildcard; scope composition by union of denials. |
| [`src/skill_grant.rs`](src/skill_grant.rs) | A skill's `allowed-tools` grant as `ToolPolicy` algebra (#2682): the grant policy, per-name `operator ∧ grant` intersection, and resolution against an advertised surface. |
| [`src/custom.rs`](src/custom.rs), [`src/validate.rs`](src/validate.rs) | Developer-defined TOML script tools — lenient discovery for a session, strict validation for `stella tools --validate` — and `CustomToolSet`, the decorator that layers them over an inner executor. |
| [`src/foundry_author.rs`](src/foundry_author.rs), [`src/foundry_gate.rs`](src/foundry_gate.rs), [`src/foundry_witness.rs`](src/foundry_witness.rs) | The tool foundry: authoring a custom-tool manifest from observed shell invocations, the adoption gate that keeps self-authored tools withheld until a human adopts them (re-checked at launch), and the witness run that proves an authored tool works. |
| [`src/exec.rs`](src/exec.rs) | Shared subprocess plumbing for the two spawn paths this crate owns (custom tools, shell hooks): the capped two-stream capture, the process-group cancellation backstop (`GroupKillGuard`), and the one model-facing middle-out elision. |
| [`src/subprocess_env.rs`](src/subprocess_env.rs) | The credential deny-list and env hygiene applied as the last env mutation before any model- or repo-controlled spawn. Downstream crates use this, never a copy. |
| [`src/hook_runner.rs`](src/hook_runner.rs) | The real-I/O half of the hooks framework (`stella-core` owns matching and blocking). |
| [`src/hook_bridge.rs`](src/hook_bridge.rs) | The shell-hook → approval-flow bridge (#2684): implements the engine's `HookApprovalRoute` port over the #2676 `ApprovalBroker`. |
| [`src/input.rs`](src/input.rs) | Typed reads of a tool's JSON input (#1267) — the "absent" vs "present but wrong type" distinction the dispatch validator and the tools share. |
| [`src/agent_use.rs`](src/agent_use.rs) | The per-session agent-invocation ledger. |

## Key concepts

**One dispatch path.** `ToolRegistry::execute`
([`src/registry.rs`](src/registry.rs)) is the only way a built-in runs, and
everything cross-cutting lives in that one function, in order: the
`tool.call.requested` blocking hook chain (a `modify` decision replaces the
input, and every later stage must see the replacement); dispatch-time input
validation against the advertised schema; execution; observer events. New
cross-cutting behaviour belongs here, not sprinkled into tools.

**The catalog is the single declaration point.**
[`src/catalog.rs`](src/catalog.rs) declares every dispatchable name once,
with its `read_only` and `speculation_safe` flags and its policy group.
Everything else derives from it: the registry's expected-name pin, the
read-only partition, `custom::RESERVED_NAMES` (aliased straight to
`ALL_NAMES`, so a custom manifest cannot shadow a built-in), and the
generated per-tool reference under `docs/tools/`. It exists because those
used to be hand-maintained integers in six places: parallel PRs each bumped
the same number off the same base and squash-merged to a
plausible-but-wrong count with no conflict. The `read_only` flag is
load-bearing, not documentation — `stella-core`'s dispatch grouping and
`ReadOnlyTools` both key on it.

**Custom tools are gated, not just discovered.** A directory scan returns an
`UngatedDiscovery`, and the only way to get runnable `CustomTool`s out of it
is `foundry_gate::gate_report` — so a new discovery site cannot skip the
adoption ruling by accident (forgetting is a compile error). Hand-written
manifests pass through untouched; self-authored (foundry-provenance)
manifests register only if the workspace adopted them, a human enabled them,
and their bytes still match what their witness ran against — re-checked at
the moment of launch, not just at scan time.

## Gotchas

- **A tool's timings never go in its `ToolOutput`.** That value is what
  `stella-core`'s loop detector keys on — `exact_repeat_threshold` counts
  identical *(name + input + output)* calls and the stagnation rung counts
  byte-identical outputs from one tool — so an elapsed time in a result
  makes both rungs permanently blind for that tool.
- **`schemas()` is sorted by name deliberately.** The list is serialized
  verbatim at position 0 of the prompt prefix and `HashMap` iteration order
  is per-process randomized. Prompt caching is a byte-level prefix match, so
  an unsorted list means every process writes a divergent cache entry.
- **Three env hygiene rules before any spawn**, all in
  [`src/subprocess_env.rs`](src/subprocess_env.rs). Scrub
  `GIT_REPO_ENV_VARS`: when Stella runs from inside a git hook (the pre-push
  gate), an inherited `GIT_DIR` aims every git call at the *outer* repo.
  Scrub `FORCED_COLOR_ENV_VARS`: everything here writes to a captured pipe,
  so an inherited `CLICOLOR_FORCE=1` wraps parseable output in ANSI escapes.
  And apply `scrub_sensitive_env` as the *final* env mutation on anything
  that can run model- or repository-controlled code — a second credential
  list elsewhere drifts.
- **No lock guard may cross an `.await`.** `execute` clones the
  `Arc<dyn Tool>` out of the map before awaiting — otherwise the future
  stops being `Send`.
- **Every `pre_exec(setsid)` spawn site uses `exec::GroupKillGuard`.** A
  `setsid` child is in its own session, so Ctrl-C's SIGINT never reaches
  it; the guard is the only thing that reaps the tree when the driving
  future is dropped.
- **The scratch directory is no longer exported as `STELLA_SCRATCH`.** That
  export rode the retired built-in shell spawns. The state tools and
  `get_environment` still own and report the directory; a host that spawns
  its own subprocesses passes the path itself.

## Testing

```bash
make test-tools          # or: cargo test -p stella-tools
```

Coverage is inline `#[cfg(test)]` modules next to the code. Registry tests
construct through `ToolRegistry::new` in a fresh tempdir, so tool counts
depend on nothing in the host environment. The one integration suite,
[`tests/approval_witness.rs`](tests/approval_witness.rs), exercises the
#2676 approval flow through the crate's public surface only. The foundry
carries property tests (every detector proposal must author and round-trip
through the real manifest parser), as does the approval precedence ladder.

## Extending it

Adding a built-in tool:

1. Add a module under `src/` with a `//!` header saying what the tool is
   *for* and why it exists. Study a sibling of similar shape first.
2. Implement `Tool`: `schema()` (name, model-facing description, JSON
   Schema, honest `read_only`) and `execute(input, root)`. Return
   `ToolOutput::Error` with a message that names the failure — never panic
   on model input.
3. Register it in `ToolRegistry::new` ([`src/registry.rs`](src/registry.rs)).
4. Add exactly one line to the `catalog!` invocation in
   [`src/catalog.rs`](src/catalog.rs). Nothing else needs a count bumped.
5. Regenerate the per-tool reference (`make tool-docs-update`).
6. Write the witness test.
7. Ensure the tool follows AGENTS.md invariant #9 (tool-first,
   single-purpose) — one thing, no mode flags.

Until steps 3 and 4 agree these fail, by name rather than by an off-by-one:
`registry_advertises_exactly_the_catalog_tool_set`,
`an_undeclared_tool_fails_the_catalog_pin_by_name`,
`read_only_flags_partition_the_registry_correctly`, and
`every_registry_tool_is_reserved_against_custom_shadowing` in
`registry/tests.rs`.

A tool that needs no Rust at all is a **custom script tool** — a TOML
manifest next to a script under `.stella/tools/` or `~/.stella/tools/`,
discovered at startup with no registry edit. See
[`src/custom.rs`](src/custom.rs) and
[`../../website/content/docs/agent-tools/custom-tools.mdx`](../../website/content/docs/agent-tools/custom-tools.mdx).

## See also

- [`../../AGENTS.md`](../../AGENTS.md) — "Architecture: ports, not
  concretions" (ports and the no-I/O rule) and invariant #9 (tool-first,
  single-purpose).
- [`../stella-core/src/ports.rs`](../stella-core/src/ports.rs) — the
  `ToolExecutor` port this crate implements, and the `ReadOnlyTools` view
  built from `read_only`.
- [`../../website/content/docs/agent-tools/index.mdx`](../../website/content/docs/agent-tools/index.mdx),
  [`hooks.mdx`](../../website/content/docs/agent-tools/hooks.mdx),
  [`permissions.mdx`](../../website/content/docs/agent-tools/permissions.mdx)
  — the user-facing tool reference, the hook events the registry emits, and
  the permission model.
