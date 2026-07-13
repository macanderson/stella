# Version & stability

OCP has **two independent version axes**, and it's important not to conflate
them:

- **The crate version** — `0.1.0` today, `[workspace.package].version` in the
  workspace root `Cargo.toml`, inherited by `ocp-types`, `ocp-host`, and
  `ocp-conformance` alike. This is ordinary Rust/Cargo semver.
- **The protocol version** — `ocp/1.0-draft`, the `PROTOCOL_VERSION` constant
  in `ocp-types::lib`. This is the wire-format identity two OCP
  implementations negotiate at handshake time, independent of what language
  or crate version either side is written in.

A crate patch release (bug fix, better error message, an added helper
method) does not imply a protocol change. A protocol change, conversely, is
what actually breaks interop between a host and a provider built against
different crate versions — that's the one that matters most to a third
party.

## What `-draft` means right now

Quoting `ocp-types::PROTOCOL_VERSION`'s doc comment verbatim, since it's the
authoritative statement:

> The protocol version string this crate implements. Frozen to `ocp/1.0`
> only at the public v1.0 release.

In other words: the wire shape captured in this `0.1.0` release —
`ContextFrame`, `ContextQuery`, `Capabilities`, the `Envelope` vocabulary, the
consent/budget/citation contracts documented in
[protocol-surface.md](./protocol-surface.md) — is **real and implemented
today** by the reference host (`ocp-host`) and conformance suite
(`ocp-conformance`), and is safe to build against. It is not yet a **frozen**
contract: a pre-1.0 revision could still change a field shape or add a
required check based on real-world provider implementation feedback, before
the public `ocp/1.0` release drops the `-draft` suffix.

## Why version families interoperate

`ocp-host::wire::versions_compatible` treats two protocol strings as
compatible when they share a **major family** — the substring up to the
first `.`. `ocp/1.0-draft` and `ocp/1.0` are both family `ocp/1` and
interoperate; `ocp/2.0` does not interoperate with either. This is
deliberate: it means the eventual freeze from `ocp/1.0-draft` to `ocp/1.0`
does not require a flag day where every already-deployed provider breaks the
instant the spec freezes — a `1.0-draft` provider and a `1.0` host (or vice
versa) still handshake successfully within the `1` family. What *does* break
interop is a jump to a new major protocol family (`ocp/2.0`), which is
reserved for a genuinely breaking protocol redesign.

## The stability guarantee, going forward

- **Pre-1.0 (now):** crate versions are `0.x`, tracking `ocp/1.0-draft`. Cargo
  semver rules mean **any `0.x → 0.y` bump may contain breaking changes** to
  either the Rust API or the wire shape — normal pre-1.0 Rust convention.
  Pin an exact version (`ocp-types = "=0.1.0"`) if you need a hard guarantee
  against churn before the freeze.
- **At the freeze:** when the protocol is declared `ocp/1.0` (the `-draft`
  suffix drops), `ocp-types`, `ocp-host`, and `ocp-conformance` bump to
  `1.0.0` in lockstep. That `1.0.0` release is the stability guarantee: from
  that point on, the crates follow ordinary semver — a `1.x → 1.y` minor is
  additive-only, and a wire-breaking protocol change requires both a new
  protocol major (`ocp/2.0`) and a new crate major (`2.0.0`).
- **Conformance is the enforcement mechanism.** "OCP conformant" is defined
  as green on `ocp-conformance`'s suite for your declared capability set
  (see [running-conformance.md](./running-conformance.md)) — that suite, not
  a hand-audited checklist, is what a third party checks their implementation
  against, before and after the freeze alike.

## Practical guidance for early adopters

- **Depend on `ocp-types` with a caret or exact pin**, per your risk
  tolerance — `^0.1` accepts any pre-1.0 patch/minor per Cargo's (unusual)
  0.x semver rules, `=0.1.0` pins exactly.
- **Re-run `ocp-conformance` after every `ocp-types`/`ocp-host` upgrade**
  before the 1.0 freeze — a 0.x bump is exactly the kind of change that can
  silently add or tighten a conformance check.
- **Don't hardcode `"ocp/1.0-draft"` or `"ocp/1.0"` in your own handshake
  code** — read `ocp_types::PROTOCOL_VERSION` and use
  `ocp_host::wire::versions_compatible` (or the equivalent major-family
  comparison, if you're implementing a provider outside Rust) so your
  implementation keeps working across the freeze without a code change.

## MSRV and edition

All three crates inherit `rust-version = "1.90"` and `edition = "2024"` from
the workspace. An MSRV bump is a minor-version-worthy change while pre-1.0
(consistent with the guidance above); after 1.0.0 it will follow the same
semver discipline as the rest of the crate's public API.
