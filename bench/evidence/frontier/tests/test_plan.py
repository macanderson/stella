"""Tests for the Frontier-Bench run planner.

Everything here is about a *silent wrong number* rather than a crash. The
planner's whole job is to keep an infrastructure limitation from arriving as a
reward-0 row that looks exactly like Stella failing a task, so the cases worth
pinning are the ones where a mistake still produces a plausible-looking plan:
a GPU task quietly scheduled on a GPU-less host, a task admitted onto a machine
that cannot hold it, a build stage mistaken for an image to pull.

Two of these are regressions. The headroom test exists because the argument
originally conflated storage with memory and subtracted a 20 GB storage
allowance from a 10 GB memory budget, which excluded all 74 tasks with a
negative number and a polite explanation. The GPU test exists because the
obvious Docker probe — ``{{if index .Runtimes "nvidia"}}`` — reports a GPU on
every host alive, Go returning a truthy zero-value struct for a missing key.

``plan.py`` is a script rather than a package and imports nothing beyond the
standard library, so it loads by path and needs no Harbor to test.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

_PLAN_PATH = Path(__file__).resolve().parent.parent / "plan.py"
_spec = importlib.util.spec_from_file_location("frontier_plan", _PLAN_PATH)
assert _spec and _spec.loader
plan = importlib.util.module_from_spec(_spec)
sys.modules["frontier_plan"] = plan
_spec.loader.exec_module(plan)


def _task(
    root: Path,
    slug: str,
    *,
    cpus: int = 1,
    memory_mb: int = 2048,
    storage_mb: int = 10240,
    gpus: int = 0,
    dockerfile: str = "FROM python:3.13-slim\n",
    compose: str | None = None,
    tests_dockerfile: str | None = None,
) -> Path:
    """Write a task tree shaped like a real exported Frontier-Bench task."""
    d = root / "frontier-bench" / slug
    (d / "environment").mkdir(parents=True)
    (d / "task.toml").write_text(
        f'[task]\nname = "frontier-bench/{slug}"\n'
        f'[metadata]\ncategory = "Software"\n'
        f"[agent]\ntimeout_sec = 7200.0\n"
        f"[environment]\ncpus = {cpus}\nmemory_mb = {memory_mb}\n"
        f"storage_mb = {storage_mb}\ngpus = {gpus}\n"
    )
    (d / "environment" / "Dockerfile").write_text(dockerfile)
    if compose is not None:
        (d / "environment" / "docker-compose.yaml").write_text(compose)
    if tests_dockerfile is not None:
        (d / "tests").mkdir()
        (d / "tests" / "Dockerfile").write_text(tests_dockerfile)
    return d


@pytest.fixture
def host(monkeypatch):
    """Pin the host capacity so a test never depends on the machine running it."""

    def _set(memory_mb=16384, cpus=8, gpu=False):
        monkeypatch.setattr(
            plan,
            "_host_capacity",
            lambda: {"memory_mb": memory_mb, "cpus": cpus, "gpu": gpu, "source": "test"},
        )

    return _set


class TestGpuExclusion:
    def test_gpu_task_is_excluded_by_default_and_says_why(self, tmp_path, host):
        host(gpu=False)
        _task(tmp_path, "needs-gpu", gpus=1)
        _task(tmp_path, "plain")

        result = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)

        assert result["runnable_tasks"] == ["plain"]
        (excluded,) = result["excluded"]
        assert excluded["slug"] == "needs-gpu"
        assert "GPU" in excluded["reason"]

    def test_allow_gpu_alone_is_not_enough_without_a_host_gpu(self, tmp_path, host):
        """The operator asking for GPU tasks does not conjure a GPU.

        Honouring the flag on a GPU-less host is the exact failure this planner
        exists to prevent: the container starts, the work is impossible, and the
        verifier returns 0.0 indistinguishably from a genuine miss.
        """
        host(gpu=False)
        _task(tmp_path, "needs-gpu", gpus=1)

        result = plan.build_plan(tmp_path, allow_gpu=True, memory_headroom_mb=2048)

        assert result["runnable_tasks"] == []
        assert "nvidia" in result["excluded"][0]["reason"]

    def test_gpu_task_runs_when_the_host_actually_has_one(self, tmp_path, host):
        host(gpu=True)
        _task(tmp_path, "needs-gpu", gpus=1)

        result = plan.build_plan(tmp_path, allow_gpu=True, memory_headroom_mb=2048)

        assert result["runnable_tasks"] == ["needs-gpu"]
        assert result["excluded"] == []


class TestHeadroom:
    def test_headroom_is_subtracted_from_memory_not_storage(self, tmp_path, host):
        """Regression: a storage-sized headroom against memory excluded everything.

        A 20 GB allowance subtracted from a 10 GB memory budget goes negative,
        and every task — including a 2 GB one — falls off the plan.
        """
        host(memory_mb=10240, cpus=8)
        _task(tmp_path, "small-task", memory_mb=2048, storage_mb=1024000)

        result = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)

        assert result["runnable_tasks"] == ["small-task"]
        assert result["counts"]["runnable"] == 1

    def test_task_larger_than_the_remaining_budget_is_excluded(self, tmp_path, host):
        host(memory_mb=10240, cpus=8)
        _task(tmp_path, "hungry", memory_mb=12288)

        result = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)

        assert result["runnable_tasks"] == []
        assert "12288" in result["excluded"][0]["reason"]

    def test_a_task_that_exactly_fills_the_budget_is_admitted(self, tmp_path, host):
        """The comparison is strictly greater-than, and should stay that way.

        10240 MB less a 2048 MB headroom leaves exactly 8192. Excluding a task
        that fits precisely would drop real tasks for no reason — 8192 MB is the
        second most common footprint in the set.
        """
        host(memory_mb=10240, cpus=8)
        _task(tmp_path, "exact", memory_mb=8192)

        result = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)

        assert result["runnable_tasks"] == ["exact"]

    def test_lowering_the_headroom_admits_the_same_task(self, tmp_path, host):
        host(memory_mb=10240, cpus=8)
        _task(tmp_path, "hungry", memory_mb=8192)

        result = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=512)

        assert result["runnable_tasks"] == ["hungry"]

    def test_task_needing_more_cpus_than_the_host_is_excluded(self, tmp_path, host):
        host(memory_mb=65536, cpus=4)
        _task(tmp_path, "wide", cpus=16)

        result = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)

        assert result["runnable_tasks"] == []
        assert "CPU" in result["excluded"][0]["reason"]


class TestTiering:
    def test_tasks_land_in_the_tier_matching_their_memory(self, tmp_path, host):
        host(memory_mb=65536, cpus=32)
        _task(tmp_path, "a-tiny", memory_mb=2048)
        _task(tmp_path, "b-mid", memory_mb=4096)
        _task(tmp_path, "c-big", memory_mb=8192)
        _task(tmp_path, "d-huge", memory_mb=32768)

        tiers = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)["tiers"]

        assert tiers["small"]["tasks"] == ["a-tiny"]
        assert tiers["medium"]["tasks"] == ["b-mid"]
        assert tiers["large"]["tasks"] == ["c-big"]
        assert tiers["xlarge"]["tasks"] == ["d-huge"]

    def test_concurrency_is_bounded_by_whichever_resource_runs_out_first(
        self, tmp_path, host
    ):
        # 16 GB usable after headroom; each task wants 4 GB, so memory allows 4.
        # But 5 CPUs against 2 per task allows only 2 — the smaller wins.
        host(memory_mb=18432, cpus=5)
        _task(tmp_path, "a", memory_mb=4096, cpus=2)
        _task(tmp_path, "b", memory_mb=4096, cpus=2)

        tiers = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)["tiers"]

        assert tiers["medium"]["concurrency"] == 2

    def test_concurrency_never_drops_below_one(self, tmp_path, host):
        """A task that only just fits still gets scheduled, serially."""
        host(memory_mb=10240, cpus=2)
        _task(tmp_path, "just-fits", memory_mb=8192, cpus=2)

        tiers = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=512)["tiers"]

        assert tiers["large"]["concurrency"] == 1

    def test_concurrency_is_capped_regardless_of_headroom(self, tmp_path, host):
        host(memory_mb=1 << 20, cpus=512)
        _task(tmp_path, "tiny", memory_mb=2048, cpus=1)

        tiers = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)["tiers"]

        assert tiers["small"]["concurrency"] == plan.MAX_CONCURRENCY


class TestBaseImages:
    def test_multi_stage_aliases_are_not_pulled(self, tmp_path, host):
        """``FROM builder`` names a local stage; pulling it would fail."""
        host()
        _task(
            tmp_path,
            "staged",
            dockerfile=(
                "FROM golang:1.24 AS builder\n"
                "RUN go build\n"
                "FROM debian:12-slim\n"
                "COPY --from=builder /app /app\n"
            ),
        )

        images = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)["base_images"]

        assert images == ["debian:12-slim", "golang:1.24"]

    def test_scratch_and_arg_substituted_bases_are_skipped(self, tmp_path, host):
        host()
        _task(
            tmp_path,
            "odd",
            dockerfile="ARG BASE\nFROM scratch\nFROM ${BASE}\nFROM ubuntu:24.04\n",
        )

        images = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)["base_images"]

        assert images == ["ubuntu:24.04"]

    def test_compose_service_images_are_included(self, tmp_path, host):
        host()
        _task(
            tmp_path,
            "composed",
            compose="services:\n  db:\n    image: postgres:16\n  app:\n    build: .\n",
        )

        images = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)["base_images"]

        assert "postgres:16" in images

    def test_the_verifier_dockerfile_counts_too(self, tmp_path, host):
        """Every task here declares environment_mode = "separate".

        The verifier builds its own container, so its base is just as much a
        precondition of the run as the agent's.
        """
        host()
        _task(tmp_path, "separate-verifier", tests_dockerfile="FROM node:22-bookworm\n")

        images = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)["base_images"]

        assert "node:22-bookworm" in images

    def test_images_are_deduplicated_across_tasks(self, tmp_path, host):
        host()
        _task(tmp_path, "one", dockerfile="FROM python:3.13-slim\n")
        _task(tmp_path, "two", dockerfile="FROM python:3.13-slim\n")

        images = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)["base_images"]

        assert images == ["python:3.13-slim"]

    def test_an_excluded_task_does_not_drag_in_its_images(self, tmp_path, host):
        """Warming an image for a task that will not run is wasted bandwidth."""
        host(gpu=False)
        _task(tmp_path, "gpu-only", gpus=1, dockerfile="FROM nvidia/cuda:12.6.3-base\n")
        _task(tmp_path, "plain", dockerfile="FROM ubuntu:24.04\n")

        images = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)["base_images"]

        assert images == ["ubuntu:24.04"]


class TestModalBackend:
    """On a per-task backend the host stops being a term in the measurement.

    These matter because the docker path's caution becomes a bug when carried
    over unchanged: a Modal run that excludes tasks for *this laptop's* memory
    would silently produce a 48-task result while looking like a complete one,
    and a submission may not subset.
    """

    def test_a_task_far_larger_than_this_host_still_runs(self, tmp_path, host):
        host(memory_mb=8192, cpus=2)  # deliberately smaller than the task
        _task(tmp_path, "huge", memory_mb=32768, cpus=16)

        result = plan.build_plan(
            tmp_path, allow_gpu=True, memory_headroom_mb=2048,
            backend="modal", concurrency=32,
        )

        assert result["runnable_tasks"] == ["huge"]
        assert result["excluded"] == []

    def test_gpu_tasks_are_runnable(self, tmp_path, host):
        host(memory_mb=8192, cpus=2, gpu=False)  # the *host* has no GPU; Modal does
        _task(tmp_path, "gpu-task", gpus=1)

        result = plan.build_plan(
            tmp_path, allow_gpu=True, memory_headroom_mb=2048,
            backend="modal", concurrency=32,
        )

        assert result["runnable_tasks"] == ["gpu-task"]

    def test_docker_probe_is_not_consulted_at_all(self, tmp_path, monkeypatch):
        """A Modal plan must not fail because Docker is absent."""

        def _explode():
            raise AssertionError("_host_capacity must not run for backend=modal")

        monkeypatch.setattr(plan, "_host_capacity", _explode)
        _task(tmp_path, "a", memory_mb=32768)

        result = plan.build_plan(
            tmp_path, allow_gpu=True, memory_headroom_mb=2048,
            backend="modal", concurrency=8,
        )

        assert result["backend"] == "modal"
        assert result["runnable_tasks"] == ["a"]

    def test_explicit_concurrency_beats_the_capacity_arithmetic(self, tmp_path, host):
        """Unknown capacity would otherwise derive 1 and serialise the run."""
        host()
        _task(tmp_path, "a", memory_mb=16384, cpus=16)

        tiers = plan.build_plan(
            tmp_path, allow_gpu=True, memory_headroom_mb=2048,
            backend="modal", concurrency=64,
        )["tiers"]

        assert tiers["xlarge"]["concurrency"] == 64

    def test_explicit_concurrency_also_overrides_on_docker(self, tmp_path, host):
        """The override is a property of the flag, not of the backend."""
        host(memory_mb=65536, cpus=64)
        _task(tmp_path, "a", memory_mb=2048, cpus=1)

        tiers = plan.build_plan(
            tmp_path, allow_gpu=False, memory_headroom_mb=2048,
            backend="docker", concurrency=2,
        )["tiers"]

        # 2, not MAX_CONCURRENCY, which the arithmetic alone would have chosen.
        assert tiers["small"]["concurrency"] == 2

    def test_backend_is_recorded_in_the_plan(self, tmp_path, host):
        host()
        _task(tmp_path, "a")
        assert plan.build_plan(
            tmp_path, allow_gpu=False, memory_headroom_mb=2048,
        )["backend"] == "docker"


class TestPlanShape:
    def test_counts_reconcile(self, tmp_path, host):
        host(memory_mb=10240, cpus=8, gpu=False)
        _task(tmp_path, "ok")
        _task(tmp_path, "gpu", gpus=1)
        _task(tmp_path, "huge", memory_mb=32768)

        result = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)

        c = result["counts"]
        assert c["total"] == 3
        assert c["runnable"] + c["excluded"] == c["total"]
        assert len(result["excluded"]) == c["excluded"]

    def test_an_empty_dataset_directory_is_fatal_rather_than_an_empty_plan(
        self, tmp_path, host
    ):
        host()
        with pytest.raises(SystemExit):
            plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)

    def test_unknown_host_capacity_still_produces_a_runnable_plan(self, tmp_path, monkeypatch):
        """`docker info` failing must not silently empty the plan.

        Excluding every task because capacity could not be read looks identical
        to a host that is too small, so the planner admits them and lets the
        preflight's own docker check be the thing that fails.
        """
        monkeypatch.setattr(
            plan, "_host_capacity",
            lambda: {"memory_mb": None, "cpus": None, "gpu": False,
                     "source": "unavailable", "error": "boom"},
        )
        _task(tmp_path, "a", memory_mb=32768)

        result = plan.build_plan(tmp_path, allow_gpu=False, memory_headroom_mb=2048)

        assert result["runnable_tasks"] == ["a"]
        assert result["tiers"]["xlarge"]["concurrency"] == 1
