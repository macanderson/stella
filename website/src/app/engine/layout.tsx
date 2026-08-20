import type { ReactNode } from "react";
import type { Viewport } from "next";
import "./engine.css";
import "./engine-stations.css";

/**
 * Chrome for /engine: none.
 *
 * Every other route mounts the fumadocs shell; the tour deliberately does not.
 * It is a walk through the inside of the machine, and a persistent docs nav
 * would put the outside of the site in view the whole way — the same reasoning
 * that lets the home page hide its bar behind the hero, taken one step
 * further. The tour carries its own minimal HUD (top strip + station rail) in
 * tour-shell.tsx, including the way back out.
 *
 * The route keeps its own stylesheets beside it, imported once here —
 * `src/app/releases/releases.css` is the pattern — namespaced `eng-` so
 * nothing leaks into /docs or the landing page. They are two files rather
 * than one because a single one crossed the 1500-line ceiling AGENTS.md sets:
 * `engine.css` is the shell that is the same at every stop, and
 * `engine-stations.css` is what differs at each. Order matters only for
 * readability — the second consumes tokens the first defines, and a custom
 * property resolves at use — so keep the shell first.
 */

/**
 * The tour is a window into the machine, so its ground is ALWAYS ink — the
 * same law as `.term` and the docs' code blocks. The browser chrome should
 * agree with it in both site themes, hence a route-level themeColor override
 * (the root layout's is per-scheme). The hex mirrors `--stella-ink` in
 * tokens.css, the same way the root layout's viewport block does — a CSS
 * custom property cannot reach a meta tag.
 */
export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  themeColor: "#0a0a0c", // --stella-ink
};

export default function Layout({ children }: { children: ReactNode }) {
  return children;
}
