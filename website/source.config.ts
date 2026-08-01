import { execSync } from "node:child_process";
import { defineDocs, defineConfig } from "fumadocs-mdx/config";
import lastModified from "fumadocs-mdx/plugins/last-modified";

export const docs = defineDocs({
  dir: "content/docs",
});

/**
 * A shallow clone still has one commit, so `git log -1 -- <file>` succeeds
 * for every file and returns that single commit's date — not "unavailable"
 * (which the plugin would turn into `undefined`), but a real date that is
 * wrong and identical across every page. Detecting the shallow clone here,
 * before the plugin runs, is what actually gets the "omit rather than lie"
 * behavior documented in src/app/sitemap.ts; the plugin's own fallback never
 * triggers on Vercel's default (shallow) checkout. Set VERCEL_DEEP_CLONE=1 on
 * Vercel to get real per-file dates instead of an omitted field.
 */
function isShallowClone(): boolean {
  try {
    return execSync("git rev-parse --is-shallow-repository").toString().trim() !== "false";
  } catch {
    return true;
  }
}

export default defineConfig({
  plugins: isShallowClone() ? [] : [lastModified()],
});
