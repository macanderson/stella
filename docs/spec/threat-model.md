---
id: threat-model
title: "Stella Threat Model"
status: living
---

# Stella Threat Model

Status: descriptive — this documents the posture as shipped, not a plan

## Purpose, and why this document did not exist

Stella's security reasoning has always been written down. It was just never
written down *together*: the project trust boundary is argued in
`crates/stella-cli/src/settings/merge.rs`, the authority ceiling in
`settings/authority.rs`, the subprocess scrub in
`crates/stella-tools/src/subprocess_env.rs`, the filesystem identity checks in
`crates/stella-store/src/private.rs`, and the dotenv refusals in
`crates/stella-cli/src/env_files.rs`. Each is careful. None of them can tell you
whether the set is *complete*, because none of them enumerates the assets or
the adversary.

`SECURITY.md` is a disclosure policy plus an in-scope/out-of-scope list.
`docs/spec/enterprise-authority-telemetry.md` is an invariants list scoped to
the managed plane. `AGENTS.md` invariant 3 covers telemetry egress. This
document is the missing middle: assets, adversaries, boundaries, and the
attack paths that cross them — including the ones Stella deliberately does not
defend against.

The most useful section is probably [Residual risk](#residual-risk). A threat
model that only lists wins is marketing.

## Method and scope

Scope is the Stella CLI, its crates, and the state it writes on a developer
workstation. Out of scope: vulnerabilities in model providers themselves, the
security of code the user chooses to run, and the Oxagen Enterprise control
plane (covered by its own design doc).

The framing throughout is a single question: **what can a repository do?**
Stella's defining exposure is that `git clone && stella` runs an agent inside
attacker-authored content. Almost every boundary below exists to answer that.

## Assets

| Asset | Where it lives | Why an attacker wants it |
|---|---|---|
| Model provider credentials | env, `~/.stella/credentials.toml`, `settings.json` | Directly monetizable; also the pivot for exfiltrating everything the agent reads |
| Web/session auth | `~/.stella/web_auth.toml` | Authenticated access to the user's internal services |
| MCP OAuth tokens | `.stella/private/mcp_oauth.json` | Third-party account access |
| Source under the workspace | the checkout | Confidentiality; also the integrity target for a supply-chain edit |
| Execution history | `.stella/private/store.db` | Full prompt text (`executions.prompt`) and full event payloads (`events.payload`) — the richest single record of the user's work |
| Cross-project telemetry | `~/.stella/usage.db` | Content-free by construction, but reveals project names and spend |
| Ambient authority on the host | env, ssh agent, git config | Escalation beyond the workspace |
| The user's compute and spend | provider accounts | Cryptomining, spend exhaustion |

## Adversaries

**A1 — Hostile repository.** The primary adversary. Supplies
`.stella/settings.json`, `.stella/mcp.toml`, `.stella/tools/*.toml`,
`.stella/{commands,agents,skills}`, `package.json` scripts, `Cargo.toml`
members, `.env` files, and every byte the agent reads. Cannot execute code
until Stella gives it a way to.

**A2 — Prompt injection via retrieved content.** Text the agent reads — a
file, a fetched page, a tool result, an issue body — that attempts to steer it
into taking an action on the attacker's behalf. Distinct from A1 in that it
attacks the *model*, not the config loader.

**A3 — Local unprivileged co-tenant.** Another uid on the same machine, or a
process running as the user, racing Stella's state files or planting files at
paths Stella will write.

**A4 — Network position.** Can observe or redirect outbound traffic. Largely
delegated to TLS.

**A5 — Malicious dependency.** In Stella's own supply chain, or in the
workspace's. Out of scope for Stella's own defenses beyond `deny.toml` and
release integrity.

Explicitly **not** modeled: an adversary with root, or with the user's own
shell. Stella's private-state hardening is about *identity* (this file is
mine, not something planted here), not about defending against someone who
already is the user.

## Trust boundaries

### B1 — Repository content → configuration authority

The load-bearing boundary. `Settings::load`
(`crates/stella-cli/src/settings/merge.rs:205`) merges user, managed, and project
scopes per provider id and per field, and the project scope is treated as
untrusted input.

Dropped from an untrusted project scope: `hooks`, `context_providers`,
per-provider `base_url` / `api_key` / `api_key_env`, `mcp.registry_url`,
any `tools.*` entry set to `"on"`, and agent `prompt` overrides. Never
read from the project scope at any trust level: the `authority` block
(`#[serde(skip)]`) and `enterprise_telemetry`.

Honored from an untrusted project: model, effort, sampling, `allowed_models`
— they carry no credential routing — and `tools.*` set to `"off"`, because
lower authority may always narrow.

The asymmetry in that last pair is the whole design: **lower-precedence input
may narrow authority but never widen it.**

Trust is granted by `STELLA_TRUST_PROJECT=1` (`settings.rs:718`). See
[R1](#r1--trust-is-an-env-var-not-a-decision-the-user-is-asked-to-make).

### B2 — Managed ceiling → everything below it

`AuthorityPolicy` (`settings/authority.rs:37`) is an org-managed ceiling read
only from the managed scope, which itself is loaded through a hardened path
(`O_NOFOLLOW`, single-link regular file, root-or-euid owner, no group/other
write). Its semantics are deliberately one-directional: `off` denies; `on`
*permits a later explicit grant but never grants by itself*.

`apply_tool_ceiling` re-denies every tool key the managed ceiling denies after
the merge, so managed denial survives explicit repository trust — the witness
is `managed_tool_denial_survives_explicit_project_trust`
(`settings/tests.rs:935`).

### B3 — Model output → code execution

The agent's tool surface is the blast radius of a successful A2. The built-in
surface is the 12 task-board / sub-agent / scratch-state / environment tools,
none of which runs a shell or spawns a workspace process. Two facts matter:

- **Built-ins ship registered.** #710 moved every built-in to on-by-default
  with a per-tool `tools.<name>: "off"` switch.
- **The surface is nonetheless not execution-free.** Workspace custom tools
  under `.stella/tools/` and hook actions execute code with the process's own
  privileges.

See [R2](#r2--the-extension-surfaces-execute-code).

### B4 — Process → subprocess (ambient authority)

`crates/stella-tools/src/subprocess_env.rs` scrubs two families before every spawn
(16 sites): credential-shaped names, and ambient-authority names that would
turn a subprocess into arbitrary code execution — `GIT_CONFIG_*` (including
the unbounded `GIT_CONFIG_KEY_n`/`VALUE_n` pairs), `GIT_SSH_COMMAND`,
`GIT_EXTERNAL_DIFF`, `GIT_PROXY_COMMAND`, `SSH_AUTH_SOCK`, and others.

The escape hatch `STELLA_SUBPROCESS_ENV_ALLOW` takes exact names only, no
globs, and is read from Stella's own environment — so a repository tool
manifest cannot widen the policy about to be applied to it. A registered
model-credential name is never re-admitted.

`crates/stella-cli/src/env_files.rs` applies the same logic to dotenv loading: it
refuses to ever apply `LD_*`, `DYLD_*`, `PATH`, `NODE_OPTIONS`, `BASH_ENV`,
`GIT_SSH_COMMAND`, `LESSOPEN`, and friends, because applying them would make
`git clone && stella` arbitrary code execution on the first subprocess.

### B5 — Workspace root → filesystem

No built-in tool opens workspace files. The built-ins' only filesystem writes
are their own state under `.stella/` — the scratch state plane and the task
board — through the store's private-state discipline (B6). The filesystem
reach that remains belongs to the extension surfaces: a custom manifest tool
or a hook action can write anywhere the user's account can, and isolating
that is structural (a container), not a matter of path resolution. See
[R3](#r3--nothing-confines-a-spawned-command-in-process).

### B6 — Stella's private state → other local processes

`crates/stella-store/src/private.rs` asserts *identity* on every private read and
write: `O_NOFOLLOW|O_CLOEXEC`, regular file, `uid == geteuid()`, `nlink == 1`,
mode `0600`, inside a `0700` directory whose parent is owner-controlled with
no group/other write. `.stella/.gitignore` is generated so private state
cannot be committed by accident.

This is the boundary most weakened off Unix — see
[R5](#r5--non-unix-platforms-are-materially-weaker).

### B7 — Local execution data → network

Zero telemetry egress by default is an `AGENTS.md` invariant with a real
enforcement mechanism: `crates/stella-store/src/content_free.rs` holds a reviewed
allowlist of hub columns plus a sentinel harness every egress encoder
registers with, and adding a column or an encoder key fails `make gate` until
the allowlist is edited in the same PR.

Automatic outbound calls are narrow. The models.dev catalog refresh never
fires on a fresh install — it requires a sync row to already exist, i.e. the
user ran `stella models refresh` at least once. Provider-native `/models`
listings do auto-sync, but only to providers the user already supplied a key
for. Enterprise export requires a signed managed enrollment with an exact
HTTPS endpoint allowlist.

## Attack paths

| # | Path | Boundary | Status |
|---|---|---|---|
| P1 | Repo sets `providers.anthropic.base_url` to attacker host → key exfiltrated on first call | B1 | Mitigated (dropped untrusted) |
| P2 | Repo sets `api_key_env` to repoint at another env var | B1 | Mitigated (dropped untrusted) |
| P3 | Repo declares a stdio MCP server → RCE at session start | B1/B3 | Mitigated (gated on `project_code_execution_trusted`) |
| P4 | Repo declares an `enabled` stdio `context_provider` → spawn at admission | B1 | Mitigated (dropped wholesale) |
| P5 | Repo ships `.env` with `GIT_SSH_COMMAND` → RCE on first git subprocess | B4 | Mitigated (refused, never applied) |
| P6 | Repo sets `GIT_CONFIG_KEY_n` in the environment a tool inherits | B4 | Mitigated (prefix-scrubbed) |
| P11 | Co-tenant plants a symlink at a private-state path | B6 | Mitigated on Unix; not off it |
| P13 | Partial-read defeats the MCP OAuth `state` CSRF check | — | Mitigated (#696 read-until-CRLFCRLF) |
| P14 | Credential reaches a log line, trace record, or panic message | — | Mitigated (`ApiKey` has no `Display`, redacted `Debug`) |
| P15 | Credential recovered from process memory after use | — | Partial — see R6 |
| P16 | Credential read from `ps` | — | **Not mitigated** — see R7 |
| P17 | Injected instruction escalates through a custom tool or hook action | B3 | **Not confined in-process** — see R2, R3; gated by `authority.project_custom_tools_allowed` and `STELLA_PROJECT_HOOKS=1` |

## Residual risk

These are known, and in several cases deliberate. They are listed here so the
choice is legible rather than discovered.

### R1 — Trust is an env var, not a decision the user is asked to make

`STELLA_TRUST_PROJECT=1` is process-wide and set before launch. There is no
trust prompt and no persisted trust store. This differs from the
"do you trust the authors of the files in this folder?" model users may expect
from editors.

Consequence: trust is all-or-nothing per invocation, and a user who exports it
in their shell profile silently trusts every repository thereafter. The
mitigation today is that the *default* is untrusted and the drops are loud
(warnings to stderr, with the repo-controlled provider id `escape_debug`'d).

### R2 — The extension surfaces execute code

No built-in executes workspace code. The risk lives in the extension
surfaces — workspace custom tools under `.stella/tools/` are auto-discovered,
and hook actions run shell commands. Custom tools are gated on
`authority.project_custom_tools_allowed`, which defaults false; project hooks
load only under `STELLA_PROJECT_HOOKS=1`.

### R3 — Nothing confines a spawned command in-process

Every path that starts a process — custom manifest tools and hook actions —
runs with the privileges of the Stella process itself. A command the model
was injected into writing can reach anything the operator's account can
reach.

This used to read "the sandbox wraps `bash` only": `STELLA_BASH_SANDBOX`
(`workspace-write` / `restricted`, Seatbelt on macOS, `bwrap` on Linux) was an
opt-in confinement on that one tool. It was removed (#1300). The removal did
not widen the exposure — every path in the list above was already unconfined,
and the setting was off by default — it removed a mitigation whose shape did
not match the threat it named, and with it the impression that "sandbox: on"
bounded a session.

What is left in-process is a *gate*, not a boundary: the `command.started`
policy chain sees a model-authored command line before anything spawns. A boundary has to
come from outside the process — run Stella in a container, where no spawn path
can step around the isolation because the isolation is not on the spawn path.
See `docs/spec/remote-sandboxes.md` §2.

### R5 — Non-Unix platforms are materially weaker

Off Unix there are no managed settings at all (the loader returns
`PermissionDenied` unconditionally), no owner/mode validation, no
`O_NOFOLLOW`, no `0600` enforcement, no credentials-file permission advisory,
and no legacy-state migration. `validate_owner_controlled_parent` degrades to
"is a real directory, not a symlink."

This is a considered position — refusing to store state is not a stronger
posture than storing it under the OS's own permissions, it just moves the
secret somewhere Stella cannot protect at all — but it means B6 and B2 should
be read as Unix-only guarantees.

### R6 — `zeroize` cannot reach copies already made

`ApiKey`, `CredentialsFileData`, `Secret`, and the masked prompt buffer wipe on
drop. The module doc is honest about the limit: it cannot reach a `String`
that grew and reallocated, a page the OS swapped out, or a `HeaderValue`
reqwest built from `reveal()`.

### R7 — `--api-key` is visible in `ps`

The credential chain accepts a CLI flag as its highest-precedence step. Flag
values are argv and therefore readable by other processes. The flag's own help
text steers users to an env var or the credentials file for anything
long-lived; nothing enforces that.

### R8 — The credentials file warns rather than enforces

A `credentials.toml` with group/other bits set produces a `LoosePermissions`
advisory and is read anyway. Both alternatives were considered and rejected:
refusing locks a user out over a condition they may not be able to fix, and
silently `chmod`-ing changes the mode of a file Stella did not create.

### R9 — No secret detector exists

#696 deliberately did *not* ship `AgentEvent::redacted()` or
`AttachmentKind::is_inlined_verbatim()`, on the grounds that both would
advertise a guarantee nothing enforces. Consequence: a secret the *user* pastes
into a prompt, or that appears in a tool result, is stored in
`store.db` (`executions.prompt`, `events.payload`) in the clear. The mitigation
available today is retention — `stella stats prune` — not redaction.

## Platform and posture summary

| Guarantee | Unix | Windows/other |
|---|---|---|
| Managed authority ceiling | Yes | **No** (loader denies) |
| Private-state owner/mode validation | Yes | No |
| `O_NOFOLLOW` on private reads | Yes | No |
| `0600` / `0700` enforcement | Yes | No |
| Credentials-file permission advisory | Yes | No |
| Subprocess env scrub | Yes | Yes |
| Settings trust boundary | Yes | Yes |

## What would change this model

- An interactive or persisted trust decision would retire R1.
- Container-based isolation — the whole process inside a boundary rather than
  a wrapper on one spawn path — would close R3 for every path at once. That is
  the direction of record (`docs/spec/remote-sandboxes.md`); the per-command
  sandbox was removed rather than extended precisely because extending it
  would have had to be repeated for every future spawn site.
- A secret detector — if one could be made accurate enough to not be
  false assurance — would narrow R9.

## Maintenance

This document describes shipped behavior. When a boundary moves, it moves
here in the same PR. The distributed doc comments cited throughout remain the
normative source for *why* each individual check exists; this file exists to
make the set reviewable as a set.
