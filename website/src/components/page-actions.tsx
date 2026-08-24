"use client";

/**
 * The "Copy page" menu at the top of every docs page — the dropdown docs
 * readers reach for when they want the page somewhere else: as markdown in
 * the clipboard (to paste into a model's context), as a markdown file, or on
 * paper.
 *
 * The markdown items all resolve to the same artifact — the page's
 * `/llms.mdx/<slug>` endpoint, which serves the page's own `_markdown` export
 * (see `src/lib/page-markdown.ts`) — so what lands in the clipboard is
 * byte-identical to what *View as Markdown* shows and *Download* saves.
 * *Print / Save as PDF* is the browser's own print path; the print rules in
 * `src/app/global.css` strip the chrome so what comes out is the article.
 *
 * A client component for the same reason the share menu is: open state, an
 * outside-click listener, and the clipboard are all browser-only. Styling
 * follows the footer's taste rules — quiet chrome, Fumadocs tokens only.
 */

import { useEffect, useRef, useState } from "react";
import { Check, ChevronDown, Copy, Download, ExternalLink, Printer } from "lucide-react";

export function PageActions({ slug }: { slug: string[] }) {
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  // Close on an outside click or on Escape — the usual dropdown contract.
  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  // The docs root has no slug segments; its markdown lives at /llms.mdx/index
  // (see the route's generateStaticParams for why).
  const mdPath = slug.length === 0 ? "/llms.mdx/index" : `/llms.mdx/${slug.join("/")}`;

  const copyMarkdown = async () => {
    try {
      const res = await fetch(mdPath);
      if (!res.ok) return;
      await navigator.clipboard.writeText(await res.text());
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
      setOpen(false);
    } catch {
      /* network or clipboard unavailable — the other menu items still work */
    }
  };

  const itemClass =
    "flex w-full items-center gap-2 rounded px-2.5 py-1.5 text-left text-xs text-fd-popover-foreground hover:bg-fd-accent";

  return (
    <div ref={rootRef} className="stella-page-actions relative not-prose">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="menu"
        aria-expanded={open}
        className="inline-flex items-center gap-1.5 rounded-md border border-fd-border px-2.5 py-1.5 text-xs text-fd-muted-foreground transition-colors hover:bg-fd-accent hover:text-fd-foreground"
      >
        <Copy className="size-3.5" aria-hidden />
        Copy page
        <ChevronDown className="size-3.5" aria-hidden />
      </button>
      {open && (
        <div
          role="menu"
          className="absolute left-0 top-full z-20 mt-2 w-56 rounded-md border border-fd-border bg-fd-popover p-1"
        >
          <button type="button" role="menuitem" onClick={copyMarkdown} className={itemClass}>
            {copied ? <Check className="size-3.5" aria-hidden /> : <Copy className="size-3.5" aria-hidden />}
            {copied ? "Copied" : "Copy page as Markdown"}
          </button>
          <a
            role="menuitem"
            href={mdPath}
            target="_blank"
            rel="noopener noreferrer"
            onClick={() => setOpen(false)}
            className={itemClass}
          >
            <ExternalLink className="size-3.5" aria-hidden />
            View as Markdown
          </a>
          <a
            role="menuitem"
            href={mdPath}
            download={`${slug[slug.length - 1] ?? "index"}.md`}
            onClick={() => setOpen(false)}
            className={itemClass}
          >
            <Download className="size-3.5" aria-hidden />
            Download as Markdown
          </a>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setOpen(false);
              window.print();
            }}
            className={itemClass}
          >
            <Printer className="size-3.5" aria-hidden />
            Print / Save as PDF
          </button>
        </div>
      )}
    </div>
  );
}
