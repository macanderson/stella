"use client";

import { useDeferredValue, useId, useMemo, useState } from "react";
import { InlineMarkdown } from "@/components/inline-markdown";
import { CHANGE_KINDS, type ChangeKind, type Release } from "@/lib/changelog";
import { REPO_URL } from "@/lib/site";

/**
 * The interactive half of /releases: search, filter by kind, expand a line.
 *
 * All state is client-side over data the server already parsed — the whole
 * changelog is a few dozen entries, so filtering it is a `filter()` per
 * keystroke rather than anything that needs a request. Sending it once and
 * querying it locally is what makes the search feel instant; paginating it
 * would add latency to a dataset that fits in a single response.
 *
 * `useDeferredValue` on the query keeps typing responsive if the list ever
 * grows past what a synchronous re-render can absorb: React renders the stale
 * list at high priority and the fresh one at low, so keystrokes never queue
 * behind a filter pass.
 *
 * ## Why the filter is on entries, not releases
 *
 * A reader searching "cache" wants the four bullets that mention caching, not
 * the three releases that contain them. So the query filters ENTRIES, a release
 * disappears when none of its entries survive, and the surviving entries are
 * the only ones drawn — the release card becomes a heading over its own hits.
 * Result counts are stated in entries for the same reason.
 */

/**
 * Per-kind accent. Hue is never the only cue — each chip and rail also carries
 * the kind's word — because the brand kit's one-color law makes functional hue
 * a supporting signal, not the message. `security` deliberately borrows the
 * danger token: it is the one kind a reader must not skim past.
 */
const KIND_TOKEN: Record<ChangeKind, string> = {
  Added: "var(--stella-gold)",
  Changed: "var(--stella-gold-600)",
  Fixed: "var(--stella-success)",
  Removed: "var(--stella-neutral-500)",
  Security: "var(--stella-danger)",
};

/** A version as a URL fragment: `0.7.0` -> `v0-7-0`. */
function anchorFor(version: string): string {
  return `v${version.replace(/\./g, "-")}`;
}

interface FilteredRelease {
  release: Release;
  entries: Release["entries"];
}

export function ReleasesBrowser({ releases }: { releases: Release[] }) {
  const [rawQuery, setRawQuery] = useState("");
  const [kinds, setKinds] = useState<ReadonlySet<ChangeKind>>(new Set());
  // A release is open unless the reader collapsed it. Tracking the CLOSED set
  // (rather than the open one) is what makes "everything expanded" the default
  // without seeding state from props — which would go stale if `releases` ever
  // changed under a client-side navigation.
  const [closed, setClosed] = useState<ReadonlySet<string>>(new Set());

  const query = useDeferredValue(rawQuery);
  const searchId = useId();

  /** Total entries per kind across every release — the chip counts. */
  const totals = useMemo(() => {
    const counts = new Map<ChangeKind, number>();
    for (const release of releases) {
      for (const entry of release.entries) {
        counts.set(entry.kind, (counts.get(entry.kind) ?? 0) + 1);
      }
    }
    return counts;
  }, [releases]);

  const filtered = useMemo<FilteredRelease[]>(() => {
    const needle = query.trim().toLowerCase();
    const result: FilteredRelease[] = [];
    for (const release of releases) {
      const entries = release.entries.filter(
        (entry) =>
          (kinds.size === 0 || kinds.has(entry.kind)) &&
          (needle === "" || entry.haystack.includes(needle)),
      );
      if (entries.length > 0) result.push({ release, entries });
    }
    return result;
  }, [releases, query, kinds]);

  const shown = filtered.reduce((n, r) => n + r.entries.length, 0);
  const total = releases.reduce((n, r) => n + r.entries.length, 0);
  const isFiltered = shown !== total;

  const toggleKind = (kind: ChangeKind) => {
    setKinds((prev) => {
      const next = new Set(prev);
      if (next.has(kind)) next.delete(kind);
      else next.add(kind);
      return next;
    });
  };

  const toggleOpen = (version: string) => {
    setClosed((prev) => {
      const next = new Set(prev);
      if (next.has(version)) next.delete(version);
      else next.add(version);
      return next;
    });
  };

  const reset = () => {
    setRawQuery("");
    setKinds(new Set());
  };

  return (
    <div className="rl-browser">
      <div className="rl-controls">
        <div className="rl-search">
          <label className="rl-search-label" htmlFor={searchId}>
            search releases
          </label>
          <input
            id={searchId}
            className="rl-search-input"
            type="search"
            value={rawQuery}
            onChange={(event) => setRawQuery(event.target.value)}
            placeholder="cache, witness, sandbox, #1867…"
            autoComplete="off"
            spellCheck={false}
          />
        </div>

        <div className="rl-chips" role="group" aria-label="filter by kind">
          {CHANGE_KINDS.filter((kind) => totals.has(kind)).map((kind) => {
            const active = kinds.has(kind);
            return (
              <button
                key={kind}
                type="button"
                className="rl-chip"
                data-active={active}
                aria-pressed={active}
                onClick={() => toggleKind(kind)}
                style={{ "--rl-kind": KIND_TOKEN[kind] } as React.CSSProperties}
              >
                <span className="rl-chip-dot" aria-hidden="true" />
                {kind.toLowerCase()}
                <span className="rl-chip-count">{totals.get(kind)}</span>
              </button>
            );
          })}
        </div>

        {/* aria-live so a screen reader hears the count change as the query is
            typed; the visual count is the same node, never a parallel one. */}
        <p className="rl-count" role="status" aria-live="polite">
          {shown === 0
            ? "no matching changes"
            : `${shown} change${shown === 1 ? "" : "s"}${isFiltered ? ` of ${total}` : ""}`}
          {isFiltered ? (
            <button type="button" className="rl-reset" onClick={reset}>
              clear
            </button>
          ) : null}
        </p>
      </div>

      {filtered.length === 0 ? (
        <p className="rl-empty">
          Nothing matches <strong>{rawQuery}</strong>. Older lines are on the{" "}
          <a href={`${REPO_URL}/releases`} target="_blank" rel="noreferrer">
            releases page
          </a>
          .
        </p>
      ) : (
        <ol className="rl-timeline">
          {filtered.map(({ release, entries }) => {
            const open = !closed.has(release.version);
            const anchor = anchorFor(release.version);
            return (
              <li key={release.version} className="rl-release" id={anchor}>
                <div className="rl-release-head">
                  <button
                    type="button"
                    className="rl-disclosure"
                    aria-expanded={open}
                    aria-controls={`${anchor}-body`}
                    onClick={() => toggleOpen(release.version)}
                  >
                    <span className="rl-marker" aria-hidden="true" />
                    <span className="rl-version">{release.version}</span>
                    {release.date ? (
                      <span className="rl-date">{release.date}</span>
                    ) : null}
                    <span className="rl-meter" aria-hidden="true">
                      {CHANGE_KINDS.filter((k) => release.counts[k]).map((k) => (
                        <span
                          key={k}
                          className="rl-meter-seg"
                          style={
                            {
                              "--rl-kind": KIND_TOKEN[k],
                              flexGrow: release.counts[k],
                            } as React.CSSProperties
                          }
                        />
                      ))}
                    </span>
                    <span className="rl-toggle" aria-hidden="true">
                      {open ? "−" : "+"}
                    </span>
                  </button>
                  <a
                    className="rl-tag"
                    href={`${REPO_URL}/releases/tag/v${release.version}`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    v{release.version} on GitHub
                  </a>
                </div>

                <div id={`${anchor}-body`} className="rl-release-body" hidden={!open}>
                  {release.summary ? (
                    <p className="rl-summary">
                      <InlineMarkdown text={release.summary} query={query} />
                    </p>
                  ) : null}

                  <ul className="rl-entries">
                    {entries.map((entry, i) => (
                      <li
                        key={`${entry.kind}-${i}`}
                        className="rl-entry"
                        style={
                          { "--rl-kind": KIND_TOKEN[entry.kind] } as React.CSSProperties
                        }
                      >
                        <span className="rl-kind">{entry.kind.toLowerCase()}</span>
                        <div className="rl-entry-text">
                          {entry.lead ? (
                            <strong className="rl-lead">
                              <InlineMarkdown text={entry.lead} query={query} />
                            </strong>
                          ) : null}{" "}
                          <InlineMarkdown text={entry.body} query={query} />
                          {entry.refs.length > 0 ? (
                            <span className="rl-refs">
                              {entry.refs.map((ref) => (
                                <a
                                  key={ref}
                                  href={`${REPO_URL}/pull/${ref}`}
                                  target="_blank"
                                  rel="noreferrer"
                                  className="rl-ref"
                                >
                                  #{ref}
                                </a>
                              ))}
                            </span>
                          ) : null}
                        </div>
                      </li>
                    ))}
                  </ul>
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}
