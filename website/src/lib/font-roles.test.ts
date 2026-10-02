import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

/**
 * Every font role the site's stylesheets set must read a variable that the
 * kit's `next/font` loader sets on <html>.
 *
 *   pnpm test
 *
 * ## What went wrong without it
 *
 * On 2026-10-02 the kit made Aeonik the house sans and renamed its loader's
 * variable from `--font-geist` to `--font-aeonik` (oxageninc/brand#81). The
 * sync copied the new loader, but `src/app/global.css` still pointed
 * `--font-sans` and `--font-display` at `--font-geist`. A var() that names an
 * unset variable makes the whole declaration invalid, so both roles fell back
 * to Tailwind's `ui-sans-serif`. Every page on stella.oxagen.sh set its text
 * and headings in the system sans, the browser never fetched Aeonik, and no
 * check failed.
 *
 * The brand sync now writes the roles from the kit into
 * `src/brand/house-type.css`, beside the loader it copies, so the two change
 * together. This test holds that: a role that names a variable the loader
 * does not set fails here, whichever stylesheet sets it.
 */

const SITE = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string): string => readFileSync(join(SITE, rel), "utf8");

/** The stylesheets that may set a font role. */
const SHEETS = ["brand/house-type.css", "app/global.css"];
/** The roles the site's rules and the kit's type classes read. */
const ROLES = ["--font-sans", "--font-display", "--font-mono", "--font-wordmark"];

test("every font role reads a variable the kit's next/font loader sets", () => {
  const loaded = new Set(
    [...read("brand/next-fonts.ts").matchAll(/variable:\s*"(--font-[\w-]+)"/g)].map((m) => m[1]),
  );
  assert.ok(loaded.size > 0, "src/brand/next-fonts.ts sets no --font-* variable. Run the brand sync.");

  const set = new Set<string>();
  for (const sheet of SHEETS) {
    const css = read(sheet).replace(/\/\*[\s\S]*?\*\//g, "");
    for (const m of css.matchAll(/(--font-[\w-]+)\s*:\s*([^;]+);/g)) {
      if (!ROLES.includes(m[1])) continue;
      set.add(m[1]);
      for (const ref of m[2].matchAll(/var\(\s*(--font-[\w-]+)/g)) {
        assert.ok(
          loaded.has(ref[1]),
          `src/${sheet} points ${m[1]} at ${ref[1]}, which src/brand/next-fonts.ts does not set, ` +
            "so the role falls back to the system face. Let the brand sync set the role in " +
            "src/brand/house-type.css, and remove it from src/app/global.css.",
        );
      }
    }
  }
  for (const role of ROLES) {
    assert.ok(set.has(role), `no stylesheet sets ${role}. Run node scripts/sync-brand-assets.mjs.`);
  }
});
