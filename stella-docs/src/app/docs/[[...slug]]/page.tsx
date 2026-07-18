import type { Metadata } from "next";
import { notFound, redirect } from "next/navigation";
import {
  DocsBody,
  DocsDescription,
  DocsPage,
  DocsTitle,
} from "fumadocs-ui/page";
import { getMDXComponents } from "@/mdx-components";
import { source } from "@/lib/source";

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

  return (
    <DocsPage toc={page.data.toc} full={page.data.full}>
      <DocsTitle>{page.data.title}</DocsTitle>
      <DocsDescription>{page.data.description}</DocsDescription>
      <DocsBody>
        <MDX components={getMDXComponents()} />
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

  return {
    title: page.data.title,
    description: page.data.description,
  };
}
