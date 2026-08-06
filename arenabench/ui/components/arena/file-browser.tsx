"use client";

import * as React from "react";
import { cn } from "@/lib/utils";

/**
 * The evidence trail for one trial, browsable in place.
 *
 * A trial directory holds the result, the agent's trajectory and event stream,
 * the verifier's output and the recording. Being able to open any of it is the
 * difference between trusting a scoreboard and being able to check it, so this
 * is a plain tree over whatever the run actually produced rather than a curated
 * set of links that goes stale when a new artifact appears.
 *
 * Loading is lazy in both directions: the tree is fetched when the browser is
 * first opened, and a file's contents only when it is selected. An event stream
 * can be hundreds of megabytes, and eager fetching would make opening a lane
 * expensive for the common case where nobody looks.
 */

export type FileNode = {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  kind: string;
  children?: FileNode[];
};

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const KIND_GLYPH: Record<string, string> = {
  dir: "▸",
  text: "·",
  video: "▶",
  image: "▪",
  binary: "◆",
  symlink: "↗",
  skipped: "⋯",
};

function Row({
  node,
  depth,
  selected,
  onSelect,
}: {
  node: FileNode;
  depth: number;
  selected: string | null;
  onSelect: (node: FileNode) => void;
}) {
  // Directories near the root start open; deeper ones do not, so a tree with a
  // nested workspace in it does not unfold into a wall on first render.
  const [open, setOpen] = React.useState(depth < 1);

  if (node.is_dir) {
    const empty = !node.children || node.children.length === 0;
    return (
      <div>
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          disabled={empty}
          className={cn(
            "flex w-full items-center gap-1.5 rounded px-1 py-[3px] text-left",
            "hover:bg-surface-2 disabled:opacity-50 disabled:hover:bg-transparent",
          )}
          style={{ paddingLeft: `${depth * 12 + 4}px` }}
        >
          <span className="w-3 flex-none text-dim">{open && !empty ? "▾" : "▸"}</span>
          <span className="truncate text-foreground">{node.name}</span>
          {node.kind === "skipped" && (
            <span className="ml-1 flex-none text-[10px] text-dim">not indexed</span>
          )}
        </button>
        {open &&
          node.children?.map((child) => (
            <Row
              key={child.path}
              node={child}
              depth={depth + 1}
              selected={selected}
              onSelect={onSelect}
            />
          ))}
      </div>
    );
  }

  return (
    <button
      type="button"
      onClick={() => onSelect(node)}
      className={cn(
        "flex w-full items-center gap-1.5 rounded px-1 py-[3px] text-left hover:bg-surface-2",
        selected === node.path && "bg-surface-2 text-accent",
      )}
      style={{ paddingLeft: `${depth * 12 + 4}px` }}
    >
      <span className="w-3 flex-none text-dim">{KIND_GLYPH[node.kind] ?? "·"}</span>
      <span className="min-w-0 flex-1 truncate">{node.name}</span>
      <span className="flex-none text-[10px] text-dim">{fmtSize(node.size)}</span>
    </button>
  );
}

function Viewer({ base, node }: { base: string; node: FileNode }) {
  const [text, setText] = React.useState<string | null>(null);
  const [truncated, setTruncated] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const url = `${base}?path=${encodeURIComponent(node.path)}`;

  React.useEffect(() => {
    if (node.kind !== "text") return;
    let live = true;
    setText(null);
    setError(null);
    fetch(url)
      .then(async (res) => {
        if (!res.ok) throw new Error(`${res.status}`);
        setTruncated(res.headers.get("X-Arena-Truncated") === "1");
        return res.text();
      })
      .then((body) => live && setText(body))
      .catch((err) => live && setError(String(err)));
    return () => {
      live = false;
    };
  }, [url, node.kind]);

  if (node.kind === "video") {
    return (
      <video controls preload="none" className="block w-full rounded-[7px] bg-black" src={url} />
    );
  }
  if (node.kind === "image") {
    // eslint-disable-next-line @next/next/no-img-element
    return <img alt={node.name} src={url} className="block max-w-full rounded-[7px]" />;
  }
  if (node.kind !== "text") {
    return (
      <div className="px-2 py-3 text-[11px] text-muted">
        Not previewable in the browser.{" "}
        <a className="text-accent underline" href={url} download={node.name}>
          Download {node.name}
        </a>
      </div>
    );
  }
  if (error) return <div className="px-2 py-3 text-[11px] text-bad">could not read: {error}</div>;
  if (text === null) return <div className="px-2 py-3 text-[11px] text-dim">reading…</div>;
  return (
    <div>
      {truncated && (
        <div className="border-b border-line-soft px-2 py-1 text-[10px] text-warn">
          showing the first 2 MB — open the file on disk for the rest
        </div>
      )}
      <pre className="max-h-[46vh] overflow-auto px-2 py-2 font-mono text-[11px] leading-[1.55] whitespace-pre-wrap break-words">
        {text}
      </pre>
    </div>
  );
}

export function FileBrowser({
  matchId,
  contestantId,
  task,
}: {
  matchId: string;
  contestantId: string;
  task: string;
}) {
  const [root, setRoot] = React.useState<FileNode | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [selected, setSelected] = React.useState<FileNode | null>(null);

  const seg = `${encodeURIComponent(matchId)}/${encodeURIComponent(contestantId)}/${encodeURIComponent(task)}`;
  const treeUrl = `/api/matches/${encodeURIComponent(matchId)}/files/${encodeURIComponent(contestantId)}/${encodeURIComponent(task)}`;
  const fileBase = `/api/matches/${encodeURIComponent(matchId)}/file/${encodeURIComponent(contestantId)}/${encodeURIComponent(task)}`;

  React.useEffect(() => {
    let live = true;
    setRoot(null);
    setError(null);
    setSelected(null);
    fetch(treeUrl)
      .then(async (res) => {
        if (!res.ok) throw new Error(res.status === 404 ? "no artifacts yet" : `${res.status}`);
        return res.json();
      })
      .then((node: FileNode) => live && setRoot(node))
      .catch((err) => live && setError(String(err.message ?? err)));
    return () => {
      live = false;
    };
  }, [treeUrl, seg]);

  if (error) return <div className="px-2 py-3 text-[11px] text-muted">{error}</div>;
  if (!root) return <div className="px-2 py-3 text-[11px] text-dim">loading files…</div>;

  return (
    <div className="grid grid-cols-[minmax(150px,34%)_1fr] gap-2">
      <div className="max-h-[46vh] overflow-auto rounded-[7px] border border-line-soft p-1 font-mono text-[11px]">
        {root.children?.length ? (
          root.children.map((child) => (
            <Row
              key={child.path}
              node={child}
              depth={0}
              selected={selected?.path ?? null}
              onSelect={setSelected}
            />
          ))
        ) : (
          <div className="px-2 py-3 text-dim">no files yet</div>
        )}
      </div>
      <div className="min-w-0 rounded-[7px] border border-line-soft">
        {selected ? (
          <>
            <div className="flex items-center gap-2 border-b border-line-soft px-2 py-1">
              <span className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-muted">
                {selected.path}
              </span>
              <a
                className="flex-none text-[10px] text-accent underline"
                href={`${fileBase}?path=${encodeURIComponent(selected.path)}`}
                download={selected.name}
              >
                download
              </a>
            </div>
            <Viewer base={fileBase} node={selected} />
          </>
        ) : (
          <div className="px-2 py-3 text-[11px] text-dim">select a file</div>
        )}
      </div>
    </div>
  );
}
