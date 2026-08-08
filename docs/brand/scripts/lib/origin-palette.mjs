// The recolor engine needs two palettes: the one already baked into the
// committed PNG/GIF pixels (to decompose FROM) and the one in
// brand-tokens.toml (to render TOWARD). Those are different things —
// brand-tokens.toml is a target a maintainer edits, so it cannot also be
// asked "what's in the file on disk right now." This module is the other
// half: the fixed palette every binary in docs/brand/pwa, docs/brand/wallpapers,
// and docs/brand/spinners was actually rendered with.
//
// It intentionally matches brand-tokens.toml's shipped defaults byte for
// byte. That equality is what makes `pnpm generate-assets` with an unedited
// brand-tokens.toml a no-op recolor (see scripts/generate-assets.mjs
// --check): origin === target means every pixel decomposes and re-renders
// to itself. Editing brand-tokens.toml only changes the target half of that
// pair, which is the point.
//
// This constant does not change when brand-tokens.toml changes. It changes
// only when the committed binaries themselves are regenerated against a new
// target and that output is committed as the new baseline — at which point
// this file is updated in the same commit to match, and target == origin
// again.

export const ORIGIN_HEX = {
  nebula: { violet: "#7C5CFF", cyan: "#1FD8E6" },
  dark: { bg: "#080B1C", fg: "#EEF1FA" },
  light: { bg: "#F6F7FB", fg: "#0B0E1A" },
};

function hexToRgb(hex) {
  return [
    parseInt(hex.slice(1, 3), 16),
    parseInt(hex.slice(3, 5), 16),
    parseInt(hex.slice(5, 7), 16),
  ];
}

export const ORIGIN = {
  nebula: {
    violet: hexToRgb(ORIGIN_HEX.nebula.violet),
    cyan: hexToRgb(ORIGIN_HEX.nebula.cyan),
  },
  dark: { bg: hexToRgb(ORIGIN_HEX.dark.bg), fg: hexToRgb(ORIGIN_HEX.dark.fg) },
  light: { bg: hexToRgb(ORIGIN_HEX.light.bg), fg: hexToRgb(ORIGIN_HEX.light.fg) },
};

export function originPaletteFor(theme) {
  return {
    bg: ORIGIN[theme].bg,
    fg: ORIGIN[theme].fg,
    violet: ORIGIN.nebula.violet,
    cyan: ORIGIN.nebula.cyan,
  };
}
