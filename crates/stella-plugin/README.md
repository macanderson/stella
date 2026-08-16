# stella-plugin

The plugin manifest: parsing and validation of a plugin's declared say in the
turn loop — slice A of #3245 (plugins as turn-loop participants). One
constructor vouches for a manifest:

```rust
let manifest = stella_plugin::PluginManifest::from_toml_str(text)?;
```

A value that came back `Ok` has passed every rule the epic states for the new
blocks: the `[loop]` participation ladder (`none` < `observer` < `steering` <
`arbiter`, monotone — each grade includes the ones below), hook grants
(`Stop` only at `arbiter`, no hooks below `steering`), `max_holds` and
`[requirements]` as arbiter-only powers, the host-run `[oracle]` contract,
and `[subloop]`/`[roles]` as declared stages with routing *intents* — never a
credential or a URL. Unknown keys, unknown hook names, and unknown grades are
load errors (`deny_unknown_fields` everywhere, the #1400 rule this crate
inherits).

The one function a host must never bypass is
`LoopGrant::permits_hook(event)` — the authoritative filter behind the
epic's rule that **an undeclared hook is never invoked**, even if the
plugin's process registers for it. It gates on both the grade and the
declared list, so even a hand-built grant cannot leak a dispatch.

## Boundary — does this change belong here?

This crate owns one decision: *what a manifest declares, and whether that
declaration is coherent*. Pure functions over borrowed text; no I/O, no
environment, and — like `stella-diag` — no workspace-crate dependencies, so
anything may depend on it and it depends on nothing.

Everything that *acts* on a manifest is out:

- Reading the manifest off disk, install consent, lifecycle states, overlay
  and namespacing — the host (#1400's platform slices).
- Binding the grants to the engine's gates — the Stop gate, the hook runner,
  the sub-agent primitive — is the host's job (#3245 slices B/C). The engine
  itself never learns plugins exist: `stella-core` must never depend on this
  crate, and this crate must never depend on `stella-core`.
- Clamping `max_holds`, resolving `[roles]` tiers against the user's BYOK
  providers, running the oracle and tracking its flip — all host, all
  elsewhere.

`HookEvent` here mirrors `stella-core::hooks::HookEvent` by name rather than
importing it, because the dependency is forbidden in both directions;
keeping the two sets identical is a review obligation tracked in #3310.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it
before it crosses.

## Layout

- `src/manifest.rs` — the types (`PluginManifest`, `LoopGrant`,
  `Participation`, `HookEvent`, `Oracle`, `Subloop`, `Role`), parsing, and
  every cross-field validation rule, each documented on the `ManifestError`
  variant that enforces it.
- `src/error.rs` — `ManifestError`, typed per rule (invariant 5).
- `tests/manifest_grades.rs` + `tests/fixtures/*.toml` — slice A's
  acceptance: one fixture per grade, round-tripped through both TOML and
  `serde_json` (invariant 4), and the undeclared-hook filter proven against
  the fixtures.

## Consumers

None shipping yet, deliberately: this is the first slice of #3245, and the
host that consumes it (manifest loading, install consent, Stop-gate binding
via the bounded verification loop, the subloop runner) arrives with slices
B–E of that epic. The crate exists first because every one of those slices
needs the same validated answer to "what did this plugin declare?".
