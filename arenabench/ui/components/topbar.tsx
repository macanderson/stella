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
 * The arena's domain, on its own. The stella wordmark used to sit to the left
 * of a hairline pipe here, and was removed deliberately: this arena scores
 * stella as one seat among several, so its mark does not belong in the chrome
 * every seat is judged under. The pipe went with it — it separated two things
 * and there is now one — as did the four cuts under public/brand/ and the
 * `.brand-mark` entrance in globals.css, which nothing else used.
 */
function Brand() {
  return (
    <span className="whitespace-nowrap text-[20px] font-bold tracking-[0.04em]">
      ARENABENCH.ORG
    </span>
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
       * so the lockup's left edge lands on the content's left edge, not the
       * page's. It needs no optical correction now that the lockup is type:
       * the wordmark it replaced carried a 9-unit left bearing in its
       * 264-wide viewBox and was pulled back -5px to compensate.
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
