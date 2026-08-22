# stella docs

The documentation site for [stella](https://github.com/macanderson/stella) —
destined for **stella.oxagen.sh**.

Built with [Next.js](https://nextjs.org) (App Router) + [Fumadocs](https://fumadocs.dev)
(`fumadocs-core` / `fumadocs-ui` / `fumadocs-mdx`) + Tailwind CSS v4.

## Brand

Brand kit v5.0, **"the comet"**: a four-point star moving fast enough to leave
a trail. One shape, one color — Gold `#EFC53F` on `#0A0A0C`, Paper for the
light ground, JetBrains Mono as the only face. Quick rules: lowercase always;
the comet flies left→right; gold is the signal, never the surface; brand text
on light grounds is ink `#141416`, because gold on paper measures 1.65:1.

**`docs/brand/` is normative** (start with its `brand-guidelines.html`). Three
layers, in order:

| Layer | File | What it holds |
| --- | --- | --- |
| Palette | `src/app/tokens.css` | Raw values, verbatim from `docs/brand/css/tokens.css`, plus the light/dark semantic aliases. |
| Semantics | `src/app/global.css` | The Fumadocs variable mapping, type scale, chrome, and component styles. |
| Marks | `src/components/brand.tsx` | The comet and wordmark geometry, rendered inline. |

Do not put a hex literal in `global.css` or a component — add a token to
`tokens.css` and reference it, so the site and the kit cannot drift.

**A route used by exactly one page may keep its own stylesheet** beside it,
imported from that route's layout — `src/app/releases/releases.css` is the
pattern. It buys the same thing `global.css` does (one stylesheet per page, not
a `<style>` tag per component) without growing a sitewide file with rules
nothing else can use. The token rule above still applies: no hex literals
anywhere, and a `--rl-`-style prefix so the selectors cannot collide.

**Brand assets are copies of the kit, not originals.** Every SVG under
`public/brand/` mirrors `docs/brand/logo/svg/`; `public/icons/*`,
`src/app/favicon.ico`, `src/app/icon.svg`, and `src/app/apple-icon.png` mirror
`docs/brand/pwa/`; the woff2 files under `src/fonts/` mirror
`docs/brand/fonts/`. When the kit regenerates, re-copy — never hand-edit the
site's copies. The one rendered-from-geometry surface is the `next/og` card in
`src/app/opengraph-image.tsx`, which draws the lockup from the constants in
`src/components/brand.tsx`.

## Develop

Every command here runs from this directory — the site owns its own
`package.json`, `pnpm-lock.yaml`, and pnpm settings, and the repo root is
pure cargo.

```bash
pnpm install
pnpm dev          # http://localhost:3400
```

## Build

```bash
pnpm build        # production Next build (what docs.yml runs in CI)
pnpm start        # serve the production build on :3400
pnpm typecheck    # tsc --noEmit
```

Every page is prerendered, but this is **not** an `output: "export"` site. Two
things need a Next.js server: the Fumadocs search route
(`src/app/api/search/route.ts`) is a real route handler, and `next.config.mjs`
declares a `redirects()` rule that keeps the old `/docs/agent-modes/goal-mode`
deep link alive. Moving to a pure static host means swapping the route for
Fumadocs' static search index and re-expressing that redirect in the host's
own configuration first.

## Structure

```
content/docs/            # all documentation (MDX + meta.json ordering)
  index.mdx              # Introduction
  getting-started/       # installation, initialization, providers
  api-providers/         # per-provider pages + the live model catalog
  inference-pipeline.mdx # the staged pipeline: triage → … → verifier
  context-engine.mdx     # bi-temporal memory, recall, citation loop
  agent-modes.mdx        # chat / run / goal / monitor / fleet, and which to use
  agent-engine-paths.mdx # which engine path a given invocation actually takes
  agent-fleets.mdx       # parallel worker fleets in git worktrees
  agent-tools/           # built-in tools, skills, permissions, custom, MCP, hooks
  configuration/         # settings.json scopes, agent-engine config, credentials
  examples/              # cost/quality profiles (dirt-cheap → max-quality)
  telemetry/             # local SQLite metering, Observatory
  principles/            # determinism + the papers
  commands/              # per-command reference (run, chat, goal, fleet, …)
  extensions.mdx         # the extension event bus
  scripting.mdx          # headless JSON output for CI
  showcase.mdx           # teams shipping with Stella (Oxagen, …)
  release-notes.mdx      # where release notes live (points at /releases)
  donate.mdx             # how to support the project

src/app/                 # Next.js App Router
  (home)/                # landing page
  docs/                  # Fumadocs docs shell
  api/search/            # Fumadocs search route (input-capped; see its header)
  tokens.css             # the palette, verbatim from docs/brand/css/tokens.css
  global.css             # semantics, type scale, chrome, component styles
src/components/          # brand marks, cards, diagrams, terminals, page footer
src/lib/source.ts        # Fumadocs content source loader
src/mdx-components.tsx   # MDX component map
```

## Add or edit a page

1. Create/edit an `.mdx` file under `content/docs/`. Every page starts with frontmatter:

   ```mdx
   ---
   title: Page Title
   description: One-sentence summary shown in search and metadata.
   ---
   ```

2. Add its slug to the nearest `meta.json` `pages` array to place it in the sidebar. Use
   `"---Label---"` entries for section separators. This is not optional: a `pages` array
   is an allowlist, so a page you forget to list still builds and still answers at its
   URL — it just vanishes from the sidebar with no warning. `showcase.mdx` sat orphaned
   that way until it was caught by audit.

3. In prose, wrap any `<placeholder>` or `{brace}` in backticks — a bare `<` or `{`
   breaks MDX parsing.

## Deploy

`stella.oxagen.sh` is a Node process on an AWS instance behind Caddy, in
account `578673726240`. It was on Vercel until that account was suspended over
an unpaid balance and every site began answering `402`; Vercel is not a
fallback and nothing here may depend on it.

**The deploy step lives in `.github/workflows/docs.yml`** (#561): its `deploy`
job publishes on every push to `main` that touches the site, and on manual
dispatch. It builds with `STANDALONE=1` — which is what switches
`next.config.mjs` to `output: standalone` — packages the server together with
the static assets and `public/` that the standalone output deliberately omits,
uploads one tarball to the deploy bucket, and sends one SSM command that
restarts the service on the node. `pnpm-workspace.yaml` (in this directory)
approves the `esbuild` / `sharp` build scripts so `pnpm install` exits cleanly.

Three constraints on that job, each of which has already been paid for once:

- **It builds on an arm runner.** The instance is a `t4g.medium`. An artifact
  built on x86 passes every test and then fails to load a native module on the
  node, at first request rather than at build.
- **`environment: production` is load-bearing.** There is no stored AWS key.
  The job exchanges its GitHub OIDC token for a session on the
  `gha-deploy-stella` role, which trusts exactly one subject:
  `repo:macanderson/stella:environment:production`. Deleting that line does not
  loosen the deploy, it breaks it. The environment must exist in repository
  settings for the exchange to succeed at all.
- **The role has no shell on the instance.** It may write one S3 object and
  send one SSM document whose only argument is a service name constrained at
  the API. That instance also runs Postgres, Neo4j and ClickHouse.

If the new build does not answer a health check within 60 seconds the node puts
the previous release back and the job goes red, so a bad merge costs a minute
of degraded service rather than an outage lasting until someone notices. The
last step then re-checks `https://stella.oxagen.sh/` through DNS, TLS and Caddy,
because the node's own health check is a loopback request and would pass for a
service that is up behind a broken front door.

The infrastructure, the node-side script and its `oxagen-run.json` contract
live in the `oxagen-aws-infra` repository (`stacks/ci-deploy`, `tools/node/`).
