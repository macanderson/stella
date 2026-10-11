import type { MetadataRoute } from "next";
import { HOUSE_COLORS } from "@/components/brand-marks.generated";

/**
 * PWA manifest. Next auto-links this at `/manifest.webmanifest`. The icon
 * files under public/icons and the theme colour are committed copies of the
 * house kit's. Edit them by hand, and keep `docs/brand/pwa/` in step.
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
    background_color: HOUSE_COLORS.ink,
    theme_color: HOUSE_COLORS.ink,
    icons: [
      { src: "/icons/icon-192.png", sizes: "192x192", type: "image/png", purpose: "any" },
      { src: "/icons/icon-512.png", sizes: "512x512", type: "image/png", purpose: "any" },
      { src: "/icons/maskable-192.png", sizes: "192x192", type: "image/png", purpose: "maskable" },
      { src: "/icons/maskable-512.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
    ],
  };
}
