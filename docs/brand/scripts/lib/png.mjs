import fs from "node:fs";
import { PNG } from "pngjs";
import sharp from "sharp";
import { recolorRGBA, palettesEqual } from "./recolor.mjs";

/**
 * Recolor a PNG file on disk and return the re-encoded bytes (does not
 * write). pngjs decodes to raw RGBA — it has no palette/indexed-color
 * encoder and its own writer's default filter+deflate strategy runs ~3x
 * larger than a properly optimized PNG on this kit's art (verified against
 * the committed files). sharp's encoder (libvips, effort 10) is what
 * actually gets committed.
 *
 * When the palettes are identical this returns the source bytes untouched
 * rather than decoding/re-encoding — see recolor.mjs's module doc for why
 * that matters for `--check`.
 */
export async function recolorPngFile(srcPath, oldPalette, newPalette) {
  const original = fs.readFileSync(srcPath);
  if (palettesEqual(oldPalette, newPalette)) return original;

  const png = PNG.sync.read(original);
  recolorRGBA(png.data, oldPalette, newPalette);
  return sharp(png.data, { raw: { width: png.width, height: png.height, channels: 4 } })
    .png({ compressionLevel: 9, effort: 10 })
    .toBuffer();
}
