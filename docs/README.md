# docs/

**The product documentation does not live here.** It lives in the
[`website/`](../website/) Next.js + Fumadocs site (deployed at
<https://stella.oxagen.sh>) — edit `website/content/docs/` for anything a
Stella *user* should read.

What lives here is the material that isn't site content: design specs a
maintainer reads before changing the engine, the decision record behind those
specs, the research papers, and the brand assets. The one deliberate exception
is [`why-stella.md`](why-stella.md), which is written for an evaluator rather
than a contributor but stays in the repo so it renders on GitHub without a
round trip to the site.

| Path | What it is |
|---|---|
| [`adr/`](adr/README.md) | Architecture Decision Records for the adaptive-context work — the ratified answers the specs below are built on. |
| [`design/`](design/) | Design specifications and RFCs: the context frame, directive schema, storage map, Context PR workflow, telemetry receipts, the serve surface, and [adaptive context](design/adaptive-context.md). |
| [`papers/`](papers/README.md) | The research notes behind Stella's design: [The Deterministic Engine](papers/deterministic-engine.md) and [Stella's Defensible Position](papers/stella-defensible-position.md). The live site links to these at their exact paths — don't move or rename them. |
| [`brand/`](brand/README.md) | Logo, mark, wordmark, and icon assets, plus the `build.py` generator and `tokens.json` every downstream copy is derived from (`make brand`). |
| [`context-reuse.md`](context-reuse.md) | **Vendored, do not edit.** The Context Graph Protocol's normative contract for context identity, usage reports, consent, and verification — the document 46 rustdoc citations point at. Re-sync from upstream rather than patching it. |
| [`why-stella.md`](why-stella.md) | The technical overview, written for someone evaluating Stella rather than contributing to it. |
| [`context-pr.md`](context-pr.md) | The canonical Context PR specification: how durable steering is proposed, reviewed, published, and retired through Git. |
| [`replay-golden-trajectories.md`](replay-golden-trajectories.md) | How the golden-trajectory replay fixtures are recorded and refreshed. |

Three documents — [`design/context-frame-spec.md`](design/context-frame-spec.md),
[`design/directive-schema.md`](design/directive-schema.md), and the vendored
[`context-reuse.md`](context-reuse.md) — carry a `NORMATIVE-HOME:` header
pinning the Context Graph Protocol revision they defer to instead of restating
its wire semantics. `scripts/check-normative-home.sh` fails CI if that pin
drifts from the `contextgraph-*` git rev in `stella-cli/Cargo.toml`, so repin
the docs and the dependency in the same PR. The check discovers files by the
marker written as an HTML comment, so prose that merely *names* the convention —
this paragraph — is not itself treated as a pinned document. (Do not paste the
comment form into prose: the guard matches the literal text and would then fail
on a file that carries no pin.)

A spec is **not** deleted just because its feature shipped. Several of the
documents under `design/` are cited by `file §section` from Rust doc comments
(`storage-map.md` from `stella-tools/src/registry.rs`, `scripts-index.md` from
`stella-tools/src/scripts.rs`, `exploration-sharing.md` from
`stella-tools/src/staleness.rs`, and others) — they are the normative reference
the code points at, so renaming or removing one means chasing every citation in
the same PR. What each spec's `**Status:**` header says is therefore load-bearing:
update it when the feature lands, and mark a document *Superseded* with a link to
its replacement rather than leaving two live specs to disagree. Notes for
features whose site docs fully replaced them (pipeline, hooks, file-touch
telemetry, memory citations, code graph, schema gate) were removed; recover them
from git history if needed.
