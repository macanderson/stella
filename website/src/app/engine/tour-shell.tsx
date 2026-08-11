"use client";

import { useEffect, useRef, useState, type ReactNode } from "react";
import Link from "next/link";
import { Mark, Wordmark } from "@/components/brand";
import { STATIONS } from "./stations-meta";

/**
 * The tour's one client component.
 *
 * The stations themselves are server-rendered markup passed in as children —
 * this shell only *orchestrates*: it observes which station the reader is
 * standing in, moves the HUD to match, arms the CSS entrance animations, and
 * wires the keyboard. All motion lives in engine.css; the shell's whole
 * output is attributes.
 *
 * Three progressive layers, so the page degrades instead of breaking:
 *
 *  - No JS at all: every station renders visible (the entrance-hidden states
 *    in engine.css are gated on the `data-eng-js` attribute this component
 *    sets), anchors still jump, the rail is just absent.
 *  - JS, reduced motion: attributes still flow, engine.css declines to
 *    animate, programmatic jumps use `behavior: "auto"` — the observer-driven
 *    HUD keeps working because state is not motion.
 *  - JS, full motion: the intended tour.
 */

/** One section either side of `-45%` counts as "standing in" the station —
 * the same centre-band trick fumadocs' TOC uses, expressed as rootMargin. */
const ACTIVE_BAND = "-45% 0px -45% 0px";

/** Entrances arm a little before the station reaches centre, so the reader
 * sees the arrival rather than the aftermath. */
const INVIEW_MARGIN = "0px 0px -18% 0px";

function prefersReducedMotion(): boolean {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

export function TourShell({ children }: { children: ReactNode }) {
  const rootRef = useRef<HTMLDivElement>(null);
  const [active, setActive] = useState<string>(STATIONS[0].id);

  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;

    // Arm the CSS entrances only now that a script is present to trigger them.
    root.setAttribute("data-eng-js", "true");

    const sections = Array.from(
      root.querySelectorAll<HTMLElement>("[data-station]"),
    );

    // Play-once entrances: the attribute is set the first time a station
    // approaches and never removed, so scrolling back up replays nothing —
    // assemble, don't spin.
    const entrances = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            entry.target.setAttribute("data-inview", "true");
            entrances.unobserve(entry.target);
          }
        }
      },
      { rootMargin: INVIEW_MARGIN },
    );

    // Which station the reader is standing in, for the HUD and the hash.
    // `replaceState` rather than assigning location.hash: the tour is one
    // page, and forty scroll positions must not become forty history entries.
    const tracker = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            const id = entry.target.getAttribute("data-station");
            if (id) {
              setActive(id);
              history.replaceState(null, "", `#${id}`);
            }
          }
        }
      },
      { rootMargin: ACTIVE_BAND },
    );

    for (const section of sections) {
      entrances.observe(section);
      tracker.observe(section);
    }

    // j/k walk the stations — the pager keys every terminal reader already
    // has in their fingers. Arrows are left native: overriding the browser's
    // own scroll keys is how a page starts fighting its reader.
    const onKey = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      if (target && /^(input|textarea|select)$/i.test(target.tagName)) return;
      if (event.key !== "j" && event.key !== "k") return;
      event.preventDefault();
      const ids = STATIONS.map((s) => s.id);
      const current = ids.indexOf(
        root.getAttribute("data-eng-active") ?? ids[0],
      );
      const next = event.key === "j" ? current + 1 : current - 1;
      const station = STATIONS[Math.max(0, Math.min(ids.length - 1, next))];
      document.getElementById(station.id)?.scrollIntoView({
        behavior: prefersReducedMotion() ? "auto" : "smooth",
      });
    };
    window.addEventListener("keydown", onKey);

    return () => {
      entrances.disconnect();
      tracker.disconnect();
      window.removeEventListener("keydown", onKey);
      root.removeAttribute("data-eng-js");
    };
  }, []);

  // The keydown handler reads the live station from this attribute instead of
  // closing over React state, so the listener is attached exactly once.
  useEffect(() => {
    rootRef.current?.setAttribute("data-eng-active", active);
  }, [active]);

  const activeIndex = Math.max(
    0,
    STATIONS.findIndex((s) => s.id === active),
  );
  const activeMeta = STATIONS[activeIndex];

  return (
    <div ref={rootRef} className="eng-tour" data-eng-active={active}>
      {/* ── top HUD: who you are inside, and the way back out ─────────── */}
      <header className="eng-hud-top">
        <Link href="/" className="eng-hud-lockup" aria-label="stella home">
          <Mark className="eng-hud-mark" />
          <Wordmark className="eng-hud-wordmark" sparkle={false} />
          <span className="eng-hud-qualifier">engine tour</span>
        </Link>
        <nav aria-label="Leave the tour" className="eng-hud-exit">
          <Link href="/docs">docs</Link>
          <Link href="/">exit tour</Link>
        </nav>
      </header>

      {/* ── the stations ──────────────────────────────────────────────── */}
      <main id="content" tabIndex={-1} className="eng-main">
        {children}
      </main>

      {/* ── bottom HUD: the station rail ──────────────────────────────── */}
      <nav className="eng-rail" aria-label="Tour stations">
        <div
          className="eng-rail-progress"
          style={
            {
              "--eng-progress": `${(activeIndex / (STATIONS.length - 1)) * 100}%`,
            } as React.CSSProperties
          }
          aria-hidden="true"
        />
        <ol className="eng-rail-stops">
          {STATIONS.map((station) => (
            <li key={station.id}>
              <a
                href={`#${station.id}`}
                className="eng-rail-stop"
                aria-current={station.id === active ? "true" : undefined}
                onClick={(event) => {
                  event.preventDefault();
                  document.getElementById(station.id)?.scrollIntoView({
                    behavior: prefersReducedMotion() ? "auto" : "smooth",
                  });
                }}
              >
                <span className="eng-rail-code">{station.code}</span>
                <span className="eng-rail-name">{station.name}</span>
              </a>
            </li>
          ))}
        </ol>
        {/* Mobile collapses the stops into one live readout. */}
        <p className="eng-rail-compact" aria-hidden="true">
          <span className="eng-rail-code">{activeMeta.code}</span>
          <span className="eng-rail-name">{activeMeta.name}</span>
        </p>
        <p className="eng-rail-hint" aria-hidden="true">
          j / k — walk the engine
        </p>
      </nav>
    </div>
  );
}
