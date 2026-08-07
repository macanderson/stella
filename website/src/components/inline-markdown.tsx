import { Fragment, type ReactNode } from "react";

/**
 * Render the inline marks that appear inside a changelog entry, as JSX.
 *
 * The alternative — running a markdown library and injecting its HTML string —
 * would mean `dangerouslySetInnerHTML` on text that, while ours today, is
 * written by a model in CI (`scripts/changelog-ai.sh`). Returning React nodes
 * makes an injected `<script>` structurally impossible rather than
 * conventionally unlikely, which is the right posture for machine-authored
 * prose no human is required to read before it deploys.
 *
 * Three marks, matching what the drafter's prompt tells it to emit:
 *
 *   `code`            -> <code>
 *   **bold**          -> <strong>
 *   [text](https://…) -> <a>, external only
 *
 * Anything else — underscores, nested emphasis, images, reference links — is
 * left as literal text. That is deliberate: an entry is one or two sentences of
 * plain prose by contract, and silently supporting more marks would invite
 * entries that depend on them and then render wrong somewhere else (the same
 * text also ships as a plain-text GitHub release body).
 *
 * ## The search-term highlight
 *
 * `highlight` wraps case-insensitive matches of a query in `<mark>`. It runs
 * over the *text* segments only, after the marks are split out, so a query
 * matching `code` cannot break a `<code>` element in half — the split is
 * structural, not string-level.
 */

/** `code`, **bold**, or [text](url) — alternatives ordered longest-first. */
const INLINE = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\[[^\]]+\]\(https?:\/\/[^)]+\))/g;
const LINK = /^\[([^\]]+)\]\((https?:\/\/[^)]+)\)$/;

/** Escape a user-typed query for use inside a RegExp. */
function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Split `text` on case-insensitive occurrences of `query`, wrapping each hit in
 * `<mark>`. Returns the string unchanged when there is no query, so the common
 * (unfiltered) render allocates nothing.
 */
function highlight(text: string, query: string, keyPrefix: string): ReactNode {
  if (!query) return text;
  const pattern = new RegExp(`(${escapeRegExp(query)})`, "ig");
  const parts = text.split(pattern);
  if (parts.length === 1) return text;
  return parts.map((part, i) =>
    // split() with one capture group alternates [text, hit, text, hit, …], so
    // odd indices are exactly the matches — no second comparison needed.
    i % 2 === 1 ? (
      <mark key={`${keyPrefix}-m${i}`} className="rl-hit">
        {part}
      </mark>
    ) : (
      <Fragment key={`${keyPrefix}-t${i}`}>{part}</Fragment>
    ),
  );
}

export function InlineMarkdown({
  text,
  query = "",
}: {
  text: string;
  /** Search term to highlight, if any. */
  query?: string;
}): ReactNode {
  const nodes: ReactNode[] = [];
  let cursor = 0;
  let index = 0;

  for (const match of text.matchAll(INLINE)) {
    const at = match.index ?? 0;
    if (at > cursor) {
      nodes.push(highlight(text.slice(cursor, at), query, `p${index}`));
    }

    const token = match[0];
    if (token.startsWith("`")) {
      nodes.push(
        <code key={`c${index}`} className="rl-code">
          {token.slice(1, -1)}
        </code>,
      );
    } else if (token.startsWith("**")) {
      nodes.push(
        <strong key={`b${index}`}>
          {highlight(token.slice(2, -2), query, `b${index}`)}
        </strong>,
      );
    } else {
      const link = token.match(LINK);
      // The regex only matches well-formed links, so `link` is non-null here;
      // the fallback keeps this total rather than asserting.
      nodes.push(
        link ? (
          <a
            key={`a${index}`}
            href={link[2]}
            target="_blank"
            rel="noreferrer"
            className="rl-link"
          >
            {highlight(link[1], query, `a${index}`)}
          </a>
        ) : (
          <Fragment key={`x${index}`}>{token}</Fragment>
        ),
      );
    }

    cursor = at + token.length;
    index += 1;
  }

  if (cursor < text.length) {
    nodes.push(highlight(text.slice(cursor), query, `p${index}`));
  }

  return <>{nodes}</>;
}
