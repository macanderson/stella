# Stella CLI Docs

The documentation site for the [Stella CLI](https://github.com/macanderson/stella) —
destined for **stella.oxagen.sh**.

Built with [Next.js](https://nextjs.org) (App Router) + [Fumadocs](https://fumadocs.dev)
(`fumadocs-core` / `fumadocs-ui` / `fumadocs-mdx`) + Tailwind CSS v4. Branded with the
Stella identity — the gold chevron+cells mark on a warm Night/Paper palette (see
`src/app/global.css` for the tokens and `public/brand/` for the logo lockups).

## Develop

```bash
pnpm install
pnpm dev          # http://localhost:3400
```

## Build

```bash
pnpm build        # static export-friendly Next build
pnpm start        # serve the production build on :3400
pnpm typecheck    # tsc --noEmit
```

## Structure

```
content/docs/            # all documentation (MDX + meta.json ordering)
  index.mdx              # Introduction
  installation.mdx
  quickstart.mdx
  providers.mdx          # provider matrix + credential chain
  commands/              # per-command reference (run, chat, goal, monitor, …)
  configuration/         # settings.json (scope hierarchy), credentials
  tools/                 # built-in tools, permissions, custom tools, MCP, hooks
  agent-engine.mdx       # the step loop + verify_done
  goal-mode.mdx          # judged rounds + cross-family judge
  memory.mdx             # memories, reflections, skills, code graph
  telemetry.mdx          # local SQLite metering + budget
  scripting.mdx          # headless JSON output for CI

src/app/                 # Next.js App Router
  (home)/                # marketing landing page
  docs/                  # Fumadocs docs shell
  api/search/            # Fumadocs search route
src/lib/source.ts        # Fumadocs content source loader
src/mdx-components.tsx    # MDX component map
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
   `"---Label---"` entries for section separators.

3. In prose, wrap any `<placeholder>` or `{brace}` in backticks — a bare `<` or `{`
   breaks MDX parsing.

## Deploy

Deploys as a standard Next.js app. On Vercel, the project auto-detects Next.js + pnpm; set
the production domain to `stella.oxagen.sh`. `pnpm-workspace.yaml` approves the `esbuild` /
`sharp` build scripts so `pnpm install` exits cleanly in CI.
