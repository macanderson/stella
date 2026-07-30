import { createMDX } from "fumadocs-mdx/next";

const withMDX = createMDX();

/**
 * Pages that were consolidated into a single scroll, and the anchor each one
 * became. The anchors are the heading ids the merged page actually emits —
 * read out of the rendered HTML rather than derived from the heading text — so
 * a reworded heading breaks a redirect and has to be re-checked here.
 *
 * These are permanent (308) because the old URLs are the ones in bookmarks,
 * search results, and every external link written before the merge. Deleting a
 * documentation URL without a redirect is how a docs site loses its inbound
 * links and its search ranking in the same week.
 */
const CONSOLIDATED = {
  // ten provider pages → /docs/api-providers
  "/docs/api-providers/anthropic": "/docs/api-providers#anthropic",
  "/docs/api-providers/openai": "/docs/api-providers#openai",
  "/docs/api-providers/gemini": "/docs/api-providers#google-gemini",
  "/docs/api-providers/vertex": "/docs/api-providers#google-vertex-ai",
  "/docs/api-providers/bedrock": "/docs/api-providers#amazon-bedrock",
  "/docs/api-providers/xai": "/docs/api-providers#xai",
  "/docs/api-providers/deepseek": "/docs/api-providers#deepseek",
  "/docs/api-providers/zai": "/docs/api-providers#zai",
  "/docs/api-providers/openrouter": "/docs/api-providers#openrouter",
  "/docs/api-providers/local": "/docs/api-providers#local-servers",
  // nine recipe pages + the section index → /docs/examples
  "/docs/examples/single-key": "/docs/examples#single-key",
  "/docs/examples/dirt-cheap": "/docs/examples#dirt-cheap",
  "/docs/examples/balanced": "/docs/examples#balanced",
  "/docs/examples/max-quality": "/docs/examples#maximum-quality",
  "/docs/examples/openrouter-gateway": "/docs/examples#one-gateway-key-openrouter",
  "/docs/examples/local-airgapped": "/docs/examples#local-and-air-gapped",
  "/docs/examples/enterprise-cloud": "/docs/examples#enterprise-cloud-vertex-and-bedrock",
  "/docs/examples/team-settings": "/docs/examples#team-shared-settings",
  // The Z.ai coding-plan recipe folded into the profile table rather than
  // keeping a heading of its own, so this one lands on the page, not an anchor.
  "/docs/examples/zai-coding-plan": "/docs/examples",
};

/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  // The site is fully static (MDX + generateStaticParams); no image
  // optimization server is needed.
  images: {
    unoptimized: true,
  },
  async redirects() {
    return [
      // Agent Modes was consolidated from a section (index + goal-mode) into a
      // single page; keep the old deep link alive for bookmarks and search hits.
      {
        source: "/docs/agent-modes/goal-mode",
        destination: "/docs/agent-modes#outcome-driven-goal-mode",
        permanent: true,
      },
      ...Object.entries(CONSOLIDATED).map(([source, destination]) => ({
        source,
        destination,
        permanent: true,
      })),
    ];
  },
};

export default withMDX(nextConfig);
