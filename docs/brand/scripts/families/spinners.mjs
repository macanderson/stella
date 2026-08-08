import fs from "node:fs";
import path from "node:path";
import { recolorGifFile } from "../lib/gif.mjs";
import { originPaletteFor, ORIGIN_HEX } from "../lib/origin-palette.mjs";
import { paletteFor } from "../lib/recolor.mjs";

const GIFS = [
  { name: "spinner-lockup-dark.gif", theme: "dark" },
  { name: "spinner-lockup-light.gif", theme: "light" },
  { name: "spinner-logomark-dark.gif", theme: "dark" },
  { name: "spinner-logomark-light.gif", theme: "light" },
];

// The animated SVGs are a real, committed source (unlike the GIFs baked
// from them), so their colors are recolored by exact hex substitution
// rather than pixel decomposition. Each entry only lists the hexes that
// SVG actually contains — spinner-logomark.svg has no letters, so it never
// touches dark/light fg at all.
const SVGS = [
  { name: "spinner-lockup-dark.svg", themes: ["dark"] },
  { name: "spinner-lockup-light.svg", themes: ["light"] },
  { name: "spinner-logomark.svg", themes: [] },
  { name: "spinner-lockup-adaptive.svg", themes: ["dark", "light"] },
];

function hexSubstitutions(tokens, themes) {
  const subs = new Map([
    [ORIGIN_HEX.nebula.violet.toUpperCase(), tokens.hex.nebula.violet.toUpperCase()],
    [ORIGIN_HEX.nebula.cyan.toUpperCase(), tokens.hex.nebula.cyan.toUpperCase()],
  ]);
  for (const theme of themes) {
    subs.set(ORIGIN_HEX[theme].fg.toUpperCase(), tokens.hex[theme].fg.toUpperCase());
  }
  return subs;
}

function recolorSvgText(text, subs) {
  let out = text;
  for (const [from, to] of subs) {
    out = out.split(from).join(to);
  }
  return out;
}

/**
 * Recolor docs/brand/spinners/*.gif (per-frame pixel recolor, animation
 * timing untouched) and docs/brand/spinners/*.svg (exact hex substitution —
 * these are real committed sources, so no decomposition is needed).
 */
export function generateSpinners({ spinnersDir, tokens, write }) {
  const outputs = new Map();

  for (const { name, theme } of GIFS) {
    const srcPath = path.join(spinnersDir, name);
    const oldPalette = originPaletteFor(theme);
    const newPalette = paletteFor(tokens, theme);
    const bytes = recolorGifFile(srcPath, oldPalette, newPalette);
    outputs.set(name, bytes);
    if (write) fs.writeFileSync(srcPath, bytes);
  }

  for (const { name, themes } of SVGS) {
    const srcPath = path.join(spinnersDir, name);
    const subs = hexSubstitutions(tokens, themes);
    const text = recolorSvgText(fs.readFileSync(srcPath, "utf8"), subs);
    const bytes = Buffer.from(text, "utf8");
    outputs.set(name, bytes);
    if (write) fs.writeFileSync(srcPath, bytes);
  }

  return outputs;
}
