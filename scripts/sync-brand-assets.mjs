#!/usr/bin/env node
/**
 * Copy the Oxagen house kit (oxageninc/brand) into this repository.
 *
 *   node scripts/sync-brand-assets.mjs [--brand <dir>] [--check]
 *
 * --brand   the kit checkout. Without it the script reads $OXAGEN_BRAND_KIT,
 *           then takes the first checkout it finds at ../oxagen-brand beside
 *           this repository or at ~/Projects/oxagen-brand. The second covers a
 *           worktree under ~/Projects/.worktrees/.
 * --check   write nothing. Exit 1 and list every file that differs from the
 *           kit or is missing. Exit 0 when the repository matches the kit.
 *
 * The kit generates every mark, icon, social card, spinner, font file, and
 * colour value for both brands. This script is the one path from the kit into
 * Stella. Each file it writes is a byte copy of a file the kit commits, or text
 * derived from one the same way on every run. It renders nothing and needs
 * Node's standard library and nothing else, so the kit's fan-out workflow can
 * run it on a bare runner with no network.
 *
 * `.github/workflows/brand-drift.yml` runs the check against the kit's `main`
 * on every pull request, on every push to `main`, and once a day.
 *
 * What it writes:
 *
 *  - the Stella marks (`logo/svg/`) under `docs/brand/logo/svg/`,
 *    `website/public/brand/`, and `website/src/app/icon.svg`;
 *  - the favicons and app icons (`icons/`) under `docs/brand/pwa/`,
 *    `website/public/icons/`, and the `website/src/app/` file conventions;
 *  - the spinners, the social art, and the three house faces with their
 *    licences;
 *  - the kit's token files under `docs/brand/css/`, and its font loader and
 *    token sheet under `website/src/brand/`;
 *  - the Observatory's favicon and wordmark cuts, which the binary embeds;
 *  - the branding skill stub, removing any other file under its folder;
 *  - `website/src/components/brand-marks.generated.ts`, the mark geometry and
 *    the house colours as data;
 *  - the hex value of every Stella token the house palette owns (`PALETTE`)
 *    in `design/tokens/stella-tokens.json` and its two hand-kept mirrors.
 *
 * What `--check` also reads: the site's own stylesheets (`GUARDED_CSS`). A
 * corner, a shadow, a type size, or a page wrap written there as a number
 * stays put when the kit's theme changes, so the check names each one with
 * the house token to use. `LITERALS` lists the values the house rules keep.
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
const CHECK = argv.includes("--check");
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

/**
 * The site's stylesheets, held to the house tokens.
 *
 * The kit's theme editor sets the corners, the shadows, the type scale, and
 * the page wrap in `house-tokens.css`, and this script copies that sheet into
 * the site. A rule that writes one of those values as a number does not move
 * when the theme does. So `--check` reads each file here and reports a
 * `border-radius`, `box-shadow`, or `font-size` that sets a length by hand,
 * and a `max-width` as wide as the house wrap or wider. A narrower
 * `max-width` is a reading measure and passes.
 *
 * It also reports a `--stella-*`, `--st-*`, or `--ox-*` reference that no
 * sheet declares. A missing token voids the whole declaration, so a border
 * written with one draws no border at all.
 *
 * It is one regex pass over these files. It needs no build and no kit.
 */
const GUARDED_CSS = [
  `${WEB}/src/app/global.css`,
  `${WEB}/src/app/tokens.css`,
  `${WEB}/src/app/engine/engine.css`,
  `${WEB}/src/app/engine/engine-stations.css`,
  `${WEB}/src/app/releases/releases.css`,
];
/** The kit's token sheet as the site loads it. */
const HOUSE_CSS = `${WEB}/src/brand/house-tokens.css`;

/**
 * The literal values the house rules keep, each with its reason.
 *
 * A value matches after its whitespace is collapsed. An entry with a
 * `selector` matches only in the rule with exactly that selector. A
 * percentage never needs an entry: `50%` is a circle and passes.
 */
const LITERALS = [
  { prop: "border-radius", value: "999px", why: "a pill" },
  {
    prop: "border-radius",
    value: "0.125rem",
    selector: ".eng-tour :focus-visible",
    why: "a focus ring, which keeps its own shape",
  },
  {
    prop: "border-radius",
    value: "0.25rem",
    selector: ".rl-disclosure:focus-visible",
    why: "a focus ring, which keeps its own shape",
  },
  {
    prop: "border-radius",
    value: "0.5rem",
    selector: ".deck-shot-img",
    why: "the clip around a deck SVG, which draws its own corner (rx 6 in a 680-wide frame)",
  },
  {
    prop: "box-shadow",
    value: "0 0 0 4px var(--color-fd-background)",
    selector: ".rl-marker",
    why: "a ring in the page ground, so the timeline rail stops short of the dot",
  },
  {
    prop: "box-shadow",
    value: "0 0 0 3px color-mix(in srgb, var(--stella-signal) 22%, transparent)",
    selector: ".rl-latest-dot",
    why: "a ring around the dot that marks the latest release",
  },
  {
    prop: "box-shadow",
    value: "0 0 0 0.35rem color-mix(in srgb, var(--stella-signal) 0%, transparent)",
    why: "the pulse ring on the intake's live dot, at its widest",
  },
  {
    prop: "font-size",
    value: "12px",
    selector: ".sdg .sdg-label",
    why: "SVG text in viewBox units, which scale with the drawing",
  },
  {
    prop: "font-size",
    value: "10px",
    selector: ".sdg .sdg-sub",
    why: "SVG text in viewBox units, which scale with the drawing",
  },
  { prop: "font-size", value: "0.9em", why: "inline code, sized to the line it sits in" },
];

/** Paths a check found different from the kit, each with an optional note. */
const drifted = [];
/** Paths a sync wrote or removed. */
const written = [];

/** Write `data` to `relPath`, or record a difference under `--check`. */
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
  if (CHECK) {
    drifted.push(current ? relPath : `${relPath} (missing)`);
    return;
  }
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

// The kit is a separate repository. Without it there is nothing to copy and
// nothing to compare, so both modes stop here.
if (!isKit(BRAND)) {
  console.error(`brand: no kit at ${BRAND}, so nothing was ${CHECK ? "checked" : "synced"}.`);
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

/**
 * The three house faces with their licences, the kit's token files, and the
 * kit's `next/font` loader.
 *
 * `next-fonts.ts` loads its faces from `../fonts/`, so it sits in
 * `website/src/brand/`, beside `website/src/fonts/`.
 */
function typeAndTokens() {
  for (const f of readdirSync(join(BRAND, "fonts")).sort()) {
    if (f.endsWith(".woff2") || f.startsWith("LICENSE")) {
      copy(`fonts/${f}`, `${WEB}/src/fonts/${f}`, `${KIT}/fonts/${f}`);
    }
  }
  copy("tokens/house-tokens.css", `${KIT}/css/house-tokens.css`, `${WEB}/src/brand/house-tokens.css`);
  copy("tokens/house-tokens.json", `${KIT}/css/house-tokens.json`);
  copy("tokens/next-fonts.ts", `${WEB}/src/brand/next-fonts.ts`);
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
    // No folder yet: a sync creates it with the stub, and a check has already
    // reported the stub missing.
  }
  for (const abs of extra.sort().reverse()) {
    const rel = relative(REPO, abs);
    if (statSync(abs).isDirectory()) {
      if (!CHECK) rmSync(abs, { recursive: true, force: true });
      continue;
    }
    if (CHECK) {
      drifted.push(`${rel} (not in the kit's stub)`);
    } else {
      rmSync(abs, { force: true });
      written.push(`${rel} (removed)`);
    }
  }
}

/** `css` with each comment blanked out, keeping every newline. */
function uncomment(css) {
  return css.replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, " "));
}

/** A length in px, from `12px`, `0.75rem`, or `0.75em` (taken at 16px). */
function toPx(num, unit) {
  return unit === "px" ? num : num * 16;
}

/** Every px, rem, em, or pt length in `value` outside a `var()`, in px. */
function lengths(value) {
  const bare = value.replace(/var\(\s*--[\w-]+\s*\)/g, "");
  return [...bare.matchAll(/(?<![\w.#])-?(\d*\.?\d+)(px|rem|em|pt)\b/g)].map((m) =>
    toPx(Math.abs(Number(m[1])), m[2]),
  );
}

/**
 * The house steps a suggestion picks from, read from the sheet the site
 * loads: the radius scale, the two type scales, and the wrap, each in px.
 */
function houseSteps() {
  const css = uncomment(readFileSync(join(REPO, HOUSE_CSS), "utf8"));
  const decl = new Map([...css.matchAll(/(--ox-[\w-]+)\s*:\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]));
  const px = (v) => {
    const m = /^(\d*\.?\d+)(px|rem)$/.exec(v ?? "");
    return m ? toPx(Number(m[1]), m[2]) : null;
  };
  const base = px(decl.get("--ox-radius-base"));
  const radius = [];
  for (const [name, v] of decl) {
    const m = /^calc\(var\(--ox-radius-base\) \* ([\d.]+)\)$/.exec(v);
    if (m && base !== null) radius.push([name, base * Number(m[1])]);
  }
  const scale = (prefix) =>
    [...decl]
      .filter(([name]) => new RegExp(`^--ox-${prefix}-(h[1-4]|body|micro)$`).test(name))
      .map(([name, v]) => [name, px(v)]);
  return { radius, app: scale("a"), marketing: scale("m"), wrap: px(decl.get("--ox-wrap")) };
}

/** The name in `steps` whose px value is closest to `target`. */
function nearest(steps, target) {
  let best = steps[0];
  for (const s of steps) if (Math.abs(s[1] - target) < Math.abs(best[1] - target)) best = s;
  return best[0];
}

/** What a literal in `prop` should read instead. */
function tokenFor(prop, value, steps) {
  const px = lengths(value)[0] ?? 0;
  if (prop === "border-radius") {
    return `var(--ox-radius) on a card, panel, or input, or var(${nearest(steps.radius, px)}) on a control (the nearest step of the house scale)`;
  }
  if (prop === "box-shadow") {
    return "no shadow on a card or panel at rest, var(--ox-shadow-pop) under a floating surface on ink (--ox-shadow-pop-ink on paper), or var(--ox-shadow-ui) under a control";
  }
  if (prop === "font-size") {
    return `var(${nearest(steps.app, px)}) on a docs or data page, or var(${nearest(steps.marketing, px)}) on a marketing page`;
  }
  return "var(--ox-wrap) for the page wrap; a wider wrap goes in LITERALS with its role";
}

/**
 * Every value in `GUARDED_CSS` that should read a house token, and every
 * token reference no sheet declares, as one line each.
 *
 * Each line starts with the file's path and a space. brand-drift.yml finds
 * the files a pull request changed by that path, so keep it a separate word.
 */
function cssFindings() {
  const steps = houseSteps();
  const sheets = GUARDED_CSS.map((path) => [path, uncomment(readFileSync(join(REPO, path), "utf8"))]);
  const declared = new Set();
  for (const [, css] of [...sheets, [HOUSE_CSS, uncomment(readFileSync(join(REPO, HOUSE_CSS), "utf8"))]]) {
    for (const m of css.matchAll(/(--[\w-]+)\s*:/g)) declared.add(m[1]);
  }
  const squash = (s) => s.replace(/\s+/g, " ").trim();
  const lineOf = (css, at) => css.slice(0, at).split("\n").length;
  // A property, or a custom property that carries the same kind of value:
  // `--stella-shadow-card` and `--stella-type-xs` hide a literal behind a name.
  const decls =
    /(^|[{;\s])(border(?:-(?:top|bottom|start|end)-(?:left|right|start|end))?-radius|box-shadow|font-size|max-width|--[\w-]*(?:radius|shadow|type|wrap)[\w-]*)\s*:\s*([^;{}]+)/g;
  const kindOf = (name) =>
    /radius/.test(name)
      ? "border-radius"
      : /shadow/.test(name)
        ? "box-shadow"
        : /type|font-size/.test(name)
          ? "font-size"
          : "max-width";
  const out = [];
  for (const [path, css] of sheets) {
    for (const m of css.matchAll(decls)) {
      const at = m.index + m[1].length;
      const prop = kindOf(m[2]);
      const value = squash(m[3]);
      const found = lengths(value);
      const literal =
        prop === "max-width"
          ? steps.wrap !== null && found.some((px) => px >= steps.wrap)
          : found.length > 0;
      if (!literal) continue;
      const open = css.lastIndexOf("{", at);
      const from = Math.max(css.lastIndexOf("}", open - 1), css.lastIndexOf("{", open - 1), css.lastIndexOf(";", open - 1));
      const selector = squash(css.slice(from + 1, open));
      const kept = LITERALS.some(
        (l) => l.prop === prop && l.value === value && (l.selector === undefined || l.selector === selector),
      );
      if (kept) continue;
      out.push(`${path} line ${lineOf(css, at)}: ${m[2]}: ${value} in ${selector}. Use ${tokenFor(prop, value, steps)}.`);
    }
    for (const m of css.matchAll(/var\(\s*(--(?:stella|st|ox)-[\w-]+)/g)) {
      if (declared.has(m[1])) continue;
      out.push(`${path} line ${lineOf(css, m.index)}: var(${m[1]}) is declared in no stylesheet the site loads. Use a token that exists.`);
    }
  }
  return out;
}

marks();
assets();
typeAndTokens();
palette();
skill();

const kit = `the house kit ${house.version}`;
if (CHECK) {
  const findings = cssFindings();
  if (drifted.length) {
    console.error(`brand: ${drifted.length} file(s) differ from ${kit} at ${BRAND}:`);
    for (const f of drifted) console.error(`  ${f}`);
  }
  if (findings.length) {
    console.error(
      `brand: ${findings.length} value(s) in the site's stylesheets do not read a house token. ` +
        "Use the token each line names, or add the value to LITERALS in scripts/sync-brand-assets.mjs with its reason:",
    );
    for (const f of findings) console.error(`  ${f}`);
  }
  if (drifted.length || findings.length) process.exit(1);
  console.log(`brand: every synced file matches ${kit}, and the site's stylesheets read the house tokens.`);
} else {
  console.log(
    written.length
      ? `brand: synced ${written.length} file(s) from ${kit}.`
      : `brand: already current with ${kit}.`,
  );
  for (const f of written) console.log(`  ${f}`);
}
