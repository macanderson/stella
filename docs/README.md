---
id: docs-readme
title: "docs/"
status: living
---

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

**`spec/` is durable; `design/` is not.** A document that code cites lives in
[`spec/`](spec/), and `make design-refs` fails the build if anything outside
`docs/design/` names a path inside it. That is what makes `design/` usable as a
scratchpad: rewrite, rename and delete in there freely, because no comment,
test or script is allowed to depend on it. When a design lands, promote it —
`git mv docs/design/<doc>.md docs/spec/<doc>.md` — and cite it by `doc:<id>`.

| Path | What it is |
|---|---|
| [`adr/`](adr/README.md) | Architecture Decision Records for the adaptive-context work — the ratified answers the specs below are built on. |
| [`spec/`](spec/) | The specifications code depends on: the diagnostic plane, storage map, serve surface, witness protocol, adaptive context, telemetry receipts, the threat model, and the rest. Cited from rustdoc and enforced by `make design-refs`. |
| [`design/`](design/) | **Work in flight — nothing outside this directory may cite it.** Proposals and RFCs that have not landed: the directive schema, agent-native delivery, the website information architecture, the pipeline journey. Churn freely. |
| [`papers/`](papers/README.md) | The research notes behind Stella's design: [The Deterministic Engine](papers/deterministic-engine.md) and [Stella's Defensible Position](papers/stella-defensible-position.md). The live site links to these at their exact paths — don't move or rename them. |
| [`brand/`](brand/README.md) | Logo, mark, wordmark, and icon assets, plus the design tokens under `tokens/`. The UI palette itself is not generated from here: `crates/stella-tui/src/palette.rs` is the hand-maintained normative source, mirrored by `website/src/app/tokens.css` — edit the two together. |
| [`spec/adaptive-context/context-reuse.md`](spec/adaptive-context/context-reuse.md) | **Vendored, do not edit.** The Context Graph Protocol's normative contract for context identity, usage reports, consent, and verification — the document 46 rustdoc citations point at. Re-sync from upstream rather than patching it. |
| [`why-stella.md`](why-stella.md) | The technical overview, written for someone evaluating Stella rather than contributing to it. |
| [`spec/adaptive-context/context-pr.md`](spec/adaptive-context/context-pr.md) | The canonical Context PR specification: how durable steering is proposed, reviewed, published, and retired through Git. |
| [`spec/replay-golden-trajectories.md`](spec/replay-golden-trajectories.md) | How the golden-trajectory replay fixtures are recorded and refreshed. |

### How to cite a document

**A document is addressed by its `id`, not by where it sits.** Every document
here that anything cites opens with frontmatter:

```yaml
---
id: context-reuse
title: Context reuse — identity, accounting, consent, verification
status: vendored
---
```

Cite it as `doc:context-reuse`, optionally with a section: `doc:context-reuse §4`.
Move the file, rename the directory, reorganise the whole tree — the citation
still resolves, because it never named a location in the first place.

**Frontmatter is the admission ticket.** A document with no `id` cannot be
cited, and `make doc-links` fails on any citation to one. That is deliberate
rather than strict: a spec nobody has bothered to give an identity to is a spec
nobody is maintaining, and this is a cheaper way to find that out than reading
it. To adopt a document, `make doc-adopt DOC=docs/design/thing.md`. <!-- doc-links:ignore -->

| Field | Required | Notes |
|---|---|---|
| `id` | yes | lowercase kebab, optionally `ns/name`. Chosen once; **never** change it — that is the one thing that would break citations. |
| `title` | yes | usually the H1. |
| `status` | yes | `living`, `proposed`, `implemented`, `superseded`, `vendored`, `archived`. |
| `superseded_by` | when `status: superseded` | the successor's `id`. Citing a superseded document fails the check and names the successor. |

**Path citations still work, and repair themselves.** 223 citations were written
as `docs/design/thing.md` before ids existed, and rewriting them all by hand <!-- doc-links:ignore -->
would be its own source of error. [`manifest.json`](manifest.json) — generated,
committed, diffable — records where each `id` lived at the last commit, so when
a document moves, `make doc-links-fix` diffs that against the tree, sees which
`id` went where, and repoints every stale path itself.

That healing is only ever applied to a move it can *prove*: the id says where
the document went. A broken path whose filename happens to match one document is
a guess, and gets reported rather than applied — a citation silently repointed at
the wrong document is worse than one that visibly dangles. `make
doc-links-fix-by-name` applies those after you have read the list.

If a `docs/…md` string in your prose is not a citation — a file you intend to
*generate*, say — end the line with `<!-- doc-links:ignore -->`.

**Cite anything outside this repository by URL.** An upstream contract or
another repo's ADR has no `id` here and never will.
[`design/adaptive-context/context-frame-spec.md`](design/adaptive-context/context-frame-spec.md),
[`design/directive-schema.md`](design/directive-schema.md) and the vendored
[`design/adaptive-context/context-reuse.md`](design/adaptive-context/context-reuse.md)
defer to the Context Graph Protocol for their wire semantics instead of
restating them, and each opens with a URL pointing at the CGP revision it defers
to. Update that link when the `contextgraph-*` dependency moves.

**What is stale?** `make doc-report` lists every document nothing cites, with
its status and how long it has sat untouched. That is a report, never a
failure — retiring a document is a judgement call, and a red gate is the wrong
way to ask for one.

A spec is **not** deleted just because its feature shipped. Several of the
documents under `design/` are cited by `file §section` from Rust doc comments
(`storage-map.md` from `crates/stella-tools/src/registry.rs`, `scripts-index.md` from
`crates/stella-tools/src/scripts.rs`, `exploration-sharing.md` from
`crates/stella-tools/src/staleness.rs`, and others) — they are the normative reference
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
(`doc:0001-semantic-taxonomy § Open questions`). `make doc-links` fails on a
`path.md:N` citation in any tracked markdown file.
