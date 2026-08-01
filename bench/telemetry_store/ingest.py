"""Load Terminal-Bench job directories into SQLite. Idempotent — re-running
replaces a run rather than duplicating it.

Usage:
    python3 ingest.py --db bench.db --jobs /path/to/jobs \
        --run TAG --kind scored|smoke [--model M] [--void "reason"]

A run is a TAG; its two arms are `TAG-armA-stella` and `TAG-armB-claudecode`.
"""

from __future__ import annotations

import argparse
import glob
import hashlib
import json
import os
import re
import sqlite3
import sys
from datetime import datetime, timezone

# Measured, not guessed: over the runs to date these four types are 87% of all
# events (block_registered 134,776 + text_delta 125,331 against ~30k of
# everything else) and none of them answers a question anyone asks twice.
DROP_EVENT_TYPES = {
    "block_registered",
    "text_delta",
    "reasoning",
    "reasoning_delta",
    "token",
    "delta",
    "text",
    # 8,363 events averaging 14.6 KB — 72% of the database on its own. It is a
    # per-step snapshot of the assembled context, which is state rather than
    # outcome: nothing about who solved what, at what cost, is derivable from
    # it. The raw stream stays recoverable through the `artifacts` rows, which
    # is the point of recording provenance for what we do not store.
    "step_manifest",
}
# Arm A is always Stella, arm B always the comparator. Matched by the `armA`/
# `armB` segment rather than the full suffix so an archived run
# (`VOID-...-glm89-armA`) still resolves — a voided run is the one you most
# need to be able to re-read.
ARMS = (("stella", "armA"), ("claude-code", "armB"))
TRIAL_SUFFIX = re.compile(r"__[A-Za-z0-9]{7}$")


def now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def jload(path: str):
    try:
        with open(path, errors="ignore") as fh:
            return json.load(fh)
    except Exception:
        return None


def connect(db: str) -> sqlite3.Connection:
    conn = sqlite3.connect(db)
    conn.execute("PRAGMA foreign_keys = ON")
    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "schema.sql")) as fh:
        conn.executescript(fh.read())
    return conn


def ingest_trial(conn, run_id, arm, agent, tdir):
    base = os.path.basename(tdir.rstrip("/"))
    task = TRIAL_SUFFIX.sub("", base)
    trial_id = f"{run_id}:{arm}:{base}"

    r = jload(os.path.join(tdir, "result.json")) or {}
    ar = r.get("agent_result") or {}
    ei = r.get("exception_info") or {}
    reward = ((r.get("verifier_result") or {}).get("rewards") or {}).get("reward")

    conn.execute(
        "INSERT OR REPLACE INTO trials (trial_id,run_id,arm,agent,task_name,trial_dir,"
        "reward,passed,exception_type,exception_message,n_input_tokens,n_cache_tokens,"
        "n_output_tokens,cost_usd_self,cost_usd_norm,wall_seconds,started_at,finished_at)"
        " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            trial_id, run_id, arm, agent, task, tdir,
            reward, 1 if (reward or 0) >= 1 else 0,
            ei.get("exception_type"), (ei.get("exception_message") or "")[:2000],
            ar.get("n_input_tokens"), ar.get("n_cache_tokens"), ar.get("n_output_tokens"),
            ar.get("cost_usd"), None, None, None, None,
        ),
    )

    # Structural events (Stella emits these; Claude Code does not).
    ev = os.path.join(tdir, "agent", "stella-events.jsonl")
    n_ev = n_tc = 0
    if os.path.exists(ev):
        with open(ev, errors="ignore") as fh:
            for seq, line in enumerate(fh):
                line = line.strip()
                if not line:
                    continue
                try:
                    e = json.loads(line)
                except Exception:
                    continue
                t = e.get("type") or ""
                if t in DROP_EVENT_TYPES:
                    continue
                conn.execute(
                    "INSERT OR REPLACE INTO events (event_id,trial_id,seq,type,payload_json)"
                    " VALUES (?,?,?,?,?)",
                    (f"{trial_id}:{seq}", trial_id, seq, t, json.dumps(e)[:20000]),
                )
                n_ev += 1
                if t in ("tool_start", "tool_call"):
                    # The name is nested under `call`, not top-level — reading
                    # `e["tool"]` silently yielded zero tool calls for every
                    # run, which looks identical to an agent that called none.
                    call = e.get("call")
                    tool = None
                    if isinstance(call, dict):
                        tool = call.get("name")
                    tool = tool or e.get("tool") or e.get("name")
                    if tool:
                        conn.execute(
                            "INSERT OR REPLACE INTO tool_calls (id,trial_id,seq,tool)"
                            " VALUES (?,?,?,?)",
                            (f"{trial_id}:{seq}", trial_id, seq, tool),
                        )
                        n_tc += 1

    # Grader detail.
    ctrf = jload(os.path.join(tdir, "verifier", "ctrf.json")) or {}
    tests = ((ctrf.get("results") or {}).get("tests")) or []
    for i, t in enumerate(tests if isinstance(tests, list) else []):
        if not isinstance(t, dict):
            continue
        conn.execute(
            "INSERT OR REPLACE INTO verifier_tests (id,trial_id,name,status,duration_ms,message)"
            " VALUES (?,?,?,?,?,?)",
            (f"{trial_id}:{i}", trial_id, str(t.get("name"))[:500], t.get("status"),
             t.get("duration"), (t.get("message") or "")[:2000]),
        )

    # Provenance for the bytes we did not store.
    n_art = 0
    for path in glob.glob(os.path.join(tdir, "**", "*"), recursive=True):
        if not os.path.isfile(path):
            continue
        try:
            size = os.path.getsize(path)
            conn.execute(
                "INSERT OR REPLACE INTO artifacts (id,trial_id,kind,rel_path,bytes,sha256)"
                " VALUES (?,?,?,?,?,?)",
                (f"{trial_id}:{os.path.relpath(path, tdir)}", trial_id,
                 os.path.basename(path), os.path.relpath(path, tdir), size, sha256(path)),
            )
            n_art += 1
        except Exception:
            continue
    return n_ev, n_tc, n_art


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True)
    ap.add_argument("--jobs", required=True)
    ap.add_argument("--run", required=True, help="tag, e.g. fair20b")
    ap.add_argument("--kind", default="smoke", choices=["scored", "smoke"])
    ap.add_argument("--model", default=None)
    ap.add_argument("--void", default=None, help="reason this run is invalid")
    ap.add_argument("--prereg", default=None)
    ap.add_argument("--preflight", default=None)
    ap.add_argument("--prefix", default="", help="job dir prefix, e.g. VOID-provider-confound-")
    a = ap.parse_args()

    conn = connect(a.db)
    conn.execute(
        "INSERT OR REPLACE INTO runs (run_id,tag,kind,void_reason,model,api_surface,"
        "dataset_digest,sut_commit,binary_sha256,taskset_sha256,task_count,prereg_json,"
        "preflight_text,started_at,finished_at,ingested_at,notes)"
        " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (a.run, a.run, a.kind, a.void, a.model, None, None, None, None, None, None,
         (open(a.prereg).read() if a.prereg and os.path.exists(a.prereg) else None),
         (open(a.preflight).read() if a.preflight and os.path.exists(a.preflight) else None),
         None, None, now(), None),
    )

    totals = {"trials": 0, "events": 0, "tools": 0, "artifacts": 0}
    for arm, seg in ARMS:
        matches = [
            d for d in glob.glob(os.path.join(a.jobs, f"{a.prefix}{a.run}-{seg}*"))
            if os.path.isdir(d)
        ]
        if not matches:
            continue
        job = sorted(matches)[0]
        for tdir in sorted(glob.glob(os.path.join(job, "*/"))):
            if not os.path.exists(os.path.join(tdir, "result.json")):
                continue
            e, t, ar = ingest_trial(conn, a.run, arm, arm, tdir)
            totals["trials"] += 1
            totals["events"] += e
            totals["tools"] += t
            totals["artifacts"] += ar
    conn.commit()
    print(f"{a.run}: {totals}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
