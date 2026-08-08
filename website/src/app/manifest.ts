import type { MetadataRoute } from "next";

/**
 * PWA manifest. Next auto-links this at `/manifest.webmanifest`. The values
 * mirror docs/brand/pwa/manifest.webmanifest (the kit is normative); the icon
 * files under public/icons are byte-for-byte copies of docs/brand/pwa/ — when
 * the kit regenerates, re-copy them rather than editing either side.
 */
export default function manifest(): MetadataRoute.Manifest {
  return {
    name: "stella",
    short_name: "stella",
    description: "the terminal agent — faster · cheaper · more accurate",
    id: "/",
    start_url: "/",
    scope: "/",
    display: "standalone",
    background_color: "#0b0b0c", // --stella-ink
    theme_color: "#0b0b0c",
    icons: [
      { src: "/icons/icon-192.png", sizes: "192x192", type: "image/png", purpose: "any" },
      { src: "/icons/icon-512.png", sizes: "512x512", type: "image/png", purpose: "any" },
      { src: "/icons/maskable-192.png", sizes: "192x192", type: "image/png", purpose: "maskable" },
      { src: "/icons/maskable-512.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
    ],
  };
}
