"use client";

/**
 * A clickable reproduction of the Command Deck's own tab strip.
 *
 * This is not an embedded terminal and it is not a screen recording — it is
 * nine static panes, one per real deck tab, and clicking a tab swaps which
 * one is showing. Each pane's content is trimmed straight out of
 * `crates/stella-tui/tests/snapshots/deck/tab_*.txt`, the golden renders that
 * `make gate` checks on every PR touching the TUI, so what is on screen here
 * is the deck's own output rather than a redrawn mock of it.
 *
 * Deliberately not a repeat of the old animated command-deck (see
 * `command-deck.tsx`'s doc comment for why that one was cut): there is no
 * timer, nothing plays on a loop, and nothing moves without a click — so
 * there is no `prefers-reduced-motion` case to design around.
 */

import { useRef, useState } from "react";

interface DeckTabSpec {
  key: string;
  label: string;
  blurb: string;
  lines: string[];
}

const TABS: DeckTabSpec[] = [
  {
    key: "session",
    label: "SESSION",
    blurb:
      "The transcript for the turn in flight — prompts, the live plan, and each stage as it runs.",
    lines: [
      "? lead  ·  needs input                                     0:00:00",
      "└─ ◆ sub:auth  needs input · model glm-5.2 · spend $0.01",
      "     ⚒ mcp__fs__grep trigger → wire automations triggers API",
      "└─ ◆ sub:ci    running    · model glm-5.2-air · spend $0.00",
      "     ⚒ no tool calls yet → watch CI + open PR",
      "",
      "  Planning the automations cluster: list, editor, triggers, workflows.",
      "☰  plan  4/7 · Wire the triggers API",
      "── WITNESS ──",
      "── EXECUTE ──",
      "● mcp__fs__read  apps/app/automations/page.tsx  ⎿ 312 lines · 42ms",
    ],
  },
  {
    key: "agents",
    label: "AGENTS",
    blurb:
      "Every execution — lead and sub-agents — with live cost, context %, and cache warmth. Paired with your installed agents.",
    lines: [
      "EXECUTIONS │ INSTALLED AGENTS",
      "Agent        Goal                          Status        Cost    $/hr",
      "  lead       web-app-2.0 · phase           needs input   $0.04   $0.45",
      "  sub:auth   wire automations triggers     needs input   $0.01   $0.13",
      "▶ sub:ci      watch CI + open PR            running       $0.00   $0.05",
    ],
  },
  {
    key: "traces",
    label: "TRACES",
    blurb:
      "One scrollable, filterable timeline of every agent's tool calls, verdicts, and spend, interleaved by time.",
    lines: [
      "traces · all · 33 events · following",
      "00:00 lead      [stage] witness",
      "00:00 lead      [verdict] witness authored: automations/triggers.test.ts",
      "00:00 lead      [verdict] oracle: fail on base",
      "00:00 lead      [tool] mcp__fs__read() → ok in 42ms",
      "00:00 lead      [file] modified automations/page.tsx +4/-1",
      "00:00 lead      [verdict] oracle: pass on new",
      "00:00 sub:auth  [stage] wire the automations triggers API",
      "00:00 sub:ci    [vcs] pr open",
    ],
  },
  {
    key: "graph",
    label: "GRAPH",
    blurb:
      "The tree-sitter code graph — pick a symbol, see its file, its relations, and its neighborhood. Answered offline, no key needed.",
    lines: [
      "nodes · 6 · /files",
      "ƒ run_turn   ◆ Engine   ◆ Router   ◆ AgentEvent   ◆ Ledger   ▤ driver.rs",
      "",
      "Engine — struct · stella-core/src/engine.rs:48",
      "relations · 3",
      "called by ← run_turn",
      "uses      → Router",
      "writes    → Ledger",
    ],
  },
  {
    key: "files",
    label: "FILES",
    blurb: "Every file this session touched, by agent, with lines added and removed.",
    lines: [
      "Files · 2",
      "File                                     Agent      Op   +    -",
      "apps/app/automations/page.tsx            lead       U    +4   -1",
      "apps/api/routes/v1/automations.ts        sub:auth   C    +3   -0",
      "2 files  +7  -1  0 reads",
    ],
  },
  {
    key: "skills",
    label: "SKILLS",
    blurb:
      "Installed skills you can toggle on or off, plus a live search of the skills registry — install without leaving the deck.",
    lines: [
      "Installed — 4 (user + project)",
      "[x] release-checklist v3  (project·workspace)  Cut a release…",
      "[x] rust-review v2/4      (user·user)          Review Rust diffs…",
      "[ ] sql-tuning v1         (user·installed)     Read a query plan…",
      "[x] bench-rig-access v1   (project·auto)       Reach the bench rig…",
      "",
      "Registry search — find: _   (⏎ to search)",
    ],
  },
  {
    key: "mcp",
    label: "MCP",
    blurb:
      "Configured MCP servers — connection state, tool counts, auth — enable, disable, or authenticate without leaving the deck.",
    lines: [
      "MCP — 3 configured / 2 connected",
      "● Stripe   http   live      ⚿ oauth ✓  · 21 tools · 14×",
      "● fs       stdio  live      · 4 tools",
      "○ Linear   http   disabled  ⚿ oauth: o to log in · 0 tools",
    ],
  },
  {
    key: "issues",
    label: "ISSUES",
    blurb:
      "Your connected GitHub or Linear tracker, with instant search and the ability to start work on an issue.",
    lines: [
      "ISSUES — 3 listed",
      "#874 [open]    Command-deck render golden tests · enhancement",
      "#826 [open]    run_deck event-loop pty harness · enhancement",
      "#689 [closed]  Surface per-server MCP tool truncation · bug",
    ],
  },
  {
    key: "settings",
    label: "SETTINGS",
    blurb: "The home of all config — agents, tools, models, credentials — including the engine-config editor.",
    lines: [
      "AGENTS │ TOOLS",
      "GLOBAL   default   worker   verifier   triage   research   plan",
      "",
      "waiting for the engine config snapshot — r to reload",
      "e edit agents config",
    ],
  },
];

export function CommandDeckExplorer() {
  const [activeKey, setActiveKey] = useState(TABS[0].key);
  const tabRefs = useRef<Record<string, HTMLButtonElement | null>>({});
  const active = TABS.find((tab) => tab.key === activeKey) ?? TABS[0];

  function focusTab(index: number) {
    const wrapped = TABS[(index + TABS.length) % TABS.length];
    setActiveKey(wrapped.key);
    tabRefs.current[wrapped.key]?.focus();
  }

  function handleKeyDown(event: React.KeyboardEvent<HTMLButtonElement>, index: number) {
    if (event.key === "ArrowRight") {
      event.preventDefault();
      focusTab(index + 1);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      focusTab(index - 1);
    }
  }

  return (
    <div className="dke">
      <div className="dke-tabs" role="tablist" aria-label="Command Deck tabs">
        {TABS.map((tab, index) => (
          <button
            key={tab.key}
            ref={(el) => {
              tabRefs.current[tab.key] = el;
            }}
            type="button"
            role="tab"
            id={`dke-tab-${tab.key}`}
            aria-selected={tab.key === activeKey}
            aria-controls={`dke-panel-${tab.key}`}
            tabIndex={tab.key === activeKey ? 0 : -1}
            className="dke-tab"
            data-active={tab.key === activeKey}
            onClick={() => setActiveKey(tab.key)}
            onKeyDown={(event) => handleKeyDown(event, index)}
          >
            {tab.label}
          </button>
        ))}
      </div>
      <div
        className="dke-pane"
        role="tabpanel"
        id={`dke-panel-${active.key}`}
        aria-labelledby={`dke-tab-${active.key}`}
      >
        <p className="dke-blurb">{active.blurb}</p>
        <pre className="dke-lines">{active.lines.join("\n")}</pre>
      </div>
    </div>
  );
}
