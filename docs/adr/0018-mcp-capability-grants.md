---
id: adr/0018-mcp-capability-grants
title: "ADR 0018: MCP servers are withheld until their handshake is granted"
status: proposed
---

# ADR 0018: MCP servers are withheld until their handshake is granted

- Status: **Proposed** — awaiting ratification by the repository owner.
- Date: 2026-08-26
- Deciders: repository owner (pending)
- Tracking: [issue #5047](https://github.com/macanderson/stella/issues/5047)
- Scope note: outside the Phase 0 series. It decides a gate on an existing
  surface — the MCP tool set — rather than a new feature.

## Context

`design/tui-v2/SPEC.md` §9.3 says new MCP servers land disabled and "the
first-enable handshake shows declared capabilities before any tool call". The
MCP tab printed that promise in its registry caption and kept none of it: `e`
sent `McpToggle` for every row, and a server's declared tool list was reachable
only *afterwards*, through the `ctrl+o` inspector.

So the shipped path from a registry search to a third party executing code in
the operator's session was two keystrokes — `↵` to install, `e` to enable —
with nothing in between that named what the server could do.

Three questions had to be answered together.

**Where does the gate sit?** Between the handshake and the first `tools/call`,
not before connect. The declared capabilities *are* the handshake's output; a
gate that refused to connect would make the thing under review unreadable, and
the operator would be granting a name.

**Where does the decision live?** In `.stella/mcp.toml`, beside the transport it
governs. A session-only grant evaporates at the next start, and an operator
asked the same question every morning learns to answer it without reading —
which is worse than no gate, because it manufactures consent and records it as
review.

**What does an entry with no recorded decision mean?** The two obvious answers
are both wrong. Reading absence as *granted*
for every entry leaves the gate governing nothing. Reading it as *withheld*
disarms every existing workspace on upgrade — every MCP server in every
`mcp.toml` on earth stops working at once, and the fastest way through is to
grant them all unread.

## Decision

`McpServerEntry::granted` is `Option<bool>` with three states, and each names
the door the server came through:

| State | Written by | Gate |
| --- | --- | --- |
| `Some(true)` | a grant, from the tab or `stella mcp grant` | usable |
| `Some(false)` | a registry install, or `--revoke` | withheld |
| absent | a human editing `mcp.toml` | usable |

A plugin-contributed server has no entry at all and is usable, for the same
reason the absent key is: installing the plugin was the decision.

The runtime half is `stella_mcp::CapabilityGrants`, a shared set of granted
names that `McpToolSet` consults on every advertise and every dispatch —
default-deny wherever a host installs one, and both hosts do (the deck and the
one-shot run), so the property does not depend on which surface is driving. An
ungranted server's tools are absent from `schemas()` and every call to it is
refused with `RefusedByPolicy` before anything reaches the transport.

## Why this is the durable option

The asymmetry is not a compromise; it is the threat model written down.
**Anyone who can write `granted = true` into `mcp.toml` can equally omit the
key.** So reading absence as withheld defends against nothing that `Some(false)`
does not already cover — it only breaks working installations. The attack the
gate actually stops is the operator's own fast keystroke on a search result
they have not read, and that path always writes a decision.

Reading absence as *granted* everywhere would be the cheap version and it
fails the ten-year test differently: the gate would exist in the type system
and govern nothing the moment anyone edited a file.

Recording the answer where the transport lives means one file explains both
what a server is and whether it may act — no second store, no drift between
them, and a run with no deck attached reads the same answer the deck wrote.

## Consequences

- A server installed from the registry advertises nothing until it is granted.
  Both surfaces say so: the tab renders `· ungranted` on the row and `e` opens
  the handshake instead of toggling; `stella mcp list` prints the remedy.
- `stella mcp grant <name>` connects, prints the declared capabilities, and
  asks. `--yes` skips the prompt so a provisioning script can record one; `--revoke` writes
  `Some(false)`, which is a recorded "no" and stays distinguishable from never
  having been asked.
- Existing workspaces are unaffected: their entries carry no `granted` key.
- The refusal is a `ToolOutput::Error`, so the model sees it and can relay the
  remedy. It is never an `Err` out of `execute` — MCP failures have always been
  model-visible data here, and a policy refusal is not the place to change that.
- Not covered: revocation does not disconnect the server, and a granted
  server's tool list is not re-reviewed if it changes mid-session. A server
  that advertises new tools after a reconnect keeps the surface negotiated at
  the first handshake (`McpClient`'s existing contract), so the grant still
  describes what was read — but a *reinstall* under the same alias does not
  re-ask, and that is the seam to watch.

## Alternatives rejected

- **Session-only grants.** No disk write, no migration question, and the
  operator is re-asked every morning until they stop reading.
- **A separate grant store** (`.stella/private/mcp_grants.json`). Keeps
  `mcp.toml` untouched, and splits "what this server is" from "may it act"
  across two files that drift.
- **Gate the connect.** Strictly safer, and it makes the capabilities under
  review unfetchable — the operator would be granting an alias.
- **Default-deny for absent keys.** The strictest rule available, and the one
  that trains operators to grant unread. See the decision above.
