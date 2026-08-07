/**
 * Parse CHANGELOG.md into the shape /releases renders. **Pure — no I/O.**
 *
 * Reading the file lives in `changelog.server.ts`; this module is imported by
 * the client component too (for `CHANGE_KINDS` and the types), and a single
 * `node:fs` import anywhere in that graph fails the build with "the chunking
 * context does not support external modules". Which is the bundler enforcing
 * the same split the Rust side of this repository states as invariant #2:
 * decision logic is pure functions over owned data, and anything that touches
 * the filesystem is a separate layer. Keeping the parser pure is also what
 * lets it be tested against a fixture string with no tmpdir.
 *
 * ## What it expects
 *
 * The subset of Keep a Changelog that `scripts/changelog-roll.sh` actually
 * produces, and nothing more:
 *
 *   ## [0.7.0] — 2026-08-07          <- a release, optionally a date or range
 *   <optional lead paragraph>
 *   ### Added                        <- a kind
 *   - **Bold lead claim.** Prose.    <- an entry
 *
 * `## [Unreleased]` and any non-version `##` heading (the "How this file works"
 * preamble, the "Earlier releases" footer) are skipped: they are instructions
 * to contributors, not release notes, and rendering them on a marketing page
 * would be noise. A release with no entries cannot occur any more — the roll
 * refuses to emit one — but it is dropped here too rather than trusted, because
 * this parser outlives any one version of that script.
 *
 * ## Why a hand-rolled parser and not a markdown library
 *
 * The whole grammar is four line shapes, all anchored at column zero, and the
 * output feeds a bespoke component tree rather than an HTML string. A markdown
 * AST would be a dependency and a traversal to arrive at the same four cases.
 * Inline markdown *within* an entry is left as text here and rendered by
 * `<InlineMarkdown>` at the leaf, which is where it can become JSX instead of
 * an HTML string — so the page never needs `dangerouslySetInnerHTML`.
 */

/**
 * The Keep a Changelog kinds, in the order the drafter is instructed to emit
 * them — which is also the order they read best: what you gained, what moved
 * under you, what stopped hurting, what went away, what was dangerous.
 *
 * Declared as a const tuple so `ChangeKind` is the union of exactly these and
 * the filter UI can iterate the same list it validates against.
 */
export const CHANGE_KINDS = [
  "Added",
  "Changed",
  "Fixed",
  "Removed",
  "Security",
] as const;

export type ChangeKind = (typeof CHANGE_KINDS)[number];

/** One bullet: a bold lead claim (optional) and the prose after it. */
export interface ChangeEntry {
  kind: ChangeKind;
  /** The `**bold**` opener, without its asterisks. Empty when the bullet has none. */
  lead: string;
  /** Everything after the lead, as raw markdown-ish text. */
  body: string;
  /** Lowercased `lead + body`, precomputed so search does no work per keystroke. */
  haystack: string;
  /** PR numbers cited as `(#1234)`, in order of appearance, deduplicated. */
  refs: string[];
}

/** One `## [x.y.z]` section. */
export interface Release {
  /** The bare version, e.g. `0.7.0`. */
  version: string;
  /** The `— …` tail of the heading: a date, or a `start → end` span. */
  date: string;
  /** The prose between the heading and the first `###`, if any. */
  summary: string;
  entries: ChangeEntry[];
  /** Entry counts by kind, for the per-release meter. Zero-count kinds omitted. */
  counts: Partial<Record<ChangeKind, number>>;
}

const KIND_SET = new Set<string>(CHANGE_KINDS);

/**
 * `## [0.7.0] — 2026-08-07` / `## [0.6.0] — 2026-07-29 → 2026-08-06`.
 * The date tail is optional so a section is still parsed if the roll ever
 * writes one without a date; better a release with a blank date than a release
 * silently missing from the page.
 */
const RELEASE_HEADING = /^## \[(\d+\.\d+\.\d+)\](?:\s*[—-]\s*(.+))?$/;
const KIND_HEADING = /^### (\w+)\s*$/;
const BULLET = /^- (.*)$/;
const LEAD = /^\*\*(.+?)\*\*\s*/;

/**
 * A PR or issue citation, matched anywhere in the bullet rather than only as a
 * trailing `(#1234)`. Entries routinely cite several at once — `(#1285, #1842)`,
 * `(#1870, #1871, #1876)`, `(#335, B1)` — and an anchored `\(#(\d+)\)` silently
 * matched NONE of those, which is exactly the kind of quiet under-reporting a
 * page like this must not do.
 */
const PR_REF = /#(\d+)\b/g;

/**
 * Collapse a bullet's continuation lines into one paragraph.
 *
 * CHANGELOG.md is hard-wrapped at 80 columns, so nearly every bullet spans
 * several source lines. Joining with a space is right for prose; the drafter is
 * instructed to emit prose, and a bullet containing a code block would be
 * outside the grammar this page renders anyway.
 */
function joinWrapped(lines: string[]): string {
  return lines
    .map((line) => line.trim())
    .filter(Boolean)
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
}

function toEntry(kind: ChangeKind, raw: string): ChangeEntry {
  const leadMatch = raw.match(LEAD);
  const lead = leadMatch ? leadMatch[1].trim() : "";
  const body = leadMatch ? raw.slice(leadMatch[0].length).trim() : raw.trim();

  const refs = [...raw.matchAll(PR_REF)].map((m) => m[1]);

  return {
    kind,
    lead,
    body,
    haystack: `${lead} ${body}`.toLowerCase(),
    refs: [...new Set(refs)],
  };
}

/** Parse changelog markdown into releases, newest first (the file's own order). */
export function parseChangelog(markdown: string): Release[] {
  const releases: Release[] = [];

  let release: Release | null = null;
  let kind: ChangeKind | null = null;
  let bullet: string[] | null = null;
  let summary: string[] = [];

  /** Close the bullet under construction, if any. */
  const flushBullet = () => {
    if (release && kind && bullet) {
      const text = joinWrapped(bullet);
      if (text) release.entries.push(toEntry(kind, text));
    }
    bullet = null;
  };

  /** Close the release under construction, dropping it if it has no entries. */
  const flushRelease = () => {
    flushBullet();
    if (release) {
      release.summary = joinWrapped(summary);
      if (release.entries.length > 0) {
        for (const entry of release.entries) {
          release.counts[entry.kind] = (release.counts[entry.kind] ?? 0) + 1;
        }
        releases.push(release);
      }
    }
    release = null;
    kind = null;
    summary = [];
  };

  for (const line of markdown.split("\n")) {
    const heading = line.match(RELEASE_HEADING);
    if (heading) {
      flushRelease();
      release = {
        version: heading[1],
        date: (heading[2] ?? "").trim(),
        summary: "",
        entries: [],
        counts: {},
      };
      continue;
    }

    // Any other `## ` ends the current release: `[Unreleased]`, the preamble,
    // the "Earlier releases" footer. None of them are release notes.
    if (line.startsWith("## ")) {
      flushRelease();
      continue;
    }

    if (!release) continue;

    const kindHeading = line.match(KIND_HEADING);
    if (kindHeading && KIND_SET.has(kindHeading[1])) {
      flushBullet();
      kind = kindHeading[1] as ChangeKind;
      continue;
    }

    const bulletStart = line.match(BULLET);
    if (bulletStart) {
      flushBullet();
      bullet = [bulletStart[1]];
      continue;
    }

    if (bullet) {
      // A blank line ends a bullet; an indented line continues it. A bullet's
      // own body can contain blank-line-separated paragraphs in this file, so
      // an indented line after a blank one re-opens the same bullet rather than
      // starting a new one — hence the check on `line.startsWith(" ")` instead
      // of treating every blank line as terminal.
      if (line.trim() === "") continue;
      if (line.startsWith("  ")) {
        bullet.push(line);
        continue;
      }
      flushBullet();
    }

    // Prose before the first `###` is the release's lead paragraph.
    if (!kind && line.trim()) summary.push(line);
  }

  flushRelease();
  return releases;
}
