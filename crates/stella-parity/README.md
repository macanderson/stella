# stella-parity

The **cross-surface capability matrix** — the structural guard against a
feature shipping on one of Stella's surfaces and silently not the other.
Stella is one engine behind two customer-facing surfaces: the CLI
(`stella-cli`, the community tool) and the API (`stella-serve`, the embeddable
sidecar). Nothing used to enforce that a capability landing on one surface
landed on — or was *deliberately declared absent from* — the other, and the
two drifted exactly the way per-provider features drifted before
`stella-model/src/provider_parity.rs`: when this matrix was written, the API
could set precisely one of `EngineConfig`'s ~15 tuning knobs, the goal loop
and sub-agents were CLI-only, and the serve crate's own route tests covered 7
of its 14 routes.

The matrix makes that class of gap structural instead of tribal, with the same
three instruments the provider matrix proved out:

- **A declared row per capability**, with a `SurfacePosture` on every surface.
  An absence is legal only as `Deferred` (naming what it waits on) or
  `NotApplicable` (naming the design reason) — never as silence.
- **Witness tests named and checked.** A `Shipped` posture names the test that
  proves the wiring on that surface, and this crate's tests fail when the
  named function no longer exists in the surface's sources.
  `ShippedUnwitnessed` counts the debt instead of hiding it, bounded by
  `UNWITNESSED_BASELINE` — a ratchet that only goes down.
- **Completeness enforced from both ends.** Every real API route
  (`stella_serve::observe::Route::ALL`) must be claimed by a row, and every
  public `Engine` entry point in `stella-core`'s driver/goal modules must be
  claimed by a row or by the `COMPOSITION_SEAMS` allowlist — so adding a route
  or an engine capability without a matrix decision fails
  `cargo test --workspace`, in the same PR that added it.

**The law for new features:** adding an engine capability, an API route, or an
agent-facing CLI behavior means updating this matrix in the same PR.
`Deferred` is an honest and expected answer — the point is that a human wrote
the answer down where a test can keep it true, not that every feature ships
everywhere at once. The embedding story the matrix serves is
[`docs/design/engine-embedding.md`](../../docs/design/engine-embedding.md).

## Boundary — does this change belong here?

One file, one job: `src/lib.rs` holds the posture types, the matrix rows, and
the tests (inline `mod tests`) that keep the rows honest. A change belongs
here when it is a matrix decision — a new row, a posture change, a witness
name, a composition-seam entry, lowering `UNWITNESSED_BASELINE` after writing
a missing witness. The capability *itself* never lives here: its engine home
is `stella-core`, its surfaces are `stella-cli` and `stella-serve`, and this
crate only records how they relate.

The dependency set is the boundary made concrete: `stella-serve` only, for
`Route::ALL` — the enumerable API surface the matrix sweeps. The CLI is a
binary crate, so its surface is checked against source text, the same
discipline `provider_parity.rs` uses for adapter witnesses. Adding any other
dependency here is a design smell: the matrix reads declarations and source
text, it does not run the stack.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. `src/lib.rs` grows a few lines per matrix row, which
is sustainable; if it ever approaches the limit, split the rows from the
types before it crosses.
