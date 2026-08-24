import { execSync } from "node:child_process";
import { defineDocs, defineConfig } from "fumadocs-mdx/config";
import lastModified from "fumadocs-mdx/plugins/last-modified";
import { DIAGRAM_PLACEHOLDER_NAMES } from "./src/lib/page-markdown";

export const docs = defineDocs({
  dir: "content/docs",
  docs: {
    postprocess: {
      // Export each page's rendered markdown as `_markdown` (see
      // src/lib/page-markdown.ts — the Copy page menu and /llms.mdx serve it).
      // The three component kinds whose meaning lives outside their children
      // are emitted as placeholders and re-rendered to markdown there.
      includeProcessedMarkdown: {
        mdxAsPlaceholder: [
          ...DIAGRAM_PLACEHOLDER_NAMES,
          "ProviderGrid",
          "CommandDeckExplorer",
        ],
        // The default stringifier keeps unknown JSX elements as literal tags
        // in the output (only their children are dropped) — so a page full of
        // <SpecCard> exported as nested JSX. Every element we render is a
        // container whose children are the content; the tags themselves are
        // presentation and do not belong in markdown.
        //
        // This is done in `stringify` rather than `filterElement` because
        // remarkLLMs spreads user options and then declares its own
        // `filterElement`, silently replacing any one passed in — while a
        // `stringify` callback is wrapped and honored. Returning the
        // stringified children is exactly what `children-only` would do.
        stringify(node, _parent, state, info) {
          switch (node.type) {
            case "mdxJsxFlowElement": {
              // Cards carry their identity in an attribute, not in their
              // children — a children-only render would export "Exploring,
              // iterating, supervising." with no name attached. Emit the
              // attribute as a heading above the children instead.
              const name = node.name;
              const attr = (key: string) =>
                node.attributes.find(
                  (a: { type: string; name?: unknown }) =>
                    a.type === "mdxJsxAttribute" && a.name === key,
                )?.value;
              const heading =
                name === "SpecCard" || name === "OptionCard" || name === "ToolCard"
                  ? (attr("title") ?? attr("name"))
                  : undefined;
              const body = state.containerFlow(node, info);
              return typeof heading === "string" && heading
                ? `#### ${heading}\n\n${body}`
                : body;
            }
            case "mdxJsxTextElement": {
              // Inline logomarks are decorative — every call site has the
              // provider's name in text beside them — so they export as
              // nothing rather than as a literal tag.
              if (node.name === "ProviderMark" || node.name === "ProviderLogo") return "";
              return state.containerPhrasing(node, info);
            }
            default:
              return undefined;
          }
        },
      },
    },
  },
});

/**
 * A shallow clone still has one commit, so `git log -1 -- <file>` succeeds
 * for every file and returns that single commit's date — not "unavailable"
 * (which the plugin would turn into `undefined`), but a real date that is
 * wrong and identical across every page. Detecting the shallow clone here,
 * before the plugin runs, is what actually gets the "omit rather than lie"
 * behavior documented in src/app/sitemap.ts; the plugin's own fallback never
 * triggers on Vercel's default (shallow) checkout. Set VERCEL_DEEP_CLONE=1 on
 * Vercel to get real per-file dates instead of an omitted field.
 */
function isShallowClone(): boolean {
  try {
    return execSync("git rev-parse --is-shallow-repository").toString().trim() !== "false";
  } catch {
    return true;
  }
}

export default defineConfig({
  plugins: isShallowClone() ? [] : [lastModified()],
});
