import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { parseChangelog } from "./changelog.ts";

/**
 * Pins that /releases actually renders a real 0.8.0 card from the repo's own
 * CHANGELOG.md — not the fixture string the other tests in changelog.test.ts
 * use. That fixture proves the parser's grammar; this proves the ACTUAL file
 * this release shipped still matches it, since a hand-written section is
 * exactly the case scripts/changelog-roll.sh warns can drift from what the
 * drafter would have produced (see CHANGELOG.md's own contributor note).
 *
 *   pnpm test          (from website/)
 */

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

function realReleases() {
  const md = readFileSync(join(REPO_ROOT, "CHANGELOG.md"), "utf8");
  return parseChangelog(md);
}

test("CHANGELOG.md's [0.8.0] section parses into a non-empty release", () => {
  const releases = realReleases();
  const release = releases.find((r) => r.version === "0.8.0");

  assert.ok(release, "no [0.8.0] release found by the parser — heading shape or /releases will be missing it");
  assert.ok(
    release.entries.length > 0,
    "[0.8.0] parsed but has zero entries — it would render as an empty card, which the roll script refuses to emit",
  );
  assert.ok(
    release.summary.trim().length > 0,
    "[0.8.0] has no lead paragraph",
  );

  // Every entry must land in a real Keep-a-Changelog kind, never silently
  // dropped by an unrecognised ### heading (e.g. a stray "No changes" bullet
  // sitting under a heading the parser does not treat as content).
  for (const entry of release.entries) {
    assert.ok(
      ["Added", "Changed", "Fixed", "Removed", "Security"].includes(entry.kind),
      `entry has an unexpected kind: ${entry.kind}`,
    );
  }
});
