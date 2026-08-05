import * as React from "react";
import { cn } from "@/lib/utils";

/** The status pill: lowercase, letterspaced, tinted by match state. */
export function StatusPill({
  status,
  className,
}: {
  status: string;
  className?: string;
}) {
  const tone =
    status === "running"
      ? "border-gold/45 bg-gold/8 text-accent"
      : status === "finished"
        ? "border-ok/40 bg-ok/7 text-ok"
        : status === "cancelled" || status === "failed"
          ? "border-bad/40 text-bad"
          : "border-line text-muted";
  return (
    <span
      className={cn(
        "rounded-full border px-2.5 py-1 font-mono text-[11px] lowercase tracking-[0.07em]",
        tone,
        className,
      )}
    >
      {status}
    </span>
  );
}
