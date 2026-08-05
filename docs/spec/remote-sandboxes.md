---
id: remote-sandboxes
title: "Design: Remote sandboxes — one sandbox type, on your disk or someone else's"
status: proposed
---

# Design: Remote sandboxes — one sandbox type, on your disk or someone else's

**Status:** Proposed · **Date:** 2026-08-02 · **Nothing here is built.**
This document describes a destination, not the code. Where it names a
file or a type that exists today (`crates/stella-tools/src/registry.rs`,
`Tool::execute`, `stella-fleet`'s worktree port), that is the *current*
state being generalized; where it names the `Sandbox` trait, its two
implementations, or the provider protocol, that is new surface to build.

**This document also proposed deleting `crates/stella-tools/src/sandbox.rs`** —
the opt-in Seatbelt/bubblewrap confinement that wrapped the `bash` tool
and nothing else. §2 makes that case from the module's own documentation
and from `threat-model.md`. **That deletion has landed (#1300);** it is
the one part of this document that is built. It landed *ahead of* the
replacement §12 sequenced it behind — see the note in §12 for what that
costs and what fills the gap in the meantime.

---

## 1. What was asked for, as invariants

A user should be able to point a Stella session at a Modal container, an
E2B sandbox, a Daytona workspace, or a box over SSH, instead of their own
laptop. Four requirements were stated, and each of them is an invariant
that this design has to be checkable against — not a goal it aspires to:

- **I1 — No vendor dependency.** No shipped crate may name Modal, E2B,
  Daytona, or any successor in its `Cargo.toml`. Adding support for a new
  vendor must not require a Stella release.
- **I2 — Stella holds no interest in the sandbox.** The sandbox is a
  worktree that happens to be somewhere else. Stella stores nothing
  authoritative there, does not manage its lifetime beyond asking, and
  loses exactly what it would lose if someone deleted a worktree —
  no more, and nothing that was not already reproducible.
- **I3 — Identical behavior.** Every tool, every TUI surface, every rung
  of the verification ladder, every event in the fold behaves the same as
  it does today. "Local" is not a privileged mode with extra features; it
  is one implementation of the same seam.
- **I4 — Concurrent, independently placed sessions.** N sessions in N
  sandboxes at once, plus local sessions alongside them, with no shared
  mutable state between them.

Everything below is in service of those four. Section 11 is a checklist
that maps each design decision back to the invariant it protects.

---

## 2. Delete the local sandbox; isolation becomes structural

`crates/stella-tools/src/sandbox.rs` appeared to occupy this space. **It should
be deleted, not generalized** — and it has been, in #1300. The case is
short, and it is made almost entirely out of the module's own
documentation and the project's own threat model. It is kept here in the
present tense because it is the argument, and because the rest of this
document builds on it.

What it was: `SandboxMode` = `off | workspace-write | restricted`, lowered
to a Seatbelt SBPL profile on macOS and a `bwrap` argv on Linux. It is
read from **one environment variable** (`STELLA_BASH_SANDBOX`) at
**exactly one call site** (`bash.rs`). No `Settings` field, no config
key, no CLI flag. The default is `off`.

What it admits about itself, under a heading titled *"Scope, stated
honestly"*:

> Every other path to a subprocess — `build_project`/`run_tests`'s
> `command` override, `verify_done`'s `test_cmd`, the `run_script`
> index-composed line, `start_process` with a shell `argv[0]`, the
> `repo_*`/`ci_status`/issue-tool `git` and `gh` invocations, custom
> manifest tools, and hook actions — goes through `exec` or its own spawn
> and runs UNSANDBOXED even when this is set. […] `STELLA_BASH_SANDBOX`
> is a bound on one tool, not on the session.

and, on the macOS backend:

> Treat it as blast-radius reduction for accidental and prompt-injected
> writes, **not as a boundary that holds against a command written to
> break it.**

What `threat-model.md` already grades it: P8 (injected instruction drives
`bash` to exfiltrate) is *"Partial — sandbox is opt-in"*; P9
(`run_script` / `start_process`) is *"**Not mitigated by the sandbox**"*;
and R3 is titled *"The sandbox wraps `bash` only."*

So: it is off by default, covers one of the many ways Stella starts a
process, rests on an API Apple has carried a deprecation notice on for
years, is graded partial-or-nothing by our own threat model, and — per
its own tradeoff section — breaks ordinary work when enabled (`cargo`
writing `~/.cargo`, `npm`/`pip` caches, `git push` under `restricted`).
A half-boundary that users believe in is worse than a clearly absent one.

### 2.1 Isolation becomes binary and structural

Delete `sandbox.rs`, and there is no strength dial anywhere. A session is
in exactly one of two states:

```rust
#[async_trait]
pub trait Sandbox: Send + Sync { /* §4 */ }

/// Your actual tree, your actual privileges — today's shipped default,
/// which is what essentially every session runs today.
pub struct LocalSandbox  { root: PathBuf }

/// A real kernel or hypervisor boundary, reached through a provider.
pub struct RemoteSandbox { provider: ProviderId, handle: SandboxHandle }
```

There is no "sort of confined" state to document, reason about, or leak
through. You are either working in your own tree, or you are working in
a sandbox — and when you are, the boundary is the container's or the
VM's, enforced by something other than a policy file that starts with
`(allow default)`.

This is a simplification of the design as well as of the product:
`strength` no longer has to be plumbed to every exec path, which was real
work in service of a mechanism nobody should rely on.

### 2.2 Local isolation still exists — as a provider

Deleting the seatbelt path does not mean "isolation requires a cloud
account." A `docker` / `podman` provider (driven by argv, not by a crate)
is a sandbox whose host happens to be localhost. It runs offline, costs
nothing, needs no vendor, and reaches the user through the *same*
interface as Modal and E2B.

So the replacement for `STELLA_BASH_SANDBOX=restricted` is
`[sandbox] location = "docker"` — a stronger boundary, covering every
tool instead of one, through one concept instead of two. §7.1 promotes
this from a nice-to-have to the thing that makes the removal safe.

### 2.3 The honest cost

Turning on `STELLA_BASH_SANDBOX` today costs nothing: no daemon, no
image, no pull. Requiring a container runtime for local isolation raises
that floor, and someone on a laptop with no Docker who wants *some*
blast-radius reduction on `bash` does lose an option.

What they lose is mostly the feeling of it — the threat model already
grades that option "Partial", and the module already says it does not
hold against a command written to break it. But it is a real removal and
belongs in a release note, not in a footnote (§11).

### 2.4 What the removal touches

| Site | Change |
|---|---|
| `crates/stella-tools/src/sandbox.rs` | delete (597 lines: ~358 implementation, ~239 tests) |
| `crates/stella-tools/src/bash.rs` | drop the `host_argv` call site and the module docs describing it |
| `crates/stella-cli/src/enterprise_telemetry.rs` | drop `STELLA_BASH_SANDBOX` from the reported-env allowlist |
| `website/content/docs/agent-tools/permissions.mdx` | replace *"Sandboxing the shell tool"* with the sandbox-location docs |
| `README.md`, `crates/stella-tools/README.md` | drop the env-var mentions |
| `docs/spec/threat-model.md` | retire R3 and re-grade P8/P9 — the mitigation is now all-or-nothing rather than bash-only |
| `bench/harbor_adapter/tests/test_adapter.py` | drop the env passthrough assertion |

Nothing else calls `host_argv`; the seam is one function reached from one
place, which is what makes the deletion clean.

---

## 3. Where to cut

This is the whole design. Everything else follows from it.

A tool call today is a stack: the agent loop calls
`ToolExecutor::execute`, which reaches `ToolRegistry::execute`, which
dispatches to a `Tool` whose signature is

```rust
async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput;
```

and which then does its own `std::fs` and `Command::new` work against
`root`. There are four places a "somewhere else" boundary could be
inserted, and three of them are wrong.

### 3.1 Cut at `ToolExecutor` — rejected

Remote the whole tool call. This is exactly what `stella-serve` already
does (`crates/stella-serve/src/remote.rs`: `RemoteToolExecutor` emits a
reverse-RPC frame and parks until the host answers), so the machinery
exists and it is tempting.

It is the wrong altitude here, for three reasons:

1. **It puts a Stella in the sandbox.** All 59 `impl Tool` blocks would
   have to execute on the far side, which means a Stella binary in the
   image, which means version skew between the two halves of one session
   and an image rebuild on every release.
2. **It puts your credentials in the sandbox.** `web_search`,
   `github_rest`, `issue_ops`, and `tracker_auth` are tools. Remoting the
   executor sends them — and the tokens they use — to a third party's
   container. That is a direct violation of **I2**.
3. **It splits the ledgers from the fold.** The registry is not a
   dispatch table; it is also the session's file-touch ledger, its
   memory-citation ledger, its agent-use ledger, its task board, and its
   workspace probe. Those feed the host's event-sourced fold. Moving them
   across the wire makes the authority story ambiguous, against **I2**.

### 3.2 Cut at the syscall — rejected

Mount the remote tree over FUSE or NFS and run commands over SSH.
Transparent, zero code change, and it is what people will suggest first.

It dies on latency arithmetic. A local `stat` is tens of microseconds; a
cross-WAN one is tens of milliseconds — three to four orders of
magnitude. `rg` across this repo issues millions of syscalls. `cargo
build` issues far more. A single `grep_files` call would take minutes.
The transparency is real and the performance is unusable, and no amount
of caching fixes a build.

Worth naming explicitly in the docs so the question is answered once.

### 3.3 Cut at `Tool::execute`'s `root` — right layer, wrong granularity

Replace `root: &Path` with a handle the tool does its I/O through. Every
tool keeps its logic and its schema; only the primitives change.

This is the right layer. The trap is *granularity*: if the handle's verbs
mirror syscalls (`open`, `read`, `stat`, `readdir`), then a tool that
walks a tree becomes a tree walk over the network and we have rebuilt
3.2 with extra steps.

### 3.4 Cut at `Tool::execute`'s `root`, with verbs chosen by round-trip cost — **recommended**

Same seam as 3.3, with one governing rule:

> **Every verb is one round trip. No verb is implemented client-side as a
> loop over other verbs.**

The verb set is therefore chosen by what a *tool* needs as a whole
operation, not by what a filesystem offers. "Search the tree for this
regex" is a verb. "Read a directory entry" is not. A repo-wide grep is
one call whose loop runs on the far side; the bytes that cross the wire
are the matches, not the corpus.

This is what makes remote workspaces viable, and it is testable — see
§9.2, where a counting fake provider turns the rule into a CI assertion.

---

## 4. The `Sandbox` port

New trait, in `stella-core` alongside the other ports (`ports.rs` already
holds `ToolExecutor`, `Clock`, `TurnGate`, `TurnSteering` — this belongs
in the same family and for the same reason: `stella-core` names the seam,
someone else implements it).

```rust
#[async_trait]
pub trait Sandbox: Send + Sync {
    /// Location and provider — for the deck chip, the session record,
    /// and error messages that must say *where* something failed.
    fn descriptor(&self) -> SandboxDescriptor;

    // ---- path-scoped: one call, one path (or a batch of them) --------
    async fn read(&self, path: &SbPath, range: Option<LineRange>) -> SbResult<FileRead>;
    /// Batched deliberately: `code_map` and `overview` want tens of files
    /// and must not pay tens of round trips for them.
    async fn read_many(&self, paths: &[SbPath]) -> SbResult<Vec<SbResult<FileRead>>>;
    async fn write(&self, path: &SbPath, bytes: &[u8], mode: WriteMode) -> SbResult<WriteReceipt>;
    async fn remove(&self, path: &SbPath, recursive: bool) -> SbResult<RemoveReceipt>;
    async fn stat_many(&self, paths: &[SbPath]) -> SbResult<Vec<Option<Stat>>>;

    // ---- tree-scoped: the loop runs on the far side ------------------
    async fn glob(&self, q: &GlobQuery) -> SbResult<GlobResult>;
    async fn grep(&self, q: &GrepQuery) -> SbResult<GrepResult>;
    /// The `WorkspaceProbe` fingerprint, computed in place. See §6.2 —
    /// this one verb is the difference between "usable" and "unusable".
    async fn fingerprint(&self, scope: &ProbeScope) -> SbResult<TreeFingerprint>;

    // ---- process -----------------------------------------------------
    async fn exec(&self, req: ExecRequest) -> SbResult<ExecStream>;   // one-shot, streaming
    async fn spawn(&self, req: ExecRequest) -> SbResult<ProcHandle>;  // long-lived
    async fn proc_read(&self, h: &ProcHandle, clear: bool) -> SbResult<ProcOutput>;
    async fn proc_stdin(&self, h: &ProcHandle, text: &str) -> SbResult<()>;
    async fn proc_stop(&self, h: &ProcHandle) -> SbResult<ProcExit>;

    // ---- bulk transfer ------------------------------------------------
    async fn push_tree(&self, spec: &TransferSpec) -> SbResult<TransferReceipt>;
    async fn pull_tree(&self, spec: &TransferSpec) -> SbResult<TransferReceipt>;
}
```

Sixteen verbs. That number is a budget, not an observation: **a new
provider implements sixteen things, not fifty-nine**, and the ratio is
what keeps third-party adapters small enough that people actually write
them.

### 4.1 `LocalSandbox` must be the *same code*, moved

The local implementation is not a new local implementation. It is the
`std::fs` and `tokio::process` code that lives in the tools today, cut
out and pasted behind the trait. This matters for **I3**: "works exactly
the same as today" is then a refactor identity provable by the existing
test suite, rather than a property two independently-written code paths
are hoped to share.

Phase 0 (§10) ships exactly this and nothing else, precisely so that the
claim gets tested before any protocol exists.

### 4.2 Receipts carry what the registry used to `stat` for

`ToolRegistry::classify_file_op` decides create-vs-update by asking the
filesystem whether the path existed *before* the write. Naively remoted,
that is a second round trip per write, and worse, a race.

So `WriteReceipt` carries it: `existed_before`, `bytes_written`,
`line_delta`, and the post-write digest. The registry reads the receipt
instead of the disk. One round trip, no race, and — critically for the
single-emitter invariant — **the `FileChange` event is still emitted
host-side by `ToolRegistry::record_touch`**, from the receipt. The
sandbox reports facts; the host is the only thing that ever writes an
event. Nothing else in the codebase may start counting file changes from
diff text.

---

## 5. Which side each tool runs on

Not every tool is workspace-bound, and getting this table wrong is how
credentials end up in a vendor's container. Three classes:

### Sandbox-bound — execute against the tree, wherever it is

`read_file` · `write_file` · `edit_file` · `apply_edits` · `delete_file`
· `glob_files` · `grep_files` · `read_symbol` · `bash` · `run_script` ·
`run_tests` · `build_project` · `start_process` · `read_output` ·
`send_stdin` · `stop_process` · `code_map` · `project_overview` ·
`impact` · `staleness` · `validate` · `verify_done` · `diagnostics` ·
git operations in `repo` · custom tools · foundry-authored tools

### Host-bound — stay on the user's machine, never reachable from the sandbox

`web_search` · `web_extract` · `github_rest` · `issues` · `issue_ops` ·
`tracker_auth` · `cite_memory` and the memory store · `exploration` ·
context records · the task board · sub-agent dispatch · **the model call
itself** · MCP servers · user hooks

This class is the security spine. The sandbox never receives the user's
provider keys, GitHub token, `~/.stella` store, or model credentials —
not by policy, but because the tools that hold them never execute there.
A vendor that is fully compromised gets the source tree, which they
already had to have in order to run anything.

### Pure — no I/O either way

`task_*` board operations, schema gating, planning helpers.

### 5.1 The credential tension, stated honestly

An agent in a sandbox will want to `git push` or run `gh`. If
credentials are host-bound, those fail. This is a real cost, not an
oversight, and there are two answers:

- **Default: no credentials cross the boundary.** `git` inside the
  sandbox works against its own clone. Publishing happens host-side —
  either the host's `repo` tool pushes, or the diff is harvested back
  (§7.3) and pushed from the user's machine with the user's identity.
  This is also what a worktree does: your worktree has no token; your
  machine does.
- **Opt-in, scoped, and loud:**

  ```toml
  [sandbox.credentials]
  forward = ["git"]        # nothing else, ever, without naming it
  ttl = "30m"
  ```

  which mints a short-lived scoped credential, logs the grant as an
  event, and shows it on the deck chip. Never a blanket environment
  forward.

---

## 6. Latency: where this design would die, and what stops it

A turn issues roughly 10–40 tool calls. At one round trip each and 5 ms
per trip, that is 50–200 ms per turn — invisible against a model call
measured in seconds. **Round trips per turn is not the problem.
Round trips per *tool call* is.** Three specific paths would otherwise
be catastrophic, and each gets a named verb:

### 6.1 `grep`, `glob` — one verb, loop on the far side

Today these shell out to `rg` / walk the tree locally. Remoted naively,
either the corpus crosses the wire or the walk does. As verbs, the
regex crosses and the matches come back.

### 6.2 `WorkspaceProbe` — the one that decides the whole design

`crates/stella-tools/src/shell_touch.rs` fingerprints the workspace *either side
of every `bash` call*, because a shell command is an opaque string and
the ledger cannot read its intent. That is two tree walks per shell call,
and `bash` was 757 of 1,063 tool calls in the measured Terminal-Bench run
that motivated the module.

Over a network, as a client-side walk, that is fatal — hundreds of
tree traversals per session, each thousands of round trips. As the
`fingerprint` verb, it is two round trips per `bash` call and the walk
happens where the files are.

There is no version of this design that works without that verb. It is
listed here rather than in §4 because it is the load-bearing one.

### 6.3 Index builders — accepted cost, measured before optimized

`code_map`, `project_overview`, `impact`, and `staleness` build indexes
by reading many files. Expressed as `glob` + `read_many`, that is two to
a handful of round trips carrying real bytes. They run roughly once per
session rather than once per step, so Phase 1 ships them that way and
measures.

A local read-through mirror of the tree is the obvious optimization and
is **deliberately deferred**: a stale mirror is a correctness bug that
presents as a hallucination, and the invalidation story (fingerprint-
driven) should be designed against measurements rather than guesses.

### 6.4 Streaming

`bash` output streams to the deck today and must keep doing so, so
`exec` returns a stream rather than a completed result. The frame shape
is the one `stella-serve` already uses for SSE — same problem, same
answer, no second protocol.

---

## 7. The vendor boundary: providers are processes, not crates

**I1** says no vendor may appear in a shipped `Cargo.toml`. The mechanism
already has a precedent in this repo: **MCP**. `stella-mcp` spawns
third-party stdio children and speaks a small JSON protocol to them, and
no MCP server's code is in this tree. Sandbox providers work the same
way.

A **sandbox provider** is an executable that speaks the Sandbox Provider
Protocol — newline-delimited JSON-RPC over stdio, one method per verb in
§4, plus the lifecycle verbs in §7.1.

There is exactly one knob, and it is `location`. No strength, no mode —
see §2.1.

```toml
[sandbox]
location = "modal"          # "local" is the default: your tree, your privileges
max_concurrent = 4

[sandbox.providers.modal]
transport = "stdio"
command   = "stella-sb-modal"
args      = ["--app", "stella-dev"]
env       = { MODAL_TOKEN_ID = "${env:MODAL_TOKEN_ID}" }
# Passed through opaquely. Stella does not parse, validate, or understand
# these — they are the vendor's vocabulary, not Stella's.
[sandbox.providers.modal.options]
image  = "ghcr.io/acme/dev:2026-08"
cpu    = 4
region = "us-east"
```

This buys three things:

- The manifest never names a vendor, and `deny.toml`'s allow-list stays
  clean.
- Adding a vendor is publishing an adapter — in any language — not
  cutting a Stella release.
- The adapter holds the vendor SDK *and* the vendor credentials. Stella
  never sees a Modal token.

**Guard:** a CI check that fails if any shipped manifest matches a vendor
denylist. This repo already likes regression witnesses of exactly this
shape (the centralized `contextgraph-*` declaration test), and **I1** is
the kind of invariant that erodes through one well-meaning convenience
dependency.

### 7.1 Three in-tree providers, none of them vendors

- **`local`** — your tree, your privileges. The default, and the only one
  that is not really a provider.
- **`docker`** (also `podman`) — drives the CLI by argv, never a crate.
  **This one is load-bearing**, because it is what makes deleting
  `sandbox.rs` (§2) a net improvement rather than a net loss: it is the
  offline, free, no-account answer to "I want isolation on my own
  machine," and it is a real kernel boundary rather than a filesystem
  policy. It must ship in the same release that removes the seatbelt
  path — see §12.
- **`ssh`** — a protocol, not a vendor, so it costs nothing against
  **I1**. It proves the protocol is implementable against a genuinely
  remote host, serves users with a dev box, and is an offline CI target
  so the remote path is exercised on every PR without a paid account.

Vendor adapters — Modal, E2B, Daytona — live **outside this repo**, in
the style of `stella-examples`.

---

## 8. Lifecycle: Stella's stake is three verbs

**I2** in mechanism form. Stella's entire relationship with a sandbox:

- **`acquire(spec) -> SandboxHandle`** — the provider returns something
  running with the tree in place. Stella does not build images, choose
  CPU counts, install packages, or know what any of those words mean for
  a given vendor; `options` (§7) passes through opaquely.
- **`bind`** — attach a session to the handle and record the descriptor
  in the session record.
- **`release(disposition)`** — Stella *requests* `keep` or `destroy` and
  the provider decides. Stella never asserts that a sandbox must persist,
  and never treats a destroyed one as data loss.

### 8.1 Seeding the tree

Three declared modes:

- **`git`** (default) — the provider clones the origin at a named ref;
  Stella then pushes the uncommitted delta. That delta is produced by the
  pattern `crates/stella-cli/src/candidate_ws.rs` already uses for best-of-N
  shadow worktrees: `git diff --binary HEAD` plus a byte-for-byte copy of
  untracked non-ignored files. Reusing a proven mechanism, not inventing
  a transfer format.
- **`upload`** — a `.gitignore`-respecting tar via `push_tree`, for
  non-git workspaces.
- **`preexisting`** — the tree is already there; just bind. This is the
  "my sandbox is my dev box" case and the one the `ssh` provider serves.

### 8.2 Harvesting

Symmetric, and this is where **I2** pays off: **the deliverable is a
diff, not a sandbox.** `stella sandbox pull` produces the same artifact
`candidate_ws` adoption produces — a patch applied to the local tree —
or the agent commits and pushes from inside and the diff arrives through
git. Either way the sandbox is disposable at every moment, which is the
property that makes it a worktree.

### 8.3 What is lost when a sandbox dies

| Thing | Lives | Survives sandbox loss |
|---|---|---|
| Event log, `store.db`, the fold | host | yes |
| Session record, checkpoints, resume state | host | yes |
| Context records, memory, mined skills | host | yes |
| Settings, credentials, provider keys | host | yes (never left) |
| Transcript, receipts, telemetry, scoreboard | host | yes |
| **The working tree** | sandbox | **no** — as if a worktree were deleted |
| **Live processes** | sandbox | **no** — as if the machine rebooted |

One rule, stated once: **the sandbox holds only what a worktree holds.**
Everything Stella treats as authoritative is written host-side through
the same event-sourced fold as today — no new durability machinery, no
daemon, no sync loop.

---

## 9. Failure, verification, and the abstain rung

### 9.1 Failure semantics

- A verb failing with a **transport** error is retried under the
  provider's bounded policy, then surfaces as a **tool error the model
  can see** — never an engine error. This matches the existing contract
  that `ToolExecutor::execute` returns an error `ToolOutput` rather than
  `Err`.
- A workspace dying **mid-turn** ends the turn with a named failure. The
  session stays resumable; on resume Stella offers to re-acquire and
  re-seed from the last known ref plus the last harvested diff. It does
  **not** silently continue against a fresh empty sandbox.

### 9.2 The round-trip regression witness

The rule in §3.4 is only real if it is enforced. A `CountingSandbox`
test double wraps any `Sandbox` and counts verbs per tool call; a test
asserts the ceiling for each workspace-bound tool (`write_file` ≤ 1,
`bash` ≤ 3 including both probe fingerprints, `grep_files` = 1, and so
on). A future change that reintroduces a client-side loop fails a test
instead of quietly making remote sessions unusable.

### 9.3 The ladder must abstain, not fail

**This is the trap most likely to be walked into.** If the workspace is
unreachable, the diff probe and the `file_change_events` channel must
report `NothingAttempted` / `Unverifiable` — **never `passed: false`.**
A network partition is not a failed verification, and collapsing the two
is exactly the distinction the abstain rung exists to preserve. Every new
`SandboxError` path that feeds the ladder needs a test pinning it to the
abstain rung.

---

## 10. Concurrency (I4)

What has to be true for N sessions in N sandboxes:

- **The workspace handle is session-scoped.** It lives in the session's
  runtime state — never a global, never a static. Corollary, and worth
  making a lint: **no `std::env::set_current_dir`, ever.** Per-call
  `Command::current_dir(root)` is fine and is what the code already does;
  a process-global CWD is not, and would silently couple concurrent
  sessions.
- **Adapters multiplex.** One adapter process per *provider config*,
  carrying a `workspace_id` on every frame, rather than one child per
  session. Adapters that cannot multiplex declare `pool = "per-session"`
  and get a child each.
- **`stella-fleet` gets this nearly free.** Fleet already hands each task
  an isolated git worktree behind the `GitCli` port; that becomes "hands
  each task a `Sandbox`," which may be a local worktree or a remote
  container. Cooperative file locking stays necessary *within* a
  workspace and becomes redundant *across* sandboxes, which are
  physically isolated.
- **Bounded and visible.** `max_concurrent` caps paid containers against
  a runaway fan-out, and idle-timeout / max-lifetime are requested of the
  provider at `acquire`. The deck shows a **sandbox chip** per session:
  a session whose tree is in a Modal container must be visibly labeled,
  or someone will eventually reason about the wrong filesystem.

---

## 11. Invariant checklist

| # | Invariant | Protected by |
|---|---|---|
| I1 | No vendor dependency | §7 out-of-process providers; CI manifest denylist; vendor adapters live outside the repo |
| I2 | No interest in the sandbox | §5 host-bound tool class; §8 three-verb lifecycle; §8.3 durability table; §8.2 diff-not-sandbox harvest |
| I3 | Identical behavior | §4.1 `LocalSandbox` is the same code moved; §12 Phase 0 as a pure refactor proven by the existing suite; §4.2 single-emitter `FileChange` preserved |
| I4 | Concurrent sessions | §10 session-scoped handles, no global CWD, multiplexed adapters, fleet integration |

**One deliberate exception to I3, which must not hide inside it.**
Removing `sandbox.rs` (§2) is a real behavior change for the small set of
users who set `STELLA_BASH_SANDBOX`: that variable stops doing anything.
`location = "docker"` is a stronger replacement covering every tool
rather than one, but it is not a drop-in — it needs a container runtime
and an image. This belongs in a release note and a migration line in
`permissions.mdx`, not in a changelog footnote. **I3 covers the refactor;
it does not cover the removal**, and the two should ship as separate,
separately-reviewable changes (§12).

---

## 12. Phasing

The ordering constraint that mattered: **the seatbelt path is not deleted
until a replacement ships.** Phase 3 removes it, Phase 2 provides
`docker`. Landing them in the other order leaves a release where local
isolation is simply gone.

> **The ordering was not held.** #1300 landed Phase 3 first, on the
> maintainer's call, with Phases 0–2 unbuilt. The reasoning is in the
> issue: a partial boundary people rely on is worse than an absent one,
> so the overstatement was worth removing immediately rather than
> carrying until a replacement existed. The cost is exactly what this
> paragraph predicted — until Phase 2 lands there is no `location =`
> knob, and the answer for a user who wants local isolation is to run
> `stella` inside a container they start themselves (`docs` and
> `permissions.mdx` say so plainly). Phases 0–2 below are unchanged and
> still the plan; Phase 3 is done.

- **Phase 0 — the refactor, alone.** `Sandbox` trait plus `LocalSandbox`;
  `Tool::execute` takes `&dyn Sandbox` instead of `&Path`. No protocol,
  no provider, no remote config, no feature flag, and **no change to
  `sandbox.rs`** — `bash` keeps calling `host_argv` exactly as it does
  today. The existing test suite is the proof of **I3**. *This is the
  phase that de-risks everything*: if the trait cannot express today's 59
  tools without behavior change, the design is wrong, and that is far
  cheaper to discover here than after a wire protocol exists.
- **Phase 1 — the protocol.** SPP over stdio, the in-tree `ssh`
  provider, and the `CountingSandbox` round-trip assertions (§9.2).
  Feature-flagged.
- **Phase 2 — lifecycle and the local container.** `acquire`/`bind`/
  `release`, seeding and harvesting, session binding and resume, the deck
  chip, the `stella sandbox` command surface, and the **`docker`
  provider** (§7.1) — the replacement that Phase 3 depends on.
- **Phase 3 — remove `sandbox.rs`. ✅ Landed (#1300).** The deletion in
  §2.4, as its own reviewable change, with the release note and the
  `permissions.mdx` migration line. It was *not* bundled with a refactor,
  so it can still be judged and reverted on its own — but it shipped
  before Phase 2 rather than after it (see the note above).
- **Phase 4 — concurrency.** Adapter multiplexing, fleet integration,
  limits and cost guards.
- **Phase 5 — vendors.** Reference Modal and E2B adapters published
  outside this repo, plus docs.

---

## 13. Open questions

- **`screenshot` and media tools** — a container has no display. Host-
  bound, workspace-bound-and-fails, or provider-declared capability?
  Leaning toward a capability the provider advertises, so tools can
  degrade with a named reason rather than an opaque error.
- **MCP servers** — host-bound in §5, since they are the user's tools
  with the user's credentials. But an MCP server whose whole job is to
  read the workspace is then pointed at the wrong tree. Possibly such
  servers need to be launched with a workspace handle of their own.
- **User hooks** — host-side (they encode the user's machine's policy),
  but a hook that lints changed files needs the tree. Proposal: hooks run
  host-side, receive the *receipt*, and may call back through the
  workspace.
- **Network policy inside a sandbox** — §2 removes the `restricted`
  network denial along with the rest of `sandbox.rs`. A container is a
  filesystem and process boundary but is online by default, so a tree
  holding your source can still reach the network from inside one. Is
  "deny egress" a provider option (`options.network = "none"`, the
  vendor's own vocabulary), or the one policy knob Stella keeps in its
  own vocabulary because it is the exfiltration control? Leaning toward
  the former, to hold the line that Stella does not model vendor
  capabilities — but this is the strongest candidate for an exception.
- **Ownership of `.stella/` inside the tree** — project-scoped settings
  and staged tool proposals live in the tree, which is now remote, while
  the store is host-side. Which of those files are read through the
  workspace and which are host-local needs a per-path answer.
