# Stella Threat Model

Status: descriptive — this documents the posture as shipped, not a plan

## Purpose, and why this document did not exist

Stella's security reasoning has always been written down. It was just never
written down *together*: the project trust boundary is argued in
`stella-cli/src/settings/merge.rs`, the authority ceiling in
`settings/authority.rs`, the subprocess scrub in
`stella-tools/src/subprocess_env.rs`, the sandbox's scope in
`stella-tools/src/sandbox.rs`, the web egress denylist in
`stella-tools/src/web_egress.rs`, the filesystem identity checks in
`stella-store/src/private.rs`, and the dotenv refusals in
`stella-cli/src/env_files.rs`. Each is careful. None of them can tell you
whether the set is *complete*, because none of them enumerates the assets or
the adversary.

`SECURITY.md` is a disclosure policy plus an in-scope/out-of-scope list.
`docs/design/enterprise-authority-telemetry.md` is an invariants list scoped to
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
(`stella-cli/src/settings/merge.rs:205`) merges user, managed, and project
scopes per provider id and per field, and the project scope is treated as
untrusted input.

Dropped from an untrusted project scope: `hooks`, `context_providers`,
per-provider `base_url` / `api_key` / `api_key_env`, `mcp.registry_url`,
`tools.bash`/`tools.web` set to `"on"`, and agent `prompt` overrides. Never
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

`apply_tool_ceiling` forces `tools.bash`/`tools.web` to `Off` after the merge,
so managed denial survives explicit repository trust — the witness is
`managed_tool_denial_survives_explicit_project_trust` (`settings.rs:1382`).

### B3 — Model output → code execution

The agent's tool surface is the blast radius of a successful A2. Two
sub-boundaries matter and they are frequently conflated:

- **`bash` ships registered** and is withheld with `tools.bash: "off"`. #710
  moved every built-in to on-by-default with a switch, because the previous
  posture covered built-ins only and most operators never found the switch.
  Assume the default surface HAS a shell.
- **The default surface is nonetheless not execution-free.** `run_script`,
  `start_process`, `repo_commit`/`repo_push`, and workspace custom tools all
  execute code without `bash` ever being enabled. `start_process` spawns argv
  directly — "no shell" is a quoting guarantee, not a power bound, which is
  why the registry routes its joined argv through the same `command.started`
  policy chain as `bash`.

See [R2](#r2--the-default-tool-surface-executes-code).

### B4 — Process → subprocess (ambient authority)

`stella-tools/src/subprocess_env.rs` scrubs two families before every spawn
(16 sites): credential-shaped names, and ambient-authority names that would
turn a subprocess into arbitrary code execution — `GIT_CONFIG_*` (including
the unbounded `GIT_CONFIG_KEY_n`/`VALUE_n` pairs), `GIT_SSH_COMMAND`,
`GIT_EXTERNAL_DIFF`, `GIT_PROXY_COMMAND`, `SSH_AUTH_SOCK`, and others.

The escape hatch `STELLA_SUBPROCESS_ENV_ALLOW` takes exact names only, no
globs, and is read from Stella's own environment — so a repository tool
manifest cannot widen the policy about to be applied to it. A registered
model-credential name is never re-admitted.

`stella-cli/src/env_files.rs` applies the same logic to dotenv loading: it
refuses to ever apply `LD_*`, `DYLD_*`, `PATH`, `NODE_OPTIONS`, `BASH_ENV`,
`GIT_SSH_COMMAND`, `LESSOPEN`, and friends, because applying them would make
`git clone && stella` arbitrary code execution on the first subprocess.

### B5 — Workspace root → filesystem

Every file tool that **opens** — read, write, edit, apply_edits, delete,
download, and the content hashing behind exploration and context packs — is
confined by `stella_tools::rootfd::RootHandle`, which holds the workspace
root's directory descriptor and walks each component `openat(dirfd, name,
O_DIRECTORY | O_NOFOLLOW)` off the one before it. `..` pops the descriptor
stack rather than opening `".."`; a symlink is read with `readlinkat` and
re-confined rather than followed; expansion is bounded. The boundary is
therefore the descriptor chain, not a string comparison, and a name is
resolved exactly once — by the kernel, at the moment it is used.

That replaced a string resolution (#938). `resolve_within_root` canonicalizes
and rejects escapes, using `symlink_metadata` rather than `exists()` so a
dangling symlink is caught, and it is still correct about a filesystem that
holds still. It could not be correct about one that does not: it approves a
path whose interior directories do not exist yet without validating them, and
everything downstream re-resolves those names, so a symlink planted in between
by the model's own `bash` tool or by a `build.rs` under audit was followed.
`O_NOFOLLOW` on the final component did not close it — the final component is
not the one that moves. It survives for the callers that need a *name* rather
than a descriptor: an argument to `rg` or `fd`, a subprocess working
directory, the shadow worktree in `verify_done`, the file-touch ledger. #695
extended it to workspace-member patterns read from `Cargo.toml`,
`package.json`, and `pnpm-workspace.yaml`, and made out-of-root skips counted
and surfaced rather than silent.

This confines Stella's own tools, not the subprocesses they spawn: `bash` can
still write anywhere the user's account can, and isolating that is structural
(a container), not a matter of path resolution. Off Unix there is no `openat`,
so the descriptor walk degrades to the string resolver — see
[R5](#r5--non-unix-platforms-are-materially-weaker).

### B6 — Stella's private state → other local processes

`stella-store/src/private.rs` asserts *identity* on every private read and
write: `O_NOFOLLOW|O_CLOEXEC`, regular file, `uid == geteuid()`, `nlink == 1`,
mode `0600`, inside a `0700` directory whose parent is owner-controlled with
no group/other write. `.stella/.gitignore` is generated so private state
cannot be committed by accident.

This is the boundary most weakened off Unix — see
[R5](#r5--non-unix-platforms-are-materially-weaker).

### B7 — Local execution data → network

Zero telemetry egress by default is an `AGENTS.md` invariant with a real
enforcement mechanism: `stella-store/src/content_free.rs` holds a reviewed
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
| P7 | Workspace member pattern escapes root via `../` or glob | B5 | Mitigated (#695) |
| P8 | Injected instruction in a read file drives `bash` to exfiltrate | B3 | Partial — sandbox is opt-in |
| P9 | Injected instruction drives `run_script` / `start_process` | B3 | **Not mitigated by the sandbox** — see R2, R3 |
| P10 | Injected instruction drives `web_fetch` at cloud metadata / localhost | B3 | Mitigated (#939 egress denylist, post-DNS and per redirect hop) — proxy residual in R4 |
| P11 | Co-tenant plants a symlink at a private-state path | B6 | Mitigated on Unix; not off it |
| P12 | SVG `data:` URI smuggled past the sanitizer | — | Mitigated (#695 replaced the `//` substring test with a real scheme test) |
| P13 | Partial-read defeats the MCP OAuth `state` CSRF check | — | Mitigated (#696 read-until-CRLFCRLF) |
| P14 | Credential reaches a log line, trace record, or panic message | — | Mitigated (`ApiKey` has no `Display`, redacted `Debug`) |
| P15 | Credential recovered from process memory after use | — | Partial — see R6 |
| P16 | Credential read from `ps` | — | **Not mitigated** — see R7 |

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

### R2 — The default tool surface executes code

"`bash` is off by default" is true and is not the same claim as "the agent
cannot run code by default." `run_script` executes commands sourced from repo
manifests (`package.json` scripts, `Makefile` targets, `Cargo.toml` aliases);
`start_process` spawns argv; `repo_commit`/`repo_push` mutate and publish;
workspace custom tools under `.stella/tools/` are auto-discovered.

Custom tools are gated on `authority.project_custom_tools_allowed`, which
defaults false. `run_script` and `start_process` are not gated that way — they
route through the `command.started` policy chain, which is a policy-handler
seam rather than an interactive confirmation.

### R3 — The sandbox wraps `bash` only

`STELLA_BASH_SANDBOX` (`workspace-write` / `restricted`, Seatbelt on macOS,
`bwrap` on Linux) is off by default and, when on, confines only the `bash`
tool. `start_process`, `run_script`, custom tools, and hooks run unconfined.
The sandbox's own doc names prompt injection as the threat it mitigates — that
mitigation therefore has a shape the threat does not.

### R4 — The web egress guard is bypassed by an HTTP proxy

#615 is ruled (#939, option A): `web_fetch`, `web_extract_assets` and
`web_download` refuse loopback, RFC1918, link-local (`169.254.0.0/16`), IPv6
unique-local (`fc00::/7`) and link-local (`fe80::/10`), carrier-grade NAT, and
the `localhost` / `.internal` / `.local` name families by default. The check
runs on the URL, on **the addresses DNS returns** (the resolver filters them,
which is what closes DNS rebinding), and again on **every redirect hop**. An
operator re-opens a specific destination with `[egress] allow` in
`~/.stella/web_auth.toml` — user scope, deliberately not `settings.json`, which
a repo can write.

Two residuals remain, both recorded in `stella-tools/src/web_egress.rs`:

- **Proxies.** With `HTTP_PROXY`/`HTTPS_PROXY` set, reqwest resolves and
  connects to the *proxy*; the real destination travels in the `CONNECT` line
  and never reaches the guarded resolver. The URL-level check still refuses
  literal-IP and denied-name targets on every hop, but a public name that
  resolves to a private address goes unchecked behind a proxy. Disabling proxy
  support would break every corporate user's fetch, which is the worse trade.
- **Port granularity at the resolver.** `reqwest::dns::Name` carries no port, so
  `allow = ["dev.internal:8080"]` is honoured host-wide by the resolver and
  port-exactly by the URL check. Every request passes the URL check first, so
  the port fence holds; the resolver half of it cannot express one.

One sharp edge is recorded in `web.rs`: reqwest strips `Cookie` and
`Authorization` across a cross-host redirect, but a secret placed in a custom
`[domains.x.headers]` entry is not in reqwest's sensitive set and **will**
follow the redirect. The egress guard does not cover it — that guard bounds
*where* a request may go, not *which* credentials ride it.

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
| Workspace-root confinement | Yes | Yes |
| Settings trust boundary | Yes | Yes |

## What would change this model

- An interactive or persisted trust decision would retire R1.
- Extending the sandbox to `start_process` / `run_script` / hooks would close
  the gap between R3 and the threat it names.
- Gating `run_script` behind an authority flag would narrow R2.
- A secret detector — if one could be made accurate enough to not be
  false assurance — would narrow R9.

## Maintenance

This document describes shipped behavior. When a boundary moves, it moves
here in the same PR. The distributed doc comments cited throughout remain the
normative source for *why* each individual check exists; this file exists to
make the set reviewable as a set.
