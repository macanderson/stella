# stella-tool-facts

Facts about the tool surface. No executor is attached.

```rust
let entry = stella_tool_facts::catalog::get("read_file").expect("a row");
assert!(entry.read_only);
assert_eq!(stella_tool_facts::catalog::group_for("read_file"), "files");
```

## Boundary

Data, and pure tests over data. Four modules. Each answers a question a
screen asks as often as the executor does.

- `catalog` — the tool table. Each built-in is declared here once. A row
  carries the read-only bit, the risk grade, and the policy group.
- `policy` — the tool switches an operator set. An exact name beats its
  group. A group beats `*`.
- `subprocess_env` — the names a child process must never inherit. Keys and
  tokens. The ambient rights of a git checkout, too.
- `readiness` — how far behind the search index is, and whether to hold a
  prompt for it.

No registry. No dispatch. No MCP client. No network. No code graph. Those
live in [`stella-tools`](../stella-tools) and stay there.

## Why this is its own crate

The terminal draws a tool list, an approval card, and an index-hold note. To
do that it once took `stella-tools`. That edge brings the whole executor. It
brings a bundled database and nine grammar builds, by way of the code graph
the `search` tool ranks over. A screen paid for a code indexer it never
calls.

So the shared half moved down here. This crate takes one workspace crate,
and that crate is types alone. Any screen can afford it. AGENTS.md § "When a
new crate is justified" lists three cases. This is case (b): the code needed
a direction the old graph did not allow.

`stella-tools` re-exports each item at its old path. Nothing outside the
terminal had to change.

## What is not here

Three types an approval card draws — `ApprovalRequest`, `ApprovalResponse`
and `ApprovalSubject`. They went to
[`stella-protocol`](../stella-protocol) instead. They cross four crate
lines, which is what that crate is for. The subject already sat one layer
down, in `stella-core`.

## God files — do not add lines

This crate has no god files. No file here is near the gate's 1500-line limit
(`scripts/check-file-size.sh`). A new one cannot cross it. The gate blocks
that, and `scripts/file-size-baseline.txt` takes no new entry. Split a file
before it gets close.
