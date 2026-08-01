# Stella website information architecture — one site for the pitch and the manual

**Status:** Phase 1 (§ Sequencing) is implemented: the five-section tree and
the command regroup are live in `website/content/docs/` as reorder-without-move
`meta.json` changes (no slug moved, so no redirect was needed), and the
no-orphans and help-text-parity invariants are enforced by
`the_docs_nav_mirrors_these_groups` and
`the_docs_index_summaries_are_the_clis_own` in `stella-cli/src/cli/help/tests.rs`.
Phases 2–4 (home rebuilt around a recorded run, guides adopting the template,
the recorded-run component) remain unbuilt. This document does not change any
visual design (`docs/brand/BRAND.md` governs that).
**Date:** 2026-07-31. **Owner:** Mac Anderson.
**Companions:** `website/content/docs/meta.json` (the tree as it stands),
`stella-cli/src/cli/help.rs` (the CLI's own command grouping),
`docs/why-stella.md` (the evaluator-facing pitch), `docs/brand/BRAND.md`
(voice and tokens).

---

## One sentence

The site stops being a manual with a brochure stapled to the front: every
page — the home page included — is built around a real problem someone has,
shows a real recorded run solving it, states what it cost, and says where it
bites.

## Why this is the design, not just a reshuffle

Stella's core claim is that an agent should have to *prove* the change it made
is the change that fixed the problem. The website should hold itself to the
same standard. A site whose marketing page makes claims and whose docs pages
describe features is two artifacts pretending to be one; a site where the
pitch *is* a receipt-backed example and the docs *are* the deep end of that
same example is one artifact — and it is a pitch no other agent's site can
copy without first building what Stella built.

Three rules follow, and everything else in this document is downstream of
them.

### Rule 1 — nothing on the site is staged

Every terminal snippet, diff, cost figure, and screenshot comes from a real
recorded run, and carries its provenance: the date, the model, the exact
command, and what it cost. If a claim cannot be shown this way, the claim
does not go on the site. This is `verify_done`'s ethic applied to marketing,
and it is the single strongest differentiator available to us: other agent
sites paste synthetic terminal output; ours can say "this happened, here is
the receipt, run it yourself."

### Rule 2 — problems first, features second

Sections are named for what a person is trying to do, not for what the
feature is called. "Fix a failing test" is a page; `verify_done` is a term
that page teaches. The current tree already leans this way in `guides/`; this
design makes it the spine of the whole site instead of one section among
eight.

### Rule 3 — every page owns its pitfalls

Real-world common problems are not quarantined in a troubleshooting appendix.
Each guide and concept page ends with a "Where this bites" section covering
the real failure modes of that surface — sourced from actual issues, actual
support questions, and the troubleshooting page's material. A shared index
aggregates them so the sum is browsable in one place.

## One site: what happens to the marketing surface

`website/` already hosts both a marketing landing (`src/app/(home)/`) and the
docs. The change is not structural, it is editorial: the home page becomes
the front door of the docs rather than a parallel artifact.

- The home page's centerpiece is one real recorded session: a failing test,
  the witness writing the proof, the fail→pass flip, the cost receipt. Scroll
  reveals the claims — and **every claim on the home page links to the doc
  page that substantiates it**, the same way `docs/why-stella.md` already
  links into the site. No claim without a destination.
- The standalone one-pager repo (`macanderson/stella-website`) stops being a
  second source of truth. Whether it redirects to stella.oxagen.sh or is
  retired outright is an owner decision — see § Open questions.
- The header nav carries the five doc sections below plus GitHub. No separate
  "Product / Docs / Pricing" split; there is nothing behind a curtain.

Voice is already settled by `docs/brand/BRAND.md` § Voice and it happens to
be exactly the voice this design needs: declarative, specific, a number over
an adjective, "a sentence that would survive in a man page survives on the
home page."

## Who arrives, and where they land

| Persona | Their question | Lands on |
|---|---|---|
| Evaluator | "Why would I switch?" | Home → *Understand → The proof* |
| New user | "Get me to a first win" | *Start* (five minutes to a proven fix) |
| Daily user | "How do I do X?" | *Do* (use-case guides) |
| Team lead / operator | "What does it cost, where does data go, who controls it?" | *Do → Keep spend under control*, *Understand → Telemetry and trust* |
| Extender | "Make it fit my project" | *Configure* |
| Embedder | "Put the engine in my app" | *Do → Put the engine in your app* → *Reference* |
| Contributor | "How is it built?" | Not this site — repo `docs/`, `AGENTS.md` (a non-goal here) |

## The navigation tree

Five words a person would actually say — Start, Do, Understand, Configure,
Reference — plus a small Project tail. Annotations in parentheses; existing
pages that move here are named in § What moves.

```
stella.oxagen.sh
│
├── Home                          the pitch is a recorded run
│     one real session end to end: failing test → witness → fail→pass
│     flip → receipt. Every claim links to its doc page. Install one-liner.
│
├── Start                         five minutes to a proven first fix
│   ├── Install                   (installation)
│   ├── Bring a key — or none    (providers; local/Ollama path is a peer,
│   │                             not a footnote)
│   ├── Your first run            one real task in a sample repo, verbatim
│   └── Reading the proof         what just happened: the witness, the
│                                 fail→pass flip, the receipt in store.db
│
├── Do                            use-case guides — the spine of the site
│   ├── Fix a failing test        (guides/fix-failing-ci, split)
│   ├── Keep CI green             stella monitor on a real PR
│   ├── Ship in a codebase you don't know
│   │                             (guides/unfamiliar-codebase: init, graph,
│   │                             storage — offline, no key)
│   ├── Refactor without breaking verify_done + budget as guardrails
│   ├── Run many tasks at once    (agent-fleets, the practical half)
│   ├── Keep spend under control  (guides/budget + what-a-run-cost + stats,
│   │                             scoreboard, usage)
│   ├── Work offline or air-gapped
│   │                             local models, offline graph, zero egress
│   ├── Teach Stella your project ingest → context records → memory →
│   │                             skills; the "she gets better here" story
│   ├── Automate and script       headless runs, JSON output, CI recipes
│   │                             (scripting)
│   ├── Put the engine in your app
│   │                             (agent-engine-in-your-app + serve)
│   └── When it goes wrong        (guides/troubleshooting) + the pitfalls
│                                 index aggregated from every page
│
├── Understand                    concepts — each anchored to something you
│   │                             can watch or query, never prose alone
│   ├── The proof                 verify_done and the witness; why a green
│   │                             suite is never accepted   ← flagship page
│   ├── The pipeline              (inference-pipeline; slug is load-bearing)
│   ├── The engine                (agent-engine-paths + principles/
│   │                             determinism: one loop you can read)
│   ├── Context and memory        (context-engine; slug is load-bearing)
│   ├── The code graph            offline tree-sitter index; graph_query
│   ├── Modes                     (agent-modes: chat, run, goal, monitor,
│   │                             fleet — one table, when to use which)
│   ├── Self-improvement          (self-improvement: reflections → skills →
│   │                             tune)
│   ├── Telemetry and trust       (telemetry/*: local-only by default, the
│   │                             two opt-in egress paths, files-touched)
│   └── Principles                (principles/*)
│
├── Configure                     make her yours
│   ├── Settings and stella.toml  (configuration/settings + scopes/authority)
│   ├── Providers and models      (api-providers/*, configuration/credentials,
│   │                             agent-engine-config: per-role routing)
│   ├── Tools and permissions     (agent-tools/custom-tools, permissions,
│   │                             sandbox, bash on/off semantics)
│   ├── Skills, commands, agents  (agent-tools/skills, commands,
│   │                             custom-agents)
│   ├── Hooks                     (agent-tools/hooks; slug is load-bearing)
│   ├── MCP                       (agent-tools/mcp)
│   └── Cookbook                  (examples + the stella-examples repo,
│                                 promoted from Showcase to a section page:
│                                 seven settings profiles, hooks, custom
│                                 tools — every configurable surface, one
│                                 working file each)
│
├── Reference                     look-up, not learning
│   ├── Commands                  (commands/*, regrouped into the CLI's own
│   │                             six groups — see § Commands)
│   ├── Configuration reference   full schema, resolution order, env vars
│   ├── Event stream and wire     (event-stream-compatibility + docs/wire
│   │                             schemas; extensions)
│   └── Release notes             (release-notes)
│
└── Project
    ├── Security                  threat model, sandbox, disclosure
    ├── Licensing                 AGPL + commercial, CLA
    ├── Roadmap                   honest: what's built, what isn't
    ├── Showcase                  community + example gallery
    └── Donate                    (donate)
```

### Commands: mirror the CLI, don't re-invent it

`stella-cli/src/cli/help.rs` already groups all 35 commands into six
task-shaped groups, and a test fails if a command is left out. The site's
commands section currently presents a flat 35-item list in a hand-curated
order. The site should adopt the CLI's grouping verbatim:

1. Run the agent — `run`, `chat`, `goal`, `resume`, `init`
2. Run many at once — `fleet`, `monitor`, `arena`
3. Ask about this workspace — `graph`, `storage`, `scripts`, `tools`, `commands`
4. Steer what the agent knows — `ingest`, `context`, `proposals`, `memory`
5. What it cost, what happened — `stats`, `scoreboard`, `observe`, `inspect`, `calibration`, `usage`, `tune`
6. Set up — `auth`, `models`, `connect`, `mcp`, `config`, `migrate`, `doctor`, `completions`, `cloud`, `telemetry`, `version`

Two invariants come along with this: the **no-orphans rule** (every command
has a page, every page is in exactly one group — enforceable with a small
build check against `commands/meta.json`, the site twin of the existing
`help.rs` test), and **help-text parity** (the one-line description on the
site index is the same string the CLI prints, so the two never drift apart in
tone or claim).

## Page templates

Templates are the mechanism that makes Rules 1–3 hold at scale. Three page
kinds, three fixed shapes.

### Guide page (the *Do* section)

1. **The problem** — two or three sentences of the real situation, written
   the way a person would describe it, not the way a feature list would.
2. **Watch it happen** — a recorded run: the exact command, the transcript,
   the diff. Pinned to a public sample repo so the reader can reproduce it.
3. **The receipt** — what it cost (tokens, dollars, wall time) from `stella
   stats`, and how to see the same numbers for your own run.
4. **How to adapt it** — the two or three flags/settings that matter for the
   variations people actually hit.
5. **Where this bites** — the known pitfalls of this workflow, each with the
   symptom first ("the run seems to hang at…") and the cause second.
6. **Deeper** — links into *Understand* and *Reference* for the surfaces the
   guide used.

### Concept page (the *Understand* section)

1. **In one sentence** — what it is.
2. **The failure it prevents** — why it exists; the concrete bad outcome
   Stella refuses to allow. (For the flagship page: "a suite that was already
   green, and an edit that doesn't exercise the fix.")
3. **See it live** — the observable evidence: a recorded run, a `stella
   inspect` replay, an Observatory screenshot, a query against store.db.
   A concept page with nothing observable is not finished.
4. **How it works** — the mechanism, at whatever depth the surface needs.
5. **Where this bites** — same section as guides; concepts have pitfalls too
   (cache invalidation, memory retirement, mode mismatch).
6. **Deeper** — reference links, and for the truly curious, the repo's design
   docs and papers.

### Reference page (commands, configuration, wire)

Synopsis, flags, one real example each — and a **"used in" backlink block**:
every command page lists the guides that exercise it, every guide lists the
commands it used. The graph must be walkable in both directions, because a
person who lands on `stella fleet` from a search engine should find "Run many
tasks at once" in one click.

## How "live" stays live

The examples must be real, and they must not rot. The corpus and its
freshness are a mechanism, not a hope:

- **Anchor repos.** A small set of public sample repositories (a home in
  `stella-examples` is the natural fit) that guides pin their recorded runs
  to. Each recorded run stores the command, the transcript, the model, the
  date, and the receipt.
- **Recorded-run component.** One MDX component renders a recorded run —
  collapsed transcript, provenance line, copyable reproduce command — so
  every page presents evidence the same way. The existing golden-trajectory
  replay fixtures (`docs/replay-golden-trajectories.md`) are the pattern to
  follow, and possibly the storage format to reuse.
- **Staleness is visible.** Every recorded run displays its date and Stella
  version. A run recorded on an old version is not a lie, but it says so.
  Re-recording sweeps ride the release process rather than ad-hoc heroics.
- **Receipts are queryable.** Where a page cites cost, it also shows the
  `stella stats` or store.db query that produced the number, so the reader
  can audit ours and produce theirs.

## What moves, what stays put

The sidebar restructure must not break inbound links. Several slugs are
load-bearing today: `docs/why-stella.md` and the README deep-link
`/docs/inference-pipeline`, `/docs/context-engine`, `/docs/agent-tools/hooks`,
`/docs/telemetry` and `/docs/telemetry/files-touched`, and the README's
command table links every `/docs/commands/<cmd>` page.

Policy, in order of preference:

1. **Reorder without moving.** Where Fumadocs `meta.json` can express the new
   grouping over files in place (the commands regroup, section separators,
   ordering), do that — the URL never changes.
2. **Move with a redirect.** Where a file genuinely changes folder (e.g.
   `examples.mdx` becoming the Cookbook section page), add the redirect in
   `next.config.mjs` in the same commit.
3. **Never reuse a vacated slug** for a different topic.

The bulk mapping: `getting-started/*` → *Start*; `guides/*` → *Do*; the
loose concept pages and `principles/*` → *Understand*; `configuration/*`,
`api-providers/*`, `agent-tools/*` → *Configure*; `commands/*`, `scripting`,
`extensions`, `event-stream-compatibility`, `release-notes` → *Reference*;
`showcase`, `donate` → *Project*. `telemetry/*` splits: the trust story is
*Understand*, the dashboard how-to is *Do/Reference* material.

## What this is not

- **Not a visual redesign.** Tokens, type, and voice are governed by
  `docs/brand/BRAND.md`; this document only decides what pages exist and how
  they relate.
- **Not a rewrite of every page in one PR.** The tree lands first (reorder +
  redirects + templates), then guides adopt the template one at a time, each
  with its recorded run. A page not yet migrated is still a correct page.
- **Not touching repo `docs/`.** Maintainer-facing specs, ADRs, and papers
  stay in the repository on purpose (`docs/README.md` states the split); the
  site links to them from *Understand → Deeper* rather than absorbing them.
- **Not adding a CMS, analytics, or any dynamic backend.** The site stays a
  static Fumadocs build; recorded runs are files in the repo.

## Sequencing (suggested, not binding)

1. **Tree + redirects** — new `meta.json` structure, section pages, command
   regroup, slug redirects. No prose rewritten.
2. **Flagship spine** — Home rebuilt around one recorded run; *Start*
   rewritten to end at "Reading the proof"; *Understand → The proof* written
   fresh. This is the "sets her apart" milestone.
3. **Guides adopt the template** — one recorded run per guide, pitfalls
   sections written, Cookbook promoted.
4. **Mechanisms** — recorded-run component, no-orphans build check, help-text
   parity check, staleness banner.

## Open questions

1. **The one-pager repo.** Redirect `macanderson/stella-website`'s deployment
   to stella.oxagen.sh, or keep it as a pure splash that links here? (Owner
   call; the brand doc names it the reference rendering for tokens, so
   retiring it moves that reference too.)
2. **Where recorded runs live.** In `website/` next to the pages that use
   them, or in `stella-examples` where the sample repos live? The second
   keeps the site repo light but adds a cross-repo pin.
3. **Top-nav shape.** One Fumadocs sidebar with separators (closest to
   today), or Fumadocs tabs per top section (*Start / Do / Understand /
   Configure / Reference*)? Tabs read better for a site that is also the
   marketing surface, but they hide the whole-tree view.
4. **Naming.** "Do" is the plainest word for the use-case section but reads
   terse in a header; "Guides" is the safe fallback. Same question for
   "Understand" vs "Concepts".
