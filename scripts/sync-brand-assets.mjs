#!/usr/bin/env node
/**
 * Copy the Oxagen house kit (oxageninc/brand) into this repository.
 *
 *   node scripts/sync-brand-assets.mjs [--brand <dir>]
 *
 * --brand   the kit checkout. Without it the script reads $OXAGEN_BRAND_KIT,
 *           then takes the first checkout it finds at ../oxagen-brand beside
 *           this repository or at ~/Projects/oxagen-brand. The second covers a
 *           worktree under ~/Projects/.worktrees/.
 *
 * The kit generates every mark, icon, social card, spinner, font file, and
 * colour value for both brands. This script is the one path from the kit into
 * Stella. Each file it writes is a byte copy of a file the kit commits, or text
 * derived from one the same way on every run. It renders nothing and needs
 * Node's standard library and nothing else, so the kit's fan-out workflow can
 * run it on a bare runner with no network.
 *
 * What it writes:
 *
 *  - the Stella marks (`logo/svg/`) under `docs/brand/logo/svg/`,
 *    `website/public/brand/`, and `website/src/app/icon.svg`;
 *  - the favicons and app icons (`icons/`) under `docs/brand/pwa/`,
 *    `website/public/icons/`, and the `website/src/app/` file conventions;
 *  - the spinners, the social art, and the three house faces with their
 *    licences;
 *  - the kit's token files and its Tailwind sheet under `docs/brand/css/`,
 *    and its font loader and token sheet under `website/src/brand/`;
 *  - `website/src/brand/house-type.css`, the kit's type classes taken out of
 *    its Tailwind sheet;
 *  - the Observatory's favicon and wordmark cuts, which the binary embeds;
 *  - the branding skill stub, removing any other file under its folder;
 *  - `website/src/components/brand-marks.generated.ts`, the mark geometry and
 *    the house colours as data;
 *  - the hex value of every Stella token the house palette owns (`PALETTE`)
 *    in `design/tokens/stella-tokens.json` and its two hand-kept mirrors.
 *
 */

import {
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const argv = process.argv.slice(2);
const brandArg = argv.indexOf("--brand");
/** Where a kit checkout sits when neither --brand nor the variable names one. */
const DEFAULT_KITS = [join(REPO, "../oxagen-brand"), join(homedir(), "Projects/oxagen-brand")];
const isKit = (dir) => {
  try {
    return statSync(join(dir, "tokens/house-tokens.json")).isFile();
  } catch {
    return false;
  }
};
const BRAND = resolve(
  brandArg >= 0 && argv[brandArg + 1]
    ? argv[brandArg + 1]
    : process.env.OXAGEN_BRAND_KIT || DEFAULT_KITS.find(isKit) || DEFAULT_KITS[0],
);

const WEB = "website";
/**
 * The in-repo mirror of what the kit emits for Stella.
 *
 * `website/src/lib/brand-parity.test.ts` holds the site to this copy, and CI
 * runs that test with no kit checked out. The kit lands here first, and the
 * site is held to it offline.
 */
const KIT = "docs/brand";
/** The Observatory embeds these with `include_str!` in its `lib.rs`. */
const OBSERVATORY = "crates/stella-observatory/src/assets";
const SKILL_DIR = ".claude/skills/oxagen-branding";

/**
 * The Stella tokens whose value the house palette owns, each with the kit
 * token it takes (`tokens` in the kit's `tokens/house-tokens.json`).
 *
 * The sync writes these values into the token JSON and into the two
 * stylesheets that mirror it, so a kit colour change reaches the website on
 * the next sync. Every other Stella token (the status hues, the diff grounds,
 * `--st-hl`, and `--st-muted`) has no house value and stays Stella's own.
 *
 * After a sync changes a value here, `make tokens-update` rewrites the files
 * generated from the JSON, the terminal theme among them. `make tokens` fails
 * until it runs.
 */
const PALETTE = [
  ["--st-bg", "ink"],
  ["--st-panel", "panel"],
  ["--st-border", "border"],
  ["--st-rule", "rule"],
  ["--st-gold", "gold"],
  ["--st-gold-bright", "gold-bright"],
  ["--st-silver", "muted"],
  ["--st-silver-type", "text-body"],
  ["--st-text", "text"],
  ["--st-dim", "dim"],
  ["--st-ink", "text-ink"],
  ["--st-paper", "paper"],
  ["--st-paper-panel", "paper-panel"],
  ["--st-paper-border", "paper-border"],
  ["--st-void", "void"],
  ["--st-gold-ink", "gold-deep"],
  ["--st-paper-ground", "paper"],
  ["--st-paper-raised", "paper-panel"],
  ["--st-paper-row", "paper-hl"],
  ["--st-paper-seam", "paper-border"],
  ["--st-ink-muted", "muted-ink"],
];
const TOKENS_JSON = "design/tokens/stella-tokens.json";
const TOKEN_SHEETS = [`${WEB}/src/app/tokens.css`, `${KIT}/css/tokens.css`];

/** Paths a sync wrote or removed. */
const written = [];

/** Write `data` to `relPath` when it differs from what is there. */
function emit(relPath, data) {
  const abs = join(REPO, relPath);
  const buf = Buffer.isBuffer(data) ? data : Buffer.from(data, "utf8");
  let current = null;
  try {
    current = readFileSync(abs);
  } catch {
    // The file does not exist yet.
  }
  if (current && current.equals(buf)) return;
  mkdirSync(dirname(abs), { recursive: true });
  writeFileSync(abs, buf);
  written.push(relPath);
}

const kitFile = (path) => readFileSync(join(BRAND, path));
const svg = (name) => join(BRAND, "logo/svg", name);
/** Copy one kit file, byte for byte, to each of `targets`. */
function copy(from, ...targets) {
  const data = kitFile(from);
  for (const to of targets) emit(to, data);
}

// The kit is a separate repository. Without it there is nothing to copy, so
// the script stops here.
if (!isKit(BRAND)) {
  console.error(`brand: no kit at ${BRAND}, so nothing was synced.`);
  console.error(
    "Clone oxageninc/brand to ~/Projects/oxagen-brand, set OXAGEN_BRAND_KIT, or pass --brand <dir>.",
  );
  process.exit(2);
}

const house = JSON.parse(kitFile("tokens/house-tokens.json").toString("utf8"));

/**
 * The marks and the house colours, as data for the React components and the
 * OG card.
 *
 * An `<img>` cannot follow the site theme, and Satori, which draws the OG
 * card, reads no stylesheet. So the geometry comes out of the kit's adaptive
 * SVGs, and the colours out of its token file, into a module the site imports.
 */
function marks() {
  const src = readFileSync(svg("stella-wordmark-adaptive.svg"), "utf8");
  const icon = readFileSync(svg("stella-icon-adaptive.svg"), "utf8");
  const viewBox = (s) => s.match(/viewBox="([^"]+)"/)[1];
  const path = (s, cls) => s.match(new RegExp(`<path class="${cls}" d="([^"]+)"`))[1];
  const transform = icon.match(/<g transform="([^"]+)"/)[1];

  /**
   * The asterisk with its placing transform applied to the coordinates.
   *
   * `MARK_PATH` needs a `<g transform>` to land in the kit's 96-unit box, and
   * Satori has no group transform. The path uses only absolute M, L, H, V, and
   * Z, so `translate(tx,ty) scale(k)` is arithmetic on each number. A curve
   * command in the kit's mark throws below.
   */
  function flatten(d) {
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
      throw new Error(`the kit's mark path carries "${cmd}", and flatten() handles only M, L, H, V, and Z`);
    });
  }
  const flat = flatten(path(icon, "mark"));

  /** The tight box around the flattened mark, with no padding. */
  function box(d) {
    const xs = [];
    const ys = [];
    for (const [, cmd, rest] of d.matchAll(/([MLHVZ])([^MLHVZ]*)/gi)) {
      const v = rest.trim() ? rest.trim().split(/[\s,]+/).map(Number) : [];
      if (cmd === "H") xs.push(...v);
      else if (cmd === "V") ys.push(...v);
      else for (let j = 0; j < v.length; j += 2) { xs.push(v[j]); ys.push(v[j + 1]); }
    }
    const r = (n) => Number(n.toFixed(2));
    const x0 = Math.min(...xs);
    const y0 = Math.min(...ys);
    return `${r(x0)} ${r(y0)} ${r(Math.max(...xs) - x0)} ${r(Math.max(...ys) - y0)}`;
  }

  const [, , w, h] = viewBox(src).split(/\s+/).map(Number);

  // The house motion: a skewed band of gold-bright swept across the letters.
  // Every number comes from the kit's own spinner, so the shimmer here and on
  // every other surface the kit renders is one animation.
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

  const colours = Object.entries(house.tokens)
    .map(([name, hex]) => `  ${JSON.stringify(name)}: ${JSON.stringify(hex)},`)
    .join("\n");

  emit(
    `${WEB}/src/components/brand-marks.generated.ts`,
    `/**
 * GENERATED by scripts/sync-brand-assets.mjs from the Oxagen house kit.
 * Do not edit. Run the sync instead.
 *
 * The kit sets the wordmark in Space Grotesk at weight 600, so the mark and
 * this site's headings share their outlines. To change a mark, change the kit.
 *
 * One glyph is gold: the asterisk. \`letters\` renders in currentColor and
 * follows the theme. \`accent\` keeps the gold in both themes, as the kit's own
 * light and dark files do.
 */

/** The kit's gold. It marks identity, never a surface or a state. */
export const BRAND_GOLD = "${house.gold.hex}";

/**
 * The house palette, from \`tokens\` in the kit's \`tokens/house-tokens.json\`.
 * For renderers with no stylesheet, such as the OG card. A page styled with
 * CSS takes these values from \`src/app/tokens.css\` instead.
 */
export const HOUSE_COLORS = {
${colours}
} as const;

/** The kit's own viewBox for the wordmark. Never re-fit it. */
export const WORDMARK_VIEW_BOX = "${viewBox(src)}";
/** Intrinsic size in viewBox units; width follows height when sized. */
export const WORDMARK_WIDTH = ${w};
export const WORDMARK_HEIGHT = ${h};
/** Every glyph but the asterisk. Renders in currentColor. */
export const WORDMARK_LETTERS_PATH =
  "${path(src, "letters")}";
/** The asterisk: the one gold glyph, and Stella's whole mark. */
export const WORDMARK_SPARKLE_PATH =
  "${path(src, "accent")}";

/** The asterisk on its own, in the kit's 96-unit box. */
export const MARK_VIEW_BOX = "${viewBox(icon)}";
/** Places the glyph in that box. */
export const MARK_TRANSFORM = "${transform}";
export const MARK_PATH =
  "${path(icon, "mark")}";

/**
 * The same mark with MARK_TRANSFORM already applied, so it draws with no
 * enclosing group. Satori, which builds the OG card, has no group transform.
 */
export const MARK_PATH_FLAT =
  "${flat}";

/** The tight box around MARK_PATH_FLAT: the mark's own ink, with no padding. */
export const MARK_BOX_FLAT = "${box(flat)}";

/**
 * The house motion: the gold sweeping across the letters.
 *
 * Every number is the kit's, read out of spinners/stella-spinner-wordmark.svg.
 * Do not re-time it. The period is the kit's shimmer rhythm.
 */
export const SWEEP = {
  /** The band, before the skew. */
  x: ${sweep.x},
  y: ${sweep.y},
  width: ${sweep.width},
  height: ${sweep.height},
  /** Degrees. The band is a parallelogram. */
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

/** The marks, icons, spinners, and social art. */
function assets() {
  // The marks. The site and docs/brand/ carry the same set.
  for (const name of [
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
  ]) {
    copy(`logo/svg/${name}`, `${KIT}/logo/svg/${name}`, `${WEB}/public/brand/${name}`);
  }

  // The tab icon. Next takes it from src/app/icon.svg by file convention.
  copy("logo/svg/stella-favicon.svg", `${KIT}/logo/svg/stella-favicon.svg`, `${WEB}/src/app/icon.svg`);

  // The kit's rasters, as it rendered them. Its ICO embeds RGBA PNGs, which
  // is the only encoding Next's ICO decoder accepts.
  copy("icons/stella-favicon.ico", `${KIT}/pwa/favicon.ico`, `${WEB}/src/app/favicon.ico`);
  for (const size of [16, 32, 48]) {
    copy(
      `icons/stella-icon-${size}.png`,
      `${KIT}/pwa/favicon-${size}.png`,
      `${WEB}/public/icons/favicon-${size}.png`,
    );
  }
  copy("icons/stella-icon-180.png", `${KIT}/pwa/apple-touch-icon.png`, `${WEB}/src/app/apple-icon.png`);
  for (const size of [192, 512]) {
    copy(`icons/stella-icon-${size}.png`, `${KIT}/pwa/icon-${size}.png`, `${WEB}/public/icons/icon-${size}.png`);
    copy(
      `icons/stella-icon-maskable-${size}.png`,
      `${KIT}/pwa/icon-maskable-${size}.png`,
      `${WEB}/public/icons/maskable-${size}.png`,
    );
    copy(`icons/stella-icon-maskable-light-${size}.png`, `${WEB}/public/icons/maskable-light-${size}.png`);
  }

  // Safari's pinned tab wants one flat shape on a transparent ground.
  copy(
    "logo/svg/stella-icon-mono-black.svg",
    `${KIT}/pwa/safari-pinned-tab.svg`,
    `${WEB}/public/icons/safari-pinned-tab.svg`,
  );

  copy("spinners/stella-spinner.svg", `${KIT}/spinners/stella-spinner.svg`, `${WEB}/public/brand/stella-spinner.svg`);
  copy("spinners/stella-spinner-wordmark.svg", `${KIT}/spinners/stella-spinner-wordmark.svg`);

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

  // The Observatory's favicon and the two wordmark cuts its header swaps by
  // theme. The binary embeds these files, so a kit change reaches it on the
  // next build.
  copy("logo/svg/stella-favicon.svg", `${OBSERVATORY}/mark.svg`);
  copy("logo/svg/stella-wordmark-dark.svg", `${OBSERVATORY}/wordmark.svg`);
  copy("logo/svg/stella-wordmark-light.svg", `${OBSERVATORY}/wordmark-light.svg`);
}

/** The two folders that hold the kit's faces and nothing else. */
const FONT_DIRS = [`${WEB}/src/fonts`, `${KIT}/fonts`];

/**
 * The font files the kit's pages load: every face its `house-fonts.css` and
 * its `next-fonts.ts` name under `../fonts/`, and every licence in its
 * `fonts/`.
 *
 * No file name is written here. The kit's theme editor writes both files, so
 * a face it adds or drops reaches the site on the next sync with no edit to
 * this script.
 */
function fontFiles() {
  const faces = new Set();
  for (const sheet of ["tokens/house-fonts.css", "tokens/next-fonts.ts"]) {
    const text = kitFile(sheet).toString("utf8");
    const found = [...text.matchAll(/\.\.\/fonts\/([\w.-]+\.woff2)\b/g)].map((m) => m[1]);
    if (!found.length) throw new Error(`the kit's ${sheet} loads no face from ../fonts/, so the sync cannot tell which faces to copy`);
    for (const f of found) faces.add(f);
  }
  const licences = readdirSync(join(BRAND, "fonts")).filter((f) => /^LICENSE.*\.txt$/.test(f));
  return [...faces, ...licences].sort();
}

/**
 * The house faces with their licences, the kit's token files, and the kit's
 * `next/font` loader.
 *
 * `next-fonts.ts` loads its faces from `../fonts/`, so it sits in
 * `website/src/brand/`, beside `website/src/fonts/`. A file in either font
 * folder that the kit no longer loads is removed, so a face the kit drops
 * leaves the site too.
 */
function typeAndTokens() {
  const fonts = fontFiles();
  for (const f of fonts) copy(`fonts/${f}`, ...FONT_DIRS.map((dir) => `${dir}/${f}`));
  for (const dir of FONT_DIRS) {
    let names = [];
    try {
      names = readdirSync(join(REPO, dir));
    } catch {
      // No folder yet: the copies above create it.
    }
    for (const name of names.filter((n) => !n.startsWith(".") && !fonts.includes(n)).sort()) {
      const rel = `${dir}/${name}`;
      rmSync(join(REPO, rel), { recursive: true, force: true });
      written.push(`${rel} (removed)`);
    }
  }
  copy("tokens/house-tokens.css", `${KIT}/css/house-tokens.css`, `${WEB}/src/brand/house-tokens.css`);
  copy("tokens/house-tokens.json", `${KIT}/css/house-tokens.json`);
  copy("tokens/house-tailwind.css", `${KIT}/css/house-tailwind.css`);
  copy("tokens/next-fonts.ts", `${WEB}/src/brand/next-fonts.ts`);
  typeClasses();
}

/** The type classes the site must find in the kit's `house-tailwind.css`. */
const TYPE_CLASSES = ["m", "a"].flatMap((scale) =>
  ["h1", "h2", "h3", "h4", "body", "micro"].map((step) => `text-${scale}-${step}`),
);

/** The font roles the site's stylesheets read, which the kit's theme sets. */
const FONT_ROLES = ["--font-sans", "--font-display", "--font-heading", "--font-mono", "--font-wordmark"];

/**
 * The kit's font roles and type classes, as a sheet the site imports.
 *
 * Both live in the kit's `house-tailwind.css`, and the site cannot import
 * that whole file. Its base layer would restyle the site's headings and focus
 * ring, and its theme block would replace the corner mappings in global.css.
 * So the sync copies two parts and nothing else:
 *
 *  - the `--font-*` lines of its first `@theme` block, which point each role
 *    at the variable the kit's `next-fonts.ts` sets, such as `--font-sans`
 *    at `--font-aeonik`. The two files change together when the kit's theme
 *    editor changes a face, so the site never names a face itself.
 *  - the `@utility` rules for type, `text-m-h1` to `text-m-micro` and
 *    `text-a-h1` to `text-a-micro`. Each sets a size and a line height from
 *    the house tokens, so a theme change reaches every element that uses one.
 *
 * The kit writes each line whole. If a role in `FONT_ROLES` or a class in
 * `TYPE_CLASSES` is missing from that shape, the sync stops rather than write
 * a partial sheet.
 */
function typeClasses() {
  const lines = kitFile("tokens/house-tailwind.css").toString("utf8").split("\n");
  const theme = lines.slice(lines.indexOf("@theme {") + 1);
  const roles = theme
    .slice(0, theme.indexOf("}"))
    .map((line) => line.trim())
    .filter((line) => /^--font-[\w-]+:.*;( \/\*.*\*\/)?$/.test(line));
  const rules = lines.filter((line) => /^@utility text-[am]-[\w-]+ \{.*\}$/.test(line));
  const missing = [
    ...FONT_ROLES.filter((name) => !roles.some((line) => line.startsWith(`${name}:`))),
    ...TYPE_CLASSES.filter((name) => !rules.some((rule) => rule.startsWith(`@utility ${name} {`))),
  ];
  if (!lines.includes("@theme {") || missing.length) {
    throw new Error(
      `the kit's tokens/house-tailwind.css has no one-line rule for ${missing.join(", ") || "@theme {"}, ` +
        "so the sync cannot write website/src/brand/house-type.css",
    );
  }
  const scale = (prefix) => rules.filter((rule) => rule.startsWith(`@utility text-${prefix}-`)).join("\n");
  emit(
    `${WEB}/src/brand/house-type.css`,
    `/*
 * The house font roles and type classes. text-m-* is the marketing scale, for
 * landing pages and posts, and text-a-* is the app scale, for docs and apps.
 *
 * GENERATED by scripts/sync-brand-assets.mjs from the kit's
 * tokens/house-tailwind.css. Do not edit. Run the sync instead.
 *
 * The @theme block points each font role at the variable that src/brand/
 * next-fonts.ts sets on <html>, so a face the kit changes reaches the site
 * with no edit here. --font-heading reads the text face here, and global.css
 * points it at the display face, because stella.oxagen.sh is a customer
 * site. Each class sets a size, a line height, a face, and a
 * weight. The size and line height read the --ox-m-* and --ox-a-* tokens in
 * house-tokens.css, and the face reads a role below. text-m-micro and
 * text-a-micro set the code face.
 */

/* the font roles */
@theme {
${roles.map((line) => `  ${line}`).join("\n")}
}

/* the marketing scale */
${scale("m")}

/* the app scale */
${scale("a")}
`,
  );
}

/**
 * Write the kit's value into every Stella token the house palette owns.
 *
 * Each value is replaced in place, so the rest of each file keeps its bytes.
 * Values compare without regard to case, and a replacement keeps the case the
 * file already writes.
 */
function palette() {
  const want = new Map(
    PALETTE.map(([css, name]) => {
      const hex = house.tokens[name];
      if (!/^#[0-9A-Fa-f]{6}$/.test(hex ?? "")) {
        throw new Error(`the kit's tokens/house-tokens.json has no colour "${name}" (PALETTE maps ${css} to it)`);
      }
      return [css, hex];
    }),
  );
  const same = (a, b) => a.toLowerCase() === b.toLowerCase();
  const cased = (old, hex) => (old === old.toLowerCase() ? hex.toLowerCase() : hex.toUpperCase());

  // The JSON: the "hex" of the object whose "css" names the token. "hex"
  // comes before "css" in each object, with no brace between them.
  let json = readFileSync(join(REPO, TOKENS_JSON), "utf8");
  for (const [css, hex] of want) {
    const at = json.indexOf(`"css": "${css}"`);
    if (at < 0) throw new Error(`${TOKENS_JSON} has no token ${css}`);
    const head = json.slice(0, at);
    const m = [...head.matchAll(/"hex": "(#[0-9A-Fa-f]{6})"/g)].pop();
    if (!m || /[{}]/.test(head.slice(m.index))) {
      throw new Error(`${TOKENS_JSON}: ${css} has no "hex" in its own object`);
    }
    if (same(m[1], hex)) continue;
    const start = m.index + m[0].indexOf("#");
    json = json.slice(0, start) + cased(m[1], hex) + json.slice(start + 7);
  }
  emit(TOKENS_JSON, json);

  // The two stylesheets that mirror the JSON. A name may be declared more
  // than once, and every declaration takes the value.
  for (const sheet of TOKEN_SHEETS) {
    let css = readFileSync(join(REPO, sheet), "utf8");
    for (const [name, hex] of want) {
      let found = false;
      css = css.replace(new RegExp(`(${name}\\s*:\\s*)(#[0-9A-Fa-f]{6})\\b`, "g"), (_, lead, old) => {
        found = true;
        return lead + (same(old, hex) ? old : cased(old, hex));
      });
      if (!found) throw new Error(`${sheet} declares no ${name} with a hex value`);
    }
    emit(sheet, css);
  }
}

/**
 * The branding skill: the kit's stub, and nothing else in its folder.
 *
 * The stub fetches the full skill from the kit's `main` on every run, so a
 * vendored copy beside it would only drift.
 */
function skill() {
  copy("skills/stub/oxagen-branding/SKILL.md", `${SKILL_DIR}/SKILL.md`);
  const root = join(REPO, SKILL_DIR);
  const walk = (dir) =>
    readdirSync(dir).flatMap((name) => {
      const abs = join(dir, name);
      return statSync(abs).isDirectory() ? [abs, ...walk(abs)] : [abs];
    });
  let extra = [];
  try {
    extra = walk(root).filter((abs) => relative(root, abs) !== "SKILL.md");
  } catch {
    // No folder yet: the copy above creates it with the stub.
  }
  for (const abs of extra.sort().reverse()) {
    const rel = relative(REPO, abs);
    if (statSync(abs).isDirectory()) {
      rmSync(abs, { recursive: true, force: true });
      continue;
    }
    rmSync(abs, { force: true });
    written.push(`${rel} (removed)`);
  }
}

marks();
assets();
typeAndTokens();
palette();
skill();

const kit = `the house kit ${house.version}`;
console.log(
  written.length ? `brand: synced ${written.length} file(s) from ${kit}.` : `brand: already current with ${kit}.`,
);
for (const f of written) console.log(`  ${f}`);
