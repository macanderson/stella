import type { Metadata } from "next";
import { SITE_URL } from "@/lib/site";
import { TourShell } from "./tour-shell";
import {
  IntakeStation,
  ToolBayStation,
  TurnLoopStation,
} from "./stations-fore";
import {
  EvolutionStation,
  ExitStation,
  GovernanceStation,
  VeraStation,
} from "./stations-aft";

/**
 * /engine — the tour.
 *
 * A marketing page that argues by *showing the machine* rather than by
 * listing features: the reader walks the engine from the intake hatch to the
 * exit, meeting the turn loop, the tool bay, vera's verification chamber, the
 * Oxagen governance plane, and the evolution deck on the way.
 *
 * Structurally it is seven server-rendered stations wrapped in one client
 * shell (tour-shell.tsx) that does nothing but set attributes; every effect
 * is CSS in engine.css. That split is deliberate — the whole argument of the
 * page is legible to a crawler, a reader-mode extension, and a browser with
 * JavaScript disabled, because the words are in the HTML and the shell only
 * decorates them.
 *
 * The landing page at `/` keeps its job (what stella is, install it, four
 * doors into the docs). This route is the one that takes a reader who wants
 * to *understand the machine* and gives them the inside of it.
 */

const TOUR_TITLE = "stella — step inside the engine";
const TOUR_DESCRIPTION =
  "A tour of the inside of a terminal coding agent: the turn loop, the tool bay, vera's deterministic verification chamber, the Oxagen governance plane, and the evolution deck where stella rebuilds itself from your team's traces.";

export const metadata: Metadata = {
  // `absolute`, for the same reason the landing page's title is: the root
  // layout's "%s — stella docs" template would append a section this page is
  // not in, and disagree with the Open Graph title right below it.
  title: { absolute: TOUR_TITLE },
  description: TOUR_DESCRIPTION,
  alternates: { canonical: `${SITE_URL}/engine` },
  openGraph: {
    title: TOUR_TITLE,
    description: TOUR_DESCRIPTION,
    url: `${SITE_URL}/engine`,
    type: "website",
  },
  twitter: {
    card: "summary_large_image",
    title: TOUR_TITLE,
    description: TOUR_DESCRIPTION,
  },
};

export default function EnginePage() {
  return (
    <TourShell>
      <IntakeStation />
      <TurnLoopStation />
      <ToolBayStation />
      <VeraStation />
      <GovernanceStation />
      <EvolutionStation />
      <ExitStation />
    </TourShell>
  );
}
