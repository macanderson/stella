import type { Metadata } from "next";
import { ReleasesBrowser } from "./releases-browser";
import { getReleases } from "@/lib/changelog.server";
import { REPO_URL, SITE_URL } from "@/lib/site";

/**
 * /releases — the release-notes microsite.
 *
 * A server component: `getReleases()` reads the repository's CHANGELOG.md at
 * build time and hands the parsed result to `<ReleasesBrowser>` for search and
 * filtering. Nothing about the notes is duplicated here, so the page cannot
 * fall behind the repository the way the hand-maintained docs page it replaces
 * did (it sat on "Current release: 0.6.44" while the tree was on 0.6.131).
 *
 * Statically rendered — the data changes only when the repository does, which
 * is to say only at deploy time. There is no revalidation window because there
 * is nothing to revalidate against.
 */

const TITLE = "stella — releases";
const DESCRIPTION =
  "What each minor line of stella changed, in the words of someone who uses the CLI. Search every change, filter by kind, and read the line you are upgrading across.";
const URL = `${SITE_URL}/releases`;

export const metadata: Metadata = {
  // `absolute` because the root layout's template is `%s — stella docs`, which
  // is right for the docs tree and wrong here: this page is not documentation,
  // and "stella — releases — stella docs" says the word twice to say less.
  title: { absolute: TITLE },
  description: DESCRIPTION,
  alternates: { canonical: URL },
  openGraph: {
    title: TITLE,
    description: DESCRIPTION,
    url: URL,
    type: "website",
  },
  twitter: {
    card: "summary_large_image",
    title: TITLE,
    description: DESCRIPTION,
  },
};

export default function ReleasesPage() {
  const releases = getReleases();
  const latest = releases[0];

  return (
    <main className="rl-page">
      <header className="rl-hero">
        <p className="rl-eyebrow">changelog</p>
        <h1 className="rl-title">releases</h1>
        <p className="rl-lede">
          What each <strong>minor line</strong> changed, written for someone
          deciding whether to upgrade. Every merge to <code>main</code> cuts a
          patch release — the 0.6 line was 127 of them in eight days — so those
          are recorded per tag on the{" "}
          <a href={`${REPO_URL}/releases`} target="_blank" rel="noreferrer">
            releases page
          </a>{" "}
          rather than itemized here.
        </p>
        {latest ? (
          <p className="rl-latest">
            <span className="rl-latest-dot" aria-hidden="true" />
            latest line <strong>{latest.version}</strong>
            {latest.date ? <span className="rl-latest-date"> · {latest.date}</span> : null}
          </p>
        ) : null}
      </header>

      <ReleasesBrowser releases={releases} />

      <footer className="rl-foot">
        <p>
          Notes are drafted from each line&rsquo;s own diff by{" "}
          <a
            href={`${REPO_URL}/blob/main/scripts/changelog-ai.sh`}
            target="_blank"
            rel="noreferrer"
          >
            <code>changelog-ai.sh</code>
          </a>{" "}
          and rolled into{" "}
          <a
            href={`${REPO_URL}/blob/main/CHANGELOG.md`}
            target="_blank"
            rel="noreferrer"
          >
            <code>CHANGELOG.md</code>
          </a>
          , which is what this page renders.
        </p>
      </footer>
    </main>
  );
}
