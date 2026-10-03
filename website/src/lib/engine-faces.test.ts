import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

/**
 * The engine tour sets its text in the text face.
 *
 *   pnpm test
 *
 * By the house type rule, every surface sets its text in Aeonik
 * (`--font-sans`) and keeps Monaspace Neon (`--font-mono`) for code. This
 * test holds the tour's root to the text face, and holds the code face to the
 * one grouped rule in engine.css that names the elements reading as code or
 * a terminal.
 */

const ENGINE = join(dirname(fileURLToPath(import.meta.url)), "..", "app", "engine");

/** Each rule as [selector, body], with comments removed. */
function rules(file: string): [string, string][] {
  const bare = readFileSync(join(ENGINE, file), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
  return [...bare.matchAll(/([^{}]*)\{([^{}]*)\}/g)].map((m): [string, string] => [
    m[1].slice(m[1].lastIndexOf(";") + 1).replace(/\s+/g, " ").trim(),
    m[2],
  ]);
}

const faceOf = (body: string): string | undefined =>
  /(?:^|;)\s*font-family\s*:\s*([^;]+)/.exec(body)?.[1].trim();

test("the tour's root sets its text in the text face", () => {
  const root = rules("engine.css").find(([selector]) => selector === ".eng-tour");
  assert.ok(root, "engine.css has no .eng-tour rule.");
  assert.equal(
    faceOf(root[1]),
    "var(--font-sans)",
    "engine.css sets .eng-tour's text in another face. The tour's text reads var(--font-sans), and only " +
      "code and terminal text takes var(--font-mono).",
  );
});

test("one rule in the tour's sheets sets the code face", () => {
  const mono = ["engine.css", "engine-stations.css"].flatMap((file) =>
    rules(file)
      .filter(([, body]) => faceOf(body) === "var(--font-mono)")
      .map(([selector]) => `${file}: ${selector}`),
  );
  assert.equal(
    mono.length,
    1,
    `The code face is set in ${mono.length} rules: ${mono.join("; ")}. Name a code or terminal element in ` +
      "the grouped rule in engine.css, so the list of what reads as code stays in one place.",
  );
});
