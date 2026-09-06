#!/usr/bin/env node
//
// Witness for the deck-fit overflow-unit bug (#2475): one overflow reported
// three different numbers because the two readings `overH`/`overW` take the
// max of were in different coordinate spaces.
//
//   node scripts/test-deck-fit-math.mjs
//
// Hermetic: reconstructs `computeOverflow` from `OVERFLOW_MATH_SRC`
// (`scripts/deck-fit-math.mjs`), the same string `scripts/deck-fit.mjs` ships
// into the browser via `page.evaluate`, so this test and the shipped code can
// never drift apart silently. Importing the math from its own leaf module
// (rather than from deck-fit.mjs itself) means this test never runs
// deck-fit.mjs's top-level launch-a-browser script. No browser, no network --
// plain node, which is why this can run as a witness in a sandbox that can't
// launch Chromium.
//
// The buggy formula this replaces took Math.max of a LAYOUT-px reading
// (scrollHeight - clientHeight) and a VISUAL-px reading (a descendant's
// getBoundingClientRect() minus the frame's own box, unscaled) at whatever
// scale k the viewport happened to produce. One real 25px overflow then
// printed 25, 30, or 40 depending on k -- reproduced below at the four k
// values measured on 2026-08-09 against the investor deck.

import { OVERFLOW_MATH_SRC } from "./deck-fit-math.mjs";

let pass = 0;
let fail = 0;

function ok(name) {
  pass += 1;
  console.log(`ok   ${name}`);
}

function no(name, detail) {
  fail += 1;
  console.log(`FAIL ${name}`);
  if (detail) console.log(`  ${detail}`);
}

// eslint-disable-next-line no-new-func -- see OVERFLOW_MATH_SRC's comment
const computeOverflow = new Function(`return (${OVERFLOW_MATH_SRC});`)();

// The buggy formula this fix replaced: Math.max of the two readings taken
// AS THEY STOOD, with no unit normalization. Kept here, inline, only so this
// witness can demonstrate the fix actually changes the answer -- it is not
// the thing under test.
function buggyOverflow(scrollDelta, boxSize, extreme) {
  return Math.max(scrollDelta, Math.round(extreme - boxSize));
}

// A layout box of height 900 with a real ~25px layout overflow (925 tall
// laid out), scaled by k. `box.height` (the frame's own rect) and `extreme`
// (a descendant's rect) are both k times their layout value; scrollHeight/
// clientHeight are never scaled, so the layout overflow reading is fixed.
const LAYOUT_HEIGHT = 900;
const LAYOUT_OVERFLOW = 25;
const LAYOUT_DELTA = LAYOUT_OVERFLOW; // scrollHeight - clientHeight, unscaled

const CASES = [
  { name: "1440x900 · laptop", k: 0.9 },
  { name: "1280x800 · small laptop", k: 0.8 },
  { name: "1920x1080 · projector", k: 1.2 },
  { name: "2560x1440 · large display", k: 1.6 },
];

// 1. The bug: at k != 1, the buggy formula reports a different magnitude for
//    the SAME layout overflow -- this is what made one overflow read as
//    three numbers across four viewports.
const buggyReadings = CASES.map(({ k }) => {
  const boxHeight = LAYOUT_HEIGHT * k;
  const extreme = (LAYOUT_HEIGHT + LAYOUT_OVERFLOW) * k;
  return buggyOverflow(LAYOUT_DELTA, boxHeight, extreme);
});
const buggyDistinct = new Set(buggyReadings).size;
if (buggyDistinct > 1) {
  ok("demonstrates the bug: the old formula disagrees across viewports");
} else {
  no(
    "demonstrates the bug: the old formula disagrees across viewports",
    `expected more than one distinct reading, got ${JSON.stringify(buggyReadings)} -- the repro no longer demonstrates the defect`,
  );
}

// 2. The fix: computeOverflow (the shipped math) reports the SAME layout-px
//    magnitude at every k, including k > 1 and k < 1.
const fixedReadings = CASES.map(({ k }) => {
  const boxHeight = LAYOUT_HEIGHT * k;
  const clientHeight = LAYOUT_HEIGHT; // clientHeight is never scaled
  const extreme = (LAYOUT_HEIGHT + LAYOUT_OVERFLOW) * k;
  return computeOverflow(LAYOUT_DELTA, boxHeight, clientHeight, extreme);
});
if (fixedReadings.every((v) => v === LAYOUT_OVERFLOW)) {
  ok("one overflow reports one number at every viewport");
} else {
  no(
    "one overflow reports one number at every viewport",
    `expected every reading to equal ${LAYOUT_OVERFLOW}, got ${JSON.stringify(
      fixedReadings.map((v, i) => `${CASES[i].name}=${v}`),
    )}`,
  );
}

// 3. No overflow stays no overflow at every k -- the fix must never turn a
//    passing slide into a failing one.
const zeroReadings = CASES.map(({ k }) => {
  const boxHeight = LAYOUT_HEIGHT * k;
  const clientHeight = LAYOUT_HEIGHT;
  const extreme = LAYOUT_HEIGHT * k; // the descendant never exceeds the box
  return computeOverflow(0, boxHeight, clientHeight, extreme);
});
if (zeroReadings.every((v) => v === 0)) {
  ok("no overflow still reads zero at every viewport");
} else {
  no("no overflow still reads zero at every viewport", JSON.stringify(zeroReadings));
}

// 4. The divide-by-zero guard: a display:none slide (clientHeight 0) must
//    fail toward reporting the layout delta, never toward NaN/Infinity --
//    a NaN would compare false against `> 0` and silently pass the slide.
const guarded = computeOverflow(12, 0, 0, 30);
if (guarded === 12 && Number.isFinite(guarded)) {
  ok("clientSize=0 falls back to the layout reading, not NaN/Infinity");
} else {
  no("clientSize=0 falls back to the layout reading, not NaN/Infinity", `got ${guarded}`);
}

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
