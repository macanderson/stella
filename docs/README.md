# docs/

**The product documentation does not live here.** It lives in the
[`website/`](../website/) Next.js + Fumadocs site (deployed at
<https://stella.oxagen.sh>) — edit `website/content/docs/` for anything a
Stella *user* should read.

What lives here is the material that isn't site content: design specs a
maintainer reads before changing the engine, the decision record behind those
specs, the research papers, and the brand assets. There is one deliberate
exception: [`why-stella.md`](why-stella.md) is written for an evaluator rather
than a contributor but stays in the repo so it renders on GitHub without a
round trip to the site.

| Path | What it is |
|---|---|
| [`adr/`](adr/README.md) | Architecture Decision Records for the adaptive-context work — the ratified answers the specs below are built on. |
| [`design/`](design/) | Design specifications and RFCs: the context frame, directive schema, storage map, Context PR workflow, telemetry receipts, the serve surface, [adaptive context](design/adaptive-context.md), [remote sandboxes](design/remote-sandboxes.md), [agent-native delivery](design/agent-native-delivery.md), and the [website information architecture](design/website-information-architecture.md). |
| [`papers/`](papers/README.md) | The research notes behind Stella's design: [The Deterministic Engine](papers/deterministic-engine.md) and [Stella's Defensible Position](papers/stella-defensible-position.md). The live site links to these at their exact paths — don't move or rename them. |
| [`brand/`](brand/README.md) | Logo, mark, wordmark, and icon assets, plus the design tokens under `tokens/`. The UI palette itself is not generated from here: `stella-tui/src/palette.rs` is the hand-maintained normative source, mirrored by `website/src/app/tokens.css` — edit the two together. |
| [`design/adaptive-context/context-reuse.md`](design/adaptive-context/context-reuse.md) | **Vendored, do not edit.** The Context Graph Protocol's normative contract for context identity, usage reports, consent, and verification — the document 46 rustdoc citations point at. Re-sync from upstream rather than patching it. |
| [`why-stella.md`](why-stella.md) | The technical overview, written for someone evaluating Stella rather than contributing to it. |
| [`design/adaptive-context/context-pr.md`](design/adaptive-context/context-pr.md) | The canonical Context PR specification: how durable steering is proposed, reviewed, published, and retired through Git. |
| [`design/replay-golden-trajectories.md`](design/replay-golden-trajectories.md) | How the golden-trajectory replay fixtures are recorded and refreshed. |

### How to cite a document

**Cite the public docs by path; cite everything else by URL.** A citation in
Rust source or in prose should name a page on the docs site
(<https://stella.oxagen.sh>) or a `website/content/docs/` path, because that is
the address a reader can actually follow and the one that survives a refactor of
this directory. An internal design spec, an ADR, or an upstream contract is
still fair to cite — link it by URL rather than by repo-relative path, so the
citation resolves for someone reading the rendered docs rather than a checkout.

Three documents — [`design/adaptive-context/context-frame-spec.md`](design/adaptive-context/context-frame-spec.md),
[`design/directive-schema.md`](design/directive-schema.md), and the vendored
[`design/adaptive-context/context-reuse.md`](design/adaptive-context/context-reuse.md)
— defer to the Context Graph Protocol for their wire semantics instead of
restating them, and each opens with a URL pointing at the CGP revision it
defers to. Update that link when the `contextgraph-*` dependency moves.

None of this is gated. There used to be two CI checks here —
`check-normative-home.sh`, which compared a `NORMATIVE-HOME:` header against the
`contextgraph-*` git rev, and `check-doc-citations.sh`, which required every
`docs/**.md` path named in a Rust comment to resolve. Both assumed code cites
internal specs by repo-relative path, which under the rule above it no longer
does — and a repo-local checker cannot follow a URL. So the guards were retired
rather than reworked. Getting a citation right is a review responsibility now.

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

**Cite a document by section, never by line number.** `file §7` survives an
edit; `file.md:LINE` does not. A markdown line number is not a stable address —
inserting a paragraph anywhere above the cited line silently repoints the
citation at unrelated prose, and because both the old and the new target render
as ordinary text, nothing surfaces the drift. The citation still *looks*
authoritative, which is what makes it worse than no citation at all. For a
document with numbered headings use `§N`; for one without, name the heading
(`docs/adr/0001-semantic-taxonomy.md § Open questions`). This is a convention,
not a gate: the check that enforced it was retired along with the rest of the
citation guards, so it holds only as far as review does.
