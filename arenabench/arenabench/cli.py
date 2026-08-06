# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""``arenabench`` command line.

The web UI is the product for exploring; the CLI is how a match becomes
repeatable. ``arenabench run match.toml`` executes a committed template with no
browser involved, which is what CI needs — the same match, the same way, every
time, with credentials coming from the environment rather than the file.
"""

from __future__ import annotations

import argparse
import json
import logging
import random
import subprocess
import sys
from dataclasses import replace
from pathlib import Path

from . import provenance
from .agents import AGENTS
from .recorder import IMAGE_TAG, build_image, docker_available, image_present
from .registry import DEFAULT_REGISTRY, export_root, sample_tasks
from .replay import ReplayError, replay_flip
from .server import default_workspace, serve

__all__ = ["main"]


def _configure_logging(verbose: bool) -> None:
    logging.basicConfig(
        level=logging.DEBUG if verbose else logging.INFO,
        format="%(asctime)s %(levelname)-7s %(name)s  %(message)s",
        datefmt="%H:%M:%S",
    )


def _cmd_run(args: argparse.Namespace) -> int:
    """Run a committed ``arenabench.toml`` headlessly — the CI entry point.

    Credentials are read from the process environment against each seat's
    declared ``required`` list, never from the file. A missing one aborts
    before any container starts, because an unauthenticated arm scores zero
    and a zero is indistinguishable from a real result on a scoreboard.
    """
    import os
    import time

    from .config import MatchTemplateError, load_match, required_env
    from .monitor import MatchWatcher
    from .runner import MatchRunner
    from .server import ArenaServer

    try:
        spec = load_match(Path(args.template).expanduser())
    except MatchTemplateError as exc:
        for problem in exc.problems:
            print(f"error: {problem}", file=sys.stderr)
        return 2

    needed = required_env(spec)
    missing: list[str] = []
    seated: list = []
    for contestant in spec.contestants:
        candidates = needed.get(contestant.id, [])
        env = {
            name: os.environ[name] for name in candidates if os.environ.get(name)
        }
        # The candidate names are alternatives, not a conjunction: a seat
        # carrying any one of them is credentialled (a Claude subscription
        # token authenticates a seat that has no ANTHROPIC_API_KEY at all).
        if candidates and not env and not args.allow_missing_env:
            missing.append(f"{contestant.name}: any of {', '.join(candidates)}")
        seated.append(replace(contestant, env=env))
    if missing:
        print("error: required environment variables are not set:", file=sys.stderr)
        for line in missing:
            print(f"  {line}", file=sys.stderr)
        print("  (use --allow-missing-env to run anyway)", file=sys.stderr)
        return 2

    spec = replace(spec, contestants=tuple(seated))
    workspace = Path(args.workspace).expanduser()
    arena = ArenaServer(workspace)
    runner: MatchRunner = arena.runner

    print(f"match     : {spec.name}")
    print(f"dataset   : {spec.dataset}")
    print(f"tasks     : {len(spec.tasks) or 'all'}")
    for contestant in spec.contestants:
        print(f"  {contestant.name:24} {contestant.agent:14} {contestant.engine.label}")

    match = runner.create(spec)
    runner.start(match)
    # The built-in supervisor: the same rules `arenabench watch` runs from
    # another process, reported inline so a CI log shows a dead arm at the
    # minute it died rather than in the postmortem (#1480).
    watcher = MatchWatcher(match.workspace)
    while match.status == "running":
        time.sleep(args.poll)
        for detection in watcher.scan():
            print(f"  !! {detection.to_line()}")
        if args.progress:
            snapshot = match.snapshot()
            parts = [
                f"{c['name']}={c['totals']['passed']}/{c['totals']['judged']}"
                for c in snapshot["contestants"]
            ]
            print(f"  [{snapshot['elapsed'] / 60:5.1f}m] " + "  ".join(parts))
    for detection in watcher.scan():
        print(f"  !! {detection.to_line()}")

    snapshot = match.snapshot()
    print(f"\nfinished in {snapshot['elapsed'] / 60:.1f}m")
    worst = 0
    for entry in snapshot["contestants"]:
        totals = entry["totals"]
        # The comparable figure first, and the agent's own number labelled as
        # its own: the two are computed from different tables, so printing them
        # in one unlabelled column would be subtracting one from the other.
        priced = totals["priced_cost"]
        cost = f"${priced:.2f}" if priced is not None else "unpriced"
        print(
            f"  {entry['name']:24} {totals['solve_rate']:5.1f}%  "
            f"({totals['passed']}/{totals['judged']})  {cost:>9}  "
            f"(self-reported ${totals['total_cost']:.2f})"
        )
        if totals["judged"] == 0:
            worst = 1
    if args.results:
        Path(args.results).expanduser().write_text(
            json.dumps(snapshot, indent=2), encoding="utf-8"
        )
        print(f"  results -> {args.results}")
    if watcher.invalidating:
        # Same contract as `arenabench watch`: exit 3 means these numbers
        # must not be published, which outranks "an arm judged nothing".
        print("match invalidated — see the !! lines above", file=sys.stderr)
        return 3
    return worst


def _cmd_template(args: argparse.Namespace) -> int:
    """Print a starter ``arenabench.toml`` to stdout or a file."""
    from .config import dump_match
    from .model import MatchSpec

    spec = MatchSpec.from_json(
        {
            "name": args.name,
            "dataset": args.dataset,
            "tasks": [],
            "attempts": 1,
            "concurrency": 1,
            "record_video": False,
            "contestants": [
                {
                    "id": "stella",
                    "name": "Stella",
                    "agent": "stella",
                    "engine": {
                        "api": "openrouter",
                        "model": "z-ai/glm-5.2",
                        "effort": "medium",
                        "max_tokens": 128000,
                        "roles": {
                            "judge": {"model": "openai/gpt-5.5", "effort": "xhigh"},
                            "triage": {"model": "z-ai/glm-4.7-flash", "effort": "low"},
                        },
                    },
                },
                {
                    "id": "claude-code",
                    "name": "Claude Code",
                    "agent": "claude-code",
                    "engine": {
                        "api": "openrouter",
                        "model": "z-ai/glm-5.2",
                        "effort": "medium",
                        "base_url": "https://openrouter.ai/api/v1",
                    },
                },
            ],
        }
    )
    text = dump_match(spec)
    if args.output:
        Path(args.output).expanduser().write_text(text, encoding="utf-8")
        print(f"wrote {args.output}")
    else:
        print(text, end="")
    return 0


def _cmd_serve(args: argparse.Namespace) -> int:
    try:
        serve(
            workspace=Path(args.workspace).expanduser(),
            host=args.host,
            port=args.port,
            open_browser=not args.no_browser,
            allow_remote=args.allow_remote,
        )
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    return 0


def _cmd_datasets(args: argparse.Namespace) -> int:
    for dataset in DEFAULT_REGISTRY.datasets.values():
        tasks = DEFAULT_REGISTRY.tasks(dataset.key)
        state = f"{len(tasks)} tasks" if tasks else "not fetched"
        print(f"{dataset.key:22} {state:>14}   {dataset.title}")
        print(f"{'':22} {'':>14}   {dataset.harbor_id}")
        if not tasks:
            print(f"{'':22} {'':>14}   fetch: harbor download {dataset.harbor_id}")
    return 0


def _cmd_provenance(args: argparse.Namespace) -> int:
    """Show — and with ``--migrate``, backfill — what each match was measured with.

    The listing groups by comparability key rather than by date, because the
    question this answers is never "what ran when": it is "which of these
    numbers may I put in the same table". Matches sharing a key may be
    compared; matches with different keys are different experiments that
    happen to live in the same directory.
    """
    root = Path(args.workspace).expanduser() / "matches"
    if not root.is_dir():
        print(f"no matches under {root}")
        return 0

    rows: list[tuple[str, str, str]] = []
    backfilled = 0
    for match_dir in sorted(p for p in root.iterdir() if p.is_dir()):
        record = provenance.read_match(match_dir)
        state = "recorded"
        if record is None:
            rebuilt = provenance.backfill_match(match_dir)
            if rebuilt is None:
                rows.append((match_dir.name, "—", "no jobs on disk"))
                continue
            if args.migrate:
                provenance.write_match(match_dir, rebuilt)
                backfilled += 1
                state = "backfilled"
            else:
                state = "would backfill"
            record = rebuilt
        elif record.source == provenance.SOURCE_BACKFILLED:
            state = "backfilled"
        rows.append((match_dir.name, record.comparability_key, state))

    width = max((len(key) for _, key, _ in rows), default=20)
    for name, key, state in rows:
        print(f"{name:14} {key:<{width}}  {state}")

    # The headline: how many distinct experiments are sitting in here. One key
    # means everything is comparable; more than one means a table built from
    # all of it would be mixing apparatus, which is the thing worth saying out
    # loud rather than leaving to be noticed.
    keys = {key for _, key, _ in rows if key != "—"}
    print()
    if len(keys) <= 1:
        print(f"{len(rows)} matches, one comparability key — safe to compare.")
    else:
        print(f"{len(rows)} matches across {len(keys)} comparability keys:")
        for key in sorted(keys):
            count = sum(1 for _, k, _ in rows if k == key)
            print(f"  {count:>3}  {key}")
        print("Numbers from different keys are different experiments. Label, never sum.")
    if args.migrate and backfilled:
        print(f"\nbackfilled {backfilled} match(es).")
    elif not args.migrate and any(state == "would backfill" for _, _, state in rows):
        print("\nre-run with --migrate to write the reconstructed records.")
    return 0


def _cmd_tasks(args: argparse.Namespace) -> int:
    tasks = DEFAULT_REGISTRY.tasks(args.dataset)
    if not tasks:
        dataset = DEFAULT_REGISTRY.get(args.dataset)
        target = dataset.harbor_id if dataset else args.dataset
        print(f"no tasks on disk; fetch with: harbor download {target}", file=sys.stderr)
        return 1
    total = len(tasks)
    seed = args.seed
    if args.random:
        # An unseeded draw still gets a seed, and prints it. Otherwise the one
        # thing you need to run this slice again is the one thing you no
        # longer have.
        if seed is None:
            seed = random.randrange(1, 2**31)
        tasks = sample_tasks(
            tasks,
            args.random,
            seed,
            exclude_heavy=args.exclude_heavy,
            max_memory_mb=args.max_memory_mb,
        )

    for task in tasks:
        heavy = " [heavy]" if task.heavy else ""
        print(f"{task.name:38} {task.difficulty:8} {task.category}{heavy}")

    if args.random:
        # Report the size of the pool actually drawn from, not the dataset. A
        # filtered draw is a sample of a smaller population, and "10 of 89"
        # would quietly claim otherwise.
        drawn_from = len(
            sample_tasks(
                DEFAULT_REGISTRY.tasks(args.dataset),
                total,
                seed,
                exclude_heavy=args.exclude_heavy,
                max_memory_mb=args.max_memory_mb,
            )
        )
        narrowed = "" if drawn_from == total else f" (of {total} in the dataset)"
        print(
            f"\n{len(tasks)} drawn from a pool of {drawn_from}{narrowed} — "
            f"reproduce with --random {args.random} --seed {seed}"
            + (" --exclude-heavy" if args.exclude_heavy else "")
            + (f" --max-memory-mb {args.max_memory_mb}" if args.max_memory_mb else ""),
            file=sys.stderr,
        )
    else:
        print(f"\n{total} tasks", file=sys.stderr)
    return 0


def _cmd_export(args: argparse.Namespace) -> int:
    """Materialise a dataset for offline running.

    Worth its own verb because it is the difference between a match that
    finishes and one that dies at task three. Harbor resolves a registry ref
    per task, at run time; an export resolves the digest once, here.
    """
    dataset = DEFAULT_REGISTRY.get(args.dataset)
    if dataset is None:
        print(f"unknown dataset: {args.dataset}", file=sys.stderr)
        return 1
    dest = Path(args.output).expanduser() if args.output else export_root() / dataset.key
    print(f"exporting {dataset.harbor_id}\n       to {dest}")
    try:
        DEFAULT_REGISTRY.fetch(dataset.key, dest=dest)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        print(f"export failed: {exc}", file=sys.stderr)
        return 1
    run_path = DEFAULT_REGISTRY.local_run_path(dataset.key)
    if run_path is None:
        print("export finished but no runnable task tree appeared", file=sys.stderr)
        return 1
    print(f"ready: {run_path}")
    return 0


def _cmd_agents(args: argparse.Namespace) -> int:
    for spec in AGENTS.values():
        kind = "built-in" if spec.harbor_agent and not spec.import_path else "arenabench"
        knobs = ",".join(sorted(spec.honours))
        print(f"{spec.slug:18} {kind:11} honours: {knobs}")
    return 0


def _task_dir_for(trial_dir: Path) -> Path | None:
    """Where the task this trial ran lives, read back from its own result.

    Harbor records the absolute path it resolved the task from, so a trial
    carries the location of its own verifier and the operator does not have to
    reconstruct which export a months-old run used.
    """
    try:
        raw = json.loads((trial_dir / "result.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    path = ((raw.get("task_id") or {}) if isinstance(raw, dict) else {}).get("path")
    return Path(str(path)) if path else None


def _cmd_flip(args: argparse.Namespace) -> int:
    trial_dir = Path(args.trial_dir).expanduser().resolve()
    if not trial_dir.is_dir():
        print(f"no such trial directory: {trial_dir}", file=sys.stderr)
        return 1
    task_dir = Path(args.task_dir).expanduser() if args.task_dir else _task_dir_for(trial_dir)
    if task_dir is None or not task_dir.is_dir():
        print(
            "could not resolve the task directory; pass --task-dir",
            file=sys.stderr,
        )
        return 1

    try:
        result = replay_flip(
            trial_dir,
            task_dir,
            verifier_timeout=args.verifier_timeout,
            confirm_window=args.confirm_window,
        )
    except ReplayError as exc:
        print(str(exc), file=sys.stderr)
        return 1

    if result.flip_index is None:
        print(
            f"no snapshot passed ({result.snapshots} captured, "
            f"{result.probes} probed, {result.unknown} unknown)"
        )
        return 0
    print(
        f"flip at snapshot {result.flip_index}/{result.snapshots - 1} "
        f"(t+{result.flip_elapsed:.0f}s)"
    )
    print(
        f"kept running {result.wasted_elapsed:.0f}s after it was already passing "
        f"— {result.probes} verifier runs, {result.unknown} unknown"
    )
    return 0


def _cmd_watch(args: argparse.Namespace) -> int:
    """Tail a match's artifacts and emit one line per detection.

    The supervising process is the customer: CI greps text lines and acts on
    the exit code; a subscriber (the fullauto loop, a ``Monitor``) reads
    ``--format jsonl`` — agent monitor protocol events, one per line — and
    reacts while the match is still running. Read-only with respect to the
    run, like everything else in the arena: a watched contestant's number
    cannot change because someone was watching.

    Exit codes: 0 = nothing invalidating (warnings and notices may still have
    printed), 2 = usage error, 3 = at least one critical detection — the
    match's numbers must not be published.
    """
    import time

    from .monitor import DEFAULT_RULES, MatchWatcher, Thresholds, stream_event

    # A match id resolves inside the workspace; a path is taken as-is, which
    # is what lets the same command run against an archive or a fixture tree.
    target = Path(args.match).expanduser()
    match_dir = (
        target
        if (target / "jobs").is_dir()
        else Path(args.workspace).expanduser() / "matches" / args.match
    )
    if not (match_dir / "jobs").is_dir():
        print(f"no such match: {args.match} (looked at {match_dir})", file=sys.stderr)
        return 2

    rules = (
        tuple(part.strip() for part in args.rules.split(",") if part.strip())
        if args.rules
        else DEFAULT_RULES
    )
    thresholds = Thresholds(
        min_steps=args.min_steps,
        min_output_tokens=args.min_output_tokens,
        stall_minutes=args.stall_minutes,
    )
    try:
        watcher = MatchWatcher(match_dir, rules=rules, thresholds=thresholds)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    jsonl = args.format == "jsonl"
    if jsonl:
        print(
            json.dumps(
                stream_event(
                    "watching",
                    source="arenabench",
                    run=watcher.run,
                    rules=list(rules),
                )
            ),
            flush=True,
        )

    try:
        while True:
            for detection in watcher.scan():
                line = (
                    json.dumps(
                        detection.to_event(source="arenabench", run=watcher.run)
                    )
                    if jsonl
                    else detection.to_line()
                )
                print(line, flush=True)
            if not args.follow:
                break
            time.sleep(args.poll)
    except KeyboardInterrupt:
        # A follow subscription ends when its supervisor says so; ^C is the
        # normal way to say it, not a crash — so the summary and the exit
        # code still happen.
        pass

    if jsonl:
        print(watcher.summary_json(), flush=True)
    return 3 if watcher.invalidating else 0


def _cmd_build_recorder(args: argparse.Namespace) -> int:
    if not docker_available():
        print("docker is not available", file=sys.stderr)
        return 1
    if image_present() and not args.force:
        print(f"{IMAGE_TAG} already built (use --force to rebuild)")
        return 0
    ok = build_image()
    print(f"{'built' if ok else 'FAILED to build'} {IMAGE_TAG}")
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="arenabench",
        description="Run coding-agent benchmarks as a live, side-by-side contest.",
    )
    parser.add_argument("-v", "--verbose", action="store_true")
    subparsers = parser.add_subparsers(dest="command", required=True)

    run_parser = subparsers.add_parser(
        "run", help="run a committed arenabench.toml (no browser; for CI)"
    )
    run_parser.add_argument("template", help="path to an arenabench.toml")
    run_parser.add_argument("--workspace", default=str(default_workspace()))
    run_parser.add_argument("--results", help="write the final snapshot JSON here")
    run_parser.add_argument("--poll", type=float, default=15.0)
    run_parser.add_argument(
        "--progress", action="store_true", help="print a line per poll"
    )
    run_parser.add_argument(
        "--allow-missing-env",
        action="store_true",
        help="launch even when a seat's declared credentials are absent",
    )
    run_parser.set_defaults(func=_cmd_run)

    template_parser = subparsers.add_parser(
        "template", help="print a starter arenabench.toml"
    )
    template_parser.add_argument("-o", "--output", help="write here instead of stdout")
    template_parser.add_argument("--name", default="my match")
    template_parser.add_argument("--dataset", default="terminal-bench-2.1")
    template_parser.set_defaults(func=_cmd_template)

    serve_parser = subparsers.add_parser("serve", help="run the arena web UI")
    serve_parser.add_argument("--host", default="127.0.0.1")
    serve_parser.add_argument("--port", type=int, default=8900)
    serve_parser.add_argument("--workspace", default=str(default_workspace()))
    serve_parser.add_argument(
        "--no-browser", action="store_true", help="do not open a browser"
    )
    serve_parser.add_argument(
        "--allow-remote",
        action="store_true",
        help="permit a non-loopback --host; anyone who can reach it can spend "
             "your provider credits",
    )
    serve_parser.set_defaults(func=_cmd_serve)

    datasets_parser = subparsers.add_parser("datasets", help="list registered datasets")
    datasets_parser.set_defaults(func=_cmd_datasets)

    provenance_parser = subparsers.add_parser(
        "provenance",
        help="what each match was measured with, grouped by comparability key",
    )
    provenance_parser.add_argument("--workspace", default=str(default_workspace()))
    provenance_parser.add_argument(
        "--migrate",
        action="store_true",
        help="write reconstructed records for matches that recorded none. The "
             "Harbor version is not recoverable and is never invented — a "
             "backfilled record says 'unknown, older than X' and is marked as "
             "reconstructed rather than measured.",
    )
    provenance_parser.set_defaults(func=_cmd_provenance)

    tasks_parser = subparsers.add_parser("tasks", help="list a dataset's tasks")
    tasks_parser.add_argument("dataset", default="terminal-bench-2.1", nargs="?")
    tasks_parser.add_argument(
        "--random",
        type=int,
        metavar="N",
        help="draw N tasks at random instead of listing all of them",
    )
    tasks_parser.add_argument(
        "--seed",
        type=int,
        help="seed for --random; one is chosen and printed if you omit it",
    )
    tasks_parser.add_argument(
        "--exclude-heavy",
        action="store_true",
        help="draw only from tasks that do not force concurrency to 1",
    )
    tasks_parser.add_argument(
        "--max-memory-mb",
        type=int,
        default=0,
        metavar="MB",
        help="draw only from tasks asking for at most MB of memory; with N "
             "contestants racing, the ceiling that matters is your Docker "
             "allocation divided by N",
    )
    tasks_parser.set_defaults(func=_cmd_tasks)

    export_parser = subparsers.add_parser(
        "export", help="materialise a dataset for offline running"
    )
    export_parser.add_argument("dataset", default="terminal-bench-2.1", nargs="?")
    export_parser.add_argument(
        "-o", "--output", help="where to write it (default: ~/.arenabench/datasets)"
    )
    export_parser.set_defaults(func=_cmd_export)

    agents_parser = subparsers.add_parser("agents", help="list available agents")
    agents_parser.set_defaults(func=_cmd_agents)

    recorder_parser = subparsers.add_parser(
        "build-recorder", help="build the MP4 screen-recorder image"
    )
    recorder_parser.add_argument("--force", action="store_true")
    recorder_parser.set_defaults(func=_cmd_build_recorder)

    flip_parser = subparsers.add_parser(
        "flip",
        help="find when a trial started passing, and what it did afterwards",
    )
    flip_parser.add_argument("trial_dir", help="a trial directory containing arena/snapshots")
    flip_parser.add_argument(
        "--task-dir",
        help="the task's dataset directory (default: resolved from result.json)",
    )
    flip_parser.add_argument(
        "--verifier-timeout",
        type=float,
        default=1800.0,
        help="seconds allowed for one verifier run (default: 1800)",
    )
    flip_parser.add_argument(
        "--confirm-window",
        type=int,
        default=3,
        help="snapshots to re-probe before the bisected answer (default: 3)",
    )
    flip_parser.set_defaults(func=_cmd_flip)

    watch_parser = subparsers.add_parser(
        "watch",
        help="tail a match's artifacts and report failure signatures live",
    )
    watch_parser.add_argument(
        "match", help="a match id in the workspace, or a path to a match directory"
    )
    watch_parser.add_argument("--workspace", default=str(default_workspace()))
    watch_parser.add_argument(
        "--rules",
        help="comma-separated rule list (default: zero-token,premature-complete,"
             "late-verdict,stall; also available: usage-incomplete)",
    )
    watch_parser.add_argument(
        "--format",
        choices=("text", "jsonl"),
        default="text",
        help="text = one human-readable line per detection; jsonl = agent "
             "monitor protocol events for a subscribing process",
    )
    watch_parser.add_argument(
        "--follow",
        action="store_true",
        help="keep watching and emit new detections as they appear (^C to stop)",
    )
    watch_parser.add_argument(
        "--poll", type=float, default=15.0, help="seconds between scans with --follow"
    )
    watch_parser.add_argument(
        "--min-steps",
        type=int,
        default=5,
        help="premature-complete floor: steps a completed-and-failed trial "
             "should have taken (default: 5)",
    )
    watch_parser.add_argument(
        "--min-output-tokens",
        type=int,
        default=300,
        help="premature-complete floor: output tokens likewise (default: 300)",
    )
    watch_parser.add_argument(
        "--stall-minutes",
        type=float,
        default=10.0,
        help="minutes of silence before a running trial counts as stalled "
             "(default: 10)",
    )
    watch_parser.set_defaults(func=_cmd_watch)

    args = parser.parse_args(argv)
    _configure_logging(args.verbose)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
