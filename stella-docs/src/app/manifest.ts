import type { MetadataRoute } from "next";

/**
 * PWA manifest for the Stella docs site. Next auto-links this at
 * `/manifest.webmanifest` and adds the corresponding <link rel="manifest">.
 * Icons live under public/icons and were generated from the current gold
 * chevron+cells mark.
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
    background_color: "#0b0a08",
    theme_color: "#0b0a08",
    icons: [
      { src: "/icons/icon-192.png", sizes: "192x192", type: "image/png", purpose: "any" },
      { src: "/icons/icon-512.png", sizes: "512x512", type: "image/png", purpose: "any" },
      { src: "/icons/maskable-192.png", sizes: "192x192", type: "image/png", purpose: "maskable" },
      { src: "/icons/maskable-512.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
    ],
  };
}
