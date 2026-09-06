// The deck-fit overflow normalization, as source text rather than a plain
// function, and in its own leaf module with no side effects.
//
// `scripts/deck-fit.mjs` runs this INSIDE the browser via `page.evaluate`,
// which can only receive JSON-serializable arguments — a function defined
// out here cannot be called from in there directly, but a string can be
// shipped across and turned back into a function with `new Function`. That
// is what buys a single source of truth: `scripts/test-deck-fit-math.mjs`
// calls this same string the same way, from plain node, with no browser at
// all. It lives in its own file (rather than inside deck-fit.mjs) so
// importing it never runs deck-fit.mjs's top-level launch-a-browser script.
//
// `fit()` transform-scales the whole stage by k = min(w/1600, h/900), so
// `scrollDelta` (scrollHeight/Width minus clientHeight/Width) is LAYOUT px —
// the transform never touches it — while `extreme` comes from a descendant's
// `getBoundingClientRect()`, which returns the SCALED box: k times the
// layout value. Taking `Math.max(scrollDelta, extreme - boxSize)` therefore
// mixed units: at k < 1 the layout reading won and the number was layout px,
// at k > 1 the sweep won and the number was visual px, so one real overflow
// printed a different magnitude at every viewport (#2475). Dividing the
// sweep back down to layout px before the max keeps both terms in the same
// unit.
//
// `clientSize` is 0 for a display:none slide — deck-fit.mjs activates one
// slide at a time, so that never happens today, but a future refactor that
// measures an inactive one hits the guard below instead of dividing by zero.
// The fallback is the layout reading alone, never NaN: NaN compares false
// against `> 0` and would silently pass a slide that may genuinely overflow.
export const OVERFLOW_MATH_SRC = `function computeOverflow(scrollDelta, boxSize, clientSize, extreme) {
  const k = clientSize === 0 ? 0 : boxSize / clientSize;
  const sweep = k === 0 ? 0 : Math.round((extreme - boxSize) / k);
  return Math.max(scrollDelta, sweep);
}`;
