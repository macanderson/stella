import { DocsLayout } from "fumadocs-ui/layouts/docs";
import type { ReactNode } from "react";
import { baseOptions } from "@/lib/layout.shared";
import { source } from "@/lib/source";

/**
 * `docsLink: false` keeps the shared "Docs" nav link out of the sidebar, where
 * fumadocs would render it as a pill above the page tree — a permanently
 * highlighted entry that is really the parent of everything below it. See
 * layout.shared.tsx for why the link still exists for the home layout.
 */
export default function Layout({ children }: { children: ReactNode }) {
  return (
    <DocsLayout tree={source.pageTree} {...baseOptions({ docsLink: false })}>
      {children}
    </DocsLayout>
  );
}
