---
id: adr/0030-the-wrapper-socket-is-the-plugin-sdk
title: "ADR 0030: The wrapper socket is the plugin SDK"
status: implemented
---

# ADR 0030: The wrapper socket is the plugin SDK

- Status: accepted
- Date: 2026-09-06
- Decides: `#6148`

## Context

The plugin plan (`#3246`) had one step left with nothing in the tree: a Python
SDK. Its closing note put two choices to a maintainer. Ship a Python package,
so that an author writes handlers and not a JSON dispatcher. Or say that the
manifest plus the socket is already the whole contract.

Seven plugins under `plugins/` are Python today. Each one reads a JSON request
on stdin. Each one looks at the point and writes a JSON reply on stdout. They
share no library.

A Python author is not left with nothing to read.
`crates/stella-plugin/src/bin/export_wrapper_wire.rs` writes three files, not
one:

- `docs/wire/wrapper.wire.json` holds every message in its fullest and its
  emptiest legal form.
- `docs/wire/wrapper.schema.json` is JSON Schema 2020-12. No language owns it.
- `docs/wire/wrapper.d.ts` states the same contract in TypeScript.

The gate's `wire-schema` step runs that tool again and diffs all three. A wire
change that skips them fails the pull request that made it.

## Decision

Ship no SDK from this repository. An author writes against the `plugin.toml`
manifest, the socket in `doc:wrapper-socket`, and those three files.

**The contract is a wire.** A library over it is a nicety. A first-party one
in one language starts a queue: Go next, then TypeScript, then Rust. Each one
is a new place to state the wire wrongly. `macanderson/stella-examples`
carries `verify-rs`, `verify-py` and `verify-ts`. That is one plugin written
three times over this socket, with no library in any of them.

**A Python package cannot live here.** `plugins/README.md` gives the reason
for the plugins themselves. A plugin built and shipped with the binary proves
the opposite of what it is here to prove. A package would need its own repo,
its own release train and its own index account. That is the coupling
`doc:pipeline-as-plugins` exists to remove.

**A wire change has to break something here.** That is the one thing this
choice leans on, and it is real. Tests in `crates/stella-runtime/tests/`,
`crates/stella-cli/tests/` and `crates/stella-tui/tests/` spawn these plugins
through the host's own transport. An SDK would add nothing to that. A missing
test would take it away.

## Consequences

`plugins/README.md` now states the contract. The next reader does not have to
ask `#3246`'s question again.

`scripts/check-plugin-graded.sh` (`make plugin-graded`, a gate step) holds the
canary in place. Every folder under `plugins/` with a `plugin.toml` has to be
named in some `crates/*/tests/**/*.rs`, on a line that is not a comment. The
claim sat in `plugins/README.md` and nothing checked it. A new plugin with no
test would have left that sentence reading true.
`scripts/test-plugin-graded.sh` (`make plugin-graded-test`) shows the guard
can still fail.

An author still writes a JSON dispatcher. This record accepts that. It is a
few lines in any language. The other road is a package this project would
have to ship for good.

`#5122` is the open question this does not answer. It grades a plugin against
a replayed wire corpus. What a plugin is graded by and what it is written
against are two things. If it lands and authors still ask for a library, amend
this record. Do not add one on the quiet.
