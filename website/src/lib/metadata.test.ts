import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

/**
 * Guards the title-template trap in `src/app/layout.tsx`.
 *
 *   pnpm test
 *
 * ## The trap
 *
 * The root layout declares a title template for the docs tree it mostly serves:
 *
 *     title: { default: "stella — docs", template: "%s — stella docs" }
 *
 * A template applies to every descendant route that sets a **plain string**
 * title. So a page outside `/docs` that writes `title: "stella — releases"`
 * silently ships as `stella — releases — stella docs`, and nothing anywhere
 * fails. Opting out means `title: { absolute: … }`.
 *
 * This has now bitten twice — the landing page shipped at 71 characters with
 * the proposition in the part a search result truncates, and /releases shipped
 * saying "docs" twice about a page that is not documentation. Both also
 * disagreed with their own `openGraph.title` and `twitter.title`, which are NOT
 * template-expanded: the tab said one thing and every shared card another.
 *
 * A third occurrence is a matter of someone adding a route, so this asserts the
 * rule instead of trusting it. The check is static — it reads the route files
 * rather than importing them, because importing a page pulls in React, the
 * component tree, and fumadocs to inspect one exported object.
 */

const APP_DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "app");

/** Every `page.tsx` under `src/app`, as repo-relative paths. */
function pageFiles(dir: string, found: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) pageFiles(path, found);
    else if (entry.name === "page.tsx") found.push(path);
  }
  return found;
}

test("every non-docs page opts out of the docs title template", () => {
  const offenders: string[] = [];
  let inspected = 0;

  for (const file of pageFiles(APP_DIR)) {
    // Match on the path RELATIVE to src/app, never the absolute one: a checkout
    // that happens to live under any directory called `docs` would otherwise
    // skip every page and report green — a guard silently covering less than it
    // claims, which is the failure mode this repo has already been bitten by
    // twice in check-file-size.sh.
    const route = relative(APP_DIR, file);

    // The docs tree is what the template is FOR — `/docs/[[...slug]]` should
    // inherit it, and does, by building its title from page frontmatter.
    if (route === "docs/page.tsx" || route.startsWith("docs/")) continue;

    const source = readFileSync(file, "utf8");

    // Only pages that declare metadata at all are in scope; a page with no
    // `metadata` export inherits the layout's `default`, which is a separate
    // (and correct) mechanism.
    if (!/export const metadata/.test(source)) continue;

    // Find the `title:` inside the metadata object. `{ absolute: …}` opts out;
    // a bare string or an identifier does not.
    const title = source.match(/export const metadata[\s\S]*?\btitle:\s*([^\n]+)/);
    if (!title) continue;

    inspected += 1;
    if (!/\{\s*absolute:/.test(title[1])) {
      offenders.push(`${route} -> title: ${title[1].trim()}`);
    }
  }

  assert.deepEqual(
    offenders,
    [],
    "these routes sit outside /docs but take a plain-string title, so the root " +
      'layout appends " — stella docs" to them. Use `title: { absolute: … }`.',
  );

  // A rule that matched nothing is indistinguishable from a rule that passed,
  // and this one has to survive a route being renamed or moved. Assert it
  // actually looked at something.
  assert.ok(
    inspected > 0,
    "no non-docs page with a metadata export was found — the walk or the skip is wrong, not the site",
  );
});

test("the root layout still declares the template this guard is about", () => {
  // If the template is ever removed, the rule above becomes noise rather than
  // a guard — and a test that passes for the wrong reason is worse than none.
  // This fails loudly so the pair is re-thought together.
  const layout = readFileSync(join(APP_DIR, "layout.tsx"), "utf8");
  assert.match(
    layout,
    /template:\s*"%s — stella docs"/,
    "the root title template changed; re-check whether the absolute-title rule above still applies",
  );
});
