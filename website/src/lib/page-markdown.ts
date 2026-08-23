/**
 * The markdown a docs page exports — the body behind the "Copy page" menu on
 * every docs page and the `/llms.mdx/<slug>` endpoint.
 *
 * The source of truth is the `_markdown` export fumadocs-mdx attaches to every
 * page when `postprocess.includeProcessedMarkdown` is on (see
 * `source.config.ts`): the page's own MDX, rendered back to markdown by the
 * same remark pipeline that renders it to HTML, so includes are resolved and
 * GFM tables survive. Reading the raw `.mdx` off disk instead would leak JSX
 * tags a markdown consumer cannot read.
 *
 * Three component kinds carry meaning outside their children — the SVG
 * diagrams (their content is the picture), `ProviderGrid` (its content is the
 * catalog data), and `CommandDeckExplorer` (nine tabbed panes). Those are
 * emitted as placeholders at build time and re-rendered to markdown here by
 * `renderPlaceholder`, from the same records the components render from, so
 * the export can never drift from what the page shows.
 *
 * This module is pure and `.ts`-only — no JSX, no `node:fs` — because it is
 * exercised by `node --test` (which strips types but cannot parse JSX) and
 * imported by `source.config.ts` (which runs before the path alias `@/` is
 * meaningful, hence the relative imports).
 */
import { renderPlaceholder } from "fumadocs-core/mdx-plugins/remark-llms.runtime";

import { DIAGRAM_DESCRIPTIONS } from "../components/diagram-descriptions.ts";
import { COMMAND_DECK_TABS } from "../components/command-deck-tabs.ts";
import { PROVIDER_CATALOG } from "../components/provider-catalog.ts";

/**
 * The diagram component names `source.config.ts` lists as `mdxAsPlaceholder`.
 * Derived from the descriptions record so a diagram added to `diagrams.tsx`
 * (and therefore to the record — `diagrams.test.ts` enforces that) is
 * placeholder-rendered without a second edit here.
 */
export const DIAGRAM_PLACEHOLDER_NAMES = Object.keys(DIAGRAM_DESCRIPTIONS);

/** Escape the two characters that would break a GFM table cell. */
function cell(text: string): string {
  return text.replace(/\|/g, "\\|").replace(/\n/g, " ");
}

function providerGridMarkdown(only: unknown): string {
  const ids = Array.isArray(only) ? only.filter((v): v is string => typeof v === "string") : undefined;
  const providers = ids
    ? ids.flatMap((id) => PROVIDER_CATALOG.filter((p) => p.id === id))
    : PROVIDER_CATALOG;
  const rows = providers.map(
    (p) =>
      `| ${cell(p.name)} | \`${p.id}\` | \`${cell(p.env)}\` | \`${cell(p.defaultModel)}\` | ${cell(p.dialect)} | ${cell(p.blurb)} |`,
  );
  return [
    "| Provider | id | Env var | Default model | Dialect | When to pick it |",
    "|---|---|---|---|---|---|",
    ...rows,
  ].join("\n");
}

function commandDeckMarkdown(): string {
  return COMMAND_DECK_TABS.map(
    (tab) => `### ${tab.label}\n\n${tab.blurb}\n\n\`\`\`text\n${tab.lines.join("\n")}\n\`\`\``,
  ).join("\n\n");
}

/**
 * Renderers for the placeholders `includeProcessedMarkdown` emits. A name
 * absent here degrades to the placeholder's children (fumadocs' default),
 * which for these three would be nothing — so the record is exhaustive by
 * construction: the placeholder list in `source.config.ts` is exactly these
 * keys, and `page-markdown.test.ts` asserts it.
 */
export const PLACEHOLDER_RENDERERS: Record<
  string,
  (data: { name: string | null; attributes: Record<string, unknown>; children: string }) => string
> = Object.fromEntries([
  ...DIAGRAM_PLACEHOLDER_NAMES.map((name) => [
    name,
    () => `*${DIAGRAM_DESCRIPTIONS[name]}*`,
  ]),
  ["ProviderGrid", (data: { attributes: Record<string, unknown> }) => providerGridMarkdown(data.attributes.only)],
  ["CommandDeckExplorer", () => commandDeckMarkdown()],
]);

export interface PageMarkdownInput {
  title: string;
  description?: string;
  /** The page's canonical path, e.g. `/docs/agent-tools`. */
  url: string;
  /** The page's `_markdown` export, with placeholders still embedded. */
  markdown: string;
}

/**
 * Assemble the final markdown document for one page: a header naming the page
 * and where it came from (so a pasted copy is attributable), then the body
 * with every placeholder rendered.
 */
export async function pageMarkdown(page: PageMarkdownInput): Promise<string> {
  const rendered = await renderPlaceholder(page.markdown, PLACEHOLDER_RENDERERS);
  // remarkLLMs' own filterElement runs ahead of any user-supplied one and
  // re-serializes self-closing inline JSX (the ProviderMark logomarks in the
  // model tables) as literal tags, ignoring the stringify callback's "" for
  // them. They are decorative — the provider's name is in the text beside
  // them — so they are stripped here, after rendering, where the result is
  // fully under this module's control.
  const body = rendered.replace(/<Provider(?:Mark|Logo)\b[^/]*\/>\s*/g, "");
  const parts = [`# ${page.title}`, ""];
  if (page.description) parts.push(`> ${page.description}`, "");
  parts.push(`Source: ${page.url}`, "", body.trim(), "");
  return parts.join("\n");
}
