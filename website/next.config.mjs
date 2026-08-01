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

/**
 * script-src and style-src both need 'unsafe-inline': Next's App Router
 * streams RSC hydration payloads as inline <script> tags on every request
 * (unavoidable on a fully static export — there's no per-request server step
 * to hand out a nonce), and Radix/shiki/the TOC indentation all render via
 * inline `style` attributes. A nonce-based CSP needs dynamic rendering, which
 * would give up the static generation this site's performance depends on.
 * Everything else is locked to same-origin: no third-party script, style,
 * font, or connect target exists anywhere in this codebase (Vercel Analytics
 * and Speed Insights are both served same-origin via /_vercel/*).
 */
const CSP = [
  "default-src 'self'",
  "script-src 'self' 'unsafe-inline'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data:",
  "font-src 'self'",
  "connect-src 'self'",
  "object-src 'none'",
  "base-uri 'self'",
  "form-action 'self'",
  "frame-ancestors 'none'",
].join("; ");

const SECURITY_HEADERS = [
  { key: "Content-Security-Policy", value: CSP },
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "X-Frame-Options", value: "DENY" },
  { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
  {
    key: "Permissions-Policy",
    value: "camera=(), microphone=(), geolocation=(), interest-cohort=(), payment=(), usb=()",
  },
  { key: "Strict-Transport-Security", value: "max-age=63072000; includeSubDomains; preload" },
];

/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  // The site is fully static (MDX + generateStaticParams); no image
  // optimization server is needed.
  images: {
    unoptimized: true,
  },
  async headers() {
    return [
      { source: "/(.*)", headers: SECURITY_HEADERS },
      // Brand/icon assets under public/ have stable, un-hashed URLs (unlike
      // /_next/static/*, which is already immutable-cached) but don't change
      // between deploys either — give them the same long-lived cache.
      {
        source: "/brand/:path*",
        headers: [{ key: "Cache-Control", value: "public, max-age=31536000, immutable" }],
      },
      {
        source: "/icons/:path*",
        headers: [{ key: "Cache-Control", value: "public, max-age=31536000, immutable" }],
      },
    ];
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
