"use client";

import * as React from "react";
import { Collapsible as BaseCollapsible } from "@base-ui/react/collapsible";
import { ChevronRight } from "lucide-react";
import { cn } from "@/lib/utils";

/** The `<details>` of the old client, kept keyboard- and reader-friendly. */
export function Disclosure({
  summary,
  children,
  className,
}: {
  summary: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <BaseCollapsible.Root className={className}>
      <BaseCollapsible.Trigger
        className={cn(
          "group flex cursor-pointer select-none items-center gap-1.5 py-1.5 font-mono text-xs text-muted",
          "hover:text-foreground data-[panel-open]:text-foreground focus-visible:outline-none focus-visible:text-foreground",
        )}
      >
        <ChevronRight className="size-3 transition-transform group-data-[panel-open]:rotate-90" />
        {summary}
      </BaseCollapsible.Trigger>
      <BaseCollapsible.Panel>{children}</BaseCollapsible.Panel>
    </BaseCollapsible.Root>
  );
}
