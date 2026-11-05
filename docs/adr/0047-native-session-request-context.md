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

A person can keep a conversation open while connecting a device to another
workspace. Oxagen must keep that conversation's hooks and model calls in its
original workspace. Stella sends no native session identity on its Anthropic
requests today. A process can host several sessions and worker lanes, so its
PID cannot identify the conversation that owns a request.

## Decision

Stella carries its existing session record ID in a typed request context.
Each engine captures that context when it is built. Completion requests,
retries, compaction calls and hook payloads carry the captured value. Shared
provider instances read it from the request. They hold no current-session cell.

The CLI's existing session binding supplies the context. It binds before the
SessionStart hook runs. Deck lanes and fleet attempts use their existing journal
keys. A resumed session keeps its existing ID. A failed journal open clears the
previous journal binding while retaining the new session's identity; it cannot
make a new conversation write into the prior session's record.

The Anthropic adapter sends `x-oxagen-session` to a configured loopback endpoint.
It bypasses system proxies and refuses redirects for those requests.
It sends no session header to a remote provider. Streaming and unary dispatch
use the same header builder. The header carries the exact ID used by hooks.
It is an identity claim within the authenticated recorder connection and grants
no workspace access by itself.

Oxagen must preserve a native Stella hook ID when one is present, qualify it by
the Stella enrollment, and reject an unknown or ambiguous route after a device
switch. That receiver change belongs to the [workspace switch implementation](https://github.com/oxageninc/product/pull/8897). Older
Stella releases remain unable to prove native model ownership across a switch.

## Consequences

Changing the selected device workspace cannot rewrite an engine's captured
identity. One provider can serve concurrent sessions without sharing their IDs.
The request body and its prompt-cache prefix remain unchanged.

This change adds no service, scheduled task or vendor. Its monthly infrastructure
cost is $0. It does not enable Oxagen's remote workspace switch. That feature
still needs authenticated routing and preserved enrollment credentials in the
product repository. Cursor's native model transport remains outside that path.
