import * as React from "react";
import { cn } from "@/lib/utils";

/** The status pill: , letterspaced, tinted by match state. */
export function StatusPill({
  status,
  className,
}: {
  status: string;
  className?: string;
}) {
  const tone =
    status === "running"
      ? "border-accent/45 bg-accent/8 text-accent"
      : status === "finished"
        ? "border-ok/40 bg-ok/7 text-ok"
        : status === "cancelled" || status === "failed"
          ? "border-bad/40 text-bad"
          : "border-line text-muted";
  return (
    <span
      className={cn(
        " border px-2.5 py-1 font-mono text-[11px] tracking-[0.07em]",
        tone,
        className,
      )}
    >
      {status}
    </span>
  );
}
