/**
 * A tool call's visual class — the six categories the Command Deck paints a
 * tool NAME by (`crates/stella-tui/src/tool_class.rs`): read, write, run,
 * verify, repo, delegate.
 *
 * The classification itself happens server-side (`arenabench.toolclass`,
 * itself a port of the same Rust module) and rides on `tool`/`tool_result`
 * entries as `meta.tool_class` / `meta.tool_class_label` — this module only
 * maps the six class keys to the Tailwind colour token defined in
 * `app/globals.css` (`--tool-*`), so the legend can list every class even
 * when the loaded transcript only exercises a few of them.
 */

export const TOOL_CLASSES = [
  "inspect",
  "mutate",
  "execute",
  "verify",
  "repo",
  "delegate",
] as const;

export type ToolClassKey = (typeof TOOL_CLASSES)[number];

/** One-word legend label — matches `ToolClass::label()` / `class_label()`. */
export const TOOL_CLASS_LABEL: Record<ToolClassKey, string> = {
  inspect: "read",
  mutate: "write",
  execute: "run",
  verify: "verify",
  repo: "repo",
  delegate: "delegate",
};

/** The Tailwind text-colour utility for a class, from `--tool-*` in globals.css. */
export const TOOL_CLASS_TEXT: Record<ToolClassKey, string> = {
  inspect: "text-tool-inspect",
  mutate: "text-tool-mutate",
  execute: "text-tool-execute",
  verify: "text-tool-verify",
  repo: "text-tool-repo",
  delegate: "text-tool-delegate",
};

/** Read a `tool_class` out of an entry's `meta`, tolerating older transcripts
 * that predate the field (a "load full transcript" fetch against an already
 * open trial, a saved recording) — those fall back to the deck's own
 * unrecognized-tool default, `execute`, rather than rendering unstyled. */
export function toolClassOf(meta: Record<string, unknown> | undefined): ToolClassKey {
  const cls = meta?.tool_class;
  return typeof cls === "string" && (TOOL_CLASSES as readonly string[]).includes(cls)
    ? (cls as ToolClassKey)
    : "execute";
}
