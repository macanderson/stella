#!/usr/bin/env node
// stella-arena CLI — `pnpm arena <command>` from the repo root.
//
//   pnpm arena run --agents oxagen,claude-code --model anthropic/claude-sonnet-5 --trials 3
//   pnpm arena dev [--port 4646]
//   pnpm arena leaderboard
//
// Zero dependencies: node:util parseArgs + node:http only.

import { parseArgs } from "node:util";
import { AGENTS, resolveAgents } from "./agents.mjs";
import { loadTasks, runArena } from "./runner.mjs";

const USAGE = `stella arena — head-to-head coding-agent benchmark

usage:
  pnpm arena run --agents <a,b,…> [--model <provider/model>] [--trials N]
                 [--tasks <t,u,…>] [--timeout SECS] [--concurrency N] [--dry-run]
  pnpm arena dev [--port PORT]          launch the results web dashboard
  pnpm arena leaderboard                standings aggregated across all local runs
  pnpm arena agents                     list available agents
  pnpm arena tasks                      list available tasks

agents:  ${Object.keys(AGENTS).join(", ")}
model:   provider/model slug, e.g. anthropic/claude-sonnet-5 (adapters translate per CLI)
results: arena/results/<run-id>/ — trials.jsonl, run.json, workspaces/ (the receipts)
`;

function fail(msg) {
  console.error(`arena: ${msg}\n`);
  console.error(USAGE);
  process.exit(1);
}

const { values, positionals } = parseArgs({
  args: process.argv.slice(2),
  allowPositionals: true,
  options: {
    agents: { type: "string" },
    model: { type: "string" },
    trials: { type: "string", default: "1" },
    tasks: { type: "string" },
    timeout: { type: "string", default: "600" },
    concurrency: { type: "string", default: "3" },
    port: { type: "string", default: "4646" },
    "dry-run": { type: "boolean", default: false },
    help: { type: "boolean", short: "h", default: false },
  },
});

const command = positionals[0];
if (values.help || !command) {
  console.log(USAGE);
  process.exit(command ? 0 : 1);
}

const intFlag = (name, raw, min) => {
  const n = Number.parseInt(raw, 10);
  if (!Number.isInteger(n) || n < min) fail(`--${name} must be an integer ≥ ${min}, got \`${raw}\``);
  return n;
};

switch (command) {
  case "run": {
    if (!values.agents) fail("run requires --agents <name,name,…>");
    const agents = (() => {
      try {
        return resolveAgents(values.agents);
      } catch (e) {
        return fail(e.message);
      }
    })();
    const needsModel = agents.some((a) => AGENTS[a].requiresModel);
    if (needsModel && !values.model) {
      fail(`--model is required for real agents (${agents.filter((a) => AGENTS[a].requiresModel).join(", ")})`);
    }
    const tasks = (() => {
      try {
        return loadTasks(values.tasks);
      } catch (e) {
        return fail(e.message);
      }
    })();
    const { summary } = await runArena({
      agents,
      tasks,
      trials: intFlag("trials", values.trials, 1),
      model: values.model ?? "mock/none",
      timeoutSec: intFlag("timeout", values.timeout, 1),
      concurrency: intFlag("concurrency", values.concurrency, 1),
      dryRun: values["dry-run"],
    });
    // Non-zero only on harness failure; losing agents are a result, not an error.
    if (!values["dry-run"] && summary.length === 0) process.exit(1);
    break;
  }

  case "dev": {
    const { serve } = await import("./server.mjs");
    serve(intFlag("port", values.port, 1));
    break;
  }

  case "leaderboard": {
    const { printLeaderboard } = await import("./results.mjs");
    printLeaderboard();
    break;
  }

  case "agents": {
    for (const [name, a] of Object.entries(AGENTS)) console.log(`  ${name.padEnd(14)} ${a.describe}`);
    break;
  }

  case "tasks": {
    for (const t of loadTasks()) console.log(`  ${t.id.padEnd(18)} ${t.title}`);
    break;
  }

  default:
    fail(`unknown command \`${command}\``);
}
