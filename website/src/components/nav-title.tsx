"use client";

import Link from "fumadocs-core/link";
import type { ComponentProps } from "react";
import { Wordmark } from "@/components/brand";

/**
 * The nav title link: the wordmark and "docs".
 *
 * Fumadocs calls `nav.title` with the link's props, and in the docs sidebar
 * those carry a fixed 15px size (`text-[0.9375rem]`), a size the house scale
 * lacks. The `stella-nav-title` class is the hook global.css uses to put the
 * link on the app body step in the sidebar, the docs header, and the landing
 * page's nav bar.
 *
 * This is a client module because the layouts that pass it are server
 * components, and a server component can hand a client component only a
 * client reference, never a plain function.
 */
export function NavTitle({ className, ...props }: ComponentProps<"a">) {
  return (
    <Link {...props} className={className ? `${className} stella-nav-title` : "stella-nav-title"}>
      <span className="inline-flex items-center gap-2.5">
        <Wordmark className="h-6 w-auto text-fd-foreground" />
        <span className="text-a-body text-fd-muted-foreground">docs</span>
      </span>
    </Link>
  );
}
