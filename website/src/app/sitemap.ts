/**
 * The sitemap for stella.oxagen.sh.
 *
 * `lastModified` is the git commit date of the page's MDX source, stamped onto
 * `page.data.lastModified` by the `last-modified` plugin configured in
 * source.config.ts — NOT the build timestamp. Emitting `new Date()` told
 * crawlers that every URL changed on every deploy, which is the exact signal
 * that makes a search engine stop trusting lastModified and fall back to its
 * own heuristics.
 *
 * When the date is unknown (git history unavailable — e.g. a shallow clone) the
 * field is omitted rather than guessed: Next drops undefined values from the
 * XML, and "no claim" is a better answer to a crawler than a false one.
 */
import type { MetadataRoute } from "next";
import { source } from "@/lib/source";
import { getReleases } from "@/lib/changelog.server";
import { SITE_URL } from "@/lib/site";

const DAY_MS = 24 * 60 * 60 * 1000;

/**
 * The date of the newest release section, for /releases' `lastModified`.
 *
 * A changelog date is either `YYYY-MM-DD` or a `start → end` span; the last
 * 10-character date in the string is the one that says when the line stopped
 * moving. Returns undefined if the heading carries no parseable date, so the
 * field is omitted rather than guessed — the same "no claim beats a false
 * claim" rule the docs rows follow.
 */
function releasesLastModified(): Date | undefined {
  const [latest] = getReleases();
  const dates = latest?.date.match(/\d{4}-\d{2}-\d{2}/g);
  if (!dates?.length) return undefined;
  const parsed = new Date(`${dates[dates.length - 1]}T00:00:00Z`);
  return Number.isNaN(parsed.getTime()) ? undefined : parsed;
}

/**
 * The `last-modified` plugin is registered in source.config.ts only when the
 * checkout has deep-enough git history to produce real per-file dates (see the
 * shallow-clone note there). That makes `lastModified` present in the generated
 * page data on deep clones and absent on shallow ones — so the generated type
 * does NOT include the field in every environment (Vercel's default checkout is
 * shallow). Read it as an optional field so the type checks regardless of how
 * the plugin was configured; at runtime it is simply `undefined` on a shallow
 * clone, which is exactly the "omit the date rather than lie" behavior above.
 */
type WithLastModified = { lastModified?: Date };

/**
 * Derive changeFrequency from how recently a page actually changed, instead of
 * declaring a flat "weekly" for a fast-moving release-notes page and a settled
 * ADR-derived principles page alike.
 */
function changeFrequency(
  lastModified: Date | undefined,
): "weekly" | "monthly" | "yearly" {
  if (!lastModified) return "monthly";
  const ageDays = (Date.now() - lastModified.getTime()) / DAY_MS;
  if (ageDays < 30) return "weekly";
  if (ageDays < 180) return "monthly";
  return "yearly";
}

export default function sitemap(): MetadataRoute.Sitemap {
  const pages = source.getPages();

  const docs = pages.map((page) => {
    const lastModified = (page.data as WithLastModified).lastModified;
    return {
      url: `${SITE_URL}${page.url}`,
      lastModified,
      changeFrequency: changeFrequency(lastModified),
      priority: 0.7,
    };
  });

  // The landing page is a React route, not MDX, so no per-file git date reaches
  // it here. It exists to surface the docs, so the newest page date is a
  // defensible stand-in — and still a real date rather than the build clock.
  const homeLastModified = pages
    .map((page) => (page.data as WithLastModified).lastModified)
    .filter((date): date is Date => date instanceof Date)
    .reduce<Date | undefined>(
      (newest, date) => (!newest || date > newest ? date : newest),
      undefined,
    );

  return [
    {
      url: SITE_URL,
      lastModified: homeLastModified,
      changeFrequency: changeFrequency(homeLastModified),
      priority: 1,
    },
    {
      // /releases is a React route rendered from CHANGELOG.md, so no per-file
      // MDX date reaches this map. Its content changes on exactly the cadence
      // the newest release does, and the newest release's date is parsed
      // rather than guessed — a real date, like every other row here.
      url: `${SITE_URL}/releases`,
      lastModified: releasesLastModified(),
      changeFrequency: "weekly" as const,
      priority: 0.8,
    },
    ...docs,
  ];
}
