#!/usr/bin/env node
//
// Measure every slide of a deck against the fixed stage it is laid out in, and
// fail if any slide overflows it.
//
// Why this exists: the investor deck previously used a fluid layout, so "does
// this slide fit?" had a different answer at every viewport, and the only way
// to find out was to open it and look. Four slides were silently overflowing a
// 1440x900 laptop (issue #2373) and two of them had been shipping that way for
// a revision. The deck now lays out inside a deterministic 1600x900 canvas that
// the page scales to fit, which turns the question into one measurement with
// one answer — and this script is the thing that takes it.
//
// It is deliberately NOT a `make gate` step: it needs a browser, and CI's gate
// runs toolchain-free guards plus cargo. `.github/workflows/deck-fit.yml` runs
// it on every change under website/public/presentations/ instead (#2425).
//
//   node scripts/deck-fit.mjs                       # default deck, default sizes
//   node scripts/deck-fit.mjs path/to/deck.html
//
// It needs playwright-core and a Chromium. Both ship in this repo's agent
// image; locally, `npm i -g playwright-core` plus any Chrome will do:
//
//   CHROME=/path/to/chrome node scripts/deck-fit.mjs
//
// Exit status is 0 when every slide fits at every viewport, 1 otherwise — with
// three statuses reserved for "the measurement never happened", because the
// workflow above now walks the whole tree recursively (#3376) and hands this
// script HTML that was never authored as a fixed-canvas deck, or that never
// finishes loading:
//
//   0  every slide fits every viewport
//   1  a slide overflows, or the file claims to be a deck and is malformed
//   2  the harness is missing (no such file, no playwright-core)
//   3  not a fixed-canvas deck — skipped, with the reason printed
//   4  the page failed to load — not proven to fit, and not counted as a
//      measurement (#6278)
//
// 3 exists so a skip is a named event rather than a silent pass. The shape it
// names is the one this script can actually measure: `.slide` elements each
// wrapping a `.frame`, the fixed stage the overflow is measured against. A
// scrolling document under the same directory (website/public/presentations/
// turn-loop/index.html has 34 `.slide` sections and no `.frame` at all) is not
// that shape and must not be counted as either a pass or a failure.
//
// 4 exists for the same reason: `investor-deck.html`'s only unusual asset is a
// self-hosted webfont, and #6278 traced a stuck CI run to a `waitForLoadState`
// call that recorded no navigation step at all before its deadline — a page
// that never reached the load state, not one that loaded slowly, and Playwright's
// own docs discourage that call as flaky for exactly this reason. Reporting it
// as its own exit status is the fix; a retry is the weaker answer, since it
// would hide how often the load itself hangs rather than showing it.
//
// A deck can hit both 1 and 4 across its viewports — one overflows while
// another never loads. 1 wins: an overflow is a proven defect, an unload only
// means that viewport's question went unanswered, and a proven defect must
// not read as "not proven to fit".

import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { OVERFLOW_MATH_SRC } from "./deck-fit-math.mjs";

const DECK = resolve(process.argv[2] ?? "website/public/presentations/investor-deck.html");

// The canvas the deck is authored against, and the viewports it is shown on.
// 1440x900 is the laptop the issue was filed from; the others bracket it.
const VIEWPORTS = [
  { w: 1440, h: 900, name: "1440x900 · laptop" },
  { w: 1280, h: 800, name: "1280x800 · small laptop" },
  { w: 1920, h: 1080, name: "1920x1080 · projector" },
  { w: 2560, h: 1440, name: "2560x1440 · large display" },
  // The deck ships its own JetBrains Mono. A slide that only fits because that
  // file arrived is a slide that clips for anyone opening the HTML without the
  // sibling font directory — over email, from a download, behind a proxy that
  // drops woff2. This pass measures the fallback metrics instead.
  { w: 1440, h: 900, name: "1440x900 · webfont blocked", blockFonts: true },
];

const CHROME = process.env.CHROME ?? "/opt/pw-browsers/chromium";

if (!existsSync(DECK)) {
  console.error(`deck-fit: no such deck: ${DECK}`);
  process.exit(2);
}

let chromium;
try {
  ({ chromium } = await import("playwright-core"));
} catch {
  console.error("deck-fit: playwright-core is not installed — skipping (npm i playwright-core)");
  process.exit(2);
}

const browser = await chromium.launch({ executablePath: CHROME });

// `goto()`'s default `waitUntil: "load"` already covers the page's own fetch.
// What it does not cover is a `@font-face` file that markup requests: the load
// event can fire while a webfont is still resolving, rendering on the fallback
// typeface until it swaps in. `document.fonts.status === "loaded"` is the
// direct signal for that settling, so this polls for it instead of
// `waitForLoadState("networkidle")`, which has no way to tell "still loading"
// from "never going to finish" and is what actually hung (#6278).
//
// `waitForFunction`, not `page.evaluate(() => document.fonts.ready)`: the
// fonts-ready promise is the thing that can hang, and `evaluate` carries no
// timeout of its own — it would await that promise forever and let the
// workflow's 10-minute job timeout kill the run before this script ever got
// to report exit 4. `waitForFunction` polls under the page's default timeout,
// so a font that never settles throws Playwright's own `TimeoutError`, the
// same shape `goto` already throws on a stuck navigation. A caller wraps this
// in try/catch and turns that into exit 4.
async function loadDeck(page, url) {
  await page.goto(url);
  await page.waitForFunction(() => document.fonts.status === "loaded");
}

// Ask what shape this file is before measuring it, in one cheap load. The
// recursive walk in deck-fit.yml means "is this a deck?" is now a real question
// with three answers, and each gets a different exit status rather than a
// guess: nothing to measure (skip), a stage to measure (proceed), or a deck
// that lost a `.frame` somewhere (a defect this script should report, not
// crash on — `slide.querySelector(".frame")` used to be dereferenced blind).
{
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  try {
    await loadDeck(page, pathToFileURL(DECK).href);
  } catch (err) {
    console.error(`deck-fit: ${DECK} — failed to load: ${err.name}: ${err.message}`);
    await page.close();
    await browser.close();
    process.exit(4);
  }
  const shape = await page.evaluate(() => {
    const slides = [...document.querySelectorAll(".slide")];
    return {
      slides: slides.length,
      framed: slides.filter((s) => s.querySelector(".frame")).length,
    };
  });
  await page.close();

  if (shape.slides === 0 || shape.framed === 0) {
    const reason =
      shape.slides === 0
        ? "no .slide elements"
        : `${shape.slides} .slide elements, none wrapping a .frame`;
    console.log(`deck-fit: skipping ${DECK} — not a fixed-canvas deck (${reason})`);
    await browser.close();
    process.exit(3);
  }
  if (shape.framed < shape.slides) {
    console.error(
      `deck-fit: ${DECK} — ${shape.slides - shape.framed} of ${shape.slides} slides have no .frame; ` +
        "a fixed-canvas deck measures overflow against that stage, so this deck cannot be measured as authored.",
    );
    await browser.close();
    process.exit(1);
  }
}

let failures = 0;
let unloaded = 0;

for (const vp of VIEWPORTS) {
  // Measure the resting layout, not the entrance animation: `.rise` parks its
  // children 12px low until the arrival animation runs, and a synchronous
  // measurement sweep never lets that frame land — which reads as a uniform
  // 12px overflow on every slide. The deck's reduced-motion rules zero those
  // transforms, so this is the honest geometry, and it is also what a viewer
  // with the OS setting on actually sees.
  const page = await browser.newPage({
    viewport: { width: vp.w, height: vp.h },
    reducedMotion: "reduce",
  });
  if (vp.blockFonts) await page.route("**/*.woff2", (r) => r.abort());
  try {
    await loadDeck(page, pathToFileURL(DECK).href);
  } catch (err) {
    console.log(`\nUNLOADED  ${vp.name}  —  ${err.name}: ${err.message}`);
    unloaded++;
    await page.close();
    continue;
  }

  const rows = await page.evaluate((mathSrc) => {
    // eslint-disable-next-line no-new-func -- see OVERFLOW_MATH_SRC's comment
    const computeOverflow = new Function(`return (${mathSrc});`)();
    const out = [];
    const slides = [...document.querySelectorAll(".slide")];
    slides.forEach((slide, i) => {
      slides.forEach((s) => s.classList.remove("active"));
      slide.classList.add("active");
      const frame = slide.querySelector(".frame");
      const box = frame.getBoundingClientRect();

      // Two independent readings of the same question, because each misses a
      // case the other catches: scrollHeight sees content the flow pushed past
      // the box, and the descendant sweep sees an absolutely-placed or
      // negatively-margined child that never grew it.
      let lowest = 0;
      let widest = 0;
      for (const el of frame.querySelectorAll("*")) {
        const r = el.getBoundingClientRect();
        if (r.height === 0 && r.width === 0) continue;
        lowest = Math.max(lowest, r.bottom - box.top);
        widest = Math.max(widest, r.right - box.left);
      }
      const overH = computeOverflow(frame.scrollHeight - frame.clientHeight, box.height, frame.clientHeight, lowest);
      const overW = computeOverflow(frame.scrollWidth - frame.clientWidth, box.width, frame.clientWidth, widest);
      out.push({
        n: i + 1,
        label: (slide.getAttribute("aria-label") || "").slice(0, 44),
        overH,
        overW,
      });
    });
    // The page also must never scroll: the stage is scaled to fit, so any
    // document overflow means the fit calculation itself is wrong.
    const docOverflow = {
      x: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      y: document.documentElement.scrollHeight - document.documentElement.clientHeight,
    };
    return { out, docOverflow };
  }, OVERFLOW_MATH_SRC);

  const bad = rows.out.filter((r) => r.overH > 0 || r.overW > 0);
  const docBad = rows.docOverflow.x > 0 || rows.docOverflow.y > 0;
  const status = bad.length === 0 && !docBad ? "PASS" : "FAIL";
  console.log(`\n${status}  ${vp.name}  —  ${rows.out.length} slides, ${bad.length} overflowing`);
  if (docBad) {
    console.log(`  document scrolls: ${rows.docOverflow.x}px x, ${rows.docOverflow.y}px y`);
  }
  for (const r of bad) {
    console.log(`  slide ${String(r.n).padStart(2, "0")}  +${r.overH}px tall  +${r.overW}px wide  ${r.label}`);
  }
  if (bad.length || docBad) failures++;
  await page.close();
}

await browser.close();

// An overflow outranks an unload: it is a proven defect, while an unload only
// means the question went unanswered at that viewport. Checking failures
// first keeps a deck that both overflowed at one viewport and failed to load
// at another from reading as merely "not proven to fit" — the overflow is
// proven, and exit 1 says so.
if (failures > 0) {
  console.log(`\ndeck-fit: ${failures} viewport(s) failed.`);
  process.exit(1);
}

if (unloaded > 0) {
  console.log(`\ndeck-fit: ${unloaded} of ${VIEWPORTS.length} viewport(s) never loaded — not proven to fit.`);
  process.exit(4);
}

console.log("\ndeck-fit: every slide fits every viewport.");
process.exit(0);
