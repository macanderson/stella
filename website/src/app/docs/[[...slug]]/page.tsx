import type { Metadata } from "next";
import { notFound, redirect } from "next/navigation";
import {
  DocsBody,
  DocsDescription,
  DocsPage,
  DocsTitle,
} from "fumadocs-ui/page";
import { getMDXComponents } from "@/mdx-components";
import { PageFooter } from "@/components/page-footer";
import { source } from "@/lib/source";
import { SITE_URL } from "@/lib/site";

// Page-tree node type, inferred from the loader output so it stays in sync with
// fumadocs-core without importing internal type paths.
type TreeNode = (typeof source.pageTree)["children"][number];

// Collect page URLs in sidebar order (depth-first, respecting meta.json `pages`).
function collectPageUrls(nodes: readonly TreeNode[]): string[] {
  const urls: string[] = [];
  for (const node of nodes) {
    if (node.type === "page") urls.push(node.url);
    else if (node.type === "folder") {
      if (node.index) urls.push(node.index.url);
      urls.push(...collectPageUrls(node.children));
    }
  }
  return urls;
}

// A section folder with no index page would 404; resolve it to the section's
// first page so section roots are always navigable.
function resolveSectionLanding(slug: string[]): string | undefined {
  if (slug.length === 0) return undefined;
  const prefix = `/docs/${slug.join("/")}`;
  return collectPageUrls(source.pageTree.children).find((url) =>
    url.startsWith(`${prefix}/`),
  );
}

// Every prefix of the slug that resolves to a real page, in order — a folder
// segment with no index page (and so no title of its own) is skipped rather
// than guessed at, so the trail only ever states pages that actually exist.
function breadcrumbTrail(slug: string[]) {
  return Array.from({ length: slug.length + 1 }, (_, i) => source.getPage(slug.slice(0, i)))
    .filter((p): p is NonNullable<typeof p> => Boolean(p))
    .map((p) => ({ name: p.data.title, url: `${SITE_URL}${p.url}` }));
}

export default async function Page(props: {
  params: Promise<{ slug?: string[] }>;
}) {
  const params = await props.params;
  const page = source.getPage(params.slug);
  if (!page) {
    const landing = resolveSectionLanding(params.slug ?? []);
    if (landing) redirect(landing);
    notFound();
  }

  const MDX = page.data.body;

  const breadcrumbJsonLd = {
    "@context": "https://schema.org",
    "@type": "BreadcrumbList",
    itemListElement: breadcrumbTrail(params.slug ?? []).map((item, i) => ({
      "@type": "ListItem",
      position: i + 1,
      name: item.name,
      item: item.url,
    })),
  };

  return (
    // fumadocs' DocsLayout has no <main> landmark of its own on /docs routes;
    // `role` passes straight through to the article element it renders (see
    // DocsPage's `...containerProps`), so this is an attribute, not a layout
    // change.
    <DocsPage toc={page.data.toc} full={page.data.full} role="main">
      <script
        type="application/ld+json"
        // Our own frontmatter titles and page-tree URLs, not user input.
        dangerouslySetInnerHTML={{ __html: JSON.stringify(breadcrumbJsonLd) }}
      />
      {/* The skip link's target (src/app/layout.tsx). Fumadocs wraps this in
       * <article id="nd-page">, so the topmost node of the reading order is
       * inside the article rather than being the article itself; `tabIndex`
       * makes it a real focus destination rather than only a scroll anchor. */}
      <span id="content" tabIndex={-1} />
      <DocsTitle>{page.data.title}</DocsTitle>
      <DocsDescription>{page.data.description}</DocsDescription>
      <DocsBody>
        <MDX components={getMDXComponents()} />
        <PageFooter path={page.url} title={page.data.title} />
      </DocsBody>
    </DocsPage>
  );
}

export function generateStaticParams() {
  return source.generateParams();
}

export async function generateMetadata(props: {
  params: Promise<{ slug?: string[] }>;
}): Promise<Metadata> {
  const params = await props.params;
  const page = source.getPage(params.slug);
  if (!page) {
    const landing = resolveSectionLanding(params.slug ?? []);
    if (landing) redirect(landing);
    notFound();
  }

  const url = `${SITE_URL}${page.url}`;

  return {
    title: page.data.title,
    description: page.data.description,
    alternates: { canonical: url },
    openGraph: {
      title: page.data.title,
      description: page.data.description,
      url,
      type: "article",
    },
    twitter: {
      card: "summary_large_image",
      title: page.data.title,
      description: page.data.description,
    },
  };
}
