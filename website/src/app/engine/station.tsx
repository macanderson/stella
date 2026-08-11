import type { ReactNode } from "react";

/**
 * Server-side primitives every station is built from. No state, no effects —
 * the shell (tour-shell.tsx) drives everything interactive by setting
 * attributes on the `data-station` sections these render, and engine.css
 * does the rest.
 */

/**
 * One full-viewport station of the tour.
 *
 * `data-station` is the hook the shell observes; `id` doubles as the deep
 * link (`/engine#vera`) and the anchor target that keeps the page navigable
 * with no JavaScript at all. `tone` lets a station shift its ambient
 * lighting (vera's chamber runs warmer) without a second stylesheet.
 */
export function Station({
  id,
  tone,
  children,
}: {
  id: string;
  tone?: "chamber";
  children: ReactNode;
}) {
  return (
    <section
      id={id}
      data-station={id}
      data-tone={tone}
      className="eng-station"
      aria-labelledby={`${id}-title`}
    >
      <div className="eng-station-inner">{children}</div>
    </section>
  );
}

/**
 * The station's header block: index + name as an overline, then the display
 * heading. The heading carries the `aria-labelledby` target for the section.
 */
export function StationHead({
  id,
  code,
  name,
  title,
  badge,
}: {
  id: string;
  code: string;
  name: string;
  title: ReactNode;
  badge?: string;
}) {
  return (
    <header className="eng-head">
      <p className="eng-overline">
        <span className="eng-overline-code">{code}</span>
        <span className="eng-overline-name">{name}</span>
        {badge ? <span className="eng-overline-badge">{badge}</span> : null}
      </p>
      <h2 id={`${id}-title`} className="eng-title">
        {title}
      </h2>
    </header>
  );
}

/** The intake station is the document's h1; same shape, different rank. */
export function StationHeadH1({
  id,
  code,
  name,
  title,
}: {
  id: string;
  code: string;
  name: string;
  title: ReactNode;
}) {
  return (
    <header className="eng-head">
      <p className="eng-overline">
        <span className="eng-overline-code">{code}</span>
        <span className="eng-overline-name">{name}</span>
      </p>
      <h1 id={`${id}-title`} className="eng-title eng-title-display">
        {title}
      </h1>
    </header>
  );
}

/** A measured lead paragraph under the station heading. */
export function Lead({ children }: { children: ReactNode }) {
  return <p className="eng-lead">{children}</p>;
}

/** The brand names, set in the running-text face: lowercase, mono, 700. */
export function BrandFace({ children }: { children: ReactNode }) {
  return <span className="eng-brand-face">{children}</span>;
}

/**
 * A strip of telemetry lines in the shape of stella-events.jsonl — the
 * engine's ground-truth journal — typed on as the station comes into view.
 *
 * Illustrative by construction and labelled so: these are the *shapes* of
 * real event lines, not a recorded run, and the caption says exactly that.
 * The honesty rule is the home page's ("the figures illustrate a run rather
 * than a benchmark"), applied to telemetry.
 */
export function EventFeed({ lines }: { lines: readonly string[] }) {
  return (
    <figure className="eng-feed" aria-label="Engine telemetry, illustrative">
      <pre className="eng-feed-body">
        {lines.map((line, i) => (
          <span
            key={line}
            className="eng-feed-line"
            style={{ "--eng-i": i } as React.CSSProperties}
          >
            {line}
            {"\n"}
          </span>
        ))}
      </pre>
      <figcaption className="eng-feed-caption">
        the shape of stella-events.jsonl — illustrative lines, not a recorded
        run
      </figcaption>
    </figure>
  );
}

/**
 * A labelled fact panel: a short claim a reader can carry away, with the
 * mechanism under it. The tour's argument is made of these.
 */
export function FactCard({
  title,
  children,
}: {
  title: string;
  children: ReactNode;
}) {
  return (
    <div className="eng-fact">
      <h3 className="eng-fact-title">{title}</h3>
      <p className="eng-fact-body">{children}</p>
    </div>
  );
}
