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
import importlib.util
import json
import os
import re
import sqlite3
import sys
from datetime import datetime, timezone

# Measured, not guessed: the list began as the four types that were 87% of
# all events over the runs to date (block_registered 134,776 + text_delta
# 125,331 against ~30k of everything else); the entries added since are the
# same shape. None of them answers a question anyone asks twice.
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


# `normalized_cost` is the one price table both arms are costed against. It is
# loaded BY PATH rather than imported as a package: the analysis package next
# door declares a `harbor` dependency this ingester does not need and CI does
# not install, while the module itself is stdlib-only. Loading the single file
# gets the table without dragging in a benchmark harness.
_NORMALIZED_COST_PATH = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "terminal_bench_analysis",
    "normalized_cost.py",
)


def _load_normalized_cost():
    spec = importlib.util.spec_from_file_location(
        "stella_bench_normalized_cost", _NORMALIZED_COST_PATH
    )
    if spec is None or spec.loader is None:  # pragma: no cover - packaging error
        raise ImportError(f"cannot load the price table from {_NORMALIZED_COST_PATH}")
    module = importlib.util.module_from_spec(spec)
    # Registered before execution: `normalized_cost` uses `from __future__
    # import annotations`, and `@dataclass` resolves those string annotations
    # by looking its own module up in `sys.modules`. Executing first would
    # leave that lookup returning None.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


normalized_cost = _load_normalized_cost()


# Why `trials.cost_usd_norm` is, or is not, populated.
#
# The column used to be inserted as a bare NULL "for the analysis step to fill
# in", and nothing ever filled it — so anyone reading `trials.cost_usd_norm`
# directly summed a column of NULLs, got 0.00, and published it as the
# normalized cost. It is computed HERE now, because ingest already holds every
# input the computation needs: the token counts, the model the trial ran, and
# the day it ran on. When it genuinely cannot be computed, the reason is
# recorded beside it rather than left to inference.
#
# The invariant that follows: `cost_norm_status = 'priced'` is the ONLY state
# in which `cost_usd_norm` means anything, and in that state a genuine $0.00 is
# distinguishable from an absent figure. Any query that aggregates the column
# must filter on it.
COST_NORM_PRICED = "priced"
COST_NORM_NO_MODEL = "no_model"
COST_NORM_UNPRICED_MODEL = "unpriced_model"
COST_NORM_NO_TOKENS = "no_tokens"
COST_NORM_NO_RUN_DATE = "no_run_date"
# Rows written by an ingester that predates this column. Their NULL cost has no
# recorded reason, which is exactly the ambiguity this vocabulary exists to end
# — so they say so rather than borrowing one of the states above.
COST_NORM_UNMIGRATED = "unmigrated"


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


def parse_timestamp(value):
    """Harbor writes ISO-8601 with a `Z`, which `fromisoformat` rejects."""
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def elapsed_seconds(start, finish):
    """Wall time between two Harbor stamps, or None if either is unreadable.

    A negative span means a clock moved under the run; that is a broken record
    and is reported as unknown rather than as a duration.
    """
    started = parse_timestamp(start)
    finished = parse_timestamp(finish)
    if started is None or finished is None:
        return None
    seconds = (finished - started).total_seconds()
    return seconds if seconds >= 0 else None


def trial_model(result, fallback_model):
    """The model this trial actually ran.

    Read from the trial's own config first and only then from the `--model`
    flag: the flag labels the run, while `config.agent.model_name` is what the
    arm was launched with. When they disagree the trial's own record is the one
    that priced the tokens.
    """
    config = result.get("config") or {}
    agent = config.get("agent") or {}
    model = agent.get("model_name") or fallback_model
    return model if isinstance(model, str) and model.strip() else None


def price_trial(result, fallback_model):
    """Return `(cost_usd_norm, cost_norm_status)` for one trial.

    Never returns a number without `COST_NORM_PRICED`, and never returns
    `COST_NORM_PRICED` without a number.
    """
    model = trial_model(result, fallback_model)
    if model is None:
        return None, COST_NORM_NO_MODEL

    agent = result.get("agent_result") or {}
    counts = {
        "n_input_tokens": agent.get("n_input_tokens"),
        "n_cache_tokens": agent.get("n_cache_tokens"),
        "n_output_tokens": agent.get("n_output_tokens"),
    }
    if any(value is None for value in counts.values()):
        # A trial that crashed before reporting usage has no tokens to price.
        # Charging it zero would make a crash look free.
        return None, COST_NORM_NO_TOKENS

    started = parse_timestamp(result.get("started_at"))
    on = started.date() if started is not None else None
    try:
        cost = normalized_cost.normalized_cost_usd(model, on=on, **counts)
    except normalized_cost.AmbiguousRunDate:
        # The model's rate changed and this trial carries no readable start
        # time, so no entry can be chosen. Guessing one is the exact error the
        # price table exists to prevent.
        return None, COST_NORM_NO_RUN_DATE
    if cost is None:
        # No rate we hold applies to this model on this day.
        return None, COST_NORM_UNPRICED_MODEL
    return cost, COST_NORM_PRICED


def migrate(conn) -> None:
    """Add columns `CREATE TABLE IF NOT EXISTS` cannot add to a store that
    already exists.

    Additive only and idempotent: a database written by an older ingester keeps
    every row it had, and its pre-existing rows are marked as predating the
    column rather than being given a reason they never had.
    """
    columns = {row[1] for row in conn.execute("PRAGMA table_info(trials)")}
    if "cost_norm_status" not in columns:
        conn.execute(
            "ALTER TABLE trials ADD COLUMN cost_norm_status TEXT NOT NULL "
            f"DEFAULT '{COST_NORM_UNMIGRATED}'"
        )


def connect(db: str) -> sqlite3.Connection:
    conn = sqlite3.connect(db)
    conn.execute("PRAGMA foreign_keys = ON")
    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "schema.sql")) as fh:
        conn.executescript(fh.read())
    migrate(conn)
    return conn


def ingest_trial(conn, run_id, arm, agent, tdir, model=None):
    base = os.path.basename(tdir.rstrip("/"))
    task = TRIAL_SUFFIX.sub("", base)
    trial_id = f"{run_id}:{arm}:{base}"

    r = jload(os.path.join(tdir, "result.json")) or {}
    ar = r.get("agent_result") or {}
    ei = r.get("exception_info") or {}
    reward = ((r.get("verifier_result") or {}).get("rewards") or {}).get("reward")

    # Computed here, not deferred. See the COST_NORM_* vocabulary above.
    cost_norm, cost_norm_status = price_trial(r, model)
    # The trial-level window, matching the bare `started_at`/`finished_at`
    # columns beside it, so the three stay internally consistent. The agent's
    # own narrower window is a different measurement and is not conflated with
    # this one.
    started_at = r.get("started_at")
    finished_at = r.get("finished_at")
    wall_seconds = elapsed_seconds(started_at, finished_at)

    conn.execute(
        "INSERT OR REPLACE INTO trials (trial_id,run_id,arm,agent,task_name,trial_dir,"
        "reward,passed,exception_type,exception_message,n_input_tokens,n_cache_tokens,"
        "n_output_tokens,cost_usd_self,cost_usd_norm,cost_norm_status,wall_seconds,"
        "started_at,finished_at)"
        " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            trial_id, run_id, arm, agent, task, tdir,
            reward, 1 if (reward or 0) >= 1 else 0,
            ei.get("exception_type"), (ei.get("exception_message") or "")[:2000],
            ar.get("n_input_tokens"), ar.get("n_cache_tokens"), ar.get("n_output_tokens"),
            ar.get("cost_usd"), cost_norm, cost_norm_status, wall_seconds,
            started_at, finished_at,
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
    # Required because `runs.model` is NOT NULL — with a None default the
    # INSERT raised IntegrityError before a single trial was ingested.
    ap.add_argument("--model", required=True, help="model id recorded on the run row")
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
    unpriced = {}
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
            e, t, ar = ingest_trial(conn, a.run, arm, arm, tdir, model=a.model)
            totals["trials"] += 1
            totals["events"] += e
            totals["tools"] += t
            totals["artifacts"] += ar
    conn.commit()

    # Report the unpriced trials at ingest rather than leaving them to be
    # discovered as a hole in a published table. A comparable-cost column that
    # is empty for one whole arm is the failure this store exists to prevent,
    # and it is cheap to say so here.
    for status, count in conn.execute(
        "SELECT cost_norm_status, COUNT(*) FROM trials WHERE run_id = ?"
        " GROUP BY cost_norm_status",
        (a.run,),
    ):
        if status != COST_NORM_PRICED:
            unpriced[status] = count
    print(f"{a.run}: {totals}")
    if unpriced:
        print(f"{a.run}: NOT priced (cost_usd_norm is NULL, not zero): {unpriced}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
