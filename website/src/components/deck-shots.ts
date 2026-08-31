/**
 * The command deck, rendered — one SVG per scenario, drawn at the character
 * grid the terminal paints on, served from `public/tui/`.
 *
 * These are the first page-referenced files under `public/`. Every other
 * graphic on this site is an inline SVG React component, and the reason these
 * are not is that they are not this site's drawings: they are renderings of a
 * terminal, authored against the deck spec, and inlining 60KB of `<text>` nodes
 * into `diagrams.tsx` would cost the file its size ceiling and buy nothing a
 * reader can see.
 *
 * The chip on every caption is the wordmark. These frames travel — one gets
 * screenshotted away from the page that framed it — so the name rides with
 * them rather than living only in the surrounding prose.
 *
 * The palette is not a coincidence: every hex in these files is a live token
 * from `design/tokens/stella-tokens.json` (`#0a0a0c` canvas, `#0f0f12` panel,
 * `#26262c` border, `#efc53f` gold), so `scripts/check-tokens.py` sweeps them
 * with everything else and a retired value cannot hide in one.
 *
 * This module is pure and `.ts`-only, like `diagram-descriptions.ts`, because
 * `src/lib/page-markdown.ts` imports it under `node --test`, which strips types
 * but cannot parse JSX.
 */

export interface DeckShot {
  /** File under `public/tui/`, without the directory or the extension. */
  file: string;
  /** Intrinsic size, so the browser reserves the box before the SVG lands. */
  width: number;
  height: number;
  /**
   * What the frame shows, as one sentence. Serves the `alt` attribute and the
   * markdown export both — a reader who cannot see the picture and a reader
   * who is handed the page as text get the same sentence.
   */
  alt: string;
  /** The line under the frame. Says what the frame proves, not what it is. */
  caption: string;
}

export const DECK_SHOTS = {
  turn: {
    file: "01-session-turn-lifecycle",
    width: 680,
    height: 520,
    alt: "A turn in the deck's session view: a skill injected, a folded read, an expanded edit diff, and a closing receipt carrying the turn's cost, token count, deterministic share, and test result.",
    caption:
      "The turn is the unit of the transcript. It opens with a rule, folds the reads that changed nothing, keeps the edit expanded, and closes on a receipt — cost, tokens, tests, and the share of the work that never reached a model.",
  },
  events: {
    file: "02-event-vocabulary",
    width: 680,
    height: 508,
    alt: "The deck's event vocabulary: a file write, a delete checked against the code graph before it ran, a memory logged and then promoted from observation to rule, and a one-line compaction whisper.",
    caption:
      "One rail colour per origin: gold is stella acting on the world, silver is the world coming in. Red appears only for failure, which is what makes a red row an alarm without anything blinking.",
  },
  task: {
    file: "03-task-zoom",
    width: 680,
    height: 520,
    alt: "A task opened to its contract: the done-means clause, three checks marked deterministic or model-judged, the planned sequence beside what actually happened, and a spend strip.",
    caption:
      "A task closes when its checks pass, not when the model says so. The contract is written before the work starts, the divergence between planned and actual is recorded rather than smoothed over, and the spend rides alongside.",
  },
  graph: {
    file: "04-graph",
    width: 680,
    height: 468,
    alt: "The deck's graph tab: symbols in a file ranked by edge count, inbound and outbound edges grouped by kind, and neighbours ranked by coupling.",
    caption:
      "Every answer on this tab is computed, so it costs $0.00 and returns in milliseconds. Coupling is the useful column: it is the blast radius if you edit this file.",
  },
  skills: {
    file: "05-skills",
    width: 680,
    height: 480,
    alt: "The deck's skills tab: installed skills with their injection counts and token cost, skills stella wrote itself after repeated wins, and a registry search showing signed and unsigned results.",
    caption:
      "Skills carry their own usage counts, so the ones that never fire are visible and can be pruned. The learned rows are the ones stella wrote from its own traces; an unsigned registry result installs disabled.",
  },
  mcp: {
    file: "06-mcp",
    width: 680,
    height: 420,
    alt: "The deck's MCP tab: four servers with transport, tool count, handshake latency and auth state, above a registry search offering signed and unsigned servers.",
    caption:
      "A new server lands disabled and shows you its capabilities before its first enable. The code graph is pinned at the top because it is the product rather than an integration.",
  },
  issues: {
    file: "07-issues",
    width: 680,
    height: 470,
    alt: "The deck's issues tab: a tracker backlog sorted by heat, one issue expanded to show its linked plan, branch and evidence, and a start-work strip.",
    caption:
      "The backlog arrives over MCP and sorts by heat — the coupling of the files an issue touches against its age, read off the graph. Status syncs back when the gates go green.",
  },
  palette: {
    file: "08-command-palette",
    width: 680,
    height: 584,
    alt: "The deck's command palette: a fuzzy-matched list of commands with the matched characters highlighted, a section of commands relevant to what is running now, and a recent section.",
    caption:
      "Fuzzy match over every command, with a section for the ones that make sense right now — while a verify turn is running, the gate commands come first.",
  },
  gate: {
    file: "09-gate-failure",
    width: 680,
    height: 520,
    alt: "A gate board with four gates green and an end-to-end smoke test red, the failing case and its assertion quoted underneath, and a proposed plan revision awaiting approval while the merge stays blocked.",
    caption:
      "A red gate is not something the model can argue with. It answers with a proposed plan revision naming the cause, and nothing runs until you approve it — while the merge stays blocked and the verify work keeps costing $0.00.",
  },
  "start-work": {
    file: "10-start-work",
    width: 680,
    height: 520,
    alt: "An issue being turned into a draft plan: the sources it was built from, four tasks each with a done-means clause, and an estimate of cost, tokens and time above an approval row.",
    caption:
      "An issue becomes a plan while you watch. The sources line names exactly what went in — the issue text, the coupled files from the graph, the memory rules that applied — and nothing is touched before you approve it.",
  },
} as const satisfies Record<string, DeckShot>;

export type DeckShotId = keyof typeof DECK_SHOTS;

/** Stable id list, for tests and for anything that iterates the set. */
export const DECK_SHOT_IDS = Object.keys(DECK_SHOTS) as DeckShotId[];
