---
id: adr/0047-native-session-request-context
title: "ADR 0047: Native session request context"
status: proposed
---

# ADR 0047: Native session request context

- Status: proposed
- Date: 2026-11-05
- Work: [Device workspace switching](https://github.com/oxageninc/product/issues/8896)

## Context

A person may switch a device to a new workspace. An old chat may still run.
Its hooks and model calls must stay with the old workspace.

Stella has an ID for each session. It does not send that ID with its model
calls today. One process may host more than one chat. Its PID cannot tell
Oxagen which chat made a call.

## Decision

Stella sends the ID it already has. It puts that ID in a typed request
context. Each engine takes a copy when it starts. Hooks and model calls use
that copy. Retries and calls to compact the chat keep it too. A shared model
client reads the ID from each call. It does not store the ID of the last chat.

The CLI binds the ID before it runs the SessionStart hook. Deck lanes and
fleet tasks use their own journal keys. A resumed chat keeps its ID.
If a new journal will not open, Stella clears the old journal handle. It
keeps the new ID. New work cannot write to the old chat's journal by mistake.

When the deck switches to another chat, it runs the SessionStart hook again.
What the hook prints replaces the old chat's text in the prompt.

The self-driving loop runs no chat of its own. A drive names its own session
ID on the hooks it fires, the same ID its audit log records. A one-shot verb
has no ID to send, so its hooks leave the field out.

The Anthropic client sends `x-oxagen-session` to the local loopback endpoint.
Those calls bypass system proxies and refuse redirects. Both the stream and
unary paths use the same header builder. Remote model hosts get no such
header. The model request body and its cache prefix stay as they were.

The header states which chat made the call. It grants no right to use a
workspace. The recorder must check the host's key. It must match the ID to
that host's Stella enrollment. It must refuse a route it cannot prove.

Oxagen must keep the same ID from Stella's hooks. This is part of the
[workspace switch work](https://github.com/oxageninc/product/pull/8897).
An older Stella build cannot prove which native model calls belong to a chat
after a device switch.

## Effects

A device switch cannot change the ID an engine has kept. Two chats can share
one model client and still send their own IDs.

This change adds no service or paid task. Its new monthly cost is $0.
It does not turn on the remote workspace switch. Oxagen still needs to check
each route and keep the old host keys while old chats run. Cursor's native
model calls are outside this path.
