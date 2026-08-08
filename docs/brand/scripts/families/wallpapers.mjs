import fs from "node:fs";
import path from "node:path";
import { recolorPngFile } from "../lib/png.mjs";
import { originPaletteFor } from "../lib/origin-palette.mjs";
import { paletteFor } from "../lib/recolor.mjs";

const SURFACES = ["desktop", "phone"];
const SIZES = ["4k", "5k", "6k"];
const THEMES = ["dark", "light"];

/**
 * Recolor every desktop/phone wallpaper (4k/5k/6k, dark/light) in place.
 * Each file is a single PNG with no separate SVG source — the composition
 * (glow, grid, starfield, grain) is preserved exactly because the recolor
 * engine only ever rewrites fill color, never geometry.
 */
export async function generateWallpapers({ wallpapersDir, tokens, write }) {
  const outputs = new Map();

  for (const surface of SURFACES) {
    const dir = path.join(wallpapersDir, surface);
    for (const size of SIZES) {
      for (const theme of THEMES) {
        const name = `stella-wallpaper-${surface}-${size}-${theme}.png`;
        const srcPath = path.join(dir, name);
        const oldPalette = originPaletteFor(theme);
        const newPalette = paletteFor(tokens, theme);
        const bytes = await recolorPngFile(srcPath, oldPalette, newPalette);
        outputs.set(path.join(surface, name), bytes);
        if (write) fs.writeFileSync(srcPath, bytes);
      }
    }
  }

  return outputs;
}
