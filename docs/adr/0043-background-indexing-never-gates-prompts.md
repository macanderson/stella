---
id: adr/0043-background-indexing-never-gates-prompts
title: "ADR 0043: Background indexing never gates prompts"
status: implemented
---

# ADR 0043: Background indexing never gates prompts

- Status: accepted
- Date: 2026-09-19
- Replaces the prompt hold from `#4043`.

## Context

The deck held a prompt while the embed pass ran with more than twelve files
left. On a large tree, the user could press Enter for minutes with no way
to start a turn. Yet search could use saved vectors, names and file text.
The agent could also use `bash` and `read_file` to find context.

The pass ran on its own thread. When the user closed the session, the pass
died too. The next launch had to resume it. The user now wants search to
work with what it has, while the index fills on its own.

## Decision

**Prompts always run.** Remove the deck's index gate. A missing or failed
embedder must not take away the agent's other tools.

**A child fills the index.** The CLI starts a hidden `index-worker` after
the graph scan. On Unix the child leaves the terminal's session. On Windows
it gets a detached process group. It can finish when the parent exits.
The parent reads counts and reaps the child while it is alive.

The child takes the graph's embed lease. Only one pass may hold it. Each
batch writes to the shared store, where search can read it at once. A new
launch can retry work that a failed pass left behind.

Send the embedder settings and keys through stdin. This keeps sealed keys
out of the child's argv and env. The child does not load project env files.

**Below 50%, report degraded search.** Still rank the vectors that exist.
Still fall back to names and file text. At half coverage the degraded label
clears. A partial index keeps its coverage note until it is full. Neither
label limits tool access. The pass keeps going until there is no more work.
The threshold counts files with whole-file vectors. Pending chunk vectors
still get a partial-index note; they add detail to files search can already
rank. If the counts cannot be read, say that coverage is unknown.

`stella init` keeps its eager pass. The change moves the session's backfill
to a child. A search embeds its query; it never fills the index itself.

## Evidence

The deck test sends prompts with empty, half and fuller indexes. The count
test checks both sides of 50% and odd file counts. Worker tests check the
pipe and child lifetime. Search tests check that a thin index says so.
