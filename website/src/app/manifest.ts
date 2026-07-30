import type { MetadataRoute } from "next";

/**
 * PWA manifest for the Stella docs site. Next auto-links this at
 * `/manifest.webmanifest` and adds the corresponding <link rel="manifest">.
 * Icons live under public/icons and are hand-maintained alongside the mark
 * (the docs/brand/build.py generator no longer exists — see website/README.md);
 * update them together when the mark changes. They are the gold mark on
 * `void`, per BRAND.md § Gold — gold is the mark, and at 11.9:1 on deep space
 * it survives being 16px wide in a browser tab.
 */
export default function manifest(): MetadataRoute.Manifest {
  return {
    name: "Stella CLI — Docs",
    short_name: "Stella",
    description:
      "A fast, BYOK, model-agnostic terminal coding agent.",
    id: "/",
    start_url: "/",
    scope: "/",
    display: "standalone",
    background_color: "#05070c", // --stella-void
    theme_color: "#080a0f", // --stella-ground
    icons: [
      { src: "/icons/icon-192.png", sizes: "192x192", type: "image/png", purpose: "any" },
      { src: "/icons/icon-512.png", sizes: "512x512", type: "image/png", purpose: "any" },
      { src: "/icons/maskable-192.png", sizes: "192x192", type: "image/png", purpose: "maskable" },
      { src: "/icons/maskable-512.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
    ],
  };
}
