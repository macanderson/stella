import { DocsLayout } from "fumadocs-ui/layouts/docs";
import type { ReactNode } from "react";
import { baseOptions } from "@/lib/layout.shared";
import { source } from "@/lib/source";

export default function Layout({ children }: { children: ReactNode }) {
  return (
    <DocsLayout tree={source.pageTree} {...baseOptions()}>
      {/* First stop in the tab order, so the sidebar's ~90 links are skippable.
       * `#nd-page` is the <article> Fumadocs wraps every docs page in. */}
      <a className="skip-link" href="#nd-page">
        Skip to content
      </a>
      {children}
    </DocsLayout>
  );
}
