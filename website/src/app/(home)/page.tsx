import type { Metadata } from "next";
import Link from "next/link";
import { HeroTerminal } from "@/components/command-deck";
import { AnimatedLockup } from "@/components/animated-lockup";
import { Mark } from "@/components/brand";
import { InstallBlock } from "@/components/install-block";
import { PROVIDER_CATALOG } from "@/components/provider-cards";
import { REPO_URL, SITE_URL } from "@/lib/site";

// The root layout's metadata is titled/described for the docs section it
// mostly serves ("stella — docs"); the landing page is the one route that
// needs its own voice instead of inheriting that.
const HOME_TITLE = "stella — the terminal agent that proves its work finished";
const HOME_DESCRIPTION =
  "A terminal coding agent that runs on the API keys you already have, speaks ten providers' own protocols, and ends a run only when a second model has confirmed the goal from evidence.";

export const metadata: Metadata = {
  title: HOME_TITLE,
  description: HOME_DESCRIPTION,
  alternates: { canonical: SITE_URL },
  openGraph: {
    title: HOME_TITLE,
    description: HOME_DESCRIPTION,
    url: SITE_URL,
    type: "website",
  },
  twitter: {
    card: "summary_large_image",
    title: HOME_TITLE,
    description: HOME_DESCRIPTION,
  },
};

const softwareJsonLd = {
  "@context": "https://schema.org",
  "@type": "SoftwareApplication",
  name: "stella",
  description: HOME_DESCRIPTION,
  url: SITE_URL,
  applicationCategory: "DeveloperApplication",
  operatingSystem: "macOS, Linux, Windows",
  license: "https://www.gnu.org/licenses/agpl-3.0.html",
  offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
};

/**
 * The landing page.
 *
 * One sentence saying what stella is, the command that installs it, a
 * transcript of a real run, the list of providers it speaks to, and the four
 * doors into the docs. Everything else is one click away and better written
 * there.
 *
 * Brand notes (docs/brand): the name is lowercase always; the comet flies
 * left→right into the wordmark; gold is the signal (the lockup, the prompt,
 * the CTA) and never the surface.
 */

/**
 * Copied from content/docs/getting-started/installation.mdx. If that page's
 * command changes, this one has to change with it — a landing page that
 * installs a different thing than the docs say is worse than no landing page.
 *
 * The URL is the site's own /install.sh, which serves the canonical script
 * from the repo (src/app/install.sh/route.ts) and counts the download — the
 * copy beacon in InstallBlock counts the other half of the funnel.
 */
const INSTALL = "curl -fsSL https://stella.oxagen.sh/install.sh | sh";

/** The four entry points, in the order a new reader needs them. */
const DOORS = [
  {
    href: "/docs/getting-started/installation",
    title: "Install and authenticate",
    body: "One binary, one API key you already have. No account, no sign-up.",
  },
  {
    href: "/docs/agent-modes",
    title: "Pick a mode",
    body: "chat, run, goal, monitor, or fleet — and which of them fits the task in front of you.",
  },
  {
    href: "/docs/agent-tools/permissions",
    title: "Decide what it may touch",
    body: "Every tool sits behind a per-tool permission model, with the shell off by default.",
  },
  {
    href: "/docs/inference-pipeline",
    title: "Read the pipeline",
    body: "triage, plan, witness, execute, verify, judge — and where a run can stop.",
  },
];

export default function HomePage() {
  return (
    <div id="content" tabIndex={-1} className="flex flex-1 flex-col">
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(softwareJsonLd) }}
      />
      {/* ── What it is ─────────────────────────────────────────────────── */}
      <section className="lp-hero">
        <div className="mx-auto w-full max-w-3xl px-4 py-20 sm:py-28">
          {/* The home page hides the standard nav bar until the reader
              scrolls (see components/home-chrome.tsx), so the lockup takes
              the corner the nav's own logo would otherwise own — large,
              top-left, first thing assembled. */}
          <AnimatedLockup className="mb-10 h-20 w-auto text-fd-foreground sm:h-28" />
          <h1 className="lp-h1">
            <span className="lp-brand-face">stella</span> is a terminal coding
            agent that proves its work finished.
          </h1>
          <p className="lp-lead mt-6">
            It runs on the API keys you already have, speaks ten providers&apos;
            own protocols, and ends a run only when a second model has confirmed
            the goal from evidence. Nothing is proxied through a hosted service,
            and telemetry never leaves your disk.
          </p>

          {/* Install and "read the docs" are the same decision — try it now,
              or read first — so they sit side by side instead of stacked. */}
          <div className="mt-12">
            <p className="mb-2 text-sm text-fd-muted-foreground">Install it:</p>
            <div className="flex flex-col gap-4 sm:flex-row sm:items-stretch">
              <div className="min-w-0 flex-1">
                <InstallBlock command={INSTALL} />
              </div>
              <Link
                href="/docs"
                className="lp-cta flex shrink-0 items-center justify-center rounded-md px-6 text-sm"
              >
                Read the docs
              </Link>
            </div>
          </div>

          <div className="mt-6">
            <a
              href={REPO_URL}
              className="text-sm text-fd-muted-foreground underline underline-offset-4 hover:text-fd-foreground"
            >
              Source on GitHub
            </a>
          </div>
        </div>
      </section>

      {/* ── Proof ──────────────────────────────────────────────────────── */}
      <section className="lp-section lp-band">
        <div className="mx-auto w-full max-w-3xl px-4 py-16">
          <h2 className="lp-eyebrow mb-5">One run, start to finish</h2>
          <HeroTerminal />
          <p className="mt-4 max-w-prose text-sm text-fd-muted-foreground">
            The stages, the metering, and the verification step are
            <span className="lp-brand-face"> stella</span>&apos;s own; the
            figures illustrate a run rather than a benchmark.{" "}
            <Link
              href="/docs/agent-modes#outcome-driven-goal-mode"
              className="underline underline-offset-4 hover:text-fd-foreground"
            >
              How goal mode decides it is done
            </Link>
            .
          </p>
        </div>
      </section>

      {/* ── Providers ──────────────────────────────────────────────────── */}
      <section className="lp-section">
        <div className="mx-auto w-full max-w-3xl px-4 py-16">
          <h2 className="lp-eyebrow mb-5">Providers</h2>
          <p className="max-w-prose text-base">
            {PROVIDER_CATALOG.map((p, i) => (
              <span key={p.id}>
                {i > 0 ? <span className="text-fd-muted-foreground"> · </span> : null}
                <Link href={p.href} className="underline underline-offset-4">
                  {p.name}
                </Link>
              </span>
            ))}
          </p>
          <p className="mt-4 max-w-prose text-sm text-fd-muted-foreground">
            <span className="lp-brand-face">stella</span> speaks each
            vendor&apos;s own wire protocol rather than normalising everything
            through one OpenAI-shaped adapter, so thinking blocks, cache
            control, and tool-call shapes are native rather than emulated.
            Override any base URL, key, or model in{" "}
            <Link
              href="/docs/configuration/settings"
              className="font-mono text-[0.9em] underline underline-offset-4"
            >
              settings.json
            </Link>
            .
          </p>
        </div>
      </section>

      {/* ── Doors into the docs ────────────────────────────────────────── */}
      <section className="lp-section lp-band">
        <div className="mx-auto w-full max-w-3xl px-4 py-16">
          <h2 className="lp-eyebrow mb-5">Start here</h2>
          <ul className="border-t border-fd-border">
            {DOORS.map((d) => (
              <li key={d.href} className="border-b border-fd-border">
                <Link
                  href={d.href}
                  className="-mx-3 block px-3 py-4 transition-colors hover:bg-fd-accent"
                >
                  <span className="text-base font-medium">{d.title}</span>
                  <span className="mt-1 block max-w-prose text-sm text-fd-muted-foreground">
                    {d.body}
                  </span>
                </Link>
              </li>
            ))}
          </ul>
        </div>
      </section>

      <footer className="lp-section lp-footer">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-4 px-4 py-10 text-sm text-fd-muted-foreground sm:flex-row sm:items-center sm:justify-between">
          <span className="inline-flex items-center gap-2">
            <Mark className="h-4 w-auto" />
            <span className="lp-brand-face text-fd-foreground">stella</span>
            <span>— AGPL 3.0</span>
          </span>
          <nav aria-label="Footer" className="flex items-center gap-5">
            <Link href="/docs" className="hover:text-fd-foreground">
              Docs
            </Link>
            <Link
              href="/docs/getting-started/installation"
              className="hover:text-fd-foreground"
            >
              Install
            </Link>
            <a href={REPO_URL} className="hover:text-fd-foreground">
              GitHub
            </a>
          </nav>
        </div>
      </footer>
    </div>
  );
}
