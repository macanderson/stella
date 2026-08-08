import fs from "node:fs";
import path from "node:path";
import { recolorPngFile } from "../lib/png.mjs";
import { buildFaviconIco } from "../lib/ico.mjs";
import { originPaletteFor } from "../lib/origin-palette.mjs";
import { paletteFor } from "../lib/recolor.mjs";

// Every PWA/app icon is composited on the dark ground — there is no
// light-mode favicon. `favicon-16/32/48.png` are straight-alpha (the mark
// alone, no bg fill: browser chrome shows through); the rest are opaque
// squares, per docs/brand/README.md's pwa/ listing.
const PNG_NAMES = [
  "favicon-16.png",
  "favicon-32.png",
  "favicon-48.png",
  "apple-touch-icon.png",
  "icon-192.png",
  "icon-512.png",
  "icon-maskable-192.png",
  "icon-maskable-512.png",
];

/**
 * Recolor docs/brand/pwa/*.png in place and rebuild favicon.ico from the
 * freshly recolored favicon-48.png. safari-pinned-tab.svg is a Safari mask
 * icon and must stay solid #000000 (docs/brand/README's pwa/ note) — this
 * family only asserts that invariant, it never rewrites the file.
 */
export async function generatePwa({ pwaDir, tokens, write }) {
  const oldPalette = originPaletteFor("dark");
  const newPalette = paletteFor(tokens, "dark");
  const outputs = new Map();

  for (const name of PNG_NAMES) {
    const srcPath = path.join(pwaDir, name);
    outputs.set(name, await recolorPngFile(srcPath, oldPalette, newPalette));
  }

  const icoBytes = buildFaviconIco(outputs.get("favicon-48.png"), { width: 48, height: 48 });
  outputs.set("favicon.ico", icoBytes);

  const maskSvgPath = path.join(pwaDir, "safari-pinned-tab.svg");
  const maskSvg = fs.readFileSync(maskSvgPath, "utf8");
  if (!maskSvg.includes("#000000")) {
    throw new Error(
      `safari-pinned-tab.svg must stay solid #000000 (Safari mask-icon contract) — found unexpected content in ${maskSvgPath}`,
    );
  }

  if (write) {
    for (const [name, bytes] of outputs) {
      fs.writeFileSync(path.join(pwaDir, name), bytes);
    }
  }

  return outputs;
}
