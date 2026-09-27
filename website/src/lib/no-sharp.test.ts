import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

/**
 * Keeps `sharp` out of the docs site.
 *
 *   pnpm test
 *
 * `next` lists sharp as an optional dependency for every platform. Fourteen
 * of its `@img/sharp-*` packages ship libvips under LGPL-3.0, a license the
 * `dependency-review` workflow does not allow. That action exempts a package
 * only by its exact name. It has no wildcard. So every exempt package had to
 * be named by hand, and each new platform package turned the check red on
 * whatever pull request brought it in.
 *
 * The site never loads sharp. `next.config.mjs` sets
 * `images: { unoptimized: true }`, so Next's image optimizer, the code that
 * loads sharp, never runs. The pnpm override `sharp: "-"` in
 * `pnpm-workspace.yaml` drops sharp from the tree, and the exemption list
 * went with it.
 *
 * The three tests below hold those facts in place. If the site ever turns
 * image optimization on, restore a version floor for sharp in
 * `pnpm-workspace.yaml`, name each LGPL package in
 * `.github/workflows/dependency-review.yml`, and delete this file.
 */

const SITE = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

function read(name: string): string {
  return readFileSync(join(SITE, name), "utf8");
}

/** The indented lines under a top-level YAML key, up to the next one. */
function block(yaml: string, key: string): string[] {
  const lines = yaml.split("\n");
  const start = lines.findIndex((line) => line === `${key}:`);
  if (start < 0) return [];
  const body: string[] = [];
  for (const line of lines.slice(start + 1)) {
    if (/^\S/.test(line)) break;
    body.push(line);
  }
  return body;
}

test("the lockfile holds no sharp package", () => {
  // A package entry sits two spaces in, as `name@version:`. The override
  // line `sharp: '-'` has no `@version`, so it does not match.
  const entry = /^ {2}'?((?:@img\/sharp-[^@']+)|sharp)@/;
  const found = read("pnpm-lock.yaml")
    .split("\n")
    .flatMap((line) => {
      const hit = entry.exec(line);
      return hit ? [hit[1]] : [];
    });
  assert.deepEqual(
    [...new Set(found)],
    [],
    "pnpm-lock.yaml holds sharp again. dependency-review cannot exempt its " +
      "LGPL libvips packages by pattern, so each one fails the check. Keep " +
      'the `sharp: "-"` override in pnpm-workspace.yaml and run pnpm install.',
  );
});

test("the pnpm override drops sharp", () => {
  const overrides = block(read("pnpm-workspace.yaml"), "overrides");
  assert.ok(
    overrides.some((line) => /^\s+sharp:\s*["']-["']\s*$/.test(line)),
    'pnpm-workspace.yaml has lost its `sharp: "-"` override. Without it, ' +
      "the next pnpm install puts every @img/sharp-* package back in the " +
      "lockfile.",
  );
});

test("the site does not optimize images", () => {
  assert.match(
    read("next.config.mjs"),
    /images:\s*\{[^}]*\bunoptimized:\s*true\b/,
    "next.config.mjs does not set `images: { unoptimized: true }`. Next's " +
      "image optimizer loads sharp, and this site removes sharp. Restore a " +
      "sharp floor in pnpm-workspace.yaml, name its LGPL packages in " +
      ".github/workflows/dependency-review.yml, and delete this test.",
  );
});
