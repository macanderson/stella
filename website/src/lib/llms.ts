/**
 * Shared page listing for llms.txt / llms-full.txt (the emerging convention
 * at llmstxt.org for what an AI crawler or answer engine should read first).
 * Walks the same page tree the sidebar renders from, in the same order,
 * grouped by the meta.json section separators — one source of truth so the
 * link index and the full-content dump can never disagree about what pages
 * exist or what order they come in.
 */
import fs from "node:fs";
import path from "node:path";
import { source } from "@/lib/source";

type TreeNode = (typeof source.pageTree)["children"][number];

export interface LlmsPageEntry {
  section: string;
  title: string;
  description: string;
  url: string;
}

const UNSECTIONED = "Documentation";

function slugFromUrl(url: string): string[] {
  return url.replace(/^\/docs\/?/, "").split("/").filter(Boolean);
}

function pushEntry(url: string, fallbackName: unknown, section: string, out: LlmsPageEntry[]): void {
  const page = source.getPage(slugFromUrl(url));
  out.push({
    section,
    title: page?.data.title ?? String(fallbackName),
    description: page?.data.description ?? "",
    url,
  });
}

function walk(nodes: readonly TreeNode[], section: string, out: LlmsPageEntry[]): void {
  for (const node of nodes) {
    if (node.type === "separator") {
      if (typeof node.name === "string") section = node.name;
    } else if (node.type === "page") {
      pushEntry(node.url, node.name, section, out);
    } else if (node.type === "folder") {
      if (node.index) pushEntry(node.index.url, node.index.name, section, out);
      walk(node.children, section, out);
    }
  }
}

/** Every docs page, in sidebar order, grouped by its top-level nav section. */
export function listLlmsPages(): LlmsPageEntry[] {
  const out: LlmsPageEntry[] = [];
  walk(source.pageTree.children, UNSECTIONED, out);
  return out;
}

const CONTENT_ROOT = path.join(process.cwd(), "content", "docs");

/**
 * Raw MDX source for a page, frontmatter stripped, or `undefined` if no file
 * on disk matches the URL. Reads the source directly rather than rendering
 * `page.data.body` — that's a compiled React component, not markdown, and
 * dumping compiled output would defeat the point of a plain-text file made
 * for an LLM to read in one pass.
 */
export function rawMdxBody(url: string): string | undefined {
  const slug = slugFromUrl(url).join("/");
  const candidates =
    slug === ""
      ? [path.join(CONTENT_ROOT, "index.mdx")]
      : [path.join(CONTENT_ROOT, `${slug}.mdx`), path.join(CONTENT_ROOT, slug, "index.mdx")];
  const file = candidates.find((p) => fs.existsSync(p));
  if (!file) return undefined;
  const text = fs.readFileSync(file, "utf8");
  if (!text.startsWith("---")) return text.trim();
  const end = text.indexOf("\n---", 3);
  return end === -1 ? text.trim() : text.slice(end + 4).trim();
}
