"use client";

import { useLayoutEffect, type ReactNode } from "react";
import { HomeLayout } from "fumadocs-ui/layouts/home";
import { baseOptions } from "@/lib/layout.shared";

/** Matches fumadocs' own `useIsScrollTop` threshold, so "scrolled" means the
 * same thing here as it does inside the library's transparent-nav mode. */
const SCROLL_THRESHOLD = 10;

/**
 * The home page's own chrome. Every other route mounts the standard fumadocs
 * top bar from first paint; the home page doesn't need it there — the hero
 * carries its own large, animated lockup in the corner a nav's logo would
 * otherwise occupy (see AnimatedLockup), so the real bar would just be a
 * second, smaller lockup competing with it. The bar fades in the moment the
 * reader leaves the top of the page.
 *
 * Implemented as a `data-home-nav` attribute on <html> rather than React
 * state so scrolling doesn't drive a re-render on every tick, and the nav's
 * `id="nd-nav"` — unique to the home layout's header, fumadocs never reuses
 * it on docs pages — is the only CSS selector this needs, in global.css.
 * useLayoutEffect (not useEffect) so the attribute is set before the
 * browser's first paint, avoiding a flash of the bar before it hides.
 */
export function HomeChrome({ children }: { children: ReactNode }) {
  useLayoutEffect(() => {
    const root = document.documentElement;
    const onScroll = () => {
      root.setAttribute(
        "data-home-nav",
        window.scrollY >= SCROLL_THRESHOLD ? "visible" : "hidden",
      );
    };
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      window.removeEventListener("scroll", onScroll);
      root.removeAttribute("data-home-nav");
    };
  }, []);

  return <HomeLayout {...baseOptions()}>{children}</HomeLayout>;
}
