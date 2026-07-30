#!/usr/bin/env python3
"""Live status of a running audit — and, after it, proof that the model mix happened.

This exists because of the failure mode that has killed more audit runs than anything else:
a long silent run provokes the user to type something, typing ends the turn, and every
in-flight subagent dies mid-read. Frequent, specific progress is not politeness — it is how
the run survives. This is a PULL (safe, no side effects), unlike a Monitor, which pushes and
gets auto-stopped when it fires too often.

    python3 progress.py [transcript-dir] [--watch 60] [--provenance] [--json out.json]

With no directory, the most recently modified workflow transcript directory is used.

The first fifteen minutes of a healthy run look exactly like a dead one, so this reports
liveness (growing transcripts, started-vs-finished) rather than waiting for a first result.
"""
from __future__ import annotations

import argparse
import collections
import glob
import json
import os
import re
import subprocess
import sys
import time

WF_GLOB = os.path.expanduser("~/.claude/projects/*/*/subagents/workflows/wf_*")
# Bounded character classes, NOT \S+. In a raw .jsonl the prompt's newlines are the two
# characters backslash-n, which \S+ happily swallows — so `model=opus` parsed as
# `opus\n\nbody"}}`, the slot lookup missed, and every mismatch check was silently skipped
# while still reporting "every assignment matched". Caught by the negative-control test.
HDR = re.compile(r'ULTRA-AUDIT-AGENT phase=([\w.:-]+) role=([^\s\\"]+) model=([\w.-]+)')
PHASE_ORDER = ["regression", "unit", "fixreview", "lens", "sweep", "verify", "refute",
               "panel", "critique", "synthesis"]


def newest_dir() -> str | None:
    dirs = [d for d in glob.glob(WF_GLOB) if os.path.isdir(d)]
    if not dirs:
        return None
    return max(dirs, key=lambda d: os.path.getmtime(d))


def rg(pattern: str, files: list[str], count: bool = False, first: bool = True) -> dict:
    """One ripgrep pass over every transcript; far cheaper than reading them in Python."""
    if not files:
        return {}
    args = ["rg", "--no-heading", "--with-filename", "-e", pattern]
    args += ["-c"] if count else ["-o"] + (["-m", "1"] if first else [])
    try:
        p = subprocess.run(args + files, capture_output=True, text=True, timeout=120)
    except Exception:
        return {}
    out: dict[str, str] = {}
    for line in p.stdout.splitlines():
        path, _, val = line.partition(":")
        out.setdefault(path, val)
    return out


def collect(d: str) -> dict:
    jpath = os.path.join(d, "journal.jsonl")
    started, results = [], set()
    agent_of_key: dict[str, str] = {}
    if os.path.exists(jpath):
        for line in open(jpath, errors="replace"):
            try:
                o = json.loads(line)
            except Exception:
                continue
            # The key is "result", not "value". A parser reading the wrong key returns null
            # for every agent, which is indistinguishable from mass agent death and has
            # already triggered one full false-alarm investigation.
            if o.get("type") == "started":
                started.append(o.get("key"))
                agent_of_key[o.get("agentId", "")] = o.get("key", "")
            elif o.get("type") == "result":
                results.add(o.get("key"))

    files = sorted(glob.glob(os.path.join(d, "agent-*.jsonl")))
    hdrs = rg(r"ULTRA-AUDIT-AGENT phase=\S+ role=\S+ model=\S+", files)
    # Tolerate both compact and spaced JSON: transcripts are compact today, but a
    # regex that assumes it silently reports "no models observed", which reads as a
    # collapsed panel rather than as a broken parser.
    models = rg(r'"model"\s*:\s*"[^"]+"', files)
    errs = rg(r"API Error", files, count=True)

    agents = []
    for f in files:
        aid = os.path.basename(f)[len("agent-"):-len(".jsonl")]
        h = HDR.search(hdrs.get(f, "") or "")
        actual = None
        mm = re.search(r'"model"\s*:\s*"([^"]+)"', models.get(f, "") or "")
        if mm:
            actual = mm.group(1)
        st = os.stat(f)
        agents.append({
            "id": aid,
            "phase": h.group(1) if h else None,
            "role": h.group(2) if h else None,
            "assigned": h.group(3) if h else None,
            "actual": actual,
            "api_errors": int(errs.get(f, 0) or 0),
            "bytes": st.st_size,
            "mtime": st.st_mtime,
            "done": agent_of_key.get(aid) in results if aid in agent_of_key else None,
        })
    return {"dir": d, "started": started, "results": results, "agents": agents}


# Which concrete model each panel slot resolves to. Checked as a prefix so a dated build
# ("claude-haiku-4-5-20251001") still matches its family.
SLOT = {"opus": "claude-opus", "fable": "claude-fable", "sonnet": "claude-sonnet",
        "haiku": "claude-haiku"}


def provenance(state: dict) -> dict:
    """Verify the mix actually happened, from what the transcripts RECORD rather than what
    the harness INTENDED. A single omitted `model` silently collapses part of the panel while
    the report still prints the cross-model guarantees."""
    rows: dict[str, dict] = {}
    mismatches, unlabelled = [], 0
    for a in state["agents"]:
        if not a["phase"]:
            unlabelled += 1
            continue
        r = rows.setdefault(a["phase"], {"agents": 0, "models": collections.Counter()})
        r["agents"] += 1
        if a["actual"]:
            r["models"][a["actual"]] += 1
        exp = SLOT.get(a["assigned"] or "")
        if exp and a["actual"] and not a["actual"].startswith(exp):
            mismatches.append({"agent": a["id"], "phase": a["phase"], "role": a["role"],
                               "assigned": a["assigned"], "actual": a["actual"]})

    ordered = sorted(rows.items(),
                     key=lambda kv: PHASE_ORDER.index(kv[0]) if kv[0] in PHASE_ORDER else 99)
    distinct = set()
    for _, r in ordered:
        distinct |= set(r["models"])
    ok = (not mismatches) and len(distinct) >= 2 and unlabelled == 0
    detail = ""
    if mismatches:
        detail = (f"{len(mismatches)} agent(s) ran on a different model than assigned — the "
                  f"cross-model guarantees do not hold.")
    elif unlabelled:
        detail = (f"{unlabelled} agent transcript(s) carried no ULTRA-AUDIT-AGENT header, so "
                  f"their model assignment cannot be verified.")
    elif len(distinct) < 2:
        detail = (f"only {len(distinct)} distinct model observed across the whole run — the "
                  f"panel collapsed to a single model.")
    else:
        detail = (f"{len(distinct)} distinct models observed across {sum(r['agents'] for _, r in ordered)} "
                  f"labelled agents; every assignment matched.")
    return {
        "ok": ok, "detail": detail,
        "distinct_models": sorted(distinct),
        "mismatches": mismatches, "unlabelled": unlabelled,
        "rows": [{"phase": p, "agents": r["agents"],
                  "models": ", ".join(f"{m} x{n}" for m, n in r["models"].most_common())}
                 for p, r in ordered],
    }


def render(state: dict, show_prov: bool) -> str:
    ag = state["agents"]
    started, results = state["started"], state["results"]
    n_start, n_uniq, n_done = len(started), len(set(started)), len(results)
    retries = n_start - n_uniq
    now = time.time()
    live = [a for a in ag if now - a["mtime"] < 120]
    errs = sum(a["api_errors"] for a in ag)

    by_phase = collections.Counter(a["phase"] or "?" for a in ag)
    done_by_phase = collections.Counter(a["phase"] or "?" for a in ag if a["done"])
    order = sorted(by_phase, key=lambda p: PHASE_ORDER.index(p) if p in PHASE_ORDER else 99)

    out = [f"{os.path.basename(state['dir'])}   "
           f"{time.strftime('%H:%M:%S')}",
           f"  agents  {n_uniq} launched · {n_done} finished · {len(live)} active in the last "
           f"2 min" + (f" · {retries} RETRIED" if retries else "")]
    if errs:
        out.append(f"  !! {errs} 'API Error' lines across transcripts — if these cluster in "
                   f"bursts, agents are dying mid-response (operations.md §1)")
    for p in order:
        bar_done = done_by_phase[p]
        total = by_phase[p]
        filled = int(12 * bar_done / total) if total else 0
        out.append(f"  {p:<12} {'#' * filled}{'.' * (12 - filled)} {bar_done}/{total}")
    if not ag:
        out.append("  (no agent transcripts yet — the harness is still starting up)")
    elif n_done == 0:
        newest = max(a["mtime"] for a in ag)
        total_kb = sum(a["bytes"] for a in ag) / 1024
        out.append(f"  LIVENESS: nothing has finished yet, which is NORMAL early on. "
                   f"{total_kb:.0f} KB of transcript, last written "
                   f"{int(now - newest)}s ago — agents are reading.")
    if show_prov:
        pv = provenance(state)
        out.append("")
        out.append(f"  MODEL PROVENANCE — {'VERIFIED' if pv['ok'] else 'PROBLEM'}: {pv['detail']}")
        for r in pv["rows"]:
            out.append(f"    {r['phase']:<12} {r['agents']:>3} agents   {r['models']}")
        for m in pv["mismatches"][:8]:
            out.append(f"    !! {m['role']} assigned {m['assigned']} but ran {m['actual']}")
    return "\n".join(out)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("dir", nargs="?")
    ap.add_argument("--watch", type=int, default=0, help="seconds between refreshes")
    ap.add_argument("--provenance", action="store_true")
    ap.add_argument("--json", default="")
    a = ap.parse_args()

    d = a.dir or newest_dir()
    if not d or not os.path.isdir(d):
        sys.exit("no workflow transcript directory found. Pass one explicitly; it lives at "
                 "<project>/<session>/subagents/workflows/wf_*/")

    while True:
        state = collect(d)
        print(render(state, a.provenance or bool(a.json)))
        if a.json:
            with open(a.json, "w") as fh:
                json.dump(provenance(state), fh, indent=1)
            print(f"\n-> {a.json}")
        if not a.watch:
            break
        time.sleep(a.watch)
        print()


if __name__ == "__main__":
    main()
