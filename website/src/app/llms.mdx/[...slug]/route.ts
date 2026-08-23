/**
 * `/llms.mdx/<slug>` — one docs page as a markdown file, the endpoint behind
 * the "Copy page" menu's *View as Markdown* / *Download as Markdown* items.
 * The dotted folder name follows the same convention as `/llms.txt` and
 * `/llms-full.txt`: a machine-readable sibling of the page tree, not part of
 * the human navigation.
 *
 * The body is the page's own `_markdown` export (see `page-markdown.ts` for
 * why that and not the raw `.mdx` on disk), so this route and the menu's
 * *Copy page* item can never disagree about what the page's markdown is.
 */
import { notFound } from "next/navigation";
import { source } from "@/lib/source";
import { pageMarkdown } from "@/lib/page-markdown";
import { SITE_URL } from "@/lib/site";

export const dynamic = "force-static";

interface RouteParams {
  params: Promise<{ slug: string[] }>;
}

export async function GET(_request: Request, { params }: RouteParams): Promise<Response> {
  const { slug } = await params;
  // `/llms.mdx/index` is the docs root — see generateStaticParams.
  const page = source.getPage(slug.length === 1 && slug[0] === "index" ? [] : slug);
  if (!page) notFound();

  const markdown = page.data._exports._markdown;
  if (typeof markdown !== "string") notFound(); // a page without MDX source has no markdown form

  const body = await pageMarkdown({
    title: page.data.title,
    description: page.data.description,
    url: `${SITE_URL}${page.url}`,
    markdown,
  });

  return new Response(body, {
    headers: { "Content-Type": "text/markdown; charset=utf-8" },
  });
}

export function generateStaticParams(): { slug: string[] }[] {
  // The docs root's slugs are `[]`, which Next would prerender as `/llms.mdx`
  // itself and then reject as an export-path mismatch — so the root page's
  // markdown lives at `/llms.mdx/index`, and the menu links there.
  return (source.generateParams() as { slug: string[] }[]).map(({ slug }) => ({
    slug: slug.length === 0 ? ["index"] : slug,
  }));
}
