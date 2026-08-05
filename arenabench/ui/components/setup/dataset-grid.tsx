"use client";

import * as React from "react";
import type { Dataset } from "@/lib/types";
import { cn } from "@/lib/utils";

export function DatasetGrid({
  datasets,
  selectedKey,
  onPick,
}: {
  datasets: Dataset[];
  selectedKey: string | null;
  onPick: (dataset: Dataset) => void;
}) {
  return (
    <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(320px,1fr))]">
      {datasets.map((dataset) => {
        const selected = dataset.key === selectedKey;
        return (
          <button
            key={dataset.key}
            type="button"
            onClick={() => onPick(dataset)}
            className={cn(
              "cursor-pointer rounded-[10px] border bg-panel p-3 text-left transition-[border-color,background,transform] hover:-translate-y-px hover:border-dim",
              selected
                ? "border-gold bg-[linear-gradient(180deg,rgb(255_176_0/0.07),transparent_70%)]"
                : "border-line",
            )}
          >
            <h3 className="mb-1 text-[15px]">{dataset.title}</h3>
            <p className="mb-3 mt-2 text-[12.5px] text-muted">{dataset.description}</p>
            <div className="flex gap-3.5 font-mono text-[11px] text-dim">
              <span className="shrink-0">
                {dataset.task_count != null ? `${dataset.task_count} tasks` : "not fetched"}
              </span>
              <span className="truncate">
                {dataset.digest ? dataset.digest.slice(0, 19) + "…" : "unpinned"}
              </span>
            </div>
          </button>
        );
      })}
    </div>
  );
}
