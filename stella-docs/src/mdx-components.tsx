import defaultMdxComponents from "fumadocs-ui/mdx";
import type { MDXComponents } from "mdx/types";

/**
 * MDX component map. Generic shadcn-neutral for now — the Fumadocs defaults
 * (callouts, tabs, cards, code blocks, headings) cover everything the CLI
 * docs use. Register bespoke components here as the site grows.
 */
export function getMDXComponents(components?: MDXComponents): MDXComponents {
  return {
    ...defaultMdxComponents,
    ...components,
  };
}
