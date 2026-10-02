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
 * On 2026-10-02 Aeonik replaced Geist as the house face, and the kit renamed
 * its loader's variable from `--font-geist` to `--font-aeonik`. The sync
 * copied the new loader, but `src/app/global.css` still pointed
 * `--font-sans` and `--font-display` at `--font-geist`. A var() that names an
 * unset variable makes the whole declaration invalid, so both roles fell back
 * to Tailwind's `ui-sans-serif`. Every page on stella.oxagen.sh set its text
 * and headings in the system sans, the browser never fetched Aeonik, and no
 * check failed.
 *
 * The brand sync now writes the roles from the kit into
 * `src/brand/house-type.css`, beside the loader it copies, so the two change
 * together. This test holds that, whichever stylesheet sets the role. A role
 * fails here when it reads a variable the loader does not set, or when it
 * reads no loader variable at all. A role may also read another role, as the
 * kit's `--font-heading` reads `--font-sans`.
 *
 * ## The heading face
 *
 * By the house type rule of 2026-10-02, h1 to h3 on a customer site take the
 * display face, Space Grotesk, and stella.oxagen.sh and its docs are one. The
 * kit's `--font-heading` reads the text face, as an app's headings do, so
 * `src/app/global.css` must point it at `--font-display`. Without that line
 * every heading on the site draws Aeonik and no check fails.
 */

const SITE = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string): string => readFileSync(join(SITE, rel), "utf8");

/** The stylesheets that may set a font role. */
const SHEETS = ["brand/house-type.css", "app/global.css"];
/** The roles the site's rules and the kit's type classes read. */
const ROLES = ["--font-sans", "--font-display", "--font-heading", "--font-mono", "--font-wordmark"];

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
      const refs = [...m[2].matchAll(/var\(\s*(--font-[\w-]+)/g)].filter((ref) => !ROLES.includes(ref[1]));
      const roleRefs = [...m[2].matchAll(/var\(\s*(--font-[\w-]+)/g)].filter((ref) => ROLES.includes(ref[1]));
      assert.ok(
        refs.length > 0 || roleRefs.length > 0,
        `src/${sheet} sets ${m[1]} to ${m[2].trim()}, which names a face without the loader's variable, ` +
          "so the face never loads. Let the brand sync set the role in src/brand/house-type.css.",
      );
      for (const ref of refs) {
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

test("the site points --font-heading at the display face, as a customer site", () => {
  const css = read("app/global.css").replace(/\/\*[\s\S]*?\*\//g, "");
  const values = [...css.matchAll(/--font-heading\s*:\s*([^;]+);/g)].map((m) => m[1].trim());
  assert.deepEqual(
    values,
    ["var(--font-display)"],
    "src/app/global.css must set --font-heading: var(--font-display) once, in :root, so h1 to h3 draw " +
      "Space Grotesk. The kit's --font-heading reads the text face, which is right for an app and wrong " +
      `for stella.oxagen.sh. It sets: ${values.join(", ") || "nothing"}.`,
  );
  const bare = /(^|\})\s*h1\s*,\s*h2\s*,\s*h3\s*\{[^}]*font-family\s*:\s*var\(--font-heading\)/m;
  assert.ok(
    bare.test(css),
    "src/app/global.css must set h1, h2, h3 { font-family: var(--font-heading); }, so a heading " +
      "with no class of its own takes the display face.",
  );
});
