#!/usr/bin/env node
/**
 * Build `docs/llms.txt` — every page of stella.oxagen.sh/docs concatenated into
 * one plain-Markdown file an LLM can be handed whole.
 *
 * The site is the source of truth, so this file is *generated*, never edited:
 * a hand-maintained copy of 78 pages would be wrong within a week, and wrong
 * documentation an agent reads confidently is worse than none. Run
 * `pnpm docs:llms` (or `make llms-txt`) after touching `content/docs` and
 * commit the regenerated output.
 *
 * Two decisions worth knowing:
 *
 *  - Order follows the site's own `meta.json` navigation, section dividers and
 *    all, rather than the filesystem. A reader arriving at `getting-started`
 *    before `commands` is the sequencing the docs were written for; alphabetical
 *    order would interleave reference pages into the tutorial.
 *  - MDX is degraded to Markdown, not stripped to prose. Fenced code blocks,
 *    tables, and link targets survive verbatim because they carry most of the
 *    answerable content; only the JSX presentation layer (callouts, logomarks,
 *    SVG diagrams) is flattened, and each flattening keeps its text — a callout
 *    becomes a blockquote, a diagram becomes its own accessible description.
 *
 * Usage: node scripts/build-llms-txt.mjs [--check]
 *   --check  exit non-zero if the committed file is stale (for CI)
 */

import { readFile, writeFile, readdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const WEBSITE_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const REPO_ROOT = path.resolve(WEBSITE_DIR, "..");
const CONTENT_DIR = path.join(WEBSITE_DIR, "content", "docs");
const DIAGRAMS_SRC = path.join(WEBSITE_DIR, "src", "components", "diagrams.tsx");
const OUT_FILE = path.join(REPO_ROOT, "docs", "llms.txt");

const SITE_URL = "https://stella.oxagen.sh";
const DOCS_BASE = `${SITE_URL}/docs`;

/* ------------------------------------------------------------------ parsing */

/**
 * Frontmatter here is three keys deep (`title`, `description`, `icon`), all
 * single-line scalars — verified across every page. Pulling in a YAML parser to
 * read that would add a dependency to a script whose whole point is to run with
 * nothing installed, so this reads the keys directly and would rather return
 * nothing than guess at a shape it does not handle.
 */
function parseFrontmatter(raw, file) {
  if (!raw.startsWith("---\n")) {
    throw new Error(`${file}: missing frontmatter`);
  }
  const end = raw.indexOf("\n---\n", 3);
  if (end === -1) throw new Error(`${file}: unterminated frontmatter`);

  const data = {};
  for (const line of raw.slice(4, end + 1).split("\n")) {
    const m = /^([A-Za-z_][\w-]*):\s*(.*)$/.exec(line);
    if (!m) continue;
    let value = m[2].trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    data[m[1]] = value;
  }
  return { data, body: raw.slice(end + 5) };
}

/**
 * The four inline SVG diagrams already carry a `<title>` and an `aria-label`
 * written for screen readers — text that describes the picture in a sentence.
 * That is exactly what a text-only reader needs, so it is lifted from the
 * component source instead of being re-invented (and left to drift) here.
 */
async function readDiagramDescriptions() {
  const src = await readFile(DIAGRAMS_SRC, "utf8");
  const descriptions = new Map();
  const fn = /export function (\w+)\(\)[\s\S]*?aria-label="([^"]+)"[\s\S]*?<title>([^<]+)<\/title>/g;
  for (const [, name, ariaLabel, title] of src.matchAll(fn)) {
    descriptions.set(name, { title, ariaLabel });
  }
  return descriptions;
}

/* -------------------------------------------------------------- MDX → text */

const CALLOUT_LABEL = { info: "Note", warn: "Warning", error: "Warning" };

/**
 * Rewrite the site's internal links to absolute URLs. In the browser
 * `/docs/commands/run` resolves against the origin; in a file being pasted into
 * a model's context there is no origin, and a bare `/docs/...` is a dead
 * reference. Same for bare `#anchor` fragments, which are only meaningful
 * relative to the page they were written on — that page is one of 78 here.
 */
function absolutiseLinks(line, pageUrl) {
  return line
    .replace(/\]\(\/docs(?=[/)#])/g, `](${DOCS_BASE}`)
    .replace(/\]\(#([^)]+)\)/g, `](${pageUrl}#$1)`);
}

/**
 * Apply `fn` to the parts of a line that are not inside an inline code span.
 *
 * This exists because the JSX strip below cannot tell `<ProviderMark />` from
 * the `<FILE>` in `` `--plan <FILE>` `` — both are angle brackets around a
 * capital letter. Without this guard the option placeholders in the command
 * reference tables silently lose their arguments, turning `--plan <FILE>` into
 * `--plan `: documentation that is not merely incomplete but wrong.
 */
function mapOutsideCode(line, fn) {
  let out = "";
  let text = "";
  let i = 0;

  const runLength = (at) => {
    let n = 0;
    while (line[at + n] === "`") n += 1;
    return n;
  };

  while (i < line.length) {
    if (line[i] === "`") {
      const open = runLength(i);
      // A span closes on a backtick run of exactly the opening length; shorter
      // or longer runs are literal content inside it (CommonMark's rule).
      let j = i + open;
      let close = -1;
      while (j < line.length) {
        if (line[j] === "`") {
          const run = runLength(j);
          if (run === open) {
            close = j;
            break;
          }
          j += run;
        } else {
          j += 1;
        }
      }
      if (close !== -1) {
        out += fn(text) + line.slice(i, close + open);
        text = "";
        i = close + open;
        continue;
      }
    }
    text += line[i];
    i += 1;
  }

  return out + fn(text);
}

function stripJsx(line, diagrams) {
  let out = line;

  // Decorative logomarks sit inside table cells and headings next to the
  // provider's name in plain text; dropping the tag loses nothing. The trailing
  // space goes with it so a `| <ProviderMark /> [Anthropic]` cell does not come
  // out double-spaced.
  out = out.replace(/<Provider(?:Mark|Logo)\b[^>]*\/>[ \t]*/g, "");

  // Diagrams become their described selves rather than vanishing — the flow
  // they illustrate is content, not decoration.
  out = out.replace(/<(\w+Diagram)\s*\/>/g, (whole, name) => {
    const d = diagrams.get(name);
    return d ? `> **Diagram — ${d.title}.** ${d.ariaLabel}` : whole;
  });

  // Anything else JSX-shaped: keep the text, drop the tag.
  out = out.replace(/<\/?[A-Z]\w*\b[^>]*\/?>/g, "");

  return out;
}

/**
 * Walks the page a line at a time because the two things that must not be
 * transformed — fenced code and the inside of a code fence *nested in a
 * callout* — can only be identified by tracking state as you go. A regex over
 * the whole document gets the nesting wrong in exactly the one page that has it.
 */
function mdxToMarkdown(body, pageUrl, diagrams) {
  const out = [];
  let inFence = false;
  let inCallout = false;

  for (const raw of body.split("\n")) {
    const fenceToggle = /^\s*(```|~~~)/.test(raw);

    if (!inFence && !fenceToggle) {
      const open = /^\s*<Callout\b[^>]*type=["'](\w+)["'][^>]*>\s*$/.exec(raw);
      if (open) {
        inCallout = true;
        out.push(`> **${CALLOUT_LABEL[open[1]] ?? "Note"}**`, ">");
        continue;
      }
      if (/^\s*<\/Callout>\s*$/.test(raw)) {
        inCallout = false;
        out.push("");
        continue;
      }
    }

    let line = raw;
    if (fenceToggle) {
      inFence = !inFence;
    } else if (!inFence) {
      line = line.replace(/\{\/\*[\s\S]*?\*\/\}/g, "");
      line = mapOutsideCode(line, (text) =>
        absolutiseLinks(stripJsx(text, diagrams), pageUrl),
      );
      if (!line.trim() && raw.trim()) continue; // line held only stripped JSX
    }

    out.push(inCallout ? (line ? `> ${line}` : ">") : line);
  }

  return out.join("\n").replace(/\n{3,}/g, "\n\n").trim();
}

/* ------------------------------------------------------------- site walking */

async function readMeta(dir) {
  try {
    return JSON.parse(await readFile(path.join(dir, "meta.json"), "utf8"));
  } catch (err) {
    if (err.code === "ENOENT") return null;
    throw err;
  }
}

/**
 * Resolve a directory's `meta.json` into an ordered list of entries, mirroring
 * how Fumadocs builds the sidebar: listed pages first in their stated order,
 * `---Label---` strings as section dividers, and anything on disk but unlisted
 * appended alphabetically. That last rule is why the walker reads the directory
 * at all — a page added without a meta.json edit still renders on the site, so
 * silently omitting it here would make this file quietly incomplete.
 */
async function walk(dir, slugPrefix, sectionTrail) {
  const meta = await readMeta(dir);
  const dirents = await readdir(dir, { withFileTypes: true });

  const files = new Set(
    dirents.filter((d) => d.isFile() && d.name.endsWith(".mdx")).map((d) => d.name.slice(0, -4)),
  );
  const dirs = new Set(dirents.filter((d) => d.isDirectory()).map((d) => d.name));

  const listed = meta?.pages ?? [];
  const named = listed.filter((p) => !p.startsWith("---"));
  const unlisted = [...files, ...dirs].filter((n) => !named.includes(n)).sort();
  if (unlisted.length) {
    process.stderr.write(
      `note: ${path.relative(WEBSITE_DIR, dir) || "."} has entries missing from meta.json, ` +
        `appended alphabetically: ${unlisted.join(", ")}\n`,
    );
  }

  const entries = [];
  let section = null;

  for (const name of [...listed, ...unlisted]) {
    const divider = /^---(.+)---$/.exec(name);
    if (divider) {
      section = divider[1].trim();
      continue;
    }

    const trail = section ? [...sectionTrail, section] : sectionTrail;

    if (dirs.has(name)) {
      const childDir = path.join(dir, name);
      const childMeta = await readMeta(childDir);
      const groupTitle = childMeta?.title ?? name;
      entries.push(...(await walk(childDir, `${slugPrefix}/${name}`, [...trail, groupTitle])));
    } else if (files.has(name)) {
      entries.push({
        file: path.join(dir, `${name}.mdx`),
        slug: name === "index" ? slugPrefix : `${slugPrefix}/${name}`,
        sectionTrail: trail,
      });
    } else {
      process.stderr.write(
        `warning: meta.json in ${path.relative(WEBSITE_DIR, dir) || "."} lists "${name}", ` +
          `which does not exist — skipped\n`,
      );
    }
  }

  return entries;
}

/* -------------------------------------------------------------- rendering */

function renderHeader(pages, intro) {
  const lines = [
    "# Stella — complete documentation",
    "",
    `> ${intro}`,
    "",
    `This file is the full text of every page on the Stella documentation site,`,
    `concatenated in the site's own navigation order. It is generated from the site`,
    `sources, so it is the same content as ${DOCS_BASE} rather than a summary of it.`,
    "",
    `- Site: ${DOCS_BASE}`,
    `- Pages: ${pages.length}`,
    `- Generated by: website/scripts/build-llms-txt.mjs (do not edit this file by hand)`,
    "",
    "Each page below starts with a level-1 heading, its canonical URL, and its",
    "source path in the repository. Headings inside a page are level 2 and deeper.",
    "",
    "---",
    "",
    "## Contents",
    "",
  ];

  let trail = null;
  for (const page of pages) {
    const key = page.sectionTrail.join(" / ");
    if (key !== trail) {
      trail = key;
      if (key) lines.push("", `**${key}**`, "");
    }
    lines.push(`- [${page.title}](${SITE_URL}${page.slug})`);
  }

  return lines.join("\n");
}

function renderPage(page) {
  const heading = page.sectionTrail.length
    ? `${page.sectionTrail.join(" / ")} / ${page.title}`
    : page.title;

  const lines = [
    "---",
    "",
    `# ${heading}`,
    "",
    `URL: ${SITE_URL}${page.slug}`,
    `Source: ${path.relative(REPO_ROOT, page.file)}`,
  ];
  if (page.description) lines.push(`Description: ${page.description}`);
  lines.push("", page.markdown);

  return lines.join("\n");
}

/* ------------------------------------------------------------------- main */

async function main() {
  const check = process.argv.includes("--check");
  const diagrams = await readDiagramDescriptions();
  if (diagrams.size === 0) {
    throw new Error(`no diagram descriptions parsed from ${DIAGRAMS_SRC}`);
  }

  const entries = await walk(CONTENT_DIR, "/docs", []);

  const pages = [];
  for (const entry of entries) {
    const raw = await readFile(entry.file, "utf8");
    const { data, body } = parseFrontmatter(raw, path.relative(REPO_ROOT, entry.file));
    pages.push({
      ...entry,
      title: data.title ?? path.basename(entry.file, ".mdx"),
      description: data.description ?? "",
      markdown: mdxToMarkdown(body, `${SITE_URL}${entry.slug}`, diagrams),
    });
  }

  const intro = pages[0]?.description ?? "The Stella terminal coding agent.";
  const document = [renderHeader(pages, intro), ...pages.map(renderPage), ""].join("\n\n");

  if (check) {
    const existing = await readFile(OUT_FILE, "utf8").catch(() => null);
    if (existing !== document) {
      process.stderr.write(
        `${path.relative(REPO_ROOT, OUT_FILE)} is out of date — run \`pnpm docs:llms\`\n`,
      );
      process.exit(1);
    }
    process.stdout.write(`${path.relative(REPO_ROOT, OUT_FILE)} is up to date\n`);
    return;
  }

  await writeFile(OUT_FILE, document, "utf8");

  const words = document.split(/\s+/).length;
  process.stdout.write(
    `wrote ${path.relative(REPO_ROOT, OUT_FILE)} — ${pages.length} pages, ` +
      `${(document.length / 1024).toFixed(0)} KiB, ~${words.toLocaleString("en-US")} words\n`,
  );
}

main().catch((err) => {
  process.stderr.write(`${err.stack ?? err}\n`);
  process.exit(1);
});
