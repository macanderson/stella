/**
 * Reading tools for the transcript page: the errors-only filter, and the raw
 * exchange behind the inspector drawer.
 *
 * Pure functions over the wire shapes, in their own module for the same reason
 * `diff-view.ts` and `word-highlight.ts` are: so `scripts/check-transcript-view.mjs`
 * can drive them from a bare checkout with no Next.js, no DOM and no
 * `npm install`. A page this dense earns tests, and the parts worth testing are
 * the decisions — which rows survive a filter, where a JSON fold ends — not the
 * JSX around them.
 */

import type { TranscriptEntry } from "./types";

/** A tool result the engine reported as failed. */
export function isFailedResult(entry: TranscriptEntry): boolean {
  return entry.kind === "tool_result" && Boolean(entry.meta?.error);
}

/**
 * The `seq`s an errors-only view keeps.
 *
 * Not simply "every entry with an error flag". A failed result read alone is
 * half a story — it names the tool and the message and says nothing about what
 * was *asked*, which is the first thing a reader wants and the only thing that
 * makes the failure actionable. So a kept result drags its own call back in by
 * `call_id`, the identifier both halves already share.
 *
 * `error` entries (the engine's own, not a tool's) are kept on their own: they
 * have no call to pair with.
 *
 * Returns a set rather than a filtered list so the caller can compose this with
 * the kind-group and tool-name filters instead of having to order them.
 */
export function erroredSeqs(entries: readonly TranscriptEntry[]): Set<number> {
  const kept = new Set<number>();
  const failedCalls = new Set<string>();

  for (const entry of entries) {
    if (entry.kind === "error") {
      kept.add(entry.seq);
      continue;
    }
    if (!isFailedResult(entry)) continue;
    kept.add(entry.seq);
    const callId = entry.meta?.call_id;
    if (typeof callId === "string" && callId) failedCalls.add(callId);
  }

  if (failedCalls.size > 0) {
    for (const entry of entries) {
      if (entry.kind !== "tool") continue;
      const callId = entry.meta?.call_id;
      if (typeof callId === "string" && failedCalls.has(callId)) kept.add(entry.seq);
    }
  }
  return kept;
}

/** One side of the inspector drawer. */
export type RawPane = {
  /** What this side is, said plainly — it is the drawer's only label. */
  label: string;
  text: string;
  /** Whether the text parsed as JSON, which is what earns the fold gutter. */
  json: boolean;
  /**
   * Set when the body reached the browser middle-elided by the streaming
   * channel's character budget, so the pane can say so instead of presenting a
   * truncated payload as the whole one. The elided bytes were never sent, so
   * the only way back is the full-transcript fetch.
   */
  capped: boolean;
};

/** A call and its result, as the drawer shows them. */
export type RawExchange = {
  /** The call id both halves share, or `""` for an unpaired entry. */
  callId: string;
  title: string;
  request: RawPane | null;
  response: RawPane | null;
};

/** `cap_middle`'s marker (`arenabench.toolout`), which is how a body says it
 *  was clipped on the way here. */
const ELISION = /… \d+ (?:characters|chars) elided …|\[\.{3}elided\.{3}\]/;

function looksCapped(text: string): boolean {
  return ELISION.test(text) || text.includes("… elided …");
}

/**
 * The raw request/response behind one entry, or `null` when it has neither.
 *
 * Defined for **tool exchanges** and nothing else, because that is where
 * "request and response body" is a true description rather than a metaphor: a
 * call carries the argument object Stella sent, and its result carries what
 * came back. Opening the same drawer on a `usage` or `verdict` row would put
 * its own summary in both panes and call one of them a request.
 *
 * Either half may be absent and that is reported rather than faked: a call
 * still streaming has no result yet, and a result whose call scrolled past the
 * stream's cursor has no arguments. A pane that is `null` is a pane the drawer
 * says is not there.
 */
export function rawExchange(
  entry: TranscriptEntry,
  byCallId: ReadonlyMap<string, { call?: TranscriptEntry; result?: TranscriptEntry }>,
): RawExchange | null {
  if (entry.kind !== "tool" && entry.kind !== "tool_result") return null;
  const callId = typeof entry.meta?.call_id === "string" ? entry.meta.call_id : "";
  const pair = callId ? byCallId.get(callId) : undefined;
  const call = pair?.call ?? (entry.kind === "tool" ? entry : undefined);
  const result = pair?.result ?? (entry.kind === "tool_result" ? entry : undefined);

  const raw = typeof call?.meta?.raw === "string" ? call.meta.raw : "";
  const request: RawPane | null =
    raw && raw !== "{}"
      ? { label: "request", text: raw, json: isJson(raw), capped: looksCapped(raw) }
      : null;

  const body = result?.body ?? "";
  const response: RawPane | null = result
    ? {
        label: result.meta?.error ? "response · failed" : "response",
        text: body,
        json: isJson(body),
        capped: looksCapped(body),
      }
    : null;

  if (!request && !response) return null;
  const name = call?.title ?? result?.title ?? "tool";
  return {
    callId,
    title: callId ? `${name} · ${callId}` : name,
    request,
    response,
  };
}

/** Index every tool call and result by the `call_id` the two share. */
export function indexByCallId(
  entries: readonly TranscriptEntry[],
): Map<string, { call?: TranscriptEntry; result?: TranscriptEntry }> {
  const index = new Map<string, { call?: TranscriptEntry; result?: TranscriptEntry }>();
  for (const entry of entries) {
    if (entry.kind !== "tool" && entry.kind !== "tool_result") continue;
    const callId = entry.meta?.call_id;
    if (typeof callId !== "string" || !callId) continue;
    const slot = index.get(callId) ?? {};
    if (entry.kind === "tool") slot.call = entry;
    else slot.result = entry;
    index.set(callId, slot);
  }
  return index;
}

function isJson(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed.startsWith("{") && !trimmed.startsWith("[")) return false;
  try {
    JSON.parse(trimmed);
    return true;
  } catch {
    return false;
  }
}

// -- JSON rendering -------------------------------------------------------

/** What a token is, which is what decides its colour. */
export type JsonTokenKind =
  | "key"
  | "string"
  | "number"
  | "boolean"
  | "null"
  | "punct"
  | "plain";

export type JsonToken = { text: string; kind: JsonTokenKind };

export type JsonLine = {
  /** 1-based, so it is the number printed in the gutter. */
  no: number;
  /** Nesting depth, for the indent. Carried rather than baked into the tokens
   *  so a fold can render its own indent without re-measuring leading spaces. */
  depth: number;
  tokens: JsonToken[];
  /**
   * For a line that opens a container, the 1-based number of the line holding
   * its close. `undefined` on every other line. This is what the fold control
   * hangs off, and computing it here rather than in the component is why the
   * fold can be tested without a DOM.
   */
  closesAt?: number;
};

const INDENT = "  ";

/**
 * Pretty-print a payload as numbered, foldable, tokenised lines.
 *
 * Written against the parsed value rather than by re-lexing
 * `JSON.stringify(…, 2)`, for two reasons a reader of this drawer will hit
 * immediately: a key and a string value are the same token to a lexer and
 * different colours to an eye, and the close-line of every container has to be
 * known to the line that opens it, which is bookkeeping a lexer would have to
 * redo.
 *
 * Anything that is not JSON — a bash tool's stdout, a stack trace, prose —
 * comes back as plain lines with the same numbering, so the gutter, the search
 * and the copy control work identically on both sides of the drawer. That is
 * the common case for the response pane and it must not look like a failure.
 */
export function formatJson(text: string): JsonLine[] {
  const trimmed = text.trim();
  if (!isJson(trimmed)) return plainLines(text);
  let value: unknown;
  try {
    value = JSON.parse(trimmed);
  } catch {
    return plainLines(text);
  }

  const lines: JsonLine[] = [];
  const push = (depth: number, tokens: JsonToken[]): number => {
    lines.push({ no: lines.length + 1, depth, tokens });
    return lines.length - 1;
  };

  /** Emit `value`, prefixing the first line with `prefix` and suffixing the
   *  last with `suffix` (the trailing comma a parent needs). */
  const walk = (
    node: unknown,
    depth: number,
    prefix: JsonToken[],
    suffix: JsonToken[],
  ): void => {
    if (Array.isArray(node)) {
      if (node.length === 0) {
        push(depth, [...prefix, punct("[]"), ...suffix]);
        return;
      }
      const opened = push(depth, [...prefix, punct("[")]);
      node.forEach((item, i) => {
        walk(item, depth + 1, [], i === node.length - 1 ? [] : [punct(",")]);
      });
      const closed = push(depth, [punct("]"), ...suffix]);
      lines[opened].closesAt = closed + 1;
      return;
    }
    if (node !== null && typeof node === "object") {
      const keys = Object.keys(node as Record<string, unknown>);
      if (keys.length === 0) {
        push(depth, [...prefix, punct("{}"), ...suffix]);
        return;
      }
      const opened = push(depth, [...prefix, punct("{")]);
      keys.forEach((key, i) => {
        walk(
          (node as Record<string, unknown>)[key],
          depth + 1,
          [{ text: JSON.stringify(key), kind: "key" }, punct(": ")],
          i === keys.length - 1 ? [] : [punct(",")],
        );
      });
      const closed = push(depth, [punct("}"), ...suffix]);
      lines[opened].closesAt = closed + 1;
      return;
    }
    push(depth, [...prefix, scalar(node), ...suffix]);
  };

  walk(value, 0, [], []);

  // A multi-line string inside JSON is the single most common thing a reader
  // opens this drawer for — a bash tool's `command`, a file's `content` — and
  // `JSON.stringify` renders it as one enormous line of `\n` escapes. Split it
  // across real lines, keeping the escapes visible, so the gutter numbers
  // something a human can point at.
  return expandEmbeddedNewlines(lines);
}

function punct(text: string): JsonToken {
  return { text, kind: "punct" };
}

function scalar(node: unknown): JsonToken {
  if (node === null) return { text: "null", kind: "null" };
  if (typeof node === "string") return { text: JSON.stringify(node), kind: "string" };
  if (typeof node === "number") return { text: String(node), kind: "number" };
  if (typeof node === "boolean") return { text: String(node), kind: "boolean" };
  return { text: JSON.stringify(node) ?? "null", kind: "plain" };
}

/**
 * Break any token carrying a literal `\n` escape across real lines.
 *
 * Renumbers everything after, and moves each `closesAt` by the number of lines
 * inserted before it — the bookkeeping that makes this safe to do as a pass
 * rather than inside `walk`, where every recursion level would have to know how
 * many lines its children grew by.
 */
function expandEmbeddedNewlines(lines: JsonLine[]): JsonLine[] {
  if (!lines.some((line) => line.tokens.some((t) => t.text.includes("\\n")))) {
    return lines;
  }
  const out: JsonLine[] = [];
  /** old 1-based line number -> new 1-based line number. */
  const moved = new Map<number, number>();

  for (const line of lines) {
    moved.set(line.no, out.length + 1);
    const pieces: JsonToken[][] = [[]];
    for (const token of line.tokens) {
      if (token.kind !== "string" || !token.text.includes("\\n")) {
        pieces[pieces.length - 1].push(token);
        continue;
      }
      // Keep the `\n` on the line it ends, so the escape is visible and the
      // text still round-trips by eye to the JSON above it.
      const parts = token.text.split("\\n");
      parts.forEach((part, i) => {
        const last = i === parts.length - 1;
        pieces[pieces.length - 1].push({
          text: last ? part : `${part}\\n`,
          kind: "string",
        });
        if (!last) pieces.push([]);
      });
    }
    pieces.forEach((tokens, i) => {
      out.push({
        no: out.length + 1,
        // A continuation is indented one past its opener, so the wrapped body
        // of a `command` reads as belonging to it rather than to the object.
        depth: i === 0 ? line.depth : line.depth + 1,
        tokens,
        closesAt: i === 0 ? line.closesAt : undefined,
      });
    });
  }

  for (const line of out) {
    if (line.closesAt !== undefined) {
      line.closesAt = moved.get(line.closesAt) ?? line.closesAt;
    }
  }
  return out;
}

function plainLines(text: string): JsonLine[] {
  const raw = text.split("\n");
  if (raw.length > 1 && raw[raw.length - 1] === "") raw.pop();
  return raw.map((line, i) => ({
    no: i + 1,
    depth: 0,
    tokens: [{ text: line, kind: "plain" as const }],
  }));
}

/**
 * `indent + every token's text` — one line as a searcher sees it.
 *
 * The search must match what is *rendered*, not the source: a folded object and
 * an expanded one show different text, and a key is `"path"` on screen with its
 * quotes. Both the match count and the per-token highlight go through this, so
 * they cannot disagree about what a line says.
 */
export function lineText(line: JsonLine): string {
  return INDENT.repeat(line.depth) + line.tokens.map((t) => t.text).join("");
}

/**
 * Line numbers a fold hides: everything strictly between an opener and its
 * close, for every opener in `folded`.
 *
 * Nested folds compose by union rather than by nesting, which is what makes a
 * collapsed parent hide a collapsed child without the caller tracking
 * containment — closing an outer object while an inner one is also closed must
 * not reopen the inner one when the outer reopens.
 */
export function hiddenLines(
  lines: readonly JsonLine[],
  folded: ReadonlySet<number>,
): Set<number> {
  const hidden = new Set<number>();
  for (const line of lines) {
    if (line.closesAt === undefined || !folded.has(line.no)) continue;
    for (let n = line.no + 1; n < line.closesAt; n += 1) hidden.add(n);
  }
  return hidden;
}

/**
 * Every opener line number, so "collapse all" has something to collapse.
 *
 * The root is excluded: folding it leaves a pane showing `{` and nothing else,
 * which is a control that can only be regretted.
 */
export function foldableLines(lines: readonly JsonLine[]): number[] {
  return lines
    .filter((line) => line.closesAt !== undefined && line.depth > 0)
    .map((line) => line.no);
}
