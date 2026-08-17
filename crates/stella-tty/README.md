# stella-tty

Whether a human is present to see and answer a prompt this process might
print, mid-run. One pure function:

```rust
stella_tty::human_can_answer(interactive_output, stdin_is_terminal, prompt_is_visible);
```

`true` only when the run would render interactive text at all, stdin is a
real terminal (so an answer can come back), and the stream the prompt is
actually printed on is a real terminal too (so the question was seen). All
three are independently required — see the function's doc comment for what
each one means and who supplies it.

## Boundary — does this change belong here?

This crate owns one decision: whether a human is present to answer. If a
planned change adds a new stream a prompt might render on, or a new reason a
run cannot host one, it belongs here, expressed as another `bool` input to
the same pure function — never as this crate reading `std::io` itself (see
the crate doc for why: each consumer's three booleans come from a genuinely
different place, and purity is what keeps the condition unit-testable without
faking a terminal).

This matters more as Stella becomes an engine embedded in other applications
(`doc:engine-embedding`): a host driving turns over HTTP has **no** human on the
other end of the process, so anything that would stop to ask has to know that
before it asks, and fail closed to the host's own approval route instead
([`stella-serve`](../stella-serve) remotes the decision back to the host). One pure
answer, reused by every door, is what stops each surface guessing separately.

Everything else is out. This crate has **no dependencies and must keep
none** — the `stella-home` shape (#1139): a leaf that pulls in nothing costs
neither `stella-cli` nor `stella-model` any isolation by depending on it.
Deciding *what to do* once a human is or isn't present (offer a tool, print a
prompt, fail closed) is the consumer's job, not this crate's.

## Why it is a crate and not a module in `stella-home`

`stella-home` already answers "where is `~/.stella`" for both `stella-cli`
and `stella-model`, and its own README states the rule for when a new leaf is
justified rather than folded into it: functionality that needs a dependency
direction the current graph forbids. That is exactly this shape (#3036) —
`stella-model`'s credential prompt needs the same derivation `stella-cli`'s
approval prompts use, but `stella-model` must never depend on `stella-cli`
(invariant 1) — and `stella-home`'s own boundary section is explicit that "what
path is the stella home" is the only decision it owns; a human-presence fact
is a different question answered for a different reason, so bolting it on
would violate that boundary rather than honour the same shape.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.

## Consumers

- `stella-cli`: `crates/stella-cli/src/interactive.rs::human_can_answer` is a
  thin wrapper (same public path, same doc, same signature — nothing else in
  the crate needed to change) so every existing call site and doc-link keeps
  working. `daemon/approval.rs` calls this crate directly for the daemon
  console's stderr-rendered prompt.
- `stella-model`: `crates/stella-model/src/credential.rs`'s interactive
  credential prompt.
