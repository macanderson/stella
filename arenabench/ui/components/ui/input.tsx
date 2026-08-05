import * as React from "react";
import { cn } from "@/lib/utils";

export function Input({ className, ...props }: React.InputHTMLAttributes<HTMLInputElement>) {
  return (
    <input
      className={cn(
        "rounded-[7px] border border-line bg-panel px-2.5 py-[7px] font-mono text-[13px] text-foreground",
        "placeholder:text-dim focus:border-accent focus:outline-none focus:ring-[3px] focus:ring-accent/10",
        className,
      )}
      {...props}
    />
  );
}

export function Textarea({
  className,
  ...props
}: React.TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      className={cn(
        "w-full resize-y rounded-[7px] border border-line bg-panel px-2.5 py-[7px] font-mono text-xs leading-normal text-foreground",
        "placeholder:text-dim focus:border-accent focus:outline-none focus:ring-[3px] focus:ring-accent/10",
        className,
      )}
      {...props}
    />
  );
}

/** A form field with the kit's lowercase, letterspaced label. */
export function Field({
  label,
  className,
  children,
}: {
  label: React.ReactNode;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <label className={cn("flex min-w-0 flex-col gap-[5px]", className)}>
      <span className="text-[11px] lowercase tracking-[0.06em] text-muted">{label}</span>
      {children}
    </label>
  );
}
