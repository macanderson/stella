// Agent adapters — how the arena invokes each contender's CLI, one-shot and
// headless, inside a per-trial workspace. Every adapter receives the composed
// task prompt and the run's `--model` slug (provider/model form, e.g.
// `anthropic/claude-sonnet-5`) and returns { bin, args }. Credentials are
// whatever the spawned CLI already uses on this machine (BYOK / logged-in
// session) — the arena never handles or stores a key.

import path from "node:path";
import { fileURLToPath } from "node:url";

const ARENA_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** `anthropic/claude-sonnet-5` → `claude-sonnet-5`; rejects non-Anthropic slugs. */
function anthropicModelOrThrow(model) {
  const [provider, ...rest] = model.split("/");
  if (rest.length === 0) return model; // bare model id — pass through
  if (provider !== "anthropic") {
    throw new Error(
      `agent \`claude-code\` can only run Anthropic models — got \`${model}\`. ` +
        `Use anthropic/<model> or a bare model id.`,
    );
  }
  return rest.join("/");
}

export const AGENTS = {
  "claude-code": {
    describe: "Claude Code CLI (`claude -p`), headless JSON mode, edits auto-accepted",
    requiresModel: true,
    command: ({ prompt, model }) => ({
      bin: "claude",
      args: [
        "-p",
        prompt,
        "--model",
        anthropicModelOrThrow(model),
        "--output-format",
        "json",
        "--permission-mode",
        "acceptEdits",
      ],
    }),
  },

  oxagen: {
    describe: "Oxagen CLI one-shot, JSON envelope (model slug passed through verbatim)",
    requiresModel: true,
    command: ({ prompt, model }) => ({
      bin: "oxagen",
      args: ["--model", model, "--output-format", "json", prompt],
    }),
  },

  stella: {
    describe: "Stella CLI one-shot (`stella run`), JSON mode (model slug verbatim)",
    requiresModel: true,
    command: ({ prompt, model }) => ({
      bin: "stella",
      args: ["--model", model, "--output-format", "json", "run", prompt],
    }),
  },

  // Free, offline contenders that exercise the full pipeline (spawn → verify →
  // record → dashboard) without an API call. `mock-solver` copies the task's
  // known-good solution/ files into the workspace, so it should pass every
  // task — which doubles as a self-test of each task's verify script.
  "mock-solver": {
    describe: "Offline: applies tasks/<task>/solution/ — must pass (verifies the verifiers)",
    requiresModel: false,
    command: ({ taskId, workspace }) => ({
      bin: process.execPath,
      args: [
        path.join(ARENA_ROOT, "src", "mock-solver.mjs"),
        path.join(ARENA_ROOT, "tasks", taskId, "solution"),
        workspace,
      ],
    }),
  },

  "mock-noop": {
    describe: "Offline: does nothing — must fail every task (verifies failure handling)",
    requiresModel: false,
    command: () => ({
      bin: process.execPath,
      args: ["-e", "console.log(JSON.stringify({ mock: 'noop', edits: 0 }))"],
    }),
  },
};

export function resolveAgents(csv) {
  const names = csv
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  if (names.length === 0) throw new Error("--agents is empty");
  for (const n of names) {
    if (!AGENTS[n]) {
      throw new Error(
        `unknown agent \`${n}\` — available: ${Object.keys(AGENTS).join(", ")}`,
      );
    }
  }
  return names;
}
