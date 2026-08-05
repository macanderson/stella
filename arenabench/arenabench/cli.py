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

from .agents import AGENTS
from .recorder import IMAGE_TAG, build_image, docker_available, image_present
from .registry import DEFAULT_REGISTRY, export_root, sample_tasks
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
        env = {
            name: os.environ[name]
            for name in needed.get(contestant.id, [])
            if os.environ.get(name)
        }
        absent = [n for n in needed.get(contestant.id, []) if n not in env]
        if absent and not args.allow_missing_env:
            missing.append(f"{contestant.name}: {', '.join(absent)}")
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
    while match.status == "running":
        time.sleep(args.poll)
        if args.progress:
            snapshot = match.snapshot()
            parts = [
                f"{c['name']}={c['totals']['passed']}/{c['totals']['judged']}"
                for c in snapshot["contestants"]
            ]
            print(f"  [{snapshot['elapsed'] / 60:5.1f}m] " + "  ".join(parts))

    snapshot = match.snapshot()
    print(f"\nfinished in {snapshot['elapsed'] / 60:.1f}m")
    worst = 0
    for entry in snapshot["contestants"]:
        totals = entry["totals"]
        print(
            f"  {entry['name']:24} {totals['solve_rate']:5.1f}%  "
            f"({totals['passed']}/{totals['judged']})  ${totals['total_cost']:.2f}"
        )
        if totals["judged"] == 0:
            worst = 1
    if args.results:
        Path(args.results).expanduser().write_text(
            json.dumps(snapshot, indent=2), encoding="utf-8"
        )
        print(f"  results -> {args.results}")
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
    serve(
        workspace=Path(args.workspace).expanduser(),
        host=args.host,
        port=args.port,
        open_browser=not args.no_browser,
    )
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
    serve_parser.set_defaults(func=_cmd_serve)

    datasets_parser = subparsers.add_parser("datasets", help="list registered datasets")
    datasets_parser.set_defaults(func=_cmd_datasets)

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

    args = parser.parse_args(argv)
    _configure_logging(args.verbose)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
