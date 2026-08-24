import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import { DIAGRAM_DESCRIPTIONS } from "../components/diagram-descriptions.ts";
import { COMMAND_DECK_TABS } from "../components/command-deck-tabs.ts";
import { PROVIDER_CATALOG } from "../components/provider-catalog.ts";
import {
  DIAGRAM_PLACEHOLDER_NAMES,
  PLACEHOLDER_RENDERERS,
  pageMarkdown,
} from "./page-markdown.ts";

/**
 * The markdown export behind the "Copy page" menu and `/llms.mdx/<slug>`.
 *
 *   pnpm test          (from website/)
 *
 * ## Why this file exists
 *
 * Every failure mode of the export is silent: a placeholder renderer that
 * stops matching the name `source.config.ts` emits degrades to the
 * component's *children* — for a diagram, that is nothing, so the exported
 * markdown just quietly loses the diagram. A `ProviderGrid` whose `only`
 * attribute stops parsing renders the full catalog on a page that shows
 * three. Nothing in the build fails either way; only a reader comparing the
 * page to its export would notice. These tests are that comparison, run on
 * every change.
 */

const HERE = dirname(fileURLToPath(import.meta.url));
const SOURCE_CONFIG = readFileSync(join(HERE, "../../source.config.ts"), "utf8");

test("every placeholder name source.config.ts emits has a renderer", () => {
  // The config builds its list from DIAGRAM_PLACEHOLDER_NAMES plus two string
  // literals; assert the literals are still there and the combined set is
  // exactly the renderer set, so neither side can drift.
  assert.match(SOURCE_CONFIG, /"ProviderGrid"/);
  assert.match(SOURCE_CONFIG, /"CommandDeckExplorer"/);
  assert.deepEqual(
    Object.keys(PLACEHOLDER_RENDERERS).sort(),
    [...DIAGRAM_PLACEHOLDER_NAMES, "ProviderGrid", "CommandDeckExplorer"].sort(),
  );
});

test("every diagram placeholder renders its aria-label sentence", async () => {
  for (const name of DIAGRAM_PLACEHOLDER_NAMES) {
    const render = PLACEHOLDER_RENDERERS[name];
    assert.ok(render, `no renderer for ${name}`);
    const out = await render({ name, attributes: {}, children: "" });
    assert.equal(out, `*${DIAGRAM_DESCRIPTIONS[name]}*`);
    assert.ok(out.length > 20, `${name} rendered suspiciously short`);
  }
});

test("ProviderGrid renders the full catalog as a GFM table", async () => {
  const out = await PLACEHOLDER_RENDERERS.ProviderGrid({
    name: "ProviderGrid",
    attributes: {},
    children: "",
  });
  assert.match(out, /^\| Provider \| id \| Env var \|/);
  for (const p of PROVIDER_CATALOG) {
    assert.ok(out.includes(`\`${p.id}\``), `missing provider ${p.id}`);
    assert.ok(out.includes(p.name), `missing name for ${p.id}`);
  }
  // One header row, one separator, one row per provider.
  assert.equal(out.trim().split("\n").length, PROVIDER_CATALOG.length + 2);
});

test("ProviderGrid honours the `only` attribute", async () => {
  const only = PROVIDER_CATALOG.slice(0, 2).map((p) => p.id);
  const out = await PLACEHOLDER_RENDERERS.ProviderGrid({
    name: "ProviderGrid",
    attributes: { only },
    children: "",
  });
  assert.equal(out.trim().split("\n").length, only.length + 2);
  assert.ok(out.includes(`\`${only[0]}\``));
  assert.ok(!out.includes(`\`${PROVIDER_CATALOG[2].id}\``));
});

test("CommandDeckExplorer renders every tab as a fenced block", async () => {
  const out = await PLACEHOLDER_RENDERERS.CommandDeckExplorer({
    name: "CommandDeckExplorer",
    attributes: {},
    children: "",
  });
  for (const tab of COMMAND_DECK_TABS) {
    assert.ok(out.includes(`### ${tab.label}`), `missing tab ${tab.label}`);
    assert.ok(out.includes(tab.lines[0]), `missing lines for ${tab.label}`);
  }
});

test("pageMarkdown assembles header, source line, and rendered body", async () => {
  const placeholder = `\0${JSON.stringify({
    name: "HeroFlowDiagram",
    attributes: {},
    children: "",
  })}\0`;
  const out = await pageMarkdown({
    title: "Agent modes",
    description: "Pick the loop that fits the work.",
    url: "https://stella.oxagen.sh/docs/agent-modes",
    markdown: `Some prose.\n\n${placeholder}\n\nMore prose.`,
  });
  assert.ok(out.startsWith("# Agent modes\n\n> Pick the loop that fits the work.\n\nSource: https://stella.oxagen.sh/docs/agent-modes\n\n"));
  assert.ok(out.includes("Some prose."));
  assert.ok(out.includes(`*${DIAGRAM_DESCRIPTIONS.HeroFlowDiagram}*`));
  assert.ok(!out.includes("\0"), "an unrendered placeholder survived");
  assert.ok(out.endsWith("\n"));
});

test("a page with no description omits the blockquote line", async () => {
  const out = await pageMarkdown({
    title: "Reference",
    url: "https://stella.oxagen.sh/docs/reference",
    markdown: "Body.",
  });
  assert.ok(out.startsWith("# Reference\n\nSource:"));
});

test("residual self-closing logomark tags are stripped from the body", async () => {
  // remarkLLMs re-serializes self-closing inline JSX as literal tags even when
  // the stringify callback returns "" for them (see page-markdown.ts); the
  // strip pass in pageMarkdown is what keeps them out of the export.
  const out = await pageMarkdown({
    title: "Models",
    url: "https://stella.oxagen.sh/docs/api-providers/models",
    markdown:
      '| <ProviderMark id="anthropic" /> `claude-fable-5` | `anthropic` |\n\n<ProviderLogo id="openai" /> inline',
  });
  assert.ok(!out.includes("<ProviderMark"), "a ProviderMark tag survived");
  assert.ok(!out.includes("<ProviderLogo"), "a ProviderLogo tag survived");
  assert.ok(out.includes("`claude-fable-5`"), "the text beside the mark was stripped too");
});
