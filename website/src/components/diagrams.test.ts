import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

/**
 * Guards the glyph trap in `src/components/diagrams.tsx`.
 *
 *   pnpm test
 *
 * ## The trap
 *
 * Diagram text is drawn in `var(--font-mono)`, which resolves to the
 * self-hosted JetBrains Mono in `src/fonts/` — and those files are the Google
 * Fonts **latin** subset, 229 codepoints. A character outside it does not fail
 * anywhere: the browser silently falls out to the next family in the stack for
 * that one glyph. The result is a different face, a different weight, and a
 * different advance width in the middle of a monospace string, so a label laid
 * out to fit its box no longer does — or renders as tofu on a machine with no
 * fallback carrying it.
 *
 * The subset's coverage is not the one you would guess. It carries `·`, `×`,
 * `—`, `…`, `↑` and `↓`, but **not `→` or `←`** — so the obvious character for
 * drawing a step sequence is exactly the one that is missing. That is how
 * this got noticed: a stage strip written `triage → recall → …` looked correct
 * in every editor and would have shipped broken. Write `->` instead — JetBrains
 * Mono ligates it into an arrow, so it draws as one and measures as two
 * ordinary characters.
 *
 * So the allowlist below is deliberately narrow — printable ASCII plus the
 * punctuation these diagrams actually use, each verified present in both
 * shipped weights. It is not derived from the font at runtime because decoding
 * a woff2 needs a parser this repo has no other use for; if the subset is ever
 * regenerated, re-verify with:
 *
 *     python3 -c "from fontTools.ttLib import TTFont; \
 *       print(sorted(TTFont('src/fonts/jetbrains-mono-latin-400-normal.woff2') \
 *       .getBestCmap()))"
 *
 * Only *rendered* strings are checked. `aria-label` is read aloud rather than
 * drawn, so it is free to say whatever reads best.
 */

const DIAGRAMS = join(dirname(fileURLToPath(import.meta.url)), "diagrams.tsx");

/** Non-ASCII characters verified present in both shipped JetBrains Mono weights. */
const ALLOWED_NON_ASCII = new Set([
  "·", // U+00B7 — the separator these diagrams use between peer items
  "×", // U+00D7 — multiplication, as in "call it N×"
  "–", // U+2013 en dash
  "—", // U+2014 em dash
  "‘", // U+2018
  "’", // U+2019
  "“", // U+201C
  "”", // U+201D
  "•", // U+2022
  "…", // U+2026
  "↑", // U+2191 — present in the subset; → and ← are NOT
  "↓", // U+2193
]);

/**
 * Source reduced to the parts that get painted: comments dropped so prose in
 * doc blocks is never checked, and `aria-label` / `<title>` dropped because
 * both are announced by a screen reader rather than drawn, and so are free to
 * use whatever character reads best.
 */
function paintedSource(): string {
  return readFileSync(DIAGRAMS, "utf8")
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/^[ \t]*\/\/.*$/gm, "")
    .replace(/aria-label="[^"]*"/g, "")
    .replace(/<title>[\s\S]*?<\/title>/g, "");
}

/**
 * Every string the SVG paints: the `label`/`sub` props the `Node` helper
 * renders, the text children of hand-rolled `<text>` elements, and the string
 * literals in each diagram's data array — those reach `<text>` through `{}`,
 * so they are drawn just the same. The literal sweep also picks up class names
 * and path data, which is harmless: those are ASCII by construction, and a
 * check that reads too much is the safe direction here.
 */
function paintedStrings(source: string): string[] {
  const found: string[] = [];
  for (const m of source.matchAll(/\b(?:label|sub)="([^"]*)"/g)) found.push(m[1]);
  for (const m of source.matchAll(/<text\b[^>]*>([\s\S]*?)<\/text>/g)) {
    const inner = m[1].trim();
    if (!inner.startsWith("{")) found.push(inner);
  }
  for (const m of source.matchAll(/"([^"\n]{2,})"/g)) found.push(m[1]);
  // JSX collapses runs of whitespace, newlines included, to a single space —
  // so a wrapped `<text>` child paints no newline and must not be reported as
  // one. Normalise to what actually reaches the glyph run.
  return found.map((s) => s.replace(/\s+/g, " ").trim());
}

test("every glyph a diagram paints is in the self-hosted font subset", () => {
  const offenders: string[] = [];
  for (const s of paintedStrings(paintedSource())) {
    for (const ch of s) {
      const cp = ch.codePointAt(0)!;
      if (cp >= 0x20 && cp <= 0x7e) continue;
      if (ALLOWED_NON_ASCII.has(ch)) continue;
      offenders.push(
        `U+${cp.toString(16).toUpperCase().padStart(4, "0")} ${JSON.stringify(ch)} in ${JSON.stringify(s)}`,
      );
    }
  }
  assert.deepEqual(
    offenders,
    [],
    `diagram text uses ${offenders.length} character(s) outside the JetBrains Mono latin subset.\n` +
      `Each one silently falls back to another font mid-string.\n  ` +
      offenders.join("\n  "),
  );
});

test("no diagram smuggles a missing arrow in as an HTML entity", () => {
  // The character check above sees `&rarr;` as seven ASCII characters and waves
  // it through, but the browser paints U+2192 — the one glyph the subset does
  // not carry. The hand-drawn diagram this component replaced did exactly that.
  const entities = /&(?:rarr|larr|#8594|#8592|#x2192|#x2190);/gi;
  const hits = paintedSource().match(entities) ?? [];
  assert.deepEqual(hits, [], `arrow entities render as U+2192/U+2190, which the font subset lacks: ${hits.join(", ")}`);
});

test("the allowlist itself stays honest about the missing arrows", () => {
  // A future author reaching for → will look here first. Assert the two
  // characters that caused this test to exist are absent, so "just add it to
  // the allowlist" fails loudly instead of quietly restoring the bug.
  assert.ok(!ALLOWED_NON_ASCII.has("→"), "U+2192 is not in the font subset");
  assert.ok(!ALLOWED_NON_ASCII.has("←"), "U+2190 is not in the font subset");
});
