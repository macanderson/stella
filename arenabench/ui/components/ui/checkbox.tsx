"use client";

import * as React from "react";
import { Checkbox as BaseCheckbox } from "@base-ui/react/checkbox";
import { Check } from "lucide-react";
import { cn } from "@/lib/utils";

export function Checkbox({
  checked,
  onCheckedChange,
  className,
  ariaLabel,
}: {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  className?: string;
  ariaLabel?: string;
}) {
  return (
    <BaseCheckbox.Root
      checked={checked}
      onCheckedChange={(next) => onCheckedChange(next)}
      aria-label={ariaLabel}
      className={cn(
        "flex size-[15px] shrink-0 cursor-pointer items-center justify-center rounded-[4px] border border-line bg-panel",
        "data-[checked]:border-gold data-[checked]:bg-gold",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/40",
        className,
      )}
    >
      <BaseCheckbox.Indicator className="text-ink data-[unchecked]:hidden">
        <Check className="size-3" strokeWidth={3} />
      </BaseCheckbox.Indicator>
    </BaseCheckbox.Root>
  );
}

/** A checkbox with an inline label, the kit's usual arrangement. */
export function LabeledCheckbox({
  checked,
  onCheckedChange,
  className,
  children,
}: {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <label className={cn("flex cursor-pointer items-center gap-2 text-[13px]", className)}>
      <Checkbox checked={checked} onCheckedChange={onCheckedChange} />
      <span>{children}</span>
    </label>
  );
}
