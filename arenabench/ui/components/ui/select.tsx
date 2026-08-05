"use client";

import * as React from "react";
import { Select as BaseSelect } from "@base-ui/react/select";
import { Check, ChevronsUpDown } from "lucide-react";
import { cn } from "@/lib/utils";

export interface SelectItem {
  value: string;
  label: string;
}

/**
 * A single-value select over Base UI, pre-composed because every select in
 * this app is the same dense form control: a trigger that looks like an
 * input, a popup that looks like a panel.
 */
export function SimpleSelect({
  value,
  onValueChange,
  items,
  disabled,
  className,
  size = "default",
  ariaLabel,
}: {
  value: string;
  onValueChange: (value: string) => void;
  items: SelectItem[];
  disabled?: boolean;
  className?: string;
  size?: "default" | "sm";
  ariaLabel?: string;
}) {
  return (
    <BaseSelect.Root
      items={items}
      value={value}
      onValueChange={(next) => {
        if (typeof next === "string") onValueChange(next);
      }}
      disabled={disabled}
    >
      <BaseSelect.Trigger
        aria-label={ariaLabel}
        className={cn(
          "inline-flex w-full cursor-pointer items-center justify-between gap-2 rounded-[7px] border border-line bg-panel font-mono text-foreground",
          size === "sm" ? "px-2 py-[5px] text-xs" : "px-2.5 py-[7px] text-[13px]",
          "focus-visible:border-accent focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-accent/10",
          "data-[disabled]:cursor-not-allowed data-[disabled]:opacity-40",
          className,
        )}
      >
        <BaseSelect.Value className="truncate" />
        <BaseSelect.Icon className="flex shrink-0 text-dim">
          <ChevronsUpDown className="size-3" />
        </BaseSelect.Icon>
      </BaseSelect.Trigger>
      <BaseSelect.Portal>
        <BaseSelect.Positioner className="z-50 outline-none" sideOffset={4}>
          <BaseSelect.Popup className="max-h-[min(24rem,var(--available-height))] min-w-[var(--anchor-width)] overflow-y-auto rounded-[8px] border border-line bg-panel py-1 font-mono text-[13px] shadow-[0_8px_30px_-12px_rgb(0_0_0/0.55)]">
            {items.map((item) => (
              <BaseSelect.Item
                key={item.value}
                value={item.value}
                className="grid cursor-pointer grid-cols-[1rem_1fr] items-center gap-1.5 py-1.5 pl-2 pr-3 text-foreground data-[highlighted]:bg-panel-2 data-[selected]:text-accent"
              >
                <BaseSelect.ItemIndicator className="col-start-1">
                  <Check className="size-3" />
                </BaseSelect.ItemIndicator>
                <BaseSelect.ItemText className="col-start-2 truncate">
                  {item.label}
                </BaseSelect.ItemText>
              </BaseSelect.Item>
            ))}
          </BaseSelect.Popup>
        </BaseSelect.Positioner>
      </BaseSelect.Portal>
    </BaseSelect.Root>
  );
}
