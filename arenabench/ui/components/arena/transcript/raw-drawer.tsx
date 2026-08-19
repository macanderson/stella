"use client";

import * as React from "react";

import {
  foldableLines,
  formatJson,
  hiddenLines,
  lineText,
  type JsonLine,
  type JsonTokenKind,
  type RawExchange,
  type RawPane,
} from "@/lib/transcript-view";
import { cn } from "@/lib/utils";

/**
 * The raw exchange behind one tool call: request on the left, response on the
 * right, in a drawer along the bottom of the page.
 *
 * **Why a bottom drawer and not a modal.** The transcript stays on screen and
 * stays scrollable while this is open, because the reason anyone opens it is to
 * check the payload *against* the row above — an overlay that hides the feed
 * turns one question into two round trips. It is also why the two payloads are
 * side by side rather than tabbed: "what did we send, and what came back" is
 * one comparison, and a tab makes the reader hold half of it in their head.
 *
 * Each side is independent in the ways a reader needs it to be — its own
 * search, its own folds, its own scroll — because the two are rarely the same
 * size or the same shape. A shared search box would be a search over two
 * documents reporting one count.
 */
export function RawDrawer({
  exchange,
  onClose,
}: {
  exchange: RawExchange;
  onClose: () => void;
}) {
  // Escape closes, which is the only keyboard affordance a drawer owes and the
  // one every reader tries first.
  React.useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="tx-drawer h-[42vh]" role="region" aria-label="raw request and response">
      <div className="flex items-center gap-3 border-b border-line px-3 py-1.5">
        <span className="text-[11px] font-semibold">raw exchange</span>
        <span className="min-w-0 truncate text-[11px] text-dim">{exchange.title}</span>
        <button
          type="button"
          onClick={onClose}
          aria-label="close the raw exchange"
          className="ml-auto cursor-pointer px-2 text-[11px] text-dim hover:text-foreground"
        >
          ✕ close
        </button>
      </div>
      <div className="tx-panes">
        <Pane pane={exchange.request} absent="this call recorded no arguments" />
        <Pane
          pane={exchange.response}
          absent="no result yet — the call is still in flight"
        />
      </div>
    </div>
  );
}

/**
 * One side of the drawer.
 *
 * `absent` is a sentence rather than an empty box on purpose: "there is no
 * result because the call has not returned" and "there is a result and it was
 * empty" are different facts about a run, and a blank pane says the second when
 * it means the first.
 */
function Pane({ pane, absent }: { pane: RawPane | null; absent: string }) {
  const [query, setQuery] = React.useState("");
  const [folded, setFolded] = React.useState<Set<number>>(() => new Set());

  const lines = React.useMemo(() => (pane ? formatJson(pane.text) : []), [pane]);
  const hidden = React.useMemo(() => hiddenLines(lines, folded), [lines, folded]);

  // Reset the view when the drawer is pointed at a different payload —
  // otherwise line 40 stays folded over a body that has no line 40.
  React.useEffect(() => {
    setFolded(new Set());
    setQuery("");
  }, [pane?.text]);

  const matches = React.useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return 0;
    return lines.filter((line) => lineText(line).toLowerCase().includes(needle)).length;
  }, [lines, query]);

  const toggleFold = React.useCallback((no: number) => {
    setFolded((state) => {
      const next = new Set(state);
      if (next.has(no)) next.delete(no);
      else next.add(no);
      return next;
    });
  }, []);

  const collapsible = React.useMemo(() => foldableLines(lines), [lines]);

  if (!pane) {
    return (
      <div className="tx-pane">
        <div className="border-b border-line px-3 py-1.5 text-[11px] text-dim">—</div>
        <div className="tx-pane-body p-3 text-[11px] text-dim">{absent}</div>
      </div>
    );
  }

  return (
    <div className="tx-pane">
      <div className="flex flex-wrap items-center gap-2 border-b border-line px-3 py-1.5">
        <span className="text-[11px] font-semibold">{pane.label}</span>
        <span className="text-[10.5px] text-dim">
          {pane.json ? "json" : "text"} · {lines.length}{" "}
          {lines.length === 1 ? "line" : "lines"}
        </span>
        {/* The one thing a pane must never do is present a clipped payload as
            the whole one. The elided bytes were never sent, so no control here
            can recover them — only the full-transcript fetch can. */}
        {pane.capped && (
          <span className="text-[10.5px] text-warn">
            clipped on the wire — use “load full transcript”
          </span>
        )}
        {collapsible.length > 0 && (
          <button
            type="button"
            onClick={() =>
              setFolded((state) =>
                state.size > 0 ? new Set() : new Set(collapsible),
              )
            }
            className="cursor-pointer text-[10.5px] text-dim hover:text-foreground"
          >
            {folded.size > 0 ? "expand all" : "collapse all"}
          </button>
        )}
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.stopPropagation();
              setQuery("");
            }
          }}
          placeholder={`search the ${pane.label.split(" ")[0]}…`}
          aria-label={`search the ${pane.label}`}
          className="ml-auto w-[150px] border border-line bg-panel px-2 py-0.5 text-[11px]"
        />
        {query.trim() && (
          <span className="text-[10.5px] text-dim">
            {matches} {matches === 1 ? "line" : "lines"}
          </span>
        )}
      </div>
      <div className="tx-pane-body">
        {lines.map((line) =>
          hidden.has(line.no) ? null : (
            <Line
              key={line.no}
              line={line}
              query={query.trim()}
              folded={folded.has(line.no)}
              onToggle={() => toggleFold(line.no)}
            />
          ),
        )}
      </div>
    </div>
  );
}

const TOKEN_CLASS: Record<JsonTokenKind, string> = {
  key: "tx-jkey",
  string: "tx-jstring",
  number: "tx-jnumber",
  boolean: "tx-jboolean",
  null: "tx-jnull",
  punct: "tx-jpunct",
  plain: "tx-jplain",
};

function Line({
  line,
  query,
  folded,
  onToggle,
}: {
  line: JsonLine;
  query: string;
  folded: boolean;
  onToggle: () => void;
}) {
  const foldable = line.closesAt !== undefined;
  return (
    <div className="tx-jline">
      <span className="tx-jno">{line.no}</span>
      {foldable ? (
        <button
          type="button"
          className="tx-jfold"
          onClick={onToggle}
          aria-expanded={!folded}
          aria-label={folded ? `expand line ${line.no}` : `collapse line ${line.no}`}
        >
          {folded ? "▸" : "▾"}
        </button>
      ) : (
        <span className="tx-jfold" aria-hidden />
      )}
      <span className="tx-jcode">
        {/* Indent as real spaces rather than padding: the pane wraps long
            lines, and a padded indent puts the wrapped remainder under the
            gutter instead of under the text it continues. */}
        {"  ".repeat(line.depth)}
        {line.tokens.map((token, i) => (
          <span key={i} className={TOKEN_CLASS[token.kind]}>
            <Mark text={token.text} query={query} />
          </span>
        ))}
        {folded && (
          <button
            type="button"
            onClick={onToggle}
            className="cursor-pointer pl-1 text-[10.5px] text-dim hover:text-foreground"
          >
            ⋯ {(line.closesAt ?? line.no) - line.no - 1} hidden
          </button>
        )}
      </span>
    </div>
  );
}

/** Case-insensitive highlight, keeping the original casing. A local copy rather
 *  than the page's, so the drawer can be lifted out on its own. */
function Mark({ text, query }: { text: string; query: string }) {
  if (!query) return <>{text}</>;
  const needle = query.toLowerCase();
  const parts: React.ReactNode[] = [];
  let rest = text;
  let key = 0;
  for (;;) {
    const at = rest.toLowerCase().indexOf(needle);
    if (at === -1) break;
    if (at > 0) parts.push(rest.slice(0, at));
    parts.push(
      <mark key={key++} className={cn("bg-accent text-on-accent")}>
        {rest.slice(at, at + query.length)}
      </mark>,
    );
    rest = rest.slice(at + query.length);
  }
  parts.push(rest);
  return <>{parts}</>;
}
