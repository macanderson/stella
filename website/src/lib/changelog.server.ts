import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { parseChangelog, type Release } from "./changelog";

/**
 * The I/O half of the changelog reader: find CHANGELOG.md and read it.
 *
 * Split from `changelog.ts` because that module is imported by the client
 * component too, and a `node:fs` anywhere in a client graph fails the Turbopack
 * build outright ("the chunking context does not support external modules").
 * That failure IS the guard — importing this from a client component cannot
 * reach production — so the `server-only` package would add a dependency to
 * restate an error the bundler already raises. The `.server` suffix is the
 * reminder; the build is the enforcement.
 *
 * The page that calls this is statically rendered, so this runs at build time —
 * which is what keeps the site and the repository from drifting: there is no
 * second copy of the release notes to forget to update. The hand-maintained MDX
 * page this replaced had been stuck on "Current release: 0.6.44" while the tree
 * was on 0.6.131.
 */

/**
 * The repo root, derived from this module's own location, so the path survives
 * being built from any working directory (Vercel builds from `website/`,
 * `pnpm dev` from wherever you launched it). `import.meta.url` rather than
 * `process.cwd()` for exactly that reason.
 */
function changelogPath(): string {
  const here = dirname(fileURLToPath(import.meta.url));
  return join(here, "..", "..", "..", "CHANGELOG.md");
}

/**
 * Every release in CHANGELOG.md, newest first (the file's own order, which the
 * roll maintains by inserting at the top).
 *
 * Deliberately uncached: this is evaluated once per build for a static route,
 * so a cache would buy nothing and a stale-cache bug on the one page whose
 * whole point is freshness would cost more than the read.
 */
export function getReleases(): Release[] {
  return parseChangelog(readFileSync(changelogPath(), "utf8"));
}
