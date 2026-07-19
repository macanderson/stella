// The trial engine: (agent × task × trial) → isolated workspace → spawn the
// agent CLI → run the task's offline verifier → append a receipt line to
// trials.jsonl. Success is decided ONLY by the verifier's exit code, never by
// the agent's — an agent that crashes but leaves a passing workspace passes,
// and vice versa (same contract as the SWE-bench harness in bench/).

import { execFile } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { AGENTS } from "./agents.mjs";

export const ARENA_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const TASKS_DIR = path.join(ARENA_ROOT, "tasks");
export const RESULTS_DIR = path.join(ARENA_ROOT, "results");

const PROMPT_SUFFIX =
  "\n\nRules: work only inside the current directory. Edit the files needed so that " +
  "`node verify.mjs` exits 0. Do not modify or delete verify.mjs. Do not create a new " +
  "project or install dependencies.";

export function loadTasks(tasksCsv) {
  const wanted = tasksCsv
    ? tasksCsv.split(",").map((s) => s.trim()).filter(Boolean)
    : null;
  const ids = fs
    .readdirSync(TASKS_DIR, { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .map((d) => d.name)
    .sort();
  const pick = wanted ?? ids;
  return pick.map((id) => {
    const file = path.join(TASKS_DIR, id, "task.json");
    if (!fs.existsSync(file)) {
      throw new Error(`unknown task \`${id}\` — available: ${ids.join(", ")}`);
    }
    const task = JSON.parse(fs.readFileSync(file, "utf8"));
    return { id, ...task };
  });
}

/** Best-effort cost/token extraction from an agent's JSON envelope (shapes differ per CLI). */
function extractMetrics(stdout) {
  const out = { costUsd: null, inputTokens: null, outputTokens: null };
  if (!stdout) return out;
  // The envelope may be the whole stdout or the last JSON-looking line.
  const candidates = [stdout.trim(), ...stdout.trim().split("\n").reverse()];
  for (const c of candidates) {
    let env;
    try {
      env = JSON.parse(c);
    } catch {
      continue;
    }
    const scan = (obj) => {
      if (!obj || typeof obj !== "object") return;
      for (const [k, v] of Object.entries(obj)) {
        if (typeof v === "number") {
          if (out.costUsd === null && /^(total_)?cost(_usd)?$|^usd$/i.test(k)) out.costUsd = v;
          if (out.inputTokens === null && /^(total_)?input_tokens$/i.test(k)) out.inputTokens = v;
          if (out.outputTokens === null && /^(total_)?output_tokens$/i.test(k)) out.outputTokens = v;
        } else if (typeof v === "object") {
          scan(v);
        }
      }
    };
    scan(env);
    return out;
  }
  return out;
}

function execCapture(bin, args, { cwd, timeoutMs }) {
  return new Promise((resolve) => {
    const started = Date.now();
    execFile(
      bin,
      args,
      {
        cwd,
        timeout: timeoutMs,
        killSignal: "SIGKILL",
        maxBuffer: 32 * 1024 * 1024,
        env: process.env,
      },
      (error, stdout, stderr) => {
        resolve({
          exitCode: error ? (typeof error.code === "number" ? error.code : null) : 0,
          timedOut: Boolean(error?.killed),
          spawnError: error && typeof error.code === "string" ? error.code : null, // e.g. ENOENT
          stdout: String(stdout ?? ""),
          stderr: String(stderr ?? ""),
          durationMs: Date.now() - started,
        });
      },
    );
  });
}

function tail(s, n = 2000) {
  return s.length > n ? `…${s.slice(-n)}` : s;
}

async function runTrial({ runId, runDir, agentName, task, trial, model, timeoutSec }) {
  const agent = AGENTS[agentName];
  const slug = `${agentName}__${task.id}__t${trial}`;
  const workspace = path.join(runDir, "workspaces", slug);
  fs.cpSync(path.join(TASKS_DIR, task.id, "fixture"), workspace, { recursive: true });

  const prompt = task.prompt + PROMPT_SUFFIX;
  const { bin, args } = agent.command({ prompt, model, taskId: task.id, workspace });

  const startedAt = new Date().toISOString();
  const run = await execCapture(bin, args, {
    cwd: workspace,
    timeoutMs: (task.timeoutSec ?? timeoutSec) * 1000,
  });
  const verify = run.spawnError
    ? null
    : await execCapture(process.execPath, ["verify.mjs"], { cwd: workspace, timeoutMs: 60_000 });

  const metrics = extractMetrics(run.stdout);
  return {
    runId,
    agent: agentName,
    task: task.id,
    trial,
    model: agent.requiresModel ? model : null,
    pass: verify?.exitCode === 0,
    startedAt,
    endedAt: new Date().toISOString(),
    durationMs: run.durationMs,
    timedOut: run.timedOut,
    agentExitCode: run.exitCode,
    spawnError: run.spawnError,
    verifyExitCode: verify?.exitCode ?? null,
    costUsd: metrics.costUsd,
    inputTokens: metrics.inputTokens,
    outputTokens: metrics.outputTokens,
    workspace: path.relative(ARENA_ROOT, workspace),
    agentStdoutTail: tail(run.stdout),
    agentStderrTail: tail(run.stderr),
    verifyOutputTail: verify ? tail(verify.stdout + verify.stderr, 500) : null,
  };
}

/** Simple promise pool: run `jobs` (thunks) with at most `limit` in flight. */
async function pool(jobs, limit) {
  const results = new Array(jobs.length);
  let next = 0;
  const worker = async () => {
    while (next < jobs.length) {
      const i = next++;
      results[i] = await jobs[i]();
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, jobs.length) }, worker));
  return results;
}

export function summarize(trials) {
  const byAgent = {};
  for (const t of trials) {
    const a = (byAgent[t.agent] ??= {
      agent: t.agent,
      model: t.model,
      trials: 0,
      passes: 0,
      totalDurationMs: 0,
      totalCostUsd: 0,
      costKnown: 0,
    });
    a.trials += 1;
    if (t.pass) a.passes += 1;
    a.totalDurationMs += t.durationMs;
    if (typeof t.costUsd === "number") {
      a.totalCostUsd += t.costUsd;
      a.costKnown += 1;
    }
  }
  return Object.values(byAgent)
    .map((a) => ({
      ...a,
      passRate: a.trials ? a.passes / a.trials : 0,
      avgDurationMs: a.trials ? Math.round(a.totalDurationMs / a.trials) : 0,
    }))
    .sort((x, y) => y.passRate - x.passRate || x.avgDurationMs - y.avgDurationMs);
}

export async function runArena({ agents, tasks, trials, model, timeoutSec, concurrency, dryRun }) {
  const runId = new Date().toISOString().replace(/[:.]/g, "-").replace("T", "_").slice(0, 19);
  const runDir = path.join(RESULTS_DIR, runId);

  const plan = [];
  for (const agentName of agents)
    for (const task of tasks)
      for (let trial = 1; trial <= trials; trial++) plan.push({ agentName, task, trial });

  if (dryRun) {
    console.log(`arena dry-run — ${plan.length} trials would run (run id ${runId}):`);
    for (const p of plan) {
      const agent = AGENTS[p.agentName];
      const { bin, args } = agent.command({
        prompt: `<${p.task.id} prompt>`,
        model,
        taskId: p.task.id,
        workspace: `<workspace ${p.agentName}__${p.task.id}__t${p.trial}>`,
      });
      console.log(`  ${p.agentName}  ${p.task.id}  trial ${p.trial}  →  ${bin} ${args.map((a) => (a.length > 60 ? `'${a.slice(0, 57)}…'` : a)).join(" ")}`);
    }
    return { runId, trials: [], summary: [] };
  }

  fs.mkdirSync(path.join(runDir, "workspaces"), { recursive: true });
  const trialsPath = path.join(runDir, "trials.jsonl");
  console.log(
    `arena run ${runId} — ${agents.length} agent(s) × ${tasks.length} task(s) × ${trials} trial(s) = ${plan.length} trials, concurrency ${concurrency}`,
  );

  let done = 0;
  const records = await pool(
    plan.map((p) => async () => {
      const rec = await runTrial({
        runId,
        runDir,
        agentName: p.agentName,
        task: p.task,
        trial: p.trial,
        model,
        timeoutSec,
      });
      fs.appendFileSync(trialsPath, `${JSON.stringify(rec)}\n`);
      done += 1;
      const status = rec.pass ? "PASS" : rec.spawnError ? `SPAWN-${rec.spawnError}` : rec.timedOut ? "TIMEOUT" : "FAIL";
      console.log(
        `  [${done}/${plan.length}] ${rec.agent.padEnd(12)} ${rec.task.padEnd(16)} t${rec.trial}  ${status.padEnd(12)} ${(rec.durationMs / 1000).toFixed(1)}s${typeof rec.costUsd === "number" ? `  $${rec.costUsd.toFixed(4)}` : ""}`,
      );
      return rec;
    }),
    concurrency,
  );

  const summary = summarize(records);
  fs.writeFileSync(
    path.join(runDir, "run.json"),
    JSON.stringify(
      {
        runId,
        startedAt: records[0]?.startedAt ?? null,
        endedAt: new Date().toISOString(),
        config: { agents, tasks: tasks.map((t) => t.id), trials, model, timeoutSec, concurrency },
        summary,
      },
      null,
      2,
    ),
  );

  console.log("\n  standings:");
  console.log(`  ${"agent".padEnd(14)} ${"pass".padEnd(10)} ${"avg time".padEnd(10)} cost`);
  for (const s of summary) {
    console.log(
      `  ${s.agent.padEnd(14)} ${`${s.passes}/${s.trials}`.padEnd(10)} ${`${(s.avgDurationMs / 1000).toFixed(1)}s`.padEnd(10)} ${s.costKnown ? `$${s.totalCostUsd.toFixed(4)}` : "—"}`,
    );
  }
  console.log(`\n  receipts: arena/results/${runId}/  (trials.jsonl, run.json, workspaces/)`);
  console.log("  browse:   pnpm arena dev");
  return { runId, trials: records, summary };
}
