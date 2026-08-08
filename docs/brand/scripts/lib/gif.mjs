import fs from "node:fs";
import { GifReader, GifWriter } from "omggif";
import { makeRecolorSession, recolorRGBAWithSession, palettesEqual } from "./recolor.mjs";

/**
 * Recolor every frame of an animated GIF, preserving frame count, size,
 * per-frame delay/disposal, and — because the recolor engine only ever
 * changes fill color, never position or alpha — every pixel of geometry and
 * timing exactly as authored. Returns the re-encoded GIF bytes.
 *
 * When the palettes are identical this returns the source bytes untouched:
 * re-encoding always rebuilds a fresh per-frame color table (see below),
 * which has no reason to match the original GIF's table byte for byte even
 * when every decoded pixel is unchanged (see recolor.mjs's module doc).
 */
export function recolorGifFile(srcPath, oldPalette, newPalette) {
  const original = fs.readFileSync(srcPath);
  if (palettesEqual(oldPalette, newPalette)) return original;

  const reader = new GifReader(original);
  const { width, height } = reader;
  const numFrames = reader.numFrames();
  const session = makeRecolorSession(oldPalette, newPalette);

  const frames = [];
  for (let f = 0; f < numFrames; f++) {
    const info = reader.frameInfo(f);
    const rgba = new Uint8Array(width * height * 4);
    reader.decodeAndBlitFrameRGBA(f, rgba);
    recolorRGBAWithSession(rgba, session);
    frames.push({ info, rgba });
  }

  // GIF is palette-indexed: build a local color table per frame from the
  // colors actually present after recoloring (never more than 256 per
  // frame in this kit's art, which is flat-filled + anti-aliased, not
  // photographic).
  const outBuf = Buffer.alloc(width * height * numFrames * 2 + 4096 * numFrames + 4096);
  const writer = new GifWriter(outBuf, width, height, { loop: 0 });
  for (const { info, rgba } of frames) {
    const paletteIndex = new Map();
    const palette = [];
    const indices = new Uint8Array(width * height);
    for (let p = 0, i = 0; p < width * height; p++, i += 4) {
      const key = (rgba[i] << 16) | (rgba[i + 1] << 8) | rgba[i + 2];
      let idx = paletteIndex.get(key);
      if (idx === undefined) {
        idx = palette.length;
        if (idx > 255) {
          throw new Error(
            `recolorGifFile: frame exceeded 256 colors after recoloring (${srcPath})`,
          );
        }
        palette.push(key);
        paletteIndex.set(key, idx);
      }
      indices[p] = idx;
    }
    let paletteLength = 2;
    while (paletteLength < palette.length) paletteLength *= 2;
    const fullPalette = palette.concat(Array(paletteLength - palette.length).fill(0));
    writer.addFrame(0, 0, width, height, indices, {
      palette: fullPalette,
      delay: info.delay,
      disposal: info.disposal,
    });
  }
  const finalLength = writer.end();
  return outBuf.subarray(0, finalLength);
}
