#!/usr/bin/env python3
"""Turn an exported Frontier-Bench dataset into a runnable schedule.

Terminal-Bench's dev-baseline runner classified tasks with one line — anything
over 4 GB or one CPU went into a serial phase — because Terminal-Bench 2.1 was
homogeneous enough for that to work. Frontier-Bench is not. Its 74 tasks declare
1 to 16 CPUs, 2 to 32 GB of memory, 4 GB to 1 TB of storage, and four of them
want a GPU. Applying the Terminal-Bench rule puts 58 of 74 in the serial phase,
which is not a schedule so much as a surrender.

Three things this file exists to prevent, each of which produces a *plausible
wrong number* rather than an error:

1. A GPU task on a GPU-less host. The container starts, the work is impossible,
   the verifier returns 0.0. That row is arithmetically identical to Stella
   genuinely failing the task. Four of them silently cost ~5.4 points of pass
   rate. They are excluded by default and named in the plan, so the denominator
   is a stated choice instead of an accident.

2. A task that cannot fit on the host at all. Same failure shape: not an error,
   a zero. Excluded, named, and counted separately from tasks that ran.

3. Oversubscription. Two 16 GB tasks scheduled together on a 16 GB Docker VM do
   not fail cleanly — they fail as an unrelated-looking scatter of OOM kills,
   build timeouts, and verifier errors spread across whichever tasks happened to
   be running. Tasks are tiered by footprint and each tier gets the concurrency
   its own headroom allows.

It also collects the base images to warm. Terminal-Bench tasks named a prebuilt
``docker_image``; every Frontier-Bench task instead ships an
``environment/Dockerfile`` (12 of them a compose file too) and builds. The thing
to pull ahead of a measured run is therefore each Dockerfile's ``FROM`` bases,
not a task image — same reasoning as the Terminal-Bench prepull, which is that
with ``--max-retries 0`` a registry hiccup becomes a permanent reward-0 row, and
image availability is a precondition of the measurement rather than a term in
it.

Usage::

    python3 plan.py <dataset-dir> -o <plan.json> [--allow-gpu] [--headroom-mb N]
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

if sys.version_info < (3, 11):  # tomllib is what makes this parse rather than guess
    sys.exit("FATAL: plan.py needs Python 3.11+ (source env.sh so the lane venv is on PATH)")

import tomllib

# Tier boundaries are memory ceilings in MB. A task lands in the first tier it
# fits. The names are load-bearing only as labels; the concurrency each tier
# gets is computed from the host, not from the name.
TIERS: tuple[tuple[str, int], ...] = (
    ("small", 2048),
    ("medium", 4096),
    ("large", 8192),
    ("xlarge", 1 << 30),
)

# Concurrency is capped regardless of how much headroom arithmetic allows.
# Docker's own build and network machinery, not task memory, is the next
# bottleneck past this point, and a stuck build is far more expensive to debug
# than a slightly longer run.
MAX_CONCURRENCY = 4

_FROM_RE = re.compile(r"^\s*FROM\s+(?:--\S+\s+)*(\S+)(?:\s+AS\s+(\S+))?", re.IGNORECASE)
_COMPOSE_IMAGE_RE = re.compile(r"^\s*image:\s*[\"']?([^\s\"']+)", re.MULTILINE)


# `.Path` and not the runtime entry itself: Go's `index` returns the zero value
# for a missing map key, and a zero-value struct is truthy in a template, so
# `{{if index .Runtimes "nvidia"}}` reports a GPU on every host alive. The
# registered binary's path is empty exactly when the runtime is absent.
_HOST_FORMAT = '{{.MemTotal}}|{{.NCPU}}|{{(index .Runtimes "nvidia").Path}}'


def _elastic_capacity() -> dict:
    """Capacity for a backend that provisions per task rather than sharing a host.

    Modal builds a sandbox per trial from that trial's own declared resources,
    so there is no fixed pool to divide and nothing to exclude — asking "does
    this task fit" is the wrong question there. Returning unbounded capacity
    makes every downstream check trivially true, which is the honest encoding:
    the constraint genuinely does not exist rather than being ignored.
    """
    return {"memory_mb": None, "cpus": None, "gpu": True, "source": "modal (per-task sandbox)"}


def _host_capacity() -> dict:
    """What the Docker daemon says it can actually give a container."""
    cap = {"memory_mb": None, "cpus": None, "gpu": False, "source": "unavailable"}
    try:
        out = subprocess.run(
            ["docker", "info", "--format", _HOST_FORMAT],
            capture_output=True, text=True, timeout=60, check=True,
        ).stdout.strip()
        mem, cpus, nvidia_path = out.split("|", 2)
        cap["memory_mb"] = int(mem) // (1024 * 1024)
        cap["cpus"] = int(cpus)
        # A GPU on the host that Docker cannot pass through is, for our
        # purposes, no GPU at all.
        cap["gpu"] = bool(nvidia_path.strip())
        cap["source"] = "docker info"
    except Exception as exc:  # noqa: BLE001 - any failure means "cannot verify"
        cap["error"] = f"{type(exc).__name__}: {exc}"
    return cap


def _read_task(task_dir: Path) -> dict:
    cfg = tomllib.loads((task_dir / "task.toml").read_text())
    env = cfg.get("environment", {}) or {}
    name = (cfg.get("task", {}) or {}).get("name") or task_dir.name
    meta = cfg.get("metadata", {}) or {}
    compose = [
        p for p in (task_dir / "environment").glob("docker-compose.y*ml")
    ] if (task_dir / "environment").is_dir() else []
    return {
        "name": name,
        "slug": task_dir.name,
        "cpus": int(env.get("cpus", 1)),
        "memory_mb": int(env.get("memory_mb", 2048)),
        "storage_mb": int(env.get("storage_mb", 0)),
        "gpus": int(env.get("gpus", 0)),
        "agent_timeout_sec": float((cfg.get("agent", {}) or {}).get("timeout_sec", 0) or 0),
        "category": meta.get("category", "?"),
        "compose": bool(compose),
        "base_images": _base_images(task_dir, compose),
    }


def _base_images(task_dir: Path, compose: list[Path]) -> list[str]:
    """Every externally-pulled image this task's build depends on.

    Multi-stage builds name intermediate stages in later ``FROM`` lines
    (``FROM builder``); those are local and must not be pulled. A stage name has
    no registry path, tag, or digest, so anything without ``/``, ``:`` or ``@``
    is dropped — along with ``scratch``, which is not an image at all.
    """
    images: set[str] = set()
    stages: set[str] = set()
    for dockerfile in sorted(task_dir.rglob("Dockerfile*")):
        text = dockerfile.read_text(errors="replace")
        for line in text.splitlines():
            m = _FROM_RE.match(line)
            if not m:
                continue
            ref, alias = m.group(1), m.group(2)
            if alias:
                stages.add(alias.lower())
            images.add(ref)
    for composefile in compose:
        images.update(_COMPOSE_IMAGE_RE.findall(composefile.read_text(errors="replace")))

    resolved = set()
    for ref in images:
        low = ref.lower()
        if low in stages or low == "scratch" or ref.startswith("$"):
            continue
        if not any(c in ref for c in ("/", ":", "@")):
            continue  # a bare word with no registry coordinates is a build stage
        resolved.add(ref)
    return sorted(resolved)


def _tier(mem_mb: int) -> str:
    for name, ceiling in TIERS:
        if mem_mb <= ceiling:
            return name
    return TIERS[-1][0]


def build_plan(
    dataset_dir: Path,
    *,
    allow_gpu: bool,
    memory_headroom_mb: int,
    backend: str = "docker",
    concurrency: int | None = None,
) -> dict:
    roots = sorted(p.parent for p in dataset_dir.glob("*/*/task.toml"))
    if not roots:
        roots = sorted(p.parent for p in dataset_dir.glob("*/task.toml"))
    if not roots:
        sys.exit(f"FATAL: no task.toml under {dataset_dir}")

    tasks = [_read_task(p) for p in roots]
    host = _elastic_capacity() if backend == "modal" else _host_capacity()
    host_mem = host.get("memory_mb")
    host_cpus = host.get("cpus")

    usable_mem = (host_mem - memory_headroom_mb) if host_mem else None
    excluded: list[dict] = []
    runnable: list[dict] = []

    for t in tasks:
        if t["gpus"] > 0 and not allow_gpu:
            excluded.append({**t, "reason": "requires a GPU; FB_ALLOW_GPU is not set"})
            continue
        if t["gpus"] > 0 and not host["gpu"]:
            excluded.append({**t, "reason": "requires a GPU; Docker exposes no nvidia runtime"})
            continue
        if usable_mem is not None and t["memory_mb"] > usable_mem:
            excluded.append({
                **t,
                "reason": f"needs {t['memory_mb']} MB; host offers {usable_mem} MB after headroom",
            })
            continue
        if host_cpus is not None and t["cpus"] > host_cpus:
            excluded.append({**t, "reason": f"needs {t['cpus']} CPUs; host has {host_cpus}"})
            continue
        runnable.append(t)

    tiers: dict[str, dict] = {}
    for t in runnable:
        tiers.setdefault(_tier(t["memory_mb"]), {"tasks": []})["tasks"].append(t["slug"])

    for name, tier in tiers.items():
        members = [t for t in runnable if t["slug"] in set(tier["tasks"])]
        peak_mem = max(t["memory_mb"] for t in members)
        peak_cpus = max(t["cpus"] for t in members)
        if concurrency is not None:
            # An explicit ceiling replaces the arithmetic entirely. On a
            # per-task backend there is no shared pool to divide, so deriving
            # concurrency from capacity would compute 1 from two unknowns and
            # serialise a run that has no reason to be serial.
            tier["concurrency"] = max(1, concurrency)
        else:
            by_mem = (usable_mem // peak_mem) if usable_mem else 1
            by_cpu = (host_cpus // peak_cpus) if host_cpus else 1
            tier["concurrency"] = max(1, min(MAX_CONCURRENCY, by_mem, by_cpu))
        tier["peak_memory_mb"] = peak_mem
        tier["peak_cpus"] = peak_cpus
        tier["tasks"].sort()

    images = sorted({img for t in runnable for img in t["base_images"]})

    # Storage is advisory: the declared figure is a ceiling the task may never
    # approach, and on macOS the Docker VM's free space is not visible from the
    # host anyway. Reporting it beats pretending to enforce it.
    free_mb = shutil.disk_usage(dataset_dir).free // (1024 * 1024)

    return {
        "dataset_dir": str(dataset_dir),
        "backend": backend,
        "host": host,
        "memory_headroom_mb": memory_headroom_mb,
        "allow_gpu": allow_gpu,
        "counts": {
            "total": len(tasks),
            "runnable": len(runnable),
            "excluded": len(excluded),
        },
        "tiers": {k: tiers[k] for k, _ in TIERS if k in tiers},
        "excluded": sorted(excluded, key=lambda t: t["slug"]),
        "base_images": images,
        "declared_storage_mb": sum(t["storage_mb"] for t in runnable),
        "host_free_mb_at_dataset": free_mb,
        "runnable_tasks": sorted(t["slug"] for t in runnable),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("dataset_dir", type=Path)
    ap.add_argument("-o", "--output", type=Path, required=True)
    # Tri-state on purpose. The default has to depend on the backend — a Modal
    # run that quietly drops the four GPU tasks is unsubmittable and looks
    # exactly like a complete one — but an operator saying "no GPUs" must still
    # win over that. `None` distinguishes "unset" from "explicitly off", which
    # `store_true` alone cannot.
    ap.add_argument("--allow-gpu", action="store_true", default=None)
    ap.add_argument("--no-allow-gpu", dest="allow_gpu", action="store_false")
    # Memory, not storage. The two were one knob in the first draft of this
    # file and a 20 GB *storage* headroom subtracted from a 10 GB Docker VM's
    # *memory* excluded all 74 tasks with a negative budget — the plan was
    # empty and said so politely.
    ap.add_argument("--memory-headroom-mb", type=int,
                    default=int(os.environ.get("FB_MEMORY_HEADROOM_MB", "2048")))
    ap.add_argument("--images-out", type=Path, default=None)
    ap.add_argument("--backend", choices=("docker", "modal"),
                    default=os.environ.get("FB_ENV", "docker"),
                    help="docker: schedule against this host. modal: per-task "
                         "sandboxes, so nothing is excluded for capacity.")
    ap.add_argument("--concurrency", type=int, default=None,
                    help="Override the derived per-tier concurrency. Required "
                         "in spirit for modal, where there is no host pool to "
                         "divide.")
    args = ap.parse_args()

    concurrency = args.concurrency
    if concurrency is None and args.backend == "modal":
        concurrency = int(os.environ.get("FB_CONCURRENCY", "16"))

    allow_gpu = args.allow_gpu
    if allow_gpu is None:
        allow_gpu = args.backend == "modal" or os.environ.get("FB_ALLOW_GPU") == "1"

    plan = build_plan(args.dataset_dir, allow_gpu=allow_gpu,
                      memory_headroom_mb=args.memory_headroom_mb,
                      backend=args.backend, concurrency=concurrency)
    args.output.write_text(json.dumps(plan, indent=1) + "\n")
    if args.images_out:
        args.images_out.write_text("\n".join(plan["base_images"]) + "\n")

    host = plan["host"]
    if plan["backend"] == "modal":
        print(f"backend: modal — {host.get('source')}; host capacity is not a constraint")
    else:
        print(f"host: {host.get('memory_mb')} MB / {host.get('cpus')} CPUs "
              f"/ gpu={host.get('gpu')} ({host.get('source')})")
    for name, tier in plan["tiers"].items():
        print(f"  tier {name:<7} tasks={len(tier['tasks']):<3} "
              f"concurrency={tier['concurrency']} peak={tier['peak_memory_mb']}MB/"
              f"{tier['peak_cpus']}cpu")
    c = plan["counts"]
    print(f"runnable={c['runnable']}/{c['total']} excluded={c['excluded']} "
          f"images={len(plan['base_images'])}")
    for t in plan["excluded"]:
        print(f"  EXCLUDED {t['slug']}: {t['reason']}")
    if plan["excluded"]:
        print("NOTE: excluded tasks are absent from the run, not scored as failures.")
        print("      Any published number must state the denominator it used.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
