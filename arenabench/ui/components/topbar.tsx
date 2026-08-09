"use client";

import * as React from "react";
import { Tabs } from "@base-ui/react/tabs";
import { cn } from "@/lib/utils";
import { ThemeToggle } from "@/components/theme-toggle";
import type { View } from "@/components/app-shell";

export interface ConnState {
  tone: "" | "live" | "error";
  label: string;
}

/*
 * The lockup: the stella wordmark from the brand kit, a hairline pipe, and
 * the arena's domain. The artwork is a byte-copy of
 * docs/brand/logo/svg/wordmark-mono-{dark,light}.svg served from
 * public/brand/ — never redrawn here, so the kit stays the single source of
 * the mark. Two fixed cuts swap with the scheme because next-themes stamps
 * `.dark` on <html>; the kit's *-adaptive.svg follows only the OS media
 * query and would ignore the in-app toggle.
 *
 * The MONO cuts, not the colour ones: this chrome carries no accent hue, and
 * the wordmark is the largest thing on the page — a gold mark here would
 * reintroduce, at the top of every view, exactly the competition with the
 * data that removing the gold accent was for. The mono cuts are a single
 * fill each (#F4F1EA on the ink page, #0B0B0C on the paper one), so the
 * lockup reads as type rather than as a logo.
 */
function Brand() {
  return (
    <div className="flex items-center gap-3.5">
      {/*
       * h-[57px] is 2.25x the height of the lockup this one replaced. The
       * artwork carries a 9-unit left bearing in its 264-wide viewBox (~5px
       * at this size); the negative margin sets the glyphs, not the
       * transparent frame, on the content edge.
       */}
      <img
        src="/brand/wordmark-mono-light.svg"
        alt="stella"
        width={157}
        height={57}
        className="brand-mark -ml-[5px] block h-[57px] w-auto flex-none dark:hidden"
      />
      <img
        src="/brand/wordmark-mono-dark.svg"
        alt="stella"
        width={157}
        height={57}
        className="brand-mark -ml-[5px] hidden h-[57px] w-auto flex-none dark:block"
      />
      <span aria-hidden="true" className="h-7 w-px flex-none bg-line" />
      <span className="whitespace-nowrap text-[20px] font-bold tracking-[0.04em]">
        ARENABENCH.ORG
      </span>
    </div>
  );
}

export function Topbar({
  view,
  onViewChange,
  arenaEnabled,
  conn,
}: {
  view: View;
  onViewChange: (view: View) => void;
  arenaEnabled: boolean;
  conn: ConnState;
}) {
  /*
   * The bar is opaque, not translucent-and-blurred. A backdrop blur is a soft
   * surface effect in the same family as the gradients this pass removed, and
   * it makes the value of the bar depend on whatever is scrolling under it —
   * which on a page whose content is measurements means the chrome shifts
   * while the reader is comparing numbers. A hairline is enough.
   */
  return (
    <header className="sticky top-0 z-40 border-b border-line bg-background">
      {/*
       * The bar itself spans the viewport (so the border does too); this
       * inner rail mirrors the active view's container — setup is
       * mx-auto max-w-[1180px] px-5, arena max-w-[1600px] px-5 sm:px-7 —
       * so the wordmark's left edge lands on the content's left edge, not
       * the page's.
       */}
      <div
        className={cn(
          "mx-auto flex items-center gap-6 px-5 py-3",
          view === "setup" ? "max-w-[1180px]" : "max-w-[1600px] sm:px-7",
        )}
      >
        <Brand />
        <Tabs.Root value={view} onValueChange={(v) => onViewChange(v as View)}>
          <Tabs.List className="flex gap-1">
            <Tabs.Tab
              value="setup"
              className={cn(
                "cursor-pointer px-3.5 py-[7px] text-[13px] text-muted",
                "hover:bg-panel hover:text-foreground",
                "data-[active]:bg-accent data-[active]:font-semibold data-[active]:text-on-accent",
              )}
            >
              Setup
            </Tabs.Tab>
            <Tabs.Tab
              value="arena"
              disabled={!arenaEnabled}
              className={cn(
                "cursor-pointer px-3.5 py-[7px] text-[13px] text-muted",
                "hover:bg-panel hover:text-foreground",
                "data-[active]:bg-accent data-[active]:font-semibold data-[active]:text-on-accent",
                "data-[disabled]:cursor-not-allowed data-[disabled]:opacity-35",
              )}
            >
              Arena
            </Tabs.Tab>
            <Tabs.Tab
              value="trends"
              className={cn(
                "cursor-pointer px-3.5 py-[7px] text-[13px] text-muted",
                "hover:bg-panel hover:text-foreground",
                "data-[active]:bg-accent data-[active]:font-semibold data-[active]:text-on-accent",
              )}
            >
              Trends
            </Tabs.Tab>
          </Tabs.List>
        </Tabs.Root>
        <div className="ml-auto flex items-center gap-2 text-xs">
          <span
            className={cn(
              "size-[7px] transition-all",
              conn.tone === "live" && "bg-ok ",
              conn.tone === "error" && "bg-bad",
              conn.tone === "" && "bg-dim",
            )}
            title={conn.tone === "live" ? "connected" : "disconnected"}
          />
          <span className="font-mono text-muted">{conn.label}</span>
          <ThemeToggle />
        </div>
      </div>
    </header>
  );
}
