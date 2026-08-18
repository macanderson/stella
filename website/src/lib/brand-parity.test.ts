import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

/**
 * The site's palette and its logo assets are copies of `docs/brand/`. This is
 * what makes that true rather than merely intended.
 *
 *   pnpm test
 *
 * ## What went wrong without it
 *
 * `src/app/tokens.css` carried the sentence "every value below is copied from
 * it verbatim — do not tune a hex here, change the kit and mirror it" while
 * sitting a whole brand version behind: the kit moved to Bronze Gold #00D1F9
 * on Ink #070B10 in the 2026-08-11 rebrand and this site stayed on v1.0's
 * Phosphor Gold #FFB000 on Ink #0B0B0C. All thirteen SVGs under
 * `public/brand/` were stale with it, as were seven of the eight PWA icons,
 * `src/app/icon.svg`, the favicon and the OG card's literals — so the site
 * served a **bronze wordmark beside phosphor chrome**, and the paragraph
 * claiming they matched was the only thing anyone had to go on.
 *
 * The kit is a shared cell that several surfaces copy, and a comment cannot
 * hold one. The Rust side already learned this: `provider_parity.rs` enforces
 * invariant #8 and `crates/stella-cli/tests/design_token_parity.rs` enforces
 * the instrument palette, both as a matrix checked from both sides in a plain
 * test. This is the same shape for the marketing surface — which is the copy
 * of the product most readers ever see, and the one nobody can eyeball beside
 * the kit.
 *
 * ## What it does and does not check
 *
 * It checks **values**, not design: the brand-core hexes, the eleven-stop gold
 * ramp, the warm-neutral ramp, and byte-identity of the logo SVGs and PWA
 * icons. The site's own tokens — the functional status hues, the type scale,
 * the `lp-*` landing layer — are deliberately outside the matrix; the kit does
 * not define them and this test must not freeze them.
 *
 * **This is not #2594 reopened.** That issue proposed holding this site to the
 * *instrument* palette — the Observatory's monochrome system — and was closed
 * `wontfix` on the correct ground that a marketing page is legitimately not an
 * instrument and may carry more gold than a page made of data. Nothing here
 * touches that question: the site keeps its own chrome, its own accent usage
 * and its own status hues. The only thing asserted is that the ramp this file
 * already calls normative is the ramp it actually ships, which is a question
 * about a copy rather than about a design.
 *
 * The comparison is textual and case-insensitive because the kit writes
 * `#00D1F9` and CSS convention here writes `#00d1f9`. That is the one
 * difference allowed between the two files.
 */

const TEST_FILE = fileURLToPath(import.meta.url);
const HERE = dirname(TEST_FILE);
const REPO = join(HERE, "..", "..", "..");
const KIT = join(REPO, "docs", "brand");
const SITE = join(HERE, "..");

function read(path: string): string {
  return readFileSync(path, "utf8");
}

/**
 * Every `--stella-*: <value>;` declaration in a CSS file, lowercased.
 *
 * Parsed rather than string-matched so a reordering of the kit cannot fail
 * this test and a changed value cannot pass it. Only the first definition of
 * a name is kept: both files define the semantic aliases twice (light then
 * dark), and this matrix is about the raw ramp above them.
 */
function tokens(css: string): Map<string, string> {
  const found = new Map<string, string>();
  for (const [, name, value] of css.matchAll(
    /(--stella-[a-z0-9-]+)\s*:\s*([^;]+);/g,
  )) {
    const key = name.toLowerCase();
    if (!found.has(key)) found.set(key, value.trim().toLowerCase());
  }
  return found;
}

/** The tokens the kit owns and the site must mirror exactly. */
const MIRRORED = [
  "--stella-brand",
  "--stella-brand-deep",
  "--stella-ink",
  "--stella-paper",
  "--stella-paper-bg",
  ...[50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 950].map(
    (stop) => `--stella-brand-${stop}`,
  ),
  ...[50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 950].map(
    (stop) => `--stella-neutral-${stop}`,
  ),
];

test("the site's brand tokens are the kit's, value for value", () => {
  const kit = tokens(read(join(KIT, "css", "tokens.css")));
  const site = tokens(read(join(SITE, "app", "tokens.css")));

  for (const name of MIRRORED) {
    const expected = kit.get(name);
    assert.ok(expected, `docs/brand/css/tokens.css defines ${name}`);
    assert.equal(
      site.get(name),
      expected,
      `${name} has drifted from docs/brand/css/tokens.css — change the kit ` +
        `and mirror it, never tune a hex here`,
    );
  }
});

test("no retired brand value survives anywhere in the site", () => {
  // Two rebrands' own regressions, asserted directly. The first block is the
  // v1.0 phosphor gold that shipped beside bronze assets for the life of the
  // 2026-08-11 drift; the second is the v2.0 bronze itself, retired by the
  // v3.0 ion recolour. A value only leaves this list if it comes back, which
  // it must not.
  const RETIRED = [
    // v1.0 — phosphor gold on ink
    "#ffb000",
    "#0b0b0c",
    "#f4f1ea",
    "#f6f2e9",
    "#a37200",
    "255 176 0",
    // v2.0 — bronze gold on warm ink, and its warm neutral ramp
    "#c58a32",
    "#8b5e1a",
    "#10100f",
    "#f2eee5",
    "#f5f0e6",
    "#a97227",
    "#d39f50",
    "#674415",
    "#a19a8e",
    "#6f675b",
    "#ded5c6",
    "197 138 50",
    "--stella-gold",
  ];

  const offenders: string[] = [];
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name !== "node_modules") walk(path);
        continue;
      }
      if (!/\.(css|tsx?|svg|html)$/.test(entry.name)) continue;
      // Two files name the retired ramps on purpose: tokens.css in the
      // comment recording why this test exists, and this test in the list
      // above. Naming the defect is not committing it.
      if (path.endsWith("app/tokens.css") || path === TEST_FILE) continue;
      const text = read(path).toLowerCase();
      for (const value of RETIRED) {
        if (text.includes(value)) {
          offenders.push(`${path} contains retired v1.0 value ${value}`);
        }
      }
    }
  };
  walk(SITE);
  walk(join(SITE, "..", "public"));

  assert.deepEqual(offenders, [], offenders.join("\n"));
});

test("the site's logo SVGs are byte-identical to the kit's", () => {
  const kitDir = join(KIT, "logo", "svg");
  const siteDir = join(SITE, "..", "public", "brand");

  const expected = readdirSync(kitDir).filter((f) => f.endsWith(".svg")).sort();
  const actual = readdirSync(siteDir).filter((f) => f.endsWith(".svg")).sort();
  assert.deepEqual(
    actual,
    expected,
    "public/brand/ must carry exactly the kit's SVG set",
  );

  for (const name of expected) {
    assert.equal(
      read(join(siteDir, name)),
      read(join(kitDir, name)),
      `public/brand/${name} has drifted from docs/brand/logo/svg/${name} — ` +
        `re-copy it rather than editing either side`,
    );
  }
});

test("the app icon is the kit's logomark", () => {
  assert.equal(
    read(join(SITE, "app", "icon.svg")),
    read(join(KIT, "logo", "svg", "logomark-color.svg")),
    "src/app/icon.svg must be docs/brand/logo/svg/logomark-color.svg",
  );
});

test("the PWA icons are byte-identical to the kit's", () => {
  // The site renames the kit's two maskables; every other file keeps its name.
  const PAIRS: Array<[site: string, kit: string]> = [
    ["favicon-16.png", "favicon-16.png"],
    ["favicon-32.png", "favicon-32.png"],
    ["favicon-48.png", "favicon-48.png"],
    ["icon-192.png", "icon-192.png"],
    ["icon-512.png", "icon-512.png"],
    ["maskable-192.png", "icon-maskable-192.png"],
    ["maskable-512.png", "icon-maskable-512.png"],
    ["safari-pinned-tab.svg", "safari-pinned-tab.svg"],
  ];

  for (const [siteName, kitName] of PAIRS) {
    const mine = readFileSync(join(SITE, "..", "public", "icons", siteName));
    const theirs = readFileSync(join(KIT, "pwa", kitName));
    assert.ok(
      mine.equals(theirs),
      `public/icons/${siteName} has drifted from docs/brand/pwa/${kitName} — ` +
        `re-copy it when the kit regenerates`,
    );
  }
});

test("favicon.ico carries the kit's art in an RGBA encoding", () => {
  // Two things have to hold at once, and they pull apart.
  //
  // The kit renders its favicons opaque — the mark on an Ink tile — so its
  // `favicon.ico` embeds **RGB** PNGs (colour type 2). Next's image pipeline
  // decodes `src/app/favicon.ico` through the `ico` crate, which accepts only
  // RGBA, and fails the whole production build on anything else:
  //
  //     Error: Turbopack build failed with 1 errors:
  //     ./website/src/app/favicon.ico
  //     Processing image failed
  //     Caused by: Format error decoding Ico: The PNG is not in RGBA format!
  //
  // So this one file is deliberately NOT a byte-copy of the kit's: it is the
  // kit's pixels re-encoded with an opaque alpha channel. That is why it is
  // absent from the PWA-icon byte-identity test above, and why the exception
  // is asserted here rather than left as a silent difference someone would
  // later "fix" by re-copying — which is exactly how the build broke.
  const ico = readFileSync(join(SITE, "app", "favicon.ico"));
  const count = ico.readUInt16LE(4);
  assert.ok(count > 0, "favicon.ico declares at least one image");

  const PNG_MAGIC = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  for (let i = 0; i < count; i++) {
    const entry = 6 + i * 16;
    const size = ico.readUInt32LE(entry + 8);
    const offset = ico.readUInt32LE(entry + 12);
    const blob = ico.subarray(offset, offset + size);
    assert.ok(
      blob.subarray(0, 8).equals(PNG_MAGIC),
      `favicon.ico entry ${i} must be PNG-encoded`,
    );
    // IHDR runs length(4) + "IHDR"(4) + width(4) + height(4) + depth(1), so
    // the colour-type byte sits at offset 25. Type 6 is truecolour + alpha.
    assert.equal(
      blob[25],
      6,
      `favicon.ico entry ${i} is PNG colour type ${blob[25]}, not 6 (RGBA) — ` +
        `Next's ico decoder rejects it and the production build fails`,
    );
  }
});

test("every icon the manifest advertises exists", () => {
  // manifest.ts points at four files by path. A rename in the kit that this
  // site mirrors without updating the manifest is a 404 on install, which no
  // page render would catch.
  const manifest = read(join(SITE, "app", "manifest.ts"));
  const iconsDir = join(SITE, "..", "public", "icons");
  const present = new Set(readdirSync(iconsDir));

  const referenced = [...manifest.matchAll(/src:\s*"\/icons\/([^"]+)"/g)].map(
    (m) => m[1],
  );
  assert.ok(referenced.length > 0, "manifest.ts references icons by path");

  for (const name of referenced) {
    assert.ok(
      present.has(name),
      `manifest.ts advertises /icons/${name}, which does not exist`,
    );
  }
});

test("the manifest's theme colours are the kit's ink", () => {
  const manifest = read(join(SITE, "app", "manifest.ts")).toLowerCase();
  const ink = tokens(read(join(KIT, "css", "tokens.css"))).get("--stella-ink");
  assert.ok(ink, "the kit defines --stella-ink");
  for (const key of ["background_color", "theme_color"]) {
    assert.ok(
      manifest.includes(`${key}: "${ink}"`),
      `manifest.ts ${key} must be the kit's ink (${ink})`,
    );
  }
});
