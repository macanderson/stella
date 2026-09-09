import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { inflateSync } from "node:zlib";

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
 * sitting a whole brand version behind: the kit moved to Bronze Gold #D6962C
 * on Ink #10100F in the 2026-08-11 rebrand and this site stayed on v1.0's
 * Phosphor Gold #FFB000 on Ink #0B0B0C. All thirteen SVGs under
 * `public/brand/` were stale with it, as were seven of the eight PWA icons,
 * `src/app/icon.svg`, the favicon and the OG card's literals — so the site
 * served a **bronze wordmark beside phosphor chrome**, and the paragraph
 * claiming they matched was the only thing anyone had to go on.
 *
 * It happened twice more, which is why the matrix below keeps growing rather
 * than being trusted as finished:
 *
 *  - **v3.0 (#3658)** recoloured the product to Ion and **v4.0 (#3970)** took
 *    the brand hue back to gold on the product surfaces only — leaving this
 *    whole site, the kit's generated cuts and every binary asset on Ion. For
 *    a while the tree shipped two identities and a reader crossing from here
 *    to the Observatory watched the brand change hue (#3968).
 *  - **`src/app/favicon.ico` was never recoloured at all.** It sat on v2.0's
 *    bronze-on-warm-ink art (ground #10100F) straight through v3.0, because
 *    the test below asserted only its PNG *colour type* and never its pixels,
 *    and the retired-value sweep cannot read a binary. A guard that checks an
 *    encoding while the art rots is worse than none: it reports green. It now
 *    compares pixels against the kit.
 *
 * The kit is a shared cell that several surfaces copy, and a comment cannot
 * hold one. The Rust side already learned this: `provider_parity.rs` enforces
 * AGENTS.md #8 and `crates/stella-cli/tests/design_token_parity.rs` enforces
 * the instrument palette, both as a matrix checked from both sides in a plain
 * test. This is the same shape for the marketing surface — which is the copy
 * of the product most readers ever see, and the one nobody can eyeball beside
 * the kit.
 *
 * ## What it does and does not check
 *
 * It checks **values**, not design: the brand-core hexes, the eleven-stop gold
 * ramp, the cool-neutral ramp, and byte-identity of the logo SVGs and PWA
 * icons. The site's own tokens — the functional status hues, the type scale,
 * the `lp-*` landing layer — are outside the matrix; the kit does
 * not define them and this test must not freeze them.
 *
 * That exclusion is about the kit not *owning* those values, not a licence for
 * them to collide with the identity: v4.0 had to move `--stella-warning`,
 * which sat 8.7° from the brand in OKLCH hue once the brand went back to gold.
 * The reasoning lives beside the token in `src/app/tokens.css`.
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
 * `#D6962C` and CSS convention here writes `#d6962c`. That is the one
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
 * The retired hexes the palette itself declares, lowercased.
 *
 * `design/tokens/stella-tokens.json` already has to name every superseded
 * anchor, with a sentence saying which kit it came from — `check-tokens.py`
 * reads the same list. Taking them from there rather than restating them keeps
 * one list where a supersession has to be recorded instead of two, and the
 * second one silently going stale is exactly the failure this file was written
 * against.
 */
function bannedValues(): string[] {
  const palette: { banned: { values: { hex: string }[] } } = JSON.parse(
    read(join(REPO, "design", "tokens", "stella-tokens.json")),
  );
  return palette.banned.values.map((v) => v.hex.toLowerCase());
}

/** The `i`th embedded image of an ICO, as its raw bytes. */
function icoImage(ico: Buffer, i: number): Buffer {
  const entry = 6 + i * 16;
  const size = ico.readUInt32LE(entry + 8);
  const offset = ico.readUInt32LE(entry + 12);
  return ico.subarray(offset, offset + size);
}

/**
 * A PNG's pixels, normalised to RGBA, so two encodings of the same art compare
 * equal.
 *
 * Minimal — 8-bit, non-interlaced, colour type 2 or 6, which is
 * every PNG the house kit and this repo's rasteriser produce. Anything else
 * throws rather than being silently accepted, because a guard that quietly skips is the failure
 * this whole file exists to prevent. No dependency: `node:zlib` is built in,
 * and adding an image library to run one assertion is not worth it.
 */
function rgbaPixels(png: Buffer): {
  width: number;
  height: number;
  pixels: Buffer;
} {
  let ihdr: Buffer | undefined;
  const idat: Buffer[] = [];
  for (let i = 8; i < png.length; ) {
    const length = png.readUInt32BE(i);
    const tag = png.subarray(i + 4, i + 8).toString("latin1");
    const body = png.subarray(i + 8, i + 8 + length);
    if (tag === "IHDR") ihdr = body;
    else if (tag === "IDAT") idat.push(body);
    i += 12 + length;
  }
  assert.ok(ihdr, "PNG has an IHDR chunk");

  const width = ihdr.readUInt32BE(0);
  const height = ihdr.readUInt32BE(4);
  const depth = ihdr[8];
  const colour = ihdr[9];
  const interlace = ihdr[12];
  assert.equal(depth, 8, `unsupported PNG bit depth ${depth}`);
  assert.equal(interlace, 0, "interlaced PNGs are not supported here");
  assert.ok(colour === 2 || colour === 6, `unsupported colour type ${colour}`);

  const bpp = colour === 6 ? 4 : 3;
  const stride = width * bpp;
  const raw = inflateSync(Buffer.concat(idat));

  const rows: Buffer[] = [];
  let prev = Buffer.alloc(stride);
  for (let y = 0, pos = 0; y < height; y++) {
    const filter = raw[pos++];
    const line = Buffer.from(raw.subarray(pos, pos + stride));
    pos += stride;
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? line[x - bpp] : 0;
      const b = prev[x];
      const c = x >= bpp ? prev[x - bpp] : 0;
      switch (filter) {
        case 0:
          break;
        case 1:
          line[x] = (line[x] + a) & 0xff;
          break;
        case 2:
          line[x] = (line[x] + b) & 0xff;
          break;
        case 3:
          line[x] = (line[x] + ((a + b) >> 1)) & 0xff;
          break;
        case 4: {
          const p = a + b - c;
          const pa = Math.abs(p - a);
          const pb = Math.abs(p - b);
          const pc = Math.abs(p - c);
          line[x] = (line[x] + (pa <= pb && pa <= pc ? a : pb <= pc ? b : c)) & 0xff;
          break;
        }
        default:
          throw new Error(`unknown PNG filter ${filter}`);
      }
    }
    rows.push(line);
    prev = line;
  }

  if (colour === 6) {
    return { width, height, pixels: Buffer.concat(rows) };
  }
  const pixels = Buffer.alloc(width * height * 4);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const to = (y * width + x) * 4;
      rows[y].copy(pixels, to, x * 3, x * 3 + 3);
      pixels[to + 3] = 0xff;
    }
  }
  return { width, height, pixels };
}

/**
 * Every `--stella-*` and `--st-*` declaration in a CSS file, lowercased.
 *
 * Parsed rather than string-matched so a reordering of the kit cannot fail
 * this test and a changed value cannot pass it. Only the first definition of
 * a name is kept: both files define the semantic aliases twice (light then
 * dark), and this matrix is about the raw ramp above them.
 *
 * `--st-*` is the generated ramp (`design/tokens/stella-tokens.json` ->
 * `scripts/gen-tokens.py`). It was outside this parse until v5.0 made it the
 * thing the roles are defined *in terms of*, at which point a matrix that read
 * only `--stella-*` could no longer see the values it was checking.
 */
function tokens(css: string): Map<string, string> {
  const found = new Map<string, string>();
  for (const [, name, value] of css.matchAll(
    /(--st(?:ella)?-[a-z0-9-]+)\s*:\s*([^;]+);/g,
  )) {
    const key = name.toLowerCase();
    if (!found.has(key)) found.set(key, value.trim().toLowerCase());
  }
  return found;
}

/**
 * The brand-core roles the kit owns. Spelled out because they are a *contract*
 * — the five names every surface reaches for — rather than whatever the kit
 * happens to contain.
 *
 * The eleven-stop `--stella-brand-*` and `--stella-neutral-*` ramps used to be
 * listed here too. v5.0 replaced both with the flat generated `--st-*` ramp
 * (#4066) and left the names behind, so this matrix spent a merge asserting
 * that the kit defines twenty-two properties it had just deleted. The ramp
 * half is now read *from the kit* below, which is what stops that recurring:
 * a stop the kit drops leaves the matrix by itself, and one it adds joins
 * without anyone remembering to.
 */
const MIRRORED_CORE = [
  "--stella-brand",
  "--stella-brand-deep",
  "--stella-ink",
  "--stella-paper",
  "--stella-paper-bg",
];

/**
 * One level of `var(--x)` indirection, resolved against the file's own map.
 *
 * The site writes `--stella-brand: var(--st-gold)` where the kit writes
 * `#D6962C`, and that difference is not drift — it is the site declining to
 * repeat a value the generated ramp above it already carries. Comparing the
 * raw declarations would fail on it, and "fixing" that by pasting the hex back
 * into the site is precisely the duplication `design/tokens/` exists to end.
 *
 * So both sides are resolved before comparison. This does not weaken the
 * check: the `--st-*` ramp the site resolves *through* is itself in the matrix
 * below, so a site that mirrored the role correctly but tuned the underlying
 * stop still fails — on the stop.
 *
 * Not a general CSS resolver: a chain longer than the eight steps
 * below, or a `var()` with a fallback, throws rather than silently comparing
 * an unresolved string, because a guard that quietly gives up is the failure
 * this whole file exists to prevent.
 */
function resolve(value: string, map: Map<string, string>): string {
  let current = value;
  for (let step = 0; step < 8; step++) {
    const match = /^var\(\s*(--[a-z0-9-]+)\s*\)$/.exec(current);
    if (!match) return current;
    const next = map.get(match[1]);
    assert.ok(next, `${current} refers to ${match[1]}, which is not defined`);
    current = next;
  }
  throw new Error(`var() chain from ${value} did not terminate in 8 steps`);
}

test("the site's brand tokens are the kit's, value for value", () => {
  const kit = tokens(read(join(KIT, "css", "tokens.css")));
  const site = tokens(read(join(SITE, "app", "tokens.css")));

  // The generated ramp, taken from the kit rather than restated here.
  const ramp = [...kit.keys()].filter((name) => name.startsWith("--st-"));
  // Every name the kit sheet declares is a token that exists. This is the half
  // the floor below cannot do: a count says nothing about a `--st-` name that
  // the palette has no entry for, and the loop underneath would then compare a
  // role against itself and pass.
  const authority: { tokens: { css: string }[] } = JSON.parse(
    read(join(REPO, "design", "tokens", "stella-tokens.json")),
  );
  const live = new Set(authority.tokens.map((t) => t.css));
  const unknown = ramp.filter((name) => !live.has(name));
  assert.deepEqual(
    unknown,
    [],
    `docs/brand/css/tokens.css declares --st-* name(s) that design/tokens/` +
      `stella-tokens.json does not define: ${unknown.join(", ")}`,
  );

  // And the other direction: every token the palette declares has a row here.
  //
  // This used to be `ramp.length >= 20`, a floor on the count — which cannot
  // see a token that was never mirrored in the first place, and twelve had not
  // been. The sheet carried 20 of the palette's 32, and the missing twelve were
  // the entire warm-paper half plus `void`: the two documents a designer reads
  // to learn the palette did not contain the light scheme (#4978). A count
  // could not have found that, and no other check over this file could either
  // — `check-tokens.py` asks whether every hex here is a live token, never
  // whether a live token is absent.
  //
  // Equality rather than a floor, because the kit is the kit. A published
  // *subset* is a defensible thing for the marketing site to be, and it is not
  // what this file is: `docs/brand/` is what a designer is handed.
  const missing = [...live].filter((name) => !kit.has(name));
  assert.deepEqual(
    missing,
    [],
    `docs/brand/css/tokens.css must carry every token in ` +
      `design/tokens/stella-tokens.json; it is missing: ${missing.join(", ")}`,
  );

  for (const name of [...MIRRORED_CORE, ...ramp]) {
    const expected = kit.get(name);
    assert.ok(expected, `docs/brand/css/tokens.css defines ${name}`);
    assert.equal(
      resolve(site.get(name) ?? "", site),
      resolve(expected, kit),
      `${name} has drifted from docs/brand/css/tokens.css — change the kit ` +
        `and mirror it, never tune a hex here`,
    );
  }
});

test("no retired brand value survives anywhere in the site", () => {
  // Every rebrand's own regression, asserted directly.
  //
  // A value leaves this list only when a later version makes it **live
  // again**, which is not hypothetical: v4.0 took the brand hue back to v2.0's
  // the gold ramp value-for-value, so #d6962c and its stops moved from this
  // list into `tokens.css`. What v4.0 did *not* take back is the warm neutral
  // page those stops used to sit on — v3.0's cool graphite ramp and Obsidian
  // ground are kept — so the warm values stay retired and are what this block
  // still names. `crates/stella-cli/src/export/tests.rs` carries the same
  // split for the same reason; the two must move together.
  const RETIRED = [
    // v1.0 — phosphor gold on ink
    //
    // These two were swept into `#d6962c`/`#10100f` — the *live* v5.0 gold and
    // canvas — by the v5.0 hex migration (#4066), which turned this block into
    // a ban on the current brand and made every correct surface an offender.
    // `scripts/check-tokens.py` now lists this file as a ban site so a sweep
    // skips it; the values below are the v1.0 ones they were before.
    "#ffb000",
    "#0b0b0c",
    // #f2eee5 — v1.0's warm Paper — is **live again** as v5.1's `text`,
    // the white every surface off the deck draws on dark. It leaves this list
    // on the rule stated above. The gold and the ground beside it do not: v1.0's
    // phosphor gold fails the resting-gold clamp and its ink is superseded, so
    // this restores one value rather than the kit.
    "#f6f2e9",
    "#a37200",
    // Both spellings of the channel triple. The space-separated form is the
    // modern CSS `rgb(r g b / a)`; the comma form is what an `rgba()` literal
    // and Satori (which has no cascade, so the OG card writes its washes out
    // by hand) actually use. Only the first was listed, and the OG card's CTA
    // shipped an `rgba(214,150,44,0.12)` wash straight through the v3.0
    // recolour because a hex sweep cannot see a channel triple and this guard
    // was not looking for one.
    "255 176 0",
    "255,176,0",
    "255, 176, 0",
    // v2.0 — the WARM page the bronze gold used to sit on.
    //
    // Two of its values LEFT this list at v6.0, on the rule stated at the top:
    // #10100f and #f2eee5 are the house system's ink and its off-white, so
    // they are live again on every surface. That is the second time this has
    // happened to them and it is the same mechanism both times — a warm kit
    // returning — which is why the rule is a rule rather than a one-off.
    "#f5f0e6",
    "#a19a8e",
    "#6f675b",
    "#ded5c6",
    // v3.0 — the ion ramp, all eleven stops, retired by v4.0's return to gold.
    // Listed in full, not by the brand core alone. The drift this catches is a
    // half-applied recolour: the site sat on ion while the kit and the product
    // surfaces had moved. A partial list is how the next one gets through.
    "#eafaff",
    "#c7f2ff",
    "#9de9ff",
    "#72e1ff",
    "#46dbff",
    "#00d1f9",
    "#00b0d2",
    "#0094b1",
    "#00778f",
    "#005769",
    "#003440",
    "0 209 249",
    "0,209,249",
    "0, 209, 249",
    "--stella-gold",
    // v5.0 — "black and gold", superseded by the house system — is NOT written
    // out here. Its values come from the palette's own ban list below, which is
    // where every superseded anchor already had to be declared.
    //
    // Restating them would be the duplication this file exists to prevent,
    // one level up: two hand-maintained lists of the same retired values, and
    // the next supersession updating one of them. The list above is the older
    // half, kept as-is because it predates the ban list carrying a reason for
    // each entry.
    ...bannedValues(),
    // The channel triples, both spellings. The OG card's washes are written out
    // by hand for Satori, which has no cascade, so a hex sweep cannot see them
    // — the lesson the v1.0 entry above records, applied ahead of time.
    ...bannedValues().flatMap((hex) => {
      const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
      return [`${r} ${g} ${b}`, `${r},${g},${b}`, `${r}, ${g}, ${b}`];
    }),
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
      // `%23` is `#` percent-encoded, which is how a hex colour is spelled
      // inside an `<svg …>` data URI — and this site ships two of those as
      // inline favicons. Normalising rather than listing every encoded twin
      // keeps one entry per retired value: `vision.html` sat on v1.0's
      // `%23FFB000` on `%230B0B0C` through three rebrands because a sweep for
      // `#d6962c` cannot see it, the same blind spot the channel-triple
      // entries above exist for.
      const text = read(path).toLowerCase().replaceAll("%23", "#");
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

/**
 * `stella-favicon.svg` is the one mark that does not land in `public/brand/`:
 * Next takes the tab icon from `src/app/icon.svg` by file convention, so it is
 * asserted on its own below. The spinner is not a logo and lives beside them
 * rather than in `logo/svg/`. Everything else is a straight copy.
 */
const FAVICON_SVG = "stella-favicon.svg";

test("the site's logo SVGs are byte-identical to the kit's", () => {
  const kitDir = join(KIT, "logo", "svg");
  const siteDir = join(SITE, "..", "public", "brand");

  const expected = readdirSync(kitDir)
    .filter((f) => f.endsWith(".svg") && f !== FAVICON_SVG)
    .sort();
  const actual = readdirSync(siteDir)
    .filter((f) => f.endsWith(".svg") && !f.includes("spinner"))
    .sort();
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
        `run \`make brand-sync\` rather than editing either side`,
    );
  }
});

test("the app icon is the kit's favicon mark", () => {
  assert.equal(
    read(join(SITE, "app", "icon.svg")),
    read(join(KIT, "logo", "svg", FAVICON_SVG)),
    `src/app/icon.svg must be docs/brand/logo/svg/${FAVICON_SVG}`,
  );
});

test("the animated lockup's motion is the kit's spinner", () => {
  // The shimmer on the landing page and the shimmer in the kit's own spinner
  // are one animation, which is only true while the numbers come from the
  // file. `SWEEP` is generated from it; this is the assertion that the file
  // the generator read is the one the site ships.
  const spinner = read(join(KIT, "spinners", "stella-spinner-wordmark.svg"));
  const generated = read(join(SITE, "components", "brand-marks.generated.ts"));
  for (const [label, key, pattern] of [
    ["duration", "durationSec", /animation:sweep-[^ ]+ ([\d.]+)s/],
    ["travel", "travelPx", /translateX\(([\d.]+)px\)/],
  ] as const) {
    const stated = spinner.match(pattern)?.[1];
    assert.ok(stated, `the kit's spinner states its ${label}`);
    const mirrored = generated.match(new RegExp(`${key}: ([\\d.]+)`))?.[1];
    assert.ok(mirrored, `SWEEP declares ${key}`);
    // Numeric, not textual: the generator writes these through `Number`, so
    // the kit's "137.90" is mirrored as `137.9`. Same value, and comparing the
    // spellings would fail on a trailing zero.
    assert.equal(
      Number(mirrored),
      Number(stated),
      `SWEEP has drifted from the kit's ${label} (${stated}) — run ` +
        `\`node scripts/sync-brand-assets.mjs\``,
    );
  }
});

test("the PWA icons are byte-identical to the kit's", () => {
  // The site renames the kit's two maskables; every other file keeps its name.
  // `scripts/sync-brand-assets.mjs` performs this exact mapping; this table is
  // the check on its work.
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
        `run \`node scripts/sync-brand-assets.mjs\` when the kit regenerates`,
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
  // So this one file is NOT a byte-copy of the kit's. It is the kit's pixels,
  // re-encoded with an opaque alpha channel. That keeps it out of the PWA-icon
  // byte-identity test above. The exception is asserted here rather than left
  // silent: a silent one gets "fixed" by re-copying, and that broke the build.
  //
  // Checking only the encoding is what let this file rot. It sat on v2.0's
  // bronze-on-warm-ink art (ground #10100F) through the whole v3.0 ion
  // recolour and into v4.0, green the entire time, because "is it RGBA?" is
  // not a question about the art. Both halves are asserted now: the encoding
  // Next requires, AND that the pixels are the kit's.
  const site = readFileSync(join(SITE, "app", "favicon.ico"));
  const kit = readFileSync(join(KIT, "pwa", "favicon.ico"));
  const count = site.readUInt16LE(4);
  assert.ok(count > 0, "favicon.ico declares at least one image");
  assert.equal(
    count,
    kit.readUInt16LE(4),
    "favicon.ico must carry the same number of sizes as the kit's",
  );

  const PNG_MAGIC = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  for (let i = 0; i < count; i++) {
    const blob = icoImage(site, i);
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

    const mine = rgbaPixels(blob);
    const theirs = rgbaPixels(icoImage(kit, i));
    assert.deepEqual(
      { width: mine.width, height: mine.height },
      { width: theirs.width, height: theirs.height },
      `favicon.ico entry ${i} is a different size from the kit's`,
    );
    assert.ok(
      mine.pixels.equals(theirs.pixels),
      `favicon.ico entry ${i} (${mine.width}×${mine.height}) does not carry ` +
        `the kit's art — re-run \`node scripts/sync-brand-assets.mjs\`, ` +
        `\`make brand-sync\` (the RGBA re-encode lives in the sync)`,
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
