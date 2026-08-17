#!/usr/bin/env node
/**
 * The TypeScript half of a cross-language parity check.
 *
 * `crates/stella-diff/src/view.rs::plan` decides how much of a diff every
 * Stella surface draws and which part. This page reimplements it in TypeScript
 * (`lib/diff-view.ts::selectDiffLines`) because it is a Next.js client with no
 * way to call into the workspace — and an arena transcript is read *beside*
 * the deck and the Observatory when someone is arguing about what a run did,
 * which is exactly when one edit must not look like two.
 *
 * The two halves cannot share a test runner, so they share a **file**:
 * `crates/stella-diff/tests/fixtures/view-plan-matrix.txt`, generated from the
 * Rust and pinned there by `tests/view_plan_matrix.rs`. This script runs the
 * TypeScript over the same matrix and asserts it reproduces that file exactly.
 * So a Rust change to the policy must re-bless the golden, and a re-blessed
 * golden fails here until the port catches up.
 *
 *   node scripts/check-diff-view-parity.mjs
 *
 * Zero dependencies on purpose. Node strips the type annotations from the
 * imported `.ts` natively (22.6+), so this runs against a checkout with no
 * `npm install` at all — which is what lets CI check it in seconds rather than
 * installing a Next.js app to test one pure function.
 *
 * This replaced a manual ritual documented in two doc comments (#3558). Its
 * one run before being automated is why: the first diff disagreed on five
 * cases, and the bug was in the Rust.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

import { selectDiffLines } from "../lib/diff-view.ts";

const here = dirname(fileURLToPath(import.meta.url));
const GOLDEN = resolve(here, "../../../crates/stella-diff/tests/fixtures/view-plan-matrix.txt");

/** Mirrors `MATRIX` in `crates/stella-diff/tests/view_plan_matrix.rs`. */
const MATRIX = [
  [15, 3],
  [80, 4],
  [31, 31],
  [100, 7],
  [6, 3],
  [500, 500],
];

/** Mirrors `CAP_MAX` there. */
const CAP_MAX = 40;

/** `keep[]` as the golden's `shown=[a-b,c-d]` spans. */
function spansOf(keep) {
  const out = [];
  let start = null;
  keep.forEach((kept, i) => {
    if (kept && start === null) start = i;
    if (!kept && start !== null) {
      out.push(`${start}-${i}`);
      start = null;
    }
  });
  if (start !== null) out.push(`${start}-${keep.length}`);
  return out;
}

function render() {
  // The header is the Rust's, verbatim: the file is compared byte for byte,
  // and a header drifting is itself a signal that one side was edited without
  // the other.
  const lines = [
    "# stella_diff::view::plan over a fixed matrix.",
    "# Generated — re-bless with:",
    "#   BLESS=1 cargo test -p stella-diff --test view_plan_matrix",
    "# The TypeScript port must reproduce this byte for byte:",
    "#   node arenabench/ui/scripts/check-diff-view-parity.mjs",
    "# `fold` is the index the elision marker precedes, or `-` for none.",
  ];
  for (const [total, step] of MATRIX) {
    // One `@@` every `step` lines — the same hunk starts the Rust is given.
    const raw = [];
    for (let i = 0; i < total; i++) raw.push(i % step === 0 ? "@@ -1 +1 @@" : "+x");
    for (let cap = 0; cap <= CAP_MAX; cap++) {
      const plan = selectDiffLines(raw, cap);
      const fold = plan.hidden === 0 ? "-" : String(plan.foldBefore);
      lines.push(
        `total=${total} step=${step} cap=${cap} shown=[${spansOf(plan.keep).join(",")}]` +
          ` hidden=${plan.hidden} fold=${fold}`,
      );
    }
  }
  return `${lines.join("\n")}\n`;
}

let golden;
try {
  golden = readFileSync(GOLDEN, "utf8");
} catch (err) {
  console.error(`diff-view-parity: cannot read ${GOLDEN}\n  ${err.message}`);
  console.error("Generate it with: BLESS=1 cargo test -p stella-diff --test view_plan_matrix");
  process.exit(2);
}

const rendered = render();
if (golden === rendered) {
  const cases = rendered.split("\n").filter((l) => l.startsWith("total=")).length;
  console.log(`diff-view-parity: OK — the TypeScript port matches the Rust on ${cases} cases.`);
  process.exit(0);
}

// Name the first differing line rather than dumping two 250-line blobs.
const g = golden.split("\n");
const r = rendered.split("\n");
const at = g.findIndex((line, i) => line !== r[i]);
console.error("diff-view-parity: the TypeScript port no longer matches the Rust policy.");
if (at !== -1) {
  console.error(`  line ${at + 1}`);
  console.error(`  rust: ${g[at] ?? "(end of file)"}`);
  console.error(`  ts:   ${r[at] ?? "(end of file)"}`);
} else {
  console.error(`  lengths differ: ${g.length} rust vs ${r.length} ts`);
}
console.error("");
console.error("Fix whichever side is wrong — the Rust is not automatically the");
console.error("right one. Port to lib/diff-view.ts, or change");
console.error("crates/stella-diff/src/view.rs and re-bless the golden with:");
console.error("  BLESS=1 cargo test -p stella-diff --test view_plan_matrix");
process.exit(1);
