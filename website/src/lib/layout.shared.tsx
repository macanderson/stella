import type { BaseLayoutProps } from "fumadocs-ui/layouts/shared";
import { GitHubMark, Mark, Wordmark } from "@/components/brand";
import { REPO_URL, SPONSOR_URL } from "@/lib/site";

/**
 * Shared layout options (nav title, links) consumed by both the docs layout
 * and the home layout.
 *
 * Branding: the lockup — gold comet flying left→right into the outlined
 * wordmark — followed by a quiet "docs" qualifier. The wordmark's own sparkle
 * is dropped here because the comet already carries the gold; two stars in a
 * nav-sized lockup is one star too many. Everything renders inline from the
 * same geometry the rest of the site uses, so the letters paint with
 * `currentColor` and invert with the theme.
 *
 * Size: the lockup fills the fixed-height header instead of floating in it —
 * mark 28px, wordmark 24px. The header's own height clamps it, so growing
 * these numbers never grows the chrome; past ~h-8 the mark starts touching
 * the header padding, which is the real ceiling.
 */
export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: (
        <span className="inline-flex items-center gap-2.5">
          <Mark className="h-7 w-auto" />
          <Wordmark className="h-6 w-auto text-fd-foreground" sparkle={false} />
          <span className="text-sm text-fd-muted-foreground">docs</span>
        </span>
      ),
    },
    links: [
      {
        text: "Docs",
        url: "/docs",
        active: "nested-url",
      },
      {
        type: "icon",
        text: "Sponsor",
        url: SPONSOR_URL,
        icon: (
          <svg
            role="img"
            aria-label="Sponsor stella on GitHub"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.75"
            strokeLinecap="round"
            strokeLinejoin="round"
            className="size-5"
          >
            <path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z" />
          </svg>
        ),
      },
      {
        type: "icon",
        text: "GitHub",
        url: REPO_URL,
        icon: <GitHubMark className="size-5" label="stella on GitHub" />,
      },
    ],
  };
}
