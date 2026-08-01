"use client";

import {
  CodeBlock,
  Pre,
} from "fumadocs-ui/components/codeblock";
import { type ComponentProps, useRef, useState } from "react";

/**
 * The docs' code block — a terminal, handled like the home page's install
 * block: the whole surface is the copy control (a click anywhere copies;
 * a drag-selection does not), and the success state is a floating "✓ copied!"
 * chip over the code in the bottom-right corner. The chip is also a real
 * button, so keyboard users have a focusable control and clicks on it don't
 * depend on the figure-level handler.
 *
 * Fumadocs' CodeBlock keeps everything else — titles, tabs, line numbers,
 * diff/highlight marks, the scroll viewport — with its own copy button
 * disabled and the chip mounted through the `Actions` slot, which anchors to
 * the figure, not the scroll area, so the chip floats over long code instead
 * of scrolling away with it.
 *
 * The copy text is cloned the way fumadocs' own button does it (drop
 * `.nd-copy-ignore`, take textContent) so what lands in the clipboard is
 * identical to upstream behavior.
 */
export function DocsCodeBlock({
  children,
  ...props
}: ComponentProps<typeof CodeBlock>) {
  const figureRef = useRef<HTMLElement>(null);
  const timer = useRef<number | undefined>(undefined);
  const [copied, setCopied] = useState(false);
  // Remounts the chip's inner span so the pop/draw animation replays on
  // every copy, without remounting the button (which would drop focus).
  const [pulse, setPulse] = useState(0);

  async function copy() {
    const pre = figureRef.current?.querySelector("pre");
    if (!pre) return;
    const clone = pre.cloneNode(true) as HTMLElement;
    clone
      .querySelectorAll(".nd-copy-ignore")
      .forEach((node) => node.replaceWith("\n"));
    try {
      await navigator.clipboard.writeText(clone.textContent ?? "");
    } catch {
      // Clipboard denied (permissions, insecure context). Claim nothing.
      return;
    }
    setCopied(true);
    setPulse((p) => p + 1);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(false), 2000);
  }

  return (
    <CodeBlock
      {...props}
      ref={figureRef}
      allowCopy={false}
      onClick={() => {
        // A mouseup that ends a text selection also fires click; copying
        // over someone's selection would be hostile, so only a plain click
        // (collapsed selection) copies.
        const selection = window.getSelection();
        if (selection && !selection.isCollapsed) return;
        void copy();
      }}
      Actions={() => (
        <>
          <button
            type="button"
            className="lp-code-copy"
            data-copied={copied || undefined}
            aria-label="Copy code"
            onClick={(event) => {
              event.stopPropagation();
              void copy();
            }}
          >
            <span key={pulse} className="lp-code-copy-face">
              {copied ? (
                <>
                  <svg
                    viewBox="0 0 12 12"
                    className="lp-code-copy-check"
                    aria-hidden="true"
                  >
                    <path d="M2.5 6.5 5 9l4.5-5.5" />
                  </svg>
                  copied!
                </>
              ) : (
                "copy"
              )}
            </span>
          </button>
          <span className="sr-only" aria-live="polite">
            {copied ? "Code copied" : ""}
          </span>
        </>
      )}
    >
      <Pre>{children}</Pre>
    </CodeBlock>
  );
}
