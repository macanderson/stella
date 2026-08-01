import type { MetadataRoute } from "next";
import { SITE_URL } from "@/lib/site";

/**
 * Everything on this site is meant to be crawled except `/api/search`, which is
 * a JSON endpoint with no reader-facing content: indexing it wastes the
 * crawler's budget and spends server CPU rebuilding search results nobody
 * asked for. `Disallow` here is a crawl-budget instruction, not a security
 * boundary — the route is public either way, and it is rate-shaped in
 * src/app/api/search/route.ts.
 */
export default function robots(): MetadataRoute.Robots {
  return {
    rules: {
      userAgent: "*",
      allow: "/",
      disallow: "/api/",
    },
    sitemap: `${SITE_URL}/sitemap.xml`,
  };
}
