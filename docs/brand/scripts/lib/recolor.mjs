// The recolor engine behind `pnpm generate-assets`.
//
// docs/brand/pwa, docs/brand/wallpapers, and docs/brand/spinners/*.gif have
// no committed vector source for their composition — only the SVG mark
// itself does. Regenerating their *geometry* from scratch (backgrounds,
// glow, starfield, grid, animation timing) would mean guessing parameters
// with no ground truth to check against, which risks exactly what this
// pipeline must not do: changing anything but color.
//
// So instead of recomposing, this engine RECOLORS the committed binaries in
// place. Every pixel is modeled as a solid ink blended toward either a solid
// background (opaque assets: wallpapers, app icons) or nothing at all
// (straight-alpha assets: the bare favicons, where alpha *is* the coverage
// and RGB is the ink regardless of coverage). The ink itself is either a
// fixed color (fg/text) or a point along the violet->cyan nebula gradient.
// Recovering that point (g) and blend weight (t) per pixel and re-rendering
// with the new palette at the same (g, t) preserves every geometric detail —
// anti-aliasing, glow falloff, starfield positions, animation frames —
// no matter what the new palette is.
//
// Anchors are resolved to unique colors only (a few hundred to a few
// thousand per asset, never per-pixel), so a 6016x3384 wallpaper recolors in
// a couple of seconds.
//
// The (g, t) fit is a discrete search (see GRADIENT_STEPS below), so it is
// NOT a perfect identity function pixel-for-pixel even when old === new —
// re-decomposing an already-quantized pixel can occasionally land one grid
// step away from where it started. That is invisible to the eye but would
// make `pnpm generate-assets --check` flake by a few bytes on files it had
// already regenerated. `palettesEqual` below is what callers use to skip
// the fit entirely — and copy the source bytes verbatim — whenever nothing
// is actually changing, which is the only case that needs to be exact.

const GRADIENT_STEPS = 96;

function lerp(a, b, t) {
  return a + (b - a) * t;
}

function lerpRgb(a, b, t) {
  return [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)];
}

function clampByte(v) {
  return Math.max(0, Math.min(255, Math.round(v)));
}

/** Best-fit weight t in [0,1] placing `target` on the segment a->b, and the squared error. */
function projectOntoSegment(a, b, target) {
  let num = 0;
  let den = 0;
  for (let c = 0; c < 3; c++) {
    const d = b[c] - a[c];
    num += (target[c] - a[c]) * d;
    den += d * d;
  }
  const t = den === 0 ? 0 : Math.max(0, Math.min(1, num / den));
  const col = lerpRgb(a, b, t);
  const err =
    (col[0] - target[0]) ** 2 + (col[1] - target[1]) ** 2 + (col[2] - target[2]) ** 2;
  return { t, err };
}

/** Best-fit (g, t) placing `target` on the ruled surface bg->lerp(violet,cyan,g). */
function fitBgGradient(bg, violet, cyan, target) {
  let best = { err: Infinity, g: 0, t: 0 };
  for (let i = 0; i <= GRADIENT_STEPS; i++) {
    const g = i / GRADIENT_STEPS;
    const ink = lerpRgb(violet, cyan, g);
    const { t, err } = projectOntoSegment(bg, ink, target);
    if (err < best.err) best = { err, g, t };
  }
  return best;
}

/** Best-fit g placing `target` directly on the violet->cyan segment (no bg blend). */
function fitInkGradient(violet, cyan, target) {
  let best = { err: Infinity, g: 0 };
  for (let i = 0; i <= GRADIENT_STEPS; i++) {
    const g = i / GRADIENT_STEPS;
    const ink = lerpRgb(violet, cyan, g);
    const err = (ink[0] - target[0]) ** 2 + (ink[1] - target[1]) ** 2 + (ink[2] - target[2]) ** 2;
    if (err < best.err) best = { err, g };
  }
  return best;
}

/**
 * Build a memoized recolor function for one (oldPalette -> newPalette) pass.
 * `palette` is `{ bg, fg, violet, cyan }`, each an [r,g,b] triple.
 */
function makeRecolorFn(oldPalette, newPalette) {
  const lutOpaque = new Map();
  const lutInk = new Map();

  return {
    /** Opaque compositing: pixel is bg blended with an ink (gradient point or fg). */
    opaque(r, g, b) {
      const key = (r << 16) | (g << 8) | b;
      let hit = lutOpaque.get(key);
      if (hit) return hit;
      const target = [r, g, b];
      const grad = fitBgGradient(oldPalette.bg, oldPalette.violet, oldPalette.cyan, target);
      const pure = projectOntoSegment(oldPalette.bg, oldPalette.fg, target);
      const rebuilt =
        grad.err <= pure.err
          ? lerpRgb(newPalette.bg, lerpRgb(newPalette.violet, newPalette.cyan, grad.g), grad.t)
          : lerpRgb(newPalette.bg, newPalette.fg, pure.t);
      hit = rebuilt.map(clampByte);
      lutOpaque.set(key, hit);
      return hit;
    },

    /** Straight alpha: RGB is the ink itself, independent of coverage. */
    ink(r, g, b) {
      const key = (r << 16) | (g << 8) | b;
      let hit = lutInk.get(key);
      if (hit) return hit;
      const target = [r, g, b];
      const grad = fitInkGradient(oldPalette.violet, oldPalette.cyan, target);
      const fgErr =
        (oldPalette.fg[0] - r) ** 2 + (oldPalette.fg[1] - g) ** 2 + (oldPalette.fg[2] - b) ** 2;
      const rebuilt =
        grad.err <= fgErr
          ? lerpRgb(newPalette.violet, newPalette.cyan, grad.g)
          : newPalette.fg;
      hit = rebuilt.map(clampByte);
      lutInk.set(key, hit);
      return hit;
    },
  };
}

/**
 * Start a recolor session for one (oldPalette -> newPalette) pass. Reuse it
 * across every frame of a single asset (e.g. a GIF's frames, all sharing one
 * palette) so the per-unique-color fit is cached once, not per frame.
 */
export function makeRecolorSession(oldPalette, newPalette) {
  return makeRecolorFn(oldPalette, newPalette);
}

/**
 * Recolor an RGBA buffer (pngjs/omggif layout: 4 bytes per pixel) in place
 * and return it, using an existing session. Auto-detects straight-alpha (any
 * pixel with alpha !== 255) vs. fully opaque and picks the matching model
 * per-buffer, matching how every asset in this kit is actually composited
 * (see module doc above).
 */
export function recolorRGBAWithSession(data, session) {
  let transparent = false;
  for (let i = 3; i < data.length; i += 4) {
    if (data[i] !== 255) {
      transparent = true;
      break;
    }
  }

  for (let i = 0; i < data.length; i += 4) {
    const a = data[i + 3];
    if (transparent && a === 0) continue; // no visible ink to recolor
    const rgb = transparent
      ? session.ink(data[i], data[i + 1], data[i + 2])
      : session.opaque(data[i], data[i + 1], data[i + 2]);
    data[i] = rgb[0];
    data[i + 1] = rgb[1];
    data[i + 2] = rgb[2];
  }
  return data;
}

/** One-shot recolor for a single buffer — starts and discards its own session. */
export function recolorRGBA(data, oldPalette, newPalette) {
  return recolorRGBAWithSession(data, makeRecolorSession(oldPalette, newPalette));
}

/** True when two `{bg, fg, violet, cyan}` palettes name the exact same RGB triples. */
export function palettesEqual(a, b) {
  const sameRgb = (x, y) => x[0] === y[0] && x[1] === y[1] && x[2] === y[2];
  return (
    sameRgb(a.bg, b.bg) &&
    sameRgb(a.fg, b.fg) &&
    sameRgb(a.violet, b.violet) &&
    sameRgb(a.cyan, b.cyan)
  );
}

export function paletteFor(tokens, theme) {
  return {
    bg: tokens[theme].bg,
    fg: tokens[theme].fg,
    violet: tokens.nebula.violet,
    cyan: tokens.nebula.cyan,
  };
}
