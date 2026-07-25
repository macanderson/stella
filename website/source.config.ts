import { defineDocs, defineConfig } from "fumadocs-mdx/config";
import lastModified from "fumadocs-mdx/plugins/last-modified";

export const docs = defineDocs({
  dir: "content/docs",
});

export default defineConfig({
  // Stamps every page with `lastModified` = the git commit date of its MDX file,
  // so the sitemap can report when a page actually changed instead of when the
  // site was last built. A build timestamp claims all ~76 URLs changed on every
  // deploy, which is precisely the signal that trains crawlers to ignore
  // lastModified entirely. The plugin yields `undefined` when git history for a
  // file is unavailable (a shallow clone) — set VERCEL_DEEP_CLONE=1 on Vercel so
  // it isn't. See src/app/sitemap.ts for what an absent date means there.
  plugins: [lastModified()],
});
