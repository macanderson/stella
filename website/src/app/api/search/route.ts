import { createFromSource } from "fumadocs-core/search/server";
import type { NextRequest } from "next/server";
import { source } from "@/lib/source";

/**
 * The Fumadocs search endpoint.
 *
 * `createFromSource` builds an in-memory Orama index over the published MDX and
 * exposes a `GET` that forwards `?query`, `?limit`, `?tag`, and `?locale`
 * straight through. Orama is a JS data structure, not a query language, so
 * there is no injection surface — a query string is tokenized, never evaluated,
 * and `tag` filters an `enum[]` field by equality. The index holds only content
 * that is already served as HTML at a public URL, so a hit leaks nothing a
 * crawler could not read anyway.
 *
 * What it did lack was any bound on what a caller may ask for. Three caps are
 * imposed below before the request reaches the handler:
 *
 * - `query` is truncated to 128 characters. Orama tokenizes the whole string
 *   and scores every token against the index; a megabyte of text in a query
 *   parameter is real CPU on a shared server, and no genuine docs search is
 *   longer than a sentence.
 * - `limit` is clamped to 1..50. The upstream default accepts any integer,
 *   including negatives (which reach Orama's slice logic) and values far larger
 *   than the index, both of which are pointless work.
 * - `tag` accepts at most 8 comma-separated values, each capped at 64
 *   characters, so a caller cannot force a 10,000-term filter.
 *
 * The caps are applied by rewriting the URL rather than by post-filtering,
 * because the upstream handler reads its options from the search params.
 */

const { GET: search } = createFromSource(source);

const MAX_QUERY_LENGTH = 128;
const MAX_LIMIT = 50;
const MAX_TAGS = 8;
const MAX_TAG_LENGTH = 64;

function clamp(url: URL) {
  const params = url.searchParams;

  const query = params.get("query");
  if (query !== null) params.set("query", query.slice(0, MAX_QUERY_LENGTH));

  const rawLimit = Number(params.get("limit"));
  if (params.has("limit")) {
    if (Number.isFinite(rawLimit)) {
      params.set("limit", String(Math.min(Math.max(Math.trunc(rawLimit), 1), MAX_LIMIT)));
    } else {
      // A non-numeric limit is not a request for a default, it is a broken
      // client — drop it rather than guessing what was meant.
      params.delete("limit");
    }
  }

  const tag = params.get("tag");
  if (tag !== null) {
    params.set(
      "tag",
      tag
        .split(",")
        .slice(0, MAX_TAGS)
        .map((t) => t.slice(0, MAX_TAG_LENGTH))
        .join(","),
    );
  }

  return url;
}

export async function GET(request: NextRequest) {
  const url = clamp(new URL(request.url));
  try {
    return await search(new Request(url, request));
  } catch (error) {
    // Same convention as /api/hit and /install.sh: never let a raw 500 with
    // no context reach the client — log it server-side and return an empty
    // result set rather than a page-breaking error.
    console.error(JSON.stringify({ event: "search_failed", error: String(error) }));
    return Response.json({ results: [] }, { status: 500 });
  }
}
