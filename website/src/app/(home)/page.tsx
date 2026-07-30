import Link from "next/link";
import { HeroTerminal } from "@/components/command-deck";
import { Mark } from "@/components/brand";
import { PROVIDER_CATALOG } from "@/components/provider-cards";

/**
 * The landing page.
 *
 * It used to carry six feature cards, three "split" cards, a provider pill
 * cloud, a looping animated fleet demo, and two hero background layers — five
 * sections that between them made the same claim ("it is fast, it is BYOK, it
 * proves its work") three times over. Repeating a claim does not make it more
 * believable; it makes the page longer.
 *
 * What is left is what a reader who has never heard of Stella actually needs:
 * one sentence saying what it is, the command that installs it, a transcript of
 * a real run, the list of providers it speaks to, and the four doors into the
 * docs. Everything else is one click away and better written there.
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
    <main id="content" className="flex flex-1 flex-col">
      {/* ── What it is ─────────────────────────────────────────────────── */}
      <section className="mx-auto w-full max-w-3xl px-4 py-20 sm:py-28">
        <Mark className="lp-mark mb-10 h-10 w-auto" cursor label="Stella" />
        <h1 className="lp-h1">
          <span className="lp-brand-face">stella</span> is a terminal coding
          agent that proves its work finished.
        </h1>
        <p className="lp-lead mt-6">
          It runs on the API keys you already have, speaks ten providers&apos; own
          protocols, and ends a run only when a second model has confirmed the goal
          from evidence. Nothing is proxied through a hosted service, and telemetry
          never leaves your disk.
        </p>

        <div className="mt-10">
          <p className="mb-2 text-sm text-fd-muted-foreground">Install it:</p>
          <div className="term">
            <pre className="term-body">
              <span className="term-prompt">$ </span>
              {INSTALL}
            </pre>
          </div>
        </div>

        <div className="mt-8 flex flex-wrap items-center gap-x-6 gap-y-3">
          <Link
            href="/docs"
            className="lp-cta inline-flex items-center rounded-md px-4 py-2 text-sm font-medium transition-colors"
          >
            Read the docs
          </Link>
          <a
            href="https://github.com/macanderson/stella"
            className="text-sm text-fd-muted-foreground underline underline-offset-4 hover:text-fd-foreground"
          >
            Source on GitHub
          </a>
        </div>
      </section>

      {/* ── Proof ──────────────────────────────────────────────────────── */}
      <section className="lp-section">
        <div className="mx-auto w-full max-w-3xl px-4 py-16">
          <h2 className="lp-eyebrow mb-5">One run, start to finish</h2>
          <HeroTerminal />
          <p className="mt-4 max-w-prose text-sm text-fd-muted-foreground">
            The stages, the metering, and the verification step are Stella&apos;s
            own; the figures illustrate a run rather than a benchmark.{" "}
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
            Stella speaks each vendor&apos;s own wire protocol rather than
            normalising everything through one OpenAI-shaped adapter, so thinking
            blocks, cache control, and tool-call shapes are native rather than
            emulated. Override any base URL, key, or model in{" "}
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
      <section className="lp-section">
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

      <footer className="lp-section">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-4 px-4 py-10 text-sm text-fd-muted-foreground sm:flex-row sm:items-center sm:justify-between">
          <span className="inline-flex items-center gap-2">
            <Mark className="lp-mark h-4 w-auto" cursor />
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
            <a
              href="https://github.com/macanderson/stella"
              className="hover:text-fd-foreground"
            >
              GitHub
            </a>
          </nav>
        </div>
      </footer>
    </main>
  );
}
