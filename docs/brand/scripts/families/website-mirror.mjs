import fs from "node:fs";
import path from "node:path";

// docs/brand/pwa/<key> -> website/<relative path>. Every one of these pairs
// is byte-identical today (verified against the committed repo before this
// pipeline existed) — this table is what keeps them that way after a recolor.
const MIRRORS = [
  ["favicon.ico", ["src", "app", "favicon.ico"]],
  ["apple-touch-icon.png", ["src", "app", "apple-icon.png"]],
  ["favicon-16.png", ["public", "icons", "favicon-16.png"]],
  ["favicon-32.png", ["public", "icons", "favicon-32.png"]],
  ["favicon-48.png", ["public", "icons", "favicon-48.png"]],
  ["icon-192.png", ["public", "icons", "icon-192.png"]],
  ["icon-512.png", ["public", "icons", "icon-512.png"]],
  ["icon-maskable-192.png", ["public", "icons", "maskable-192.png"]],
  ["icon-maskable-512.png", ["public", "icons", "maskable-512.png"]],
];

/**
 * Mirror the freshly generated docs/brand/pwa outputs to their website/
 * copies. `pwaOutputs` is the Map returned by families/pwa.generatePwa.
 */
export function mirrorToWebsite({ websiteDir, pwaOutputs, write }) {
  const outputs = new Map();
  for (const [pwaName, relParts] of MIRRORS) {
    const bytes = pwaOutputs.get(pwaName);
    if (!bytes) {
      throw new Error(`mirrorToWebsite: no generated output for docs/brand/pwa/${pwaName}`);
    }
    const destPath = path.join(websiteDir, ...relParts);
    outputs.set(destPath, bytes);
    if (write) fs.writeFileSync(destPath, bytes);
  }
  return outputs;
}
