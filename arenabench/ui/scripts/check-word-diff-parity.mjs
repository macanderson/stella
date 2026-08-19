#!/usr/bin/env node
/**
 * The TypeScript half of a cross-language parity check.
 *
 * `crates/stella-transcript/src/word.rs::highlight` decides what inside a
 * modified line every Stella surface tints — the Command Deck's character grid
 * and the jet-black web transcript the Observatory serves. This page
 * reimplements it in TypeScript (`lib/word-diff.ts`) because it is a Next.js
 * client with no way to call into the workspace — and an arena transcript is
 * read *beside* the deck and the Observatory when someone is arguing about what
 * a run did, which is exactly when one edit must not look like two.
 *
 * The two halves cannot share a test runner, so they share a **file**:
 * `crates/stella-transcript/tests/fixtures/word-highlight-matrix.txt`,
 * generated from the Rust and pinned there by `tests/word_highlight_matrix.rs`.
 * This script runs the TypeScript over the same matrix and asserts it
 * reproduces that file exactly. So a Rust change to the policy must re-bless
 * the golden, and a re-blessed golden fails here until the port catches up.
 *
 *   node scripts/check-word-diff-parity.mjs
 *
 * The matrix itself lives in the golden's own rows rather than being duplicated
 * here: this script parses the case index off each line and drives the port
 * with the pairs mirrored below. That mirror is the one thing both sides must
 * be edited together — which the golden's line count catches, since a case
 * added on one side alone changes it.
 *
 * Zero dependencies on purpose. Node strips the type annotations from the
 * imported `.ts` natively (22.6+), so this runs against a checkout with no
 * `npm install` at all — the same arrangement `check-diff-view-parity.mjs` has.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

import { highlight } from "../lib/word-diff.ts";

const here = dirname(fileURLToPath(import.meta.url));
const GOLDEN = resolve(
  here,
  "../../../crates/stella-transcript/tests/fixtures/word-highlight-matrix.txt",
);

/** Mirrors `MATRIX` in `crates/stella-transcript/tests/word_highlight_matrix.rs`. */
const MATRIX = [
  ["same line", "same line"],
  ["\\setlength{\\parindent}{15pt}", "\\setlength{\\parindent}{12pt}"],
  ["{a}{b}", "{a}{c}"],
  ["x = 1", "x = 2"],
  ["--fast", "--fast2"],
  ["prefix_alpha", "prefix_beta"],
  ["let x = 1;", "let x = 1; // note"],
  ["let x = 1; // note", "let x = 1;"],
  ["    indented", "        indented"],
  ["completely different", "nothing alike here"],
  [
    "keep this part the same but change the tail",
    "keep this part the same but change the end",
  ],
  ["a 1 b 2 c", "a 9 b 8 c"],
  ["", "added"],
  ["removed", ""],
  ["café ünïcode", "café ünicode"],
  ["status: ✅ done", "status: ❌ done"],
  ["a 🎉 b", "a 🎈 b"],
  [
    '{"model": "claude-opus-5", "effort": "high"}',
    '{"model": "claude-opus-5", "effort": "medium"}',
  ],
  [
    "pub fn resolve(&self, id: &str) -> Option<Pin> {",
    "pub fn resolve(&self, id: &TaskId) -> Option<Pin> {",
  ],
];

/** Mirrors `escape` in the Rust: backslash first, or it double-escapes. */
function escape(text) {
  return [...text]
    .map((ch) => {
      if (ch === "\\") return "\\\\";
      if (ch === "\n") return "\\n";
      if (ch === "|") return "\\|";
      if (ch === "[") return "\\[";
      if (ch === "]") return "\\]";
      return ch;
    })
    .join("");
}

/** Mirrors `render_spans`: `|unchanged|` and `[changed]`, concatenated. */
function renderSpans(spans) {
  return spans
    .map((span) => (span.changed ? `[${escape(span.text)}]` : `|${escape(span.text)}|`))
    .join("");
}

function renderMatrix() {
  const out = [
    "# stella_transcript::word::highlight over a fixed matrix.",
    "# Generated — re-bless with:",
    "#   BLESS=1 cargo test -p stella-transcript --test word_highlight_matrix",
    "# The TypeScript port must reproduce this byte for byte:",
    "#   node arenabench/ui/scripts/check-word-diff-parity.mjs",
    "# Spans: |unchanged| and [changed]. `\\n` `\\|` `\\[` `\\]` are escaped.",
  ];
  MATRIX.forEach(([old, next], index) => {
    const [oldSpans, newSpans] = highlight(old, next);
    out.push(
      `case=${index} old=${renderSpans(oldSpans)} new=${renderSpans(newSpans)}`,
    );
  });
  return `${out.join("\n")}\n`;
}

const golden = readFileSync(GOLDEN, "utf8");
const rendered = renderMatrix();

if (golden === rendered) {
  console.log(
    `word-diff parity: ${MATRIX.length} cases agree with ${GOLDEN.split("/").slice(-1)[0]}`,
  );
  process.exit(0);
}

// Name the differing lines rather than dumping two blobs: the answer to "what
// did the port get wrong" is one row, and usually the same row for every case.
const goldenLines = golden.split("\n");
const renderedLines = rendered.split("\n");
const differing = [];
for (let i = 0; i < Math.max(goldenLines.length, renderedLines.length); i += 1) {
  if (goldenLines[i] !== renderedLines[i]) {
    differing.push(
      `  line ${i + 1}\n    golden: ${goldenLines[i] ?? "(missing)"}\n    port:   ${renderedLines[i] ?? "(missing)"}`,
    );
  }
}

console.error(
  `lib/word-diff.ts disagrees with ${GOLDEN} on ${differing.length} line(s):\n` +
    `${differing.slice(0, 10).join("\n")}\n` +
    (differing.length > 10 ? `  …and ${differing.length - 10} more\n` : "") +
    "\nEither the port is wrong, or the Rust policy moved and the golden was\n" +
    "re-blessed without porting the change. Read\n" +
    "  crates/stella-transcript/src/word.rs\n" +
    "and make lib/word-diff.ts say the same thing.",
);
process.exit(1);
