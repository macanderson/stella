import type { ReactNode } from "react";
import { HomeLayout } from "fumadocs-ui/layouts/home";
import { baseOptions } from "@/lib/layout.shared";
import "./releases.css";

/**
 * Chrome for /releases: the standard nav bar, present from first paint.
 *
 * Deliberately NOT the home page's `HomeChrome`, which hides the bar until the
 * reader scrolls — that exists because the hero there carries its own large
 * animated lockup in the corner the nav's logo would occupy, and two lockups
 * would compete. This page's hero is type, so there is nothing to compete with
 * and no reason to make a reader scroll to find their way back to the docs.
 */
export default function Layout({ children }: { children: ReactNode }) {
  return <HomeLayout {...baseOptions()}>{children}</HomeLayout>;
}
