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
 * The lockup: "arenabench / by stella*", where the asterisk is the comet
 * itself — a four-point star with its trail, flying left to right, drawn
 * from the same geometry as the stella logomark so the two are the same
 * mark at two sizes. Gold appears exactly once, on the star.
 */
function Brand() {
  return (
    <div className="flex items-center gap-2.5">
      <svg className="block h-3 w-[34px] flex-none" viewBox="0 0 96 34" role="img" aria-label="stella">
        <line className="comet-trail stroke-gold" x1="6" y1="17" x2="34" y2="17" strokeWidth="7" strokeLinecap="round" />
        <line className="comet-trail stroke-gold" x1="17" y1="4" x2="34" y2="4" strokeWidth="7" strokeLinecap="round" />
        <line className="comet-trail stroke-gold" x1="17" y1="30" x2="34" y2="30" strokeWidth="7" strokeLinecap="round" />
        <path
          className="comet-star fill-gold"
          d="M68 -5 C69.65 8.2 76.8 15.35 90 17 C76.8 18.65 69.65 25.8 68 39
             C66.35 25.8 59.2 18.65 46 17 C59.2 15.35 66.35 8.2 68 -5 Z"
        />
      </svg>
      <span className="flex flex-col gap-px leading-none">
        <span className="text-[15px] font-extrabold lowercase tracking-[-0.02em]">
          arena<span className="text-accent">bench</span>
        </span>
        <span className="flex items-center gap-1 text-[9.5px] lowercase tracking-[0.14em] text-dim">
          by <b className="font-medium text-muted">stella</b>
        </span>
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
  return (
    <header className="sticky top-0 z-40 flex h-14 items-center gap-6 border-b border-line bg-background/85 px-5 backdrop-blur-md">
      <Brand />
      <Tabs.Root value={view} onValueChange={(v) => onViewChange(v as View)}>
        <Tabs.List className="flex gap-1">
          <Tabs.Tab
            value="setup"
            className={cn(
              "cursor-pointer rounded-[7px] px-3.5 py-[7px] text-[13px] lowercase text-muted",
              "hover:bg-panel hover:text-foreground",
              "data-[selected]:bg-gold data-[selected]:font-semibold data-[selected]:text-ink",
            )}
          >
            Setup
          </Tabs.Tab>
          <Tabs.Tab
            value="arena"
            disabled={!arenaEnabled}
            className={cn(
              "cursor-pointer rounded-[7px] px-3.5 py-[7px] text-[13px] lowercase text-muted",
              "hover:bg-panel hover:text-foreground",
              "data-[selected]:bg-gold data-[selected]:font-semibold data-[selected]:text-ink",
              "data-[disabled]:cursor-not-allowed data-[disabled]:opacity-35",
            )}
          >
            Arena
          </Tabs.Tab>
        </Tabs.List>
      </Tabs.Root>
      <div className="ml-auto flex items-center gap-2 text-xs">
        <span
          className={cn(
            "size-[7px] rounded-full transition-all",
            conn.tone === "live" && "bg-ok shadow-[0_0_0_3px_rgb(92_230_138/0.16)]",
            conn.tone === "error" && "bg-bad",
            conn.tone === "" && "bg-dim",
          )}
          title={conn.tone === "live" ? "connected" : "disconnected"}
        />
        <span className="font-mono text-muted">{conn.label}</span>
        <ThemeToggle />
      </div>
    </header>
  );
}
