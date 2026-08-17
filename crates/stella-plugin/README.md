# stella-plugin

The plugin manifest: parsing and validation of a plugin's declared say in the
turn loop — slice A of the plugins-as-turn-loop-participants epic (#3245, whose
sequencing now lives in #3246). One constructor vouches for a manifest:

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

`[wrapper]` (#3381) is the turn-loop wrapper's stage order, declared instead
of hardcoded: an ordered `[[wrapper.stages]]` list under one variant id — the
id the store's `pipeline_variant` column records (#3388). Two properties make
it a gate rather than documentation:

- **The `if` field is a closed grammar**, not an expression language:
  `[no-]<boolean-signal>` or `<count-signal> <op> <number>`, over a published
  signal set, evaluated by a pure function. A condition naming a signal the
  host does not publish is a load error — a manifest that quietly does nothing
  is worse than one that refuses to load.
- **The stage graph is load-checked.** A condition reading a signal that only
  a *later* stage publishes is rejected at load, so a hand-written variant
  fails with a reason instead of wedging mid-run.

Nothing here dispatches a stage: the four wrapper interception points are
#3380 and do not exist yet, so a stage name is a declared name that
load-checks. The crate takes no engine dependency by contract, which is what
lets the load-time contract be complete ahead of the socket it describes.

## Direction — this crate is the front of the product

Stella's shape is one turn loop with a plugin architecture around it, embeddable
in any application through the Rust ports or the HTTP surface. This crate holds
the contract that makes "plugin" mean something enforceable rather than
aspirational: a declaration a host can check *before* anything runs, written by
someone who does not get to see the engine. Two things follow from that.

**The first plugin is Stella's own verification.** The staged pipeline
([`stella-pipeline`](../stella-pipeline)) is the wrapper the `[wrapper]` block
was designed for, and it is leaving the workspace to become a plugin (#3246,
`doc:turn-loop-wrappers`). When it does, Stella no longer verifies its own work
unless that plugin is installed — which is exactly why the manifest's rules are
load errors rather than warnings: an opt-in verifier that silently declined to
participate would be worse than none.

**Declared, then dispatched, in that order.** The manifest is deliberately ahead
of the socket it describes. The four wrapper interception points are #3380 and do
not exist yet; the `[wrapper]` stage list parses and load-checks but nothing
reads it yet (#3408). That gap is tracked, not incidental — the crate takes no
engine dependency, which is what lets the load-time contract be complete before
the runtime half exists.

The one function a host must never bypass is
`LoopGrant::permits_hook(event)` — the authoritative filter behind the
epic's rule that **an undeclared hook is never invoked**, even if the
plugin's process registers for it. It gates on both the grade and the
declared list, so even a hand-built grant cannot leak a dispatch.

## Boundary — does this change belong here?

This crate owns one decision: *what a manifest declares, and whether that
declaration is coherent*. Pure functions over borrowed text; no I/O, no
environment, and exactly one workspace dependency: `stella-protocol`, for the
shared `HookEvent` vocabulary (#3310). That edge exists because it *removes* a
hand-kept mirror — the grant names the engine's dispatch points, and two
copies of one enum in two crates that may not depend on each other drift with
nothing red. `stella-protocol` is types-only, so it costs this crate no
behaviour; a second workspace dependency here needs the same argument made
again, not this one cited.

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

`HookEvent` here is not a type of this crate's own: it is re-exported from
`stella_protocol::hook`, and so is `stella-core::hooks::HookEvent` (#3310).
The vocabulary lives underneath both because the dependency between them is
forbidden in both directions — which used to mean two hand-kept copies, and a
sixth engine event undeclarable in a manifest until someone remembered the
mirror. It is one edit now, and `hook_vocabulary_is_the_shared_one`
(`tests/manifest_grades.rs`) is the assertion that the sets cannot part.

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
- `src/wrapper.rs` — the `[wrapper]` block: `Wrapper`, `WrapperStage`, the
  closed `StageName` and `Signal` vocabularies, the `Condition` grammar and
  its parser, and the load-time stage-graph check.
- `src/error.rs` — `ManifestError`, typed per rule (invariant 5).
- `tests/manifest_grades.rs` + `tests/fixtures/*.toml` — slice A's
  acceptance: one fixture per grade, round-tripped through both TOML and
  `serde_json` (invariant 4), and the undeclared-hook filter proven against
  the fixtures.
- `tests/wrapper_stages.rs` + `tests/fixtures/wrapper-*.toml` — #3381's
  acceptance: the shipped stage order and a cheaper second variant, differing
  in nothing but their text, plus one rejection test per load rule.

## Consumers

None shipping yet, deliberately: this is the first slice of the epic, and the
host that consumes it (manifest loading, install consent, Stop-gate binding
via the bounded verification loop, the subloop runner) arrives with slices
B–E of that epic. The crate exists first because every one of those slices
needs the same validated answer to "what did this plugin declare?".
