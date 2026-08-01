/**
 * llms-full.txt (https://llmstxt.org) — every docs page's raw MDX source in
 * one file, sidebar order, frontmatter stripped. Where /llms.txt is a link
 * index sized to skim, this is the whole corpus for a client that wants to
 * paste stella's documentation into a model's context in a single fetch.
 *
 * Reads source.mdx files directly (see rawMdxBody in @/lib/llms) rather than
 * rendering page.data.body, which is a compiled React component — dumping
 * its output would mean HTML, not the markdown a model actually wants.
 */
import { SITE_NAME, SITE_URL } from "@/lib/site";
import { listLlmsPages, rawMdxBody } from "@/lib/llms";

export const dynamic = "force-static";

export async function GET(): Promise<Response> {
  const pages = listLlmsPages();

  const parts: string[] = [
    `# ${SITE_NAME} — full documentation`,
    "",
    `Generated from ${SITE_URL}/docs. One section per page, in sidebar order.`,
    "",
  ];

  for (const page of pages) {
    const body = rawMdxBody(page.url);
    if (body === undefined) continue; // a React-route page (e.g. the landing page) has no MDX source
    parts.push(`# ${page.title}`, "", `Source: ${SITE_URL}${page.url}`, "", body, "", "---", "");
  }

  return new Response(parts.join("\n").trimEnd() + "\n", {
    headers: { "Content-Type": "text/markdown; charset=utf-8" },
  });
}
