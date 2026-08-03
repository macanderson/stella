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
import sys
from pathlib import Path

from .agents import AGENTS
from .recorder import IMAGE_TAG, build_image, docker_available, image_present
from .registry import DEFAULT_REGISTRY
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
    for task in tasks:
        heavy = " [heavy]" if task.memory_mb > 4096 or task.cpus > 1 else ""
        print(f"{task.name:38} {task.difficulty:8} {task.category}{heavy}")
    print(f"\n{len(tasks)} tasks", file=sys.stderr)
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
    tasks_parser.set_defaults(func=_cmd_tasks)

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
