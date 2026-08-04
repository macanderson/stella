# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""``arenabench`` command line.

Four verbs, because the web UI is the product and the CLI exists to get you to
it, to prove the pieces work before you spend money, and to fetch what a match
will need.
"""

from __future__ import annotations

import argparse
import logging
import random
import subprocess
import sys
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
