/**
 * llms.txt (https://llmstxt.org) — a curated link index for AI crawlers and
 * answer engines, the same job robots.txt/sitemap.xml do for traditional
 * search, but sized for a model's context window instead of a spider's
 * crawl budget: titles and one-line descriptions, not full page bodies. The
 * full text lives at /llms-full.txt for a client that wants to read
 * everything in one request.
 */
import { SITE_NAME, SITE_URL } from "@/lib/site";
import { listLlmsPages } from "@/lib/llms";

export const dynamic = "force-static";

export async function GET(): Promise<Response> {
  const pages = listLlmsPages();

  const sections = new Map<string, typeof pages>();
  for (const page of pages) {
    const bucket = sections.get(page.section) ?? [];
    bucket.push(page);
    sections.set(page.section, bucket);
  }

  const lines: string[] = [
    `# ${SITE_NAME}`,
    "",
    "> stella is a fast, bring-your-own-key, model-agnostic terminal coding agent. Point it at any provider, give it a goal, and let a verifier decide when it's done.",
    "",
    `Full documentation, one page per file, is linked below. For the complete text in a single request, fetch ${SITE_URL}/llms-full.txt instead.`,
    "",
  ];

  for (const [section, entries] of sections) {
    lines.push(`## ${section}`, "");
    for (const entry of entries) {
      const desc = entry.description ? `: ${entry.description}` : "";
      lines.push(`- [${entry.title}](${SITE_URL}${entry.url})${desc}`);
    }
    lines.push("");
  }

  return new Response(lines.join("\n").trimEnd() + "\n", {
    headers: { "Content-Type": "text/markdown; charset=utf-8" },
  });
}
