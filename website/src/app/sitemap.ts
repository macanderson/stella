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

const SITE_URL = "https://stella.oxagen.sh";

const DAY_MS = 24 * 60 * 60 * 1000;

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
    const lastModified = page.data.lastModified;
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
    .map((page) => page.data.lastModified)
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
    ...docs,
  ];
}
