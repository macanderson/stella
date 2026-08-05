"use client";

import * as React from "react";
import { Tooltip as BaseTooltip } from "@base-ui/react/tooltip";

export const TooltipProvider = BaseTooltip.Provider;

/** Hover text for terse labels. `children` is the trigger, rendered as-is. */
export function Tip({
  content,
  children,
}: {
  content: React.ReactNode;
  children: React.ReactElement;
}) {
  if (!content) return children;
  return (
    <BaseTooltip.Root>
      <BaseTooltip.Trigger render={children} />
      <BaseTooltip.Portal>
        <BaseTooltip.Positioner sideOffset={6} className="z-50">
          <BaseTooltip.Popup className="max-w-72 rounded-[7px] border border-line bg-panel-2 px-2.5 py-1.5 font-mono text-[11.5px] leading-relaxed text-foreground shadow-[0_8px_30px_-12px_rgb(0_0_0/0.55)]">
            {content}
          </BaseTooltip.Popup>
        </BaseTooltip.Positioner>
      </BaseTooltip.Portal>
    </BaseTooltip.Root>
  );
}
