import fs from "node:fs";
import { spawnSync } from "node:child_process";
import { PNG } from "pngjs";
import { recolorRGBA, palettesEqual } from "./recolor.mjs";

/**
 * Recolor a PNG file on disk and return the re-encoded bytes (does not
 * write). pngjs decodes to raw RGBA — it has no palette/indexed-color
 * encoder, and its own writer's default filter+deflate strategy runs ~3x
 * larger than a properly optimized PNG on this kit's art (verified against
 * the committed files). `oxipng` is what actually re-encodes the committed
 * bytes: it's a lossless PNG optimizer, MIT-licensed and installed via
 * `brew install oxipng` — the repo already leans on an external brew tool
 * this way for `social/build_social.py`'s `rsvg-convert`. `sharp` (its usual
 * npm alternative) was tried first and reverted: it bundles libvips built
 * with `LGPL-3.0-or-later`, which fails this AGPL/commercial dual-licensed
 * repo's dependency-review gate — see `deny.toml`'s license allow-list and
 * AGENTS.md's "If cargo deny rejects a new dependency, drop the dependency."
 * npm has no equivalent guard, so the GitHub Action caught it instead.
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
  const raw = PNG.sync.write(png);

  const result = spawnSync("oxipng", ["-o", "2", "--stdout", "-"], { input: raw, maxBuffer: 1 << 30 });
  if (result.error?.code === "ENOENT") {
    throw new Error(
      "recolorPngFile: `oxipng` was not found on PATH. Install it with `brew install oxipng` (MIT-licensed PNG optimizer — see this file's module doc for why it's used instead of an npm encoder).",
    );
  }
  if (result.status !== 0) {
    throw new Error(`recolorPngFile: oxipng exited ${result.status} on ${srcPath}: ${result.stderr}`);
  }
  return result.stdout;
}
