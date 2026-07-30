import { HomeLayout } from "fumadocs-ui/layouts/home";
import type { ReactNode } from "react";
import { baseOptions } from "@/lib/layout.shared";

export default function Layout({ children }: { children: ReactNode }) {
  return (
    <HomeLayout {...baseOptions()}>
      {/* First stop in the tab order; the landing page's <main> owns #content. */}
      <a className="skip-link" href="#content">
        Skip to content
      </a>
      {children}
    </HomeLayout>
  );
}
