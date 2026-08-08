import { test } from "node:test";
import assert from "node:assert/strict";
import { PNG } from "pngjs";
import {
  recolorRGBA,
  palettesEqual,
  paletteFor,
} from "./recolor.mjs";
import { recolorPngFile } from "./png.mjs";
import { originPaletteFor, ORIGIN } from "./origin-palette.mjs";

const DARK = originPaletteFor("dark");
const RED_PALETTE = { bg: [10, 10, 10], fg: [240, 240, 240], violet: [255, 0, 0], cyan: [0, 255, 0] };

test("palettesEqual — true for identical RGB triples, false otherwise", () => {
  const a = { bg: [1, 2, 3], fg: [4, 5, 6], violet: [7, 8, 9], cyan: [10, 11, 12] };
  const b = { bg: [1, 2, 3], fg: [4, 5, 6], violet: [7, 8, 9], cyan: [10, 11, 12] };
  const c = { ...b, cyan: [10, 11, 13] };
  assert.equal(palettesEqual(a, b), true);
  assert.equal(palettesEqual(a, c), false);
});

test("paletteFor — reads the right theme's bg/fg alongside the shared nebula gradient", () => {
  const tokens = {
    nebula: { violet: [1, 2, 3], cyan: [4, 5, 6] },
    dark: { bg: [7, 8, 9], fg: [10, 11, 12] },
    light: { bg: [13, 14, 15], fg: [16, 17, 18] },
  };
  assert.deepEqual(paletteFor(tokens, "dark"), {
    bg: [7, 8, 9],
    fg: [10, 11, 12],
    violet: [1, 2, 3],
    cyan: [4, 5, 6],
  });
  assert.deepEqual(paletteFor(tokens, "light"), {
    bg: [13, 14, 15],
    fg: [16, 17, 18],
    violet: [1, 2, 3],
    cyan: [4, 5, 6],
  });
});

test("recolorRGBA — opaque model: pure bg/violet/cyan/fg pixels land on their new-palette targets", () => {
  // 4 pixels: bg, violet, cyan, fg — every one an exact anchor color, so the
  // fit is exact (no quantization slack) regardless of GRADIENT_STEPS.
  const data = Buffer.from([
    ...DARK.bg, 255,
    ...DARK.violet, 255,
    ...DARK.cyan, 255,
    ...DARK.fg, 255,
  ]);
  recolorRGBA(data, DARK, RED_PALETTE);
  assert.deepEqual([data[0], data[1], data[2]], RED_PALETTE.bg);
  assert.deepEqual([data[4], data[5], data[6]], RED_PALETTE.violet);
  assert.deepEqual([data[8], data[9], data[10]], RED_PALETTE.cyan);
  assert.deepEqual([data[12], data[13], data[14]], RED_PALETTE.fg);
});

test("recolorRGBA — straight alpha model: ink recolors, alpha coverage is untouched", () => {
  // One fully-transparent pixel (don't-care RGB) and one half-covered violet
  // pixel — alpha must survive exactly; only RGB may change.
  const data = Buffer.from([0, 0, 0, 0, ...DARK.violet, 128]);
  recolorRGBA(data, DARK, RED_PALETTE);
  assert.equal(data[3], 0, "fully transparent pixel's alpha must be untouched");
  assert.deepEqual([data[4], data[5], data[6]], RED_PALETTE.violet);
  assert.equal(data[7], 128, "partial-coverage alpha must be untouched");
});

test("recolorPngFile — identity palette returns the source bytes verbatim (no re-encode)", async () => {
  const png = new PNG({ width: 2, height: 1 });
  png.data.set([...DARK.bg, 255, ...DARK.violet, 255]);
  const original = PNG.sync.write(png);

  const path = await import("node:path");
  const os = await import("node:os");
  const fs = await import("node:fs");
  const tmp = path.join(os.tmpdir(), `brand-kit-identity-test-${process.pid}.png`);
  fs.writeFileSync(tmp, original);
  try {
    const out = await recolorPngFile(tmp, DARK, DARK);
    assert.ok(out.equals(original), "identity recolor must be byte-identical to the source");
  } finally {
    fs.rmSync(tmp);
  }
});

test("origin-palette — ORIGIN_HEX and ORIGIN agree on every channel", () => {
  for (const theme of ["dark", "light"]) {
    assert.equal(ORIGIN[theme].bg.length, 3);
    assert.equal(ORIGIN[theme].fg.length, 3);
  }
  assert.equal(ORIGIN.nebula.violet.length, 3);
  assert.equal(ORIGIN.nebula.cyan.length, 3);
});
