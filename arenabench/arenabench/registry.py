# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The benchmark registry: which datasets exist, and what tasks are in them.

A **dataset** is a pinned, content-addressed corpus of tasks that Harbor knows
how to fetch and verify. ArenaBench does not reimplement any of that — it
registers the dataset's canonical id (name **plus** digest, because an
unversioned name is not a freeze) and reads the task list out of whatever
Harbor has already materialised on disk.

Enumeration is deliberately offline-first, in this order:

1. an explicit export directory (``harbor download --export -o DIR``);
2. Harbor's own package cache, ``~/.cache/harbor/tasks/packages/<ns>/``;
3. nothing — the caller is told to fetch, rather than being surprised by a
   network call in the middle of rendering a task picker.

Adding a dataset is one :class:`Dataset` entry. That is the entire extension
point, and it is why the UI can say "any dataset registered with the tool"
without any of the UI knowing what a dataset is.
"""

from __future__ import annotations

import os
import random
import subprocess
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterator, Sequence

__all__ = [
    "Task",
    "Dataset",
    "Registry",
    "DEFAULT_REGISTRY",
    "harbor_cache_root",
    "sample_tasks",
]


@dataclass(frozen=True)
class Task:
    """One task in a dataset, as described by its own ``task.toml``."""

    #: Bare name, e.g. ``fix-git``. This is what the UI shows and checkboxes.
    name: str
    #: Fully qualified name Harbor expects, e.g. ``terminal-bench/fix-git``.
    qualified: str
    description: str = ""
    difficulty: str = ""
    category: str = ""
    tags: tuple[str, ...] = ()
    #: Container resources the task asks for — a task wanting more memory than
    #: the host can share must not run beside another, so the UI surfaces it.
    memory_mb: int = 0
    cpus: int = 0
    agent_timeout_sec: float = 0.0
    docker_image: str = ""

    @property
    def heavy(self) -> bool:
        """Whether this task forces concurrency down to 1.

        One definition, used by the picker's filter, the CLI listing and the
        random sampler alike — three places that must agree about which tasks
        a small machine cannot run beside another, or a seat gets excluded
        from one of them and not the others.
        """
        return self.memory_mb > 4096 or self.cpus > 1

    def to_json(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "qualified": self.qualified,
            "description": self.description,
            "difficulty": self.difficulty,
            "category": self.category,
            "tags": list(self.tags),
            "memory_mb": self.memory_mb,
            "cpus": self.cpus,
            "agent_timeout_sec": self.agent_timeout_sec,
            "docker_image": self.docker_image,
            "heavy": self.heavy,
        }


@dataclass(frozen=True)
class Dataset:
    """A registered benchmark.

    ``harbor_id`` is passed verbatim to ``harbor run --dataset``. Keep the
    ``@sha256:`` suffix: it is the difference between a benchmark and a moving
    target, and every number ArenaBench reports is only comparable to another
    number from the same digest.
    """

    key: str
    title: str
    #: The exact ``--dataset`` argument, digest included.
    harbor_id: str
    #: Package namespace inside Harbor's cache, e.g. ``terminal-bench``.
    namespace: str
    description: str = ""
    homepage: str = ""

    @property
    def name_only(self) -> str:
        """The dataset name with its digest stripped."""
        return self.harbor_id.split("@", 1)[0]

    @property
    def digest(self) -> str:
        _, _, digest = self.harbor_id.partition("@")
        return digest

    def to_json(self, task_count: int | None = None) -> dict[str, Any]:
        return {
            "key": self.key,
            "title": self.title,
            "harbor_id": self.harbor_id,
            "name": self.name_only,
            "digest": self.digest,
            "namespace": self.namespace,
            "description": self.description,
            "homepage": self.homepage,
            "task_count": task_count,
        }


def harbor_cache_root() -> Path:
    """Where Harbor unpacks task packages.

    Honours ``HARBOR_CACHE_DIR`` if set, so a CI runner or a second checkout
    can point at its own cache without ArenaBench knowing.
    """
    override = os.environ.get("HARBOR_CACHE_DIR")
    if override:
        return Path(override).expanduser() / "tasks" / "packages"
    return Path.home() / ".cache" / "harbor" / "tasks" / "packages"


def _read_task_toml(path: Path, fallback_name: str) -> Task | None:
    """Parse one ``task.toml`` into a :class:`Task`, or ``None`` if unreadable.

    Unreadable is not fatal: a half-extracted cache entry should cost that one
    task from the picker, not the whole dataset.
    """
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError, tomllib.TOMLDecodeError):
        return None
    if not isinstance(data, dict):
        return None

    task = data.get("task") if isinstance(data.get("task"), dict) else {}
    meta = data.get("metadata") if isinstance(data.get("metadata"), dict) else {}
    env = data.get("environment") if isinstance(data.get("environment"), dict) else {}
    agent = data.get("agent") if isinstance(data.get("agent"), dict) else {}

    qualified = str(task.get("name") or fallback_name)
    bare = qualified.rsplit("/", 1)[-1]
    tags = meta.get("tags") or task.get("keywords") or []
    return Task(
        name=bare,
        qualified=qualified,
        description=str(task.get("description") or "").strip(),
        difficulty=str(meta.get("difficulty") or ""),
        category=str(meta.get("category") or ""),
        tags=tuple(str(t) for t in tags if isinstance(t, str)),
        memory_mb=int(env.get("memory_mb") or 0),
        cpus=int(env.get("cpus") or 0),
        agent_timeout_sec=float(agent.get("timeout_sec") or 0.0),
        docker_image=str(env.get("docker_image") or ""),
    )


def _iter_task_tomls(root: Path) -> Iterator[tuple[Path, str]]:
    """Yield ``(task.toml, task-name)`` under a cache or export root.

    Two layouts are supported because Harbor uses both. The cache interposes a
    content digest (``<task>/<sha>/task.toml``); an export does not
    (``<task>/task.toml``). Globbing for the file itself and taking the nearest
    *named* ancestor handles both without branching on which one we were given.
    """
    if not root.is_dir():
        return
    for toml_path in sorted(root.glob("*/task.toml")):
        yield toml_path, toml_path.parent.name
    for toml_path in sorted(root.glob("*/*/task.toml")):
        # <task>/<digest>/task.toml — the task name is the grandparent.
        yield toml_path, toml_path.parent.parent.name


@dataclass
class Registry:
    """Every dataset ArenaBench can run, plus task enumeration for each."""

    datasets: dict[str, Dataset] = field(default_factory=dict)
    #: Optional explicit export directories, keyed by dataset key.
    export_dirs: dict[str, Path] = field(default_factory=dict)

    def add(self, dataset: Dataset) -> "Registry":
        self.datasets[dataset.key] = dataset
        return self

    def get(self, key: str) -> Dataset | None:
        return self.datasets.get(key)

    def task_roots(self, dataset: Dataset) -> list[Path]:
        """Candidate directories that might hold this dataset's tasks."""
        roots: list[Path] = []
        explicit = self.export_dirs.get(dataset.key)
        if explicit:
            roots.append(Path(explicit))
        env_override = os.environ.get(f"ARENABENCH_TASKS_{dataset.key.upper().replace('-', '_')}")
        if env_override:
            roots.append(Path(env_override).expanduser())
        roots.append(harbor_cache_root() / dataset.namespace)
        return roots

    def tasks(self, key: str) -> list[Task]:
        """Every task in a dataset, sorted by name. Empty if not materialised.

        Deduplicates by task name, first root wins — so an explicit export
        directory shadows the shared Harbor cache, which is what an operator
        pinning a specific export expects.
        """
        dataset = self.get(key)
        if dataset is None:
            return []
        found: dict[str, Task] = {}
        for root in self.task_roots(dataset):
            for toml_path, dir_name in _iter_task_tomls(root):
                fallback = f"{dataset.namespace}/{dir_name}"
                task = _read_task_toml(toml_path, fallback)
                if task is not None and task.name not in found:
                    found[task.name] = task
        return [found[name] for name in sorted(found)]

    def fetch(self, key: str, dest: Path | None = None, timeout: float = 900.0) -> list[Task]:
        """Materialise a dataset with ``harbor download``, then enumerate it.

        This is the only method here that touches the network, and it is never
        called implicitly — a task picker that silently downloads gigabytes is
        a surprise, not a feature.
        """
        dataset = self.get(key)
        if dataset is None:
            raise KeyError(f"unknown dataset: {key}")
        command = ["harbor", "download", dataset.harbor_id]
        if dest is not None:
            dest.mkdir(parents=True, exist_ok=True)
            command += ["--export", "-o", str(dest)]
            self.export_dirs[key] = dest
        subprocess.run(command, check=True, timeout=timeout)
        return self.tasks(key)

    def to_json(self) -> list[dict[str, Any]]:
        return [
            dataset.to_json(task_count=len(self.tasks(key)))
            for key, dataset in sorted(self.datasets.items())
        ]


def sample_tasks(
    tasks: Sequence[Task],
    count: int,
    seed: int,
    *,
    exclude_heavy: bool = False,
) -> list[Task]:
    """A reproducible random subset of ``tasks``.

    **Seeded, because "we ran ten random tasks" is not a claim anyone can
    check.** With the seed recorded the same ten come back on demand, so a
    result that turned on a lucky draw can be told apart from one that did
    not — and a rival can re-run the identical slice rather than a differently
    lucky one of their own.

    Returned in the dataset's own order rather than draw order. *Which* ten
    were chosen is the finding; the sequence they came out of the generator in
    carries no information, and a stable order lets two selections be compared
    at a glance.

    ``exclude_heavy`` narrows the pool before drawing, for a host that cannot
    give a task 8 GB and still run a second contestant beside it. It changes
    what the sample is a sample *of*, so a caller that sets it owes the reader
    that detail — this is a smaller population, not the same one.
    """
    pool = [task for task in tasks if not (exclude_heavy and task.heavy)]
    if count >= len(pool):
        return pool
    if count <= 0:
        return []
    chosen = random.Random(seed).sample(range(len(pool)), count)
    return [pool[index] for index in sorted(chosen)]


#: Terminal-Bench 2.1, pinned by the digest the Stella benchmark protocol uses.
TERMINAL_BENCH_21 = Dataset(
    key="terminal-bench-2.1",
    title="Terminal-Bench 2.1",
    harbor_id=(
        "terminal-bench/terminal-bench-2-1"
        "@sha256:7d7bdc1cbedad549fc1140404bd4dc45e5fd0ea7c4186773687d177ad3a0699a"
    ),
    namespace="terminal-bench",
    description=(
        "89 end-to-end terminal tasks with canonical Docker images and "
        "independent verifiers. The agent gets a shell and an instruction; the "
        "verifier decides whether the task was actually solved."
    ),
    homepage="https://www.tbench.ai/",
)

DEFAULT_REGISTRY = Registry().add(TERMINAL_BENCH_21)
