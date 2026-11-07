import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

/**
 * Every button on the site reads the kit's --button-* roles.
 *
 *   pnpm test
 *
 * Mac's rule of 2026-10-03: buttons, corners, and colours come from semantic
 * tokens. `src/app/global.css` maps the kit's --button-* names onto this
 * site's roles, once for paper (`:root`) and once for ink (`.dark`), and the
 * `.btn` classes read them. This test holds both halves. A
 * button whose colour is written in its own rule, or a theme that leaves a
 * role undeclared, fails here.
 */

const CSS = join(dirname(fileURLToPath(import.meta.url)), "..", "app", "global.css");

/** The roles the .btn classes read, with the kit's names. */
const ROLES = [
  "--button-primary-bg",
  "--button-primary-fg",
  "--button-primary-border",
  "--button-primary-hover-bg",
  "--button-primary-active-bg",
  "--button-primary-ring",
  "--button-default-bg",
  "--button-default-fg",
  "--button-default-border",
  "--button-default-hover-bg",
  "--button-default-active-bg",
  "--button-default-ring",
  "--button-disabled-bg",
  "--button-disabled-fg",
  "--button-link-fg",
];

/** Each top-level rule as [selector, body], with comments removed. */
function rules(css: string): [string, string][] {
  const bare = css.replace(/\/\*[\s\S]*?\*\//g, "");
  return [...bare.matchAll(/([^{}]*)\{([^{}]*)\}/g)].map((m): [string, string] => [
    m[1].slice(m[1].lastIndexOf(";") + 1).replace(/\s+/g, " ").trim(),
    m[2],
  ]);
}

test("both themes declare every button role", () => {
  const all = rules(readFileSync(CSS, "utf8"));
  for (const theme of [":root", ".dark"]) {
    const declared = new Set(
      all
        .filter(([selector]) => selector === theme)
        .flatMap(([, body]) => [...body.matchAll(/(--button-[\w-]+)\s*:/g)].map((m) => m[1])),
    );
    const missing = ROLES.filter((role) => !declared.has(role));
    assert.deepEqual(
      missing,
      [],
      `src/app/global.css's ${theme} block declares no ${missing.join(", ")}. Map each role onto a ` +
        "site role token there, so the .btn classes have a value in that theme.",
    );
  }
});

test("the .btn classes read their colours from the button roles and their padding from the space unit", () => {
  const btn = rules(readFileSync(CSS, "utf8")).filter(([selector]) =>
    selector.split(",").some((s) => /^\.btn(?:-[\w-]+)?(?::[\w-]+|\[[^\]]+\])*$/.test(s.trim())),
  );
  assert.ok(btn.length > 0, "src/app/global.css has no .btn rules.");
  for (const [selector, body] of btn) {
    for (const m of body.matchAll(/(?:^|;)\s*([a-z-]+)\s*:\s*([^;]+)/g)) {
      const [prop, value] = [m[1], m[2].trim()];
      if (/^(?:background|background-color|color|border-color|outline-color)$/.test(prop)) {
        assert.match(
          value,
          /^(?:var\(--button-[\w-]+\)|transparent|none)$/,
          `${selector} sets ${prop}: ${value}. A button reads its colour from a --button-* role.`,
        );
      }
      if (prop === "outline") {
        assert.match(value, /var\(--button-[\w-]+\)/, `${selector} draws its focus ring outside the button roles.`);
      }
      if (prop === "padding") {
        assert.ok(
          value === "0" || value.split(/\s+(?![^(]*\))/).every((part) => part === "0" || part.includes("var(--ox-space)")),
          `${selector} sets padding: ${value}. A button's padding reads the space unit.`,
        );
      }
    }
  }
});
