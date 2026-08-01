"use client";

import { useRef, useState } from "react";

/**
 * The copyable install one-liner.
 *
 * One stretched, invisible button covers the whole block, so a click anywhere
 * copies the command and keyboard/screen-reader users get exactly one control
 * ("Copy install command"). The corner chip is purely visual state — it is
 * aria-hidden, and the announcement rides a polite live region instead.
 *
 * Every successful copy fires a beacon at /api/hit. Together with the
 * download counter behind /install.sh this yields the two numbers worth
 * having: how many people copied the command, and how many actually ran it.
 * Counting is fire-and-forget — it must never delay or break the copy.
 */
export function InstallBlock({ command }: { command: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);

  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
    } catch {
      // Clipboard denied (permissions, insecure context). Claim nothing.
      return;
    }
    setCopied(true);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(false), 2000);
    try {
      navigator.sendBeacon("/api/hit");
    } catch {
      // Counting is best-effort by design.
    }
  }

  return (
    <div className="lp-install">
      <div className="term">
        <pre className="term-body">
          <span className="term-prompt">$ </span>
          {command}
        </pre>
      </div>
      <button
        type="button"
        className="lp-install-hit"
        aria-label="Copy install command"
        onClick={() => void copy()}
      />
      <span className="lp-install-chip" aria-hidden="true">
        {copied ? "✓ copied" : "copy"}
      </span>
      <span className="sr-only" aria-live="polite">
        {copied ? "Install command copied" : ""}
      </span>
    </div>
  );
}
