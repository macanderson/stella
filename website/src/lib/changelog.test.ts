import assert from "node:assert/strict";
import { test } from "node:test";
import { parseChangelog } from "./changelog.ts";

/**
 * Tests for the CHANGELOG.md parser behind /releases.
 *
 *   pnpm test          (from website/)
 *
 * Node's built-in runner, with its native TypeScript stripping — the parser is
 * a pure function over a string, so a test framework would be a dependency
 * bought to call one function.
 *
 * ## Why this file exists
 *
 * The parser's input is written by a model in CI (`scripts/changelog-ai.sh`),
 * and every way it can go wrong is SILENT: a heading shape it stops matching
 * makes a release vanish from the page, and a bullet shape it stops matching
 * makes entries vanish from a release. Both render as a page that looks
 * completely fine and is missing things. Nothing else in the build fails.
 *
 * So the cases below are the shapes the drafter is instructed to emit, plus
 * the three non-release headings that live in the same file and must never be
 * rendered as releases.
 */

const SAMPLE = `# Changelog

Preamble prose that is not a release.

## How this file works

Instructions to contributors. Not release notes.

## [Unreleased]

## [0.7.0] — 2026-08-07

### Changed

- **A bold lead claim.** One sentence of prose that wraps
  across two source lines (#1234).

### Added

- A bullet with no bold lead at all.

## [0.6.0] — 2026-07-29 → 2026-08-06

A lead paragraph describing the line.

### Fixed

- **Something broke.** It does not any more (#100, #200).
- **A second fix.** With \`code\` and a [link](https://example.com).

### Removed

- **A thing is gone.** And is not coming back.

## Earlier releases

0.4.0 and earlier predate this file.
`;

test("parses every release heading, and only release headings", () => {
  const releases = parseChangelog(SAMPLE);
  assert.deepEqual(
    releases.map((r) => r.version),
    ["0.7.0", "0.6.0"],
    "Unreleased, the preamble, 'How this file works' and 'Earlier releases' must not appear",
  );
});

test("keeps the date tail verbatim, including a span", () => {
  const [latest, previous] = parseChangelog(SAMPLE);
  assert.equal(latest.date, "2026-08-07");
  assert.equal(previous.date, "2026-07-29 → 2026-08-06");
});

test("drops a release with no entries rather than rendering an empty card", () => {
  // The roll cannot produce this any more (scripts/changelog-roll.sh refuses),
  // but the page must not depend on that: this file outlives that script.
  const releases = parseChangelog("## [Unreleased]\n\n## [9.9.9] — 2026-01-01\n");
  assert.deepEqual(releases, []);
});

test("splits a bold lead from its body, and joins wrapped lines", () => {
  const [latest] = parseChangelog(SAMPLE);
  const entry = latest.entries[0];
  assert.equal(entry.kind, "Changed");
  assert.equal(entry.lead, "A bold lead claim.");
  assert.equal(
    entry.body,
    "One sentence of prose that wraps across two source lines (#1234).",
    "the 80-column wrap must collapse to one paragraph, with single spaces",
  );
});

test("handles a bullet with no bold lead", () => {
  const [latest] = parseChangelog(SAMPLE);
  const entry = latest.entries[1];
  assert.equal(entry.kind, "Added");
  assert.equal(entry.lead, "");
  assert.equal(entry.body, "A bullet with no bold lead at all.");
});

test("extracts every PR reference, deduplicated", () => {
  const [, previous] = parseChangelog(SAMPLE);
  assert.deepEqual(previous.entries[0].refs, ["100", "200"]);
  assert.deepEqual(previous.entries[1].refs, []);
});

test("counts entries per kind, omitting kinds with none", () => {
  const [, previous] = parseChangelog(SAMPLE);
  assert.deepEqual(previous.counts, { Fixed: 2, Removed: 1 });
});

test("captures the lead paragraph before the first kind heading", () => {
  const [latest, previous] = parseChangelog(SAMPLE);
  assert.equal(previous.summary, "A lead paragraph describing the line.");
  assert.equal(latest.summary, "", "a release with no lead paragraph gets an empty one");
});

test("lowercases the haystack so search does no work per keystroke", () => {
  const [latest] = parseChangelog(SAMPLE);
  assert.ok(latest.entries[0].haystack.includes("a bold lead claim"));
  assert.equal(
    latest.entries[0].haystack,
    latest.entries[0].haystack.toLowerCase(),
  );
});

test("ignores an unknown ### heading rather than inventing a kind", () => {
  const releases = parseChangelog(
    "## [1.0.0] — 2026-01-01\n\n### Deprecated\n\n- orphaned\n\n### Fixed\n\n- **Real.** Kept.\n",
  );
  assert.deepEqual(
    releases[0].entries.map((e) => e.kind),
    ["Fixed"],
    "a bullet under a heading that is not a Keep-a-Changelog kind is dropped, not miscategorised",
  );
});
