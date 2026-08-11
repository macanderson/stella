import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { parseChangelog } from "./changelog.ts";

/**
 * Pins that /releases renders a real 0.9.0 card from the repo's own
 * CHANGELOG.md, and that it is the card the page leads with.
 *
 * The sibling `changelog.0-8-0.test.ts` proves the same thing for the section
 * that shipped before it; this file carries one assertion that one cannot,
 * because it is about the file's ORDER rather than a section's contents.
 * `/releases` takes `releases[0]` as the current line (see
 * `src/app/releases/page.tsx`), so a section appended to the bottom of
 * CHANGELOG.md — the natural mistake when writing a release section by hand
 * rather than letting `scripts/changelog-roll.sh` insert it under
 * `[Unreleased]` — parses perfectly and still renders the page as though the
 * previous line were current.
 *
 *   pnpm test          (from website/)
 */

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

function realReleases() {
  const md = readFileSync(join(REPO_ROOT, "CHANGELOG.md"), "utf8");
  return parseChangelog(md);
}

test("CHANGELOG.md's [0.9.0] section parses into a non-empty release", () => {
  const releases = realReleases();
  const release = releases.find((r) => r.version === "0.9.0");

  assert.ok(release, "no [0.9.0] release found by the parser — heading shape or /releases will be missing it");
  assert.ok(
    release.entries.length > 0,
    "[0.9.0] parsed but has zero entries — it would render as an empty card, which the roll script refuses to emit",
  );
  assert.ok(
    release.summary.trim().length > 0,
    "[0.9.0] has no lead paragraph",
  );

  for (const entry of release.entries) {
    assert.ok(
      ["Added", "Changed", "Fixed", "Removed", "Security"].includes(entry.kind),
      `entry has an unexpected kind: ${entry.kind}`,
    );
  }
});

test("[0.9.0] is the newest section, so /releases leads with it", () => {
  const releases = realReleases();

  assert.equal(
    releases[0]?.version,
    "0.9.0",
    "the newest section is not [0.9.0] — /releases takes releases[0] as the current line",
  );
});
