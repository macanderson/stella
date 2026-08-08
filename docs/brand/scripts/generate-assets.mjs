#!/usr/bin/env node
// Regenerate every color-derived binary in the stella brand kit — see
// docs/brand/README.md's "generated assets" section for the workflow this
// implements, and lib/recolor.mjs's module doc for how it works.
//
//   pnpm generate-assets           # recolor pwa/, wallpapers/, spinners/*.gif
//                                   # + their website/ mirrors, in place
//   pnpm generate-assets --check   # regenerate in memory, diff against what's
//                                   # committed, exit 1 on any mismatch
import path from "node:path";
import { fileURLToPath } from "node:url";
import { loadTokens, defaultTokensPath } from "./lib/tokens.mjs";
import { generatePwa } from "./families/pwa.mjs";
import { generateWallpapers } from "./families/wallpapers.mjs";
import { generateSpinners } from "./families/spinners.mjs";
import { mirrorToWebsite } from "./families/website-mirror.mjs";
import { readFileSync, existsSync } from "node:fs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const brandDir = path.resolve(__dirname, "..");
const repoRoot = path.resolve(brandDir, "../..");
const websiteDir = path.join(repoRoot, "website");

const check = process.argv.includes("--check");
const tokens = loadTokens(defaultTokensPath(__dirname));

function diffAgainstDisk(outputs, label) {
  const mismatches = [];
  for (const [destPath, bytes] of outputs) {
    if (!existsSync(destPath)) {
      mismatches.push(`${destPath} (missing)`);
      continue;
    }
    const onDisk = readFileSync(destPath);
    if (!onDisk.equals(bytes)) {
      mismatches.push(`${destPath} (${onDisk.length} bytes on disk vs ${bytes.length} generated)`);
    }
  }
  if (mismatches.length > 0) {
    console.error(`✗ ${label}: ${mismatches.length} file(s) out of date`);
    for (const m of mismatches) console.error(`    ${m}`);
  } else {
    console.log(`✓ ${label}: ${outputs.size} file(s) up to date`);
  }
  return mismatches.length;
}

function report(outputs, label) {
  console.log(`✓ ${label}: wrote ${outputs.size} file(s)`);
}

const pwaOutputs = await generatePwa({
  pwaDir: path.join(brandDir, "pwa"),
  tokens,
  write: !check,
});
const wallpaperOutputs = await generateWallpapers({
  wallpapersDir: path.join(brandDir, "wallpapers"),
  tokens,
  write: !check,
});
const spinnerOutputs = generateSpinners({
  spinnersDir: path.join(brandDir, "spinners"),
  tokens,
  write: !check,
});
const mirrorOutputs = mirrorToWebsite({
  websiteDir,
  pwaOutputs: new Map(
    [...pwaOutputs].map(([name, bytes]) => [name, bytes]),
  ),
  write: !check,
});

if (check) {
  const pwaAbs = new Map(
    [...pwaOutputs].map(([name, bytes]) => [path.join(brandDir, "pwa", name), bytes]),
  );
  const wallpaperAbs = new Map(
    [...wallpaperOutputs].map(([rel, bytes]) => [path.join(brandDir, "wallpapers", rel), bytes]),
  );
  const spinnerAbs = new Map(
    [...spinnerOutputs].map(([name, bytes]) => [path.join(brandDir, "spinners", name), bytes]),
  );

  let failures = 0;
  failures += diffAgainstDisk(pwaAbs, "pwa");
  failures += diffAgainstDisk(wallpaperAbs, "wallpapers");
  failures += diffAgainstDisk(spinnerAbs, "spinners");
  failures += diffAgainstDisk(mirrorOutputs, "website mirrors");

  if (failures > 0) {
    console.error(
      `\n${failures} file(s) don't match brand-tokens.toml. Run \`pnpm generate-assets\` (no --check) to update them.`,
    );
    process.exit(1);
  }
  console.log("\nAll generated brand assets match brand-tokens.toml.");
} else {
  report(pwaOutputs, "pwa");
  report(wallpaperOutputs, "wallpapers");
  report(spinnerOutputs, "spinners");
  report(mirrorOutputs, "website mirrors");
  console.log("\nDone.");
}
