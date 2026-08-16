"use client";

import * as React from "react";
import { api } from "@/lib/api";
import { cn } from "@/lib/utils";

/**
 * The experiments gallery (#3215): every stored experiment document, one
 * card each, one click to the full document.
 *
 * Deliberately a *reading* surface with no interpretation layer: an
 * experiment document is assembled elsewhere from the ground-truth artifacts
 * it cites, and this view renders what is stored rather than recomputing
 * anything — the same "nothing here recomputes a verdict" rule the trends
 * view holds. The store is generic product surface: agents, models and
 * datasets are data inside documents, never concepts this view knows.
 *
 * A workspace that never stored an experiment shows an empty state, not an
 * error — the store is optional by contract (matches-on-disk discovery must
 * never depend on it).
 */

interface ExperimentSummary {
  id: string | null;
  title: string | null;
  status: string | null;
  schema: number | null;
  calculation_version: string | null;
}

function StatusBadge({ status }: { status: string | null }) {
  if (!status) return null;
  return (
    <span
      className={cn(
        "px-1.5 py-0.5 font-mono text-[11px]",
        status === "final" && "bg-ok/15 text-ok",
        status === "draft" && "bg-panel text-muted",
        status !== "final" && status !== "draft" && "bg-panel text-muted",
      )}
    >
      {status}
    </span>
  );
}

/**
 * The document, rendered as its own JSON. Pretty-printed rather than
 * summarized: the document's whole value is that no field was dropped
 * between the artifacts and the reader, and a bespoke renderer would be a
 * second schema for the store's deliberately schema-less phase. Structure
 * grows here when the normalization decision does (see
 * docs/experiments-normalization.md).
 */
function DocumentPane({
  id,
  document,
  error,
}: {
  id: string;
  document: unknown | null;
  error: string | null;
}) {
  if (error) {
    return <p className="text-[13px] text-bad">{error}</p>;
  }
  if (document === null) {
    return <p className="text-[13px] text-muted">loading {id}…</p>;
  }
  return (
    <pre className="overflow-x-auto border border-line bg-panel p-4 font-mono text-[12px] leading-[1.5]">
      {JSON.stringify(document, null, 2)}
    </pre>
  );
}

export function ExperimentsView({ active }: { active: boolean }) {
  const [summaries, setSummaries] = React.useState<ExperimentSummary[] | null>(null);
  const [listError, setListError] = React.useState<string | null>(null);
  const [openId, setOpenId] = React.useState<string | null>(null);
  const [documentBody, setDocumentBody] = React.useState<unknown | null>(null);
  const [documentError, setDocumentError] = React.useState<string | null>(null);
  const loadedOnce = React.useRef(false);

  // Fetch on first activation, refresh on every later one — the store can
  // gain documents while the tab sits in the background.
  React.useEffect(() => {
    if (!active) return;
    let cancelled = false;
    (async () => {
      try {
        const payload = await api<{ experiments: ExperimentSummary[] }>("/api/experiments");
        if (!cancelled) {
          setSummaries(payload.experiments);
          setListError(null);
          loadedOnce.current = true;
        }
      } catch (error) {
        if (!cancelled && !loadedOnce.current) setListError(String(error));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [active]);

  const open = React.useCallback(async (id: string) => {
    setOpenId(id);
    setDocumentBody(null);
    setDocumentError(null);
    try {
      setDocumentBody(await api<unknown>(`/api/experiments/${encodeURIComponent(id)}`));
    } catch (error) {
      setDocumentError(String(error));
    }
  }, []);

  return (
    <div className="mx-auto max-w-[1180px] px-5 py-8">
      <h1 className="text-[18px] font-semibold">Experiments</h1>
      <p className="mt-1 text-[13px] text-muted">
        Stored experiment documents — assembled from the artifacts they cite,
        rendered without recomputation.
      </p>

      {listError && <p className="mt-6 text-[13px] text-bad">{listError}</p>}
      {summaries !== null && summaries.length === 0 && (
        <p className="mt-6 text-[13px] text-muted">
          No experiments stored yet. Documents land in ~/.arenabench/experiments.db.
        </p>
      )}

      <div className="mt-6 grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {(summaries ?? []).map((summary, index) => {
          const id = summary.id ?? `(unnamed #${index})`;
          const openable = summary.id !== null;
          return (
            <button
              key={id}
              type="button"
              disabled={!openable}
              onClick={() => openable && open(summary.id as string)}
              className={cn(
                "border border-line bg-background p-4 text-left",
                openable && "cursor-pointer hover:bg-panel",
                openId === summary.id && "border-accent",
                !openable && "opacity-60",
              )}
            >
              <div className="flex items-center justify-between gap-2">
                <span className="font-mono text-[12px] text-muted">{id}</span>
                <StatusBadge status={summary.status} />
              </div>
              <div className="mt-2 text-[14px]">{summary.title ?? "(untitled)"}</div>
              {summary.calculation_version && (
                <div className="mt-2 font-mono text-[11px] text-muted">
                  calc {summary.calculation_version}
                </div>
              )}
            </button>
          );
        })}
      </div>

      {openId !== null && (
        <section className="mt-8">
          <h2 className="mb-3 font-mono text-[13px] text-muted">{openId}</h2>
          <DocumentPane id={openId} document={documentBody} error={documentError} />
        </section>
      )}
    </div>
  );
}
