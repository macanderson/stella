// `pnpm arena dev` — the arena web app: a zero-dependency local server that
// serves the dashboard (web/index.html) and a tiny read-only JSON API over
// the receipts in arena/results/. Local-only by design; binds 127.0.0.1.

import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { ARENA_ROOT } from "./runner.mjs";
import { listRuns, readTrials, leaderboard } from "./results.mjs";

const WEB_DIR = path.join(ARENA_ROOT, "web");
const RUN_ID = /^[A-Za-z0-9_-]+$/;

function json(res, body, status = 200) {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

export function serve(port) {
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, "http://localhost");
    try {
      if (url.pathname === "/" || url.pathname === "/index.html") {
        res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
        res.end(fs.readFileSync(path.join(WEB_DIR, "index.html")));
      } else if (url.pathname === "/api/runs") {
        json(res, listRuns());
      } else if (url.pathname === "/api/leaderboard") {
        json(res, leaderboard());
      } else if (url.pathname.startsWith("/api/runs/")) {
        const runId = url.pathname.slice("/api/runs/".length);
        if (!RUN_ID.test(runId)) return json(res, { error: "bad run id" }, 400);
        const run = listRuns().find((r) => r.runId === runId);
        if (!run) return json(res, { error: "no such run" }, 404);
        json(res, { ...run, trials: readTrials(runId) });
      } else {
        json(res, { error: "not found" }, 404);
      }
    } catch (e) {
      json(res, { error: String(e?.message ?? e) }, 500);
    }
  });
  server.listen(port, "127.0.0.1", () => {
    console.log(`⚔ arena dashboard → http://127.0.0.1:${port}`);
    console.log("   (results refresh on reload; Ctrl-C to stop)");
  });
}
