#!/usr/bin/env node
/**
 * Pull the Oxagen house brand system into this repository.
 *
 * The house kit (macanderson/oxagen-house-brand) generates every mark, icon,
 * social card and spinner for BOTH brands from `build/`. Nothing in it is drawn
 * by hand, so nothing here is copied by hand either: this script is the one seam
 * between the kit and the site, and re-running it after a kit rebuild re-flows
 * every asset.
 *
 *   node scripts/sync-brand-assets.mjs [--brand <dir>] [--check]
 *
 * --brand   where the kit is checked out. Defaults to $OXAGEN_HOUSE_BRAND, then
 *           ../oxagen-house-brand beside this repo.
 * --check   verify the vendored files match what the kit would emit, write
 *           nothing, exit non-zero on drift. This is what the gate runs.
 *
 * It replaces `scripts/mirror-brand-icons.py`, which mirrored `docs/brand/` —
 * this repo's own kit, retired with the house system. The failure that script
 * was written against is unchanged and so is the answer to it: a step that
 * exists only as a sentence ("re-copy it when the kit regenerates") is a step
 * that gets skipped, and the way v5.0 shipped the site on v4.0 icons.
 *
 * ## What Stella's marks are now
 *
 * The comet is retired. Stella's mark is the ASTERISK, and it already lives
 * inside the word: `stella*`, set in Space Grotesk at the kit's logo weight with
 * the asterisk in gold. That combined form IS the Stella lockup, and it is the
 * only one there is — nothing is ever placed to the left of the word. The kit
 * emits no separate Stella lockup, which is why this script writes none.
 *
 * The icon is the asterisk alone. Unlike Oxagen's one-colour `Ox` lettermark it
 * ships gold, because a lone asterisk in ink reads as punctuation.
 */

import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const argv = process.argv.slice(2);
const CHECK = argv.includes("--check");
const brandArg = argv.indexOf("--brand");
const BRAND = resolve(
  brandArg >= 0 && argv[brandArg + 1]
    ? argv[brandArg + 1]
    : process.env.OXAGEN_HOUSE_BRAND || join(REPO, "../oxagen-house-brand"),
);

const WEB = "website";
/**
 * The in-repo mirror of the house kit.
 *
 * `docs/brand/` used to be Stella's OWN kit — a generator (`build_marks.py`,
 * `cometkit.py`) that drew the comet and everything around it. The house system
 * retires that, and this directory becomes a vendored copy of what the house kit
 * emits for Stella instead.
 *
 * It is kept rather than deleted for two reasons. It is what a designer is
 * handed, and it is what `website/src/lib/brand-parity.test.ts` compares the
 * site against — a check that has to run in CI, where the house kit is not
 * checked out. So the kit lands here first, offline, and the site is held to
 * this copy.
 */
const KIT = "docs/brand";
const FAVICON_PNG = [16, 32, 48];
const ICO_SIZES = [16, 32, 48];
const APP_ICONS = [192, 512];
const MASKABLE = [192, 512];

/** Rasterise an SVG at an exact square size. rsvg-convert ships with librsvg. */
function raster(svgPath, size) {
  return execFileSync(
    "rsvg-convert",
    ["-w", String(size), "-h", String(size), "-f", "png", svgPath],
    { maxBuffer: 64 * 1024 * 1024 },
  );
}

/**
 * A maskable icon is the tile with its corners let out and the mark pulled into
 * the 80% safe circle Android masks against. 0.72 keeps the asterisk inside
 * that circle with room to spare.
 */
function maskableSvg(tileSvgPath) {
  const src = readFileSync(tileSvgPath, "utf8");
  const inset = (96 * (1 - 0.72)) / 2;
  return src
    .replace(/(<rect width="96" height="96")\s+rx="20"/, "$1")
    .replace(/<g transform=/, `<g transform="translate(${inset} ${inset}) scale(0.72)"><g transform=`)
    .replace(/<\/g><\/svg>$/, "</g></g></svg>");
}

/**
 * An .ico is a 6-byte header, a 16-byte directory entry per image, then the
 * payloads.
 *
 * The payloads are PNG, and they are re-encoded to RGBA rather than passed
 * through. The kit renders its favicons opaque, so its own ICO embeds RGB PNGs
 * (colour type 2); Next's image pipeline decodes this file through the `ico`
 * crate, which accepts only RGBA and fails the production build outright:
 *
 *     Caused by: Format error decoding Ico: The PNG is not in RGBA format!
 *
 * rsvg-convert already emits RGBA, so rasterising here rather than copying the
 * kit's ICO gets that for free — which is the whole reason this script
 * rasterises the favicon sizes instead of copying `icons/stella-icon-16.png`.
 */
function ico(pngs) {
  const head = Buffer.alloc(6);
  head.writeUInt16LE(0, 0);
  head.writeUInt16LE(1, 2);
  head.writeUInt16LE(pngs.length, 4);
  let offset = 6 + 16 * pngs.length;
  const dir = [];
  for (const { size, data } of pngs) {
    const e = Buffer.alloc(16);
    e.writeUInt8(size >= 256 ? 0 : size, 0);
    e.writeUInt8(size >= 256 ? 0 : size, 1);
    e.writeUInt16LE(1, 4);
    e.writeUInt16LE(32, 6);
    e.writeUInt32LE(data.length, 8);
    e.writeUInt32LE(offset, 12);
    offset += data.length;
    dir.push(e);
  }
  return Buffer.concat([head, ...dir, ...pngs.map((p) => p.data)]);
}

const written = [];
const drifted = [];

function emit(relPath, data) {
  const abs = join(REPO, relPath);
  const buf = Buffer.isBuffer(data) ? data : Buffer.from(data, "utf8");
  let current = null;
  try {
    current = readFileSync(abs);
  } catch {
    /* new file */
  }
  const same = current && current.equals(buf);
  if (CHECK) {
    if (!same) drifted.push(relPath);
    return;
  }
  if (same) return;
  mkdirSync(dirname(abs), { recursive: true });
  writeFileSync(abs, buf);
  written.push(relPath);
}

const svg = (name) => join(BRAND, "logo/svg", name);
const copy = (from, to) => emit(to, readFileSync(join(BRAND, from)));

/**
 * The marks, as data, for the React components.
 *
 * `<img>` cannot follow the site theme, and hand-copying path data into a .tsx
 * is exactly the drift this script exists to prevent — so the geometry is
 * extracted from the kit's adaptive SVGs and written to a generated module the
 * components import.
 */
function marks() {
  const src = readFileSync(svg("stella-wordmark-adaptive.svg"), "utf8");
  const icon = readFileSync(svg("stella-icon-adaptive.svg"), "utf8");
  const viewBox = (s) => s.match(/viewBox="([^"]+)"/)[1];
  const path = (s, cls) => s.match(new RegExp(`<path class="${cls}" d="([^"]+)"`))[1];
  const gold = JSON.parse(readFileSync(join(BRAND, "tokens/house-tokens.json"), "utf8")).gold.hex;

  /**
   * The asterisk with its placing transform baked into the coordinates.
   *
   * `MARK_PATH` needs a `<g transform>` around it to land in the kit's 96-unit
   * box, and some renderers have no group transform to give it — Satori, which
   * builds the OG card, is the one this repo actually hits. Rather than teach
   * every such caller to pre-solve the transform by hand (which is how a mark
   * ends up subtly misplaced on one surface), it is solved once, here.
   *
   * The path uses only absolute M/L/H/V/Z, so applying `translate(tx,ty)
   * scale(k)` is arithmetic on the numbers with no curve maths involved. If the
   * kit ever emits a curve command in this mark, the throw below is the handoff.
   */
  function flatten(d, transform) {
    const [, tx, ty, k] = transform
      .match(/translate\(([-\d.]+),([-\d.]+)\) scale\(([\d.]+)\)/)
      .map(Number);
    const round = (n) => Number(n.toFixed(4));
    const X = (n) => round(tx + k * n);
    const Y = (n) => round(ty + k * n);
    return d.replace(/([MLHVZ])([^MLHVZ]*)/gi, (_, cmd, rest) => {
      const nums = rest.trim() ? rest.trim().split(/[\s,]+/).map(Number) : [];
      if (cmd === "Z") return "Z";
      if (cmd === "H") return "H" + nums.map(X).join(" ");
      if (cmd === "V") return "V" + nums.map(Y).join(" ");
      if (cmd === "M" || cmd === "L") {
        const out = [];
        for (let i = 0; i < nums.length; i += 2) out.push(X(nums[i]), Y(nums[i + 1]));
        return cmd + out.join(" ");
      }
      throw new Error(`mark path carries an unsupported command "${cmd}" — flatten() needs curve support`);
    });
  }
  const [, , w, h] = viewBox(src).split(/\s+/).map(Number);

  // The house motion: a skewed band of gold-bright swept across the letters,
  // clipped to them. Pulled out of the kit's own spinner rather than re-timed
  // here, so the shimmer on this site and the shimmer in every other surface
  // the kit renders are one animation.
  const spin = readFileSync(join(BRAND, "spinners/stella-spinner-wordmark.svg"), "utf8");
  const rect = spin.match(/<rect class="sweep-[^"]+"([^>]+)\/>/)[1];
  const attr = (name) => rect.match(new RegExp(`${name}="([^"]+)"`))[1];
  const sweep = {
    x: Number(attr("x")),
    y: Number(attr("y")),
    width: Number(attr("width")),
    height: Number(attr("height")),
    skewDeg: Number(spin.match(/skewX\((-?[\d.]+)\)/)[1]),
    travelPx: Number(spin.match(/translateX\(([\d.]+)px\)/)[1]),
    durationSec: Number(spin.match(/animation:sweep-[^ ]+ ([\d.]+)s/)[1]),
    easing: spin.match(/animation:sweep-[^ ]+ [\d.]+s ([^ ]+) /)[1],
    holdPct: Number(spin.match(/(\d+)%,100%\{transform/)[1]),
    highlight: spin.match(/stop-color="(#[0-9A-Fa-f]{6})" stop-opacity="0.95"/)[1],
  };

  emit(
    `${WEB}/src/components/brand-marks.generated.ts`,
    `/**
 * GENERATED by scripts/sync-brand-assets.mjs from the Oxagen house brand kit.
 * Do not edit — run the sync instead.
 *
 * The kit reproduces the wordmark from Space Grotesk itself (weight 600, one em,
 * HarfBuzz spacing including kerning), so the mark and this site's running text
 * are the same outlines. Editing a path here would break that; changing the mark
 * means changing the kit.
 *
 * ONE GLYPH IS GOLD: the asterisk. \`letters\` renders in currentColor and flips
 * with the theme; \`accent\` keeps the metal in BOTH themes, which is what the
 * kit's own light and dark files do — the "gold becomes its deep shade on paper"
 * rule governs gold WORDS, not the mark.
 */

/** The kit's gold, pinned. Identity only — never a surface, never a state. */
export const BRAND_GOLD = "${gold}";

/** The kit's own viewBox for the wordmark. Never re-fit it. */
export const WORDMARK_VIEW_BOX = "${viewBox(src)}";
/** Intrinsic size in viewBox units; width follows height when sized. */
export const WORDMARK_WIDTH = ${w};
export const WORDMARK_HEIGHT = ${h};
/** Every glyph but the asterisk. Renders in currentColor. */
export const WORDMARK_LETTERS_PATH =
  "${path(src, "letters")}";
/** The asterisk — the one gold glyph, and Stella's whole mark. */
export const WORDMARK_SPARKLE_PATH =
  "${path(src, "accent")}";

/** The asterisk on its own, in the kit's 96-unit box. */
export const MARK_VIEW_BOX = "${viewBox(icon)}";
/** Places the glyph in that box. */
export const MARK_TRANSFORM = "${icon.match(/<g transform="([^"]+)"/)[1]}";
export const MARK_PATH =
  "${path(icon, "mark")}";

/**
 * The same mark with MARK_TRANSFORM already applied, so it draws correctly with
 * no enclosing group. For renderers with no group transform — Satori, which
 * builds the OG card. Identical geometry, solved once instead of per caller.
 */
export const MARK_PATH_FLAT =
  "${flatten(path(icon, "mark"), icon.match(/<g transform="([^"]+)"/)[1])}";

/** The tight box around MARK_PATH_FLAT: the mark's own ink, with no padding. */
export const MARK_BOX_FLAT = "${(() => {
  const flat = flatten(path(icon, "mark"), icon.match(/<g transform="([^"]+)"/)[1]);
  const nums = flat.match(/-?[\d.]+/g).map(Number);
  // M/L pairs, H on x, V on y — walk the commands to keep the axes straight.
  const xs = [], ys = [];
  let i = 0;
  for (const [, cmd, rest] of flat.matchAll(/([MLHVZ])([^MLHVZ]*)/gi)) {
    const v = rest.trim() ? rest.trim().split(/[\s,]+/).map(Number) : [];
    if (cmd === "H") xs.push(...v);
    else if (cmd === "V") ys.push(...v);
    else for (let j = 0; j < v.length; j += 2) { xs.push(v[j]); ys.push(v[j + 1]); }
    i++;
  }
  void nums; void i;
  const r = (n) => Number(n.toFixed(2));
  const x0 = Math.min(...xs), y0 = Math.min(...ys);
  return `${r(x0)} ${r(y0)} ${r(Math.max(...xs) - x0)} ${r(Math.max(...ys) - y0)}`;
})()}";

/**
 * The house motion — the metal sweeping across the letters.
 *
 * Every number is the kit's, read out of spinners/stella-spinner-wordmark.svg,
 * so the shimmer here and the shimmer on every other surface the kit renders
 * are one animation. Do not re-time it: the period is the kit's own shimmer
 * rhythm, and a faster sweep reads as a different brand.
 */
export const SWEEP = {
  /** The band, before the skew. */
  x: ${sweep.x},
  y: ${sweep.y},
  width: ${sweep.width},
  height: ${sweep.height},
  /** Degrees. The band is a parallelogram, not a rectangle. */
  skewDeg: ${sweep.skewDeg},
  /** How far it travels, in viewBox units. */
  travelPx: ${sweep.travelPx},
  /** Seconds for one pass, including the rest at the end. */
  durationSec: ${sweep.durationSec},
  easing: "${sweep.easing}",
  /** Percent of the period at which the travel is finished and it rests. */
  holdPct: ${sweep.holdPct},
  /** The colour the shimmer passes through. */
  highlight: "${sweep.highlight}",
} as const;
`,
  );
}

/** The site's public assets. */
function site() {
  const tileDark = svg("stella-icon-tile-dark.svg");
  const tileLight = svg("stella-icon-tile-light.svg");

  const svgSet = [
    "stella-wordmark-adaptive.svg",
    "stella-wordmark-dark.svg",
    "stella-wordmark-light.svg",
    "stella-wordmark-mono-black.svg",
    "stella-wordmark-mono-white.svg",
    "stella-icon-adaptive.svg",
    "stella-icon-mono-black.svg",
    "stella-icon-mono-white.svg",
    "stella-icon-tile-dark.svg",
    "stella-icon-tile-light.svg",
  ];
  for (const name of svgSet) {
    const body = readFileSync(svg(name));
    emit(`${KIT}/logo/svg/${name}`, body);
    emit(`${WEB}/public/brand/${name}`, body);
  }

  // Favicons. The SVG is adaptive; the rasters come off the dark tile, opaque,
  // so they stay legible whatever colour the tab is painted.
  const favicon = readFileSync(svg("stella-favicon.svg"));
  emit(`${KIT}/logo/svg/stella-favicon.svg`, favicon);
  emit(`${WEB}/src/app/icon.svg`, favicon);

  const icoParts = [];
  for (const size of FAVICON_PNG) {
    const data = raster(tileDark, size);
    emit(`${KIT}/pwa/favicon-${size}.png`, data);
    emit(`${WEB}/public/icons/favicon-${size}.png`, data);
    if (ICO_SIZES.includes(size)) icoParts.push({ size, data });
  }
  const favIco = ico(icoParts);
  emit(`${KIT}/pwa/favicon.ico`, favIco);
  emit(`${WEB}/src/app/favicon.ico`, favIco);

  const apple = raster(tileDark, 180);
  emit(`${KIT}/pwa/apple-touch-icon.png`, apple);
  emit(`${WEB}/src/app/apple-icon.png`, apple);

  for (const size of APP_ICONS) {
    const data = raster(tileDark, size);
    emit(`${KIT}/pwa/icon-${size}.png`, data);
    emit(`${WEB}/public/icons/icon-${size}.png`, data);
  }
  const scratch = mkdtempSync(join(tmpdir(), "stella-brand-"));
  const maskDark = join(scratch, "maskable-dark.svg");
  const maskLight = join(scratch, "maskable-light.svg");
  writeFileSync(maskDark, maskableSvg(tileDark));
  writeFileSync(maskLight, maskableSvg(tileLight));
  for (const size of MASKABLE) {
    const dark = raster(maskDark, size);
    emit(`${KIT}/pwa/icon-maskable-${size}.png`, dark);
    emit(`${WEB}/public/icons/maskable-${size}.png`, dark);
    emit(`${WEB}/public/icons/maskable-light-${size}.png`, raster(maskLight, size));
  }

  // Safari's pinned tab wants one flat path on a transparent ground.
  const pinned = readFileSync(svg("stella-icon-mono-black.svg"));
  emit(`${KIT}/pwa/safari-pinned-tab.svg`, pinned);
  emit(`${WEB}/public/icons/safari-pinned-tab.svg`, pinned);

  const spinner = readFileSync(join(BRAND, "spinners/stella-spinner.svg"));
  emit(`${KIT}/spinners/stella-spinner.svg`, spinner);
  emit(`${WEB}/public/brand/stella-spinner.svg`, spinner);
  copy("spinners/stella-spinner-wordmark.svg", `${KIT}/spinners/stella-spinner-wordmark.svg`);

  // The kit's own social art, so docs/brand/social/ stops carrying the comet.
  for (const scheme of ["dark", "light"]) {
    for (const [from, to] of [
      [`stella-og-1200x630-${scheme}.png`, `stella-og-image-${scheme}.png`],
      [`stella-avatar-${scheme}.png`, `stella-avatar-${scheme}.png`],
      [`stella-x-header-${scheme}.png`, `stella-x-banner-${scheme}.png`],
      [`stella-linkedin-banner-${scheme}.png`, `stella-linkedin-banner-${scheme}.png`],
      [`stella-youtube-banner-${scheme}.png`, `stella-youtube-banner-${scheme}.png`],
    ]) {
      copy(`social/${from}`, `${KIT}/social/${to}`);
    }
  }
}

/** The face. Space Grotesk, the typeface the wordmark is cut from. */
function fonts() {
  for (const f of readdirSync(join(BRAND, "fonts"))) {
    if (f.endsWith(".woff2") && f.startsWith("space-grotesk")) {
      copy(`fonts/${f}`, `${WEB}/src/fonts/${f}`);
      copy(`fonts/${f}`, `${KIT}/fonts/${f}`);
    }
  }
  copy("fonts/LICENSE-OFL.txt", `${WEB}/src/fonts/LICENSE-OFL.txt`);
}

try {
  readFileSync(join(BRAND, "tokens/house-tokens.json"));
} catch {
  console.error(
    `brand kit not found at ${BRAND}\n` +
      `clone macanderson/oxagen-house-brand beside this repo, or set OXAGEN_HOUSE_BRAND.`,
  );
  process.exit(2);
}

marks();
site();
fonts();

const version = JSON.parse(readFileSync(join(BRAND, "tokens/house-tokens.json"), "utf8")).version;

if (CHECK) {
  if (drifted.length) {
    console.error(`brand assets are stale against house kit ${version}:`);
    for (const f of drifted) console.error(`  ${f}`);
    console.error(`\nrun: node scripts/sync-brand-assets.mjs`);
    process.exit(1);
  }
  console.log(`brand: every vendored asset matches house kit ${version}`);
} else {
  console.log(
    written.length
      ? `brand: synced ${written.length} file(s) from house kit ${version}`
      : `brand: already current with house kit ${version}`,
  );
}
