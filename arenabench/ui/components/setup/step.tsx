import * as React from "react";
import { cn } from "@/lib/utils";

export function Step({
  n,
  title,
  hint,
  children,
  className,
}: {
  n?: string;
  title: React.ReactNode;
  hint?: React.ReactNode;
  children?: React.ReactNode;
  className?: string;
}) {
  return (
    <section className={cn("mb-10", className)}>
      <h2 className="mb-[5px] flex items-center gap-[11px] text-base">
        {n !== undefined && (
          <span className="grid size-[22px] place-items-center rounded-md border border-line bg-panel-2 font-mono text-[11px] text-accent">
            {n}
          </span>
        )}
        {title}
      </h2>
      {hint && <p className="mb-4 ml-[33px] text-[13px] text-muted">{hint}</p>}
      {children}
    </section>
  );
}

export function ErrorBox({ children }: { children: React.ReactNode }) {
  return (
    <div className="mt-3.5 rounded-lg border border-bad/40 bg-bad/7 px-3.5 py-[11px] text-[13px] text-bad">
      {children}
    </div>
  );
}
