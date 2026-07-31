#!/usr/bin/env python3
"""Freeze the identity of a development-baseline run into a run manifest.

`#909`'s acceptance is that "the score is reproducible from the committed
manifest". That requires the manifest to pin every input that could change the
number: the SUT commit and the binary SHA the adapter verified per trial, the
dataset digest, the harbor version, the engine-posture hash, the sampling
parameters, and the host the trials actually ran on.

The posture hash is read from the adapter's own `_benchmark_engine_posture`, not
recomputed here, so a hand-written value cannot drift from what the agent
actually sent. The `assurance` block does the same for which verification tiers
the run exercised — a scored run either runs a tier or declares it off here,
never leaves it discoverable only by grepping trajectories (#1007).

Usage::

    python make_manifest.py --job-dir <dir> --run-dir <dir> \
        --sut-commit <sha> --binary-sha256 <sha> --model <spec> \
        --dataset <name@sha256:...> --tasks 89 --attempts 1 \
        --concurrency 3 --budget-per-trial 0.60

Add `--witness-author <provider/model>` for the witness-on arm; omit it for the
control arm, whose posture hash and number stay exactly as published.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import subprocess
import sys
from pathlib import Path
from typing import Any

# v2 adds the `assurance` block (#1007). The bump is the point: a v1 manifest
# has no tier declaration, so "which rungs of the ladder did this number come
# from" is unanswerable for it — and that must stay visible rather than being
# papered over by an optional field that is simply absent on older runs.
SCHEMA = "stella-tb21-dev-baseline-manifest-v2"


def _run(*command: str) -> str | None:
    """Captured stdout, or None if the command could not run or said nothing."""
    try:
        return subprocess.run(command, capture_output=True, text=True, timeout=60).stdout.strip() or None
    except (OSError, subprocess.SubprocessError):
        return None


def _succeeds(*command: str) -> bool:
    """Whether the command exited 0.

    Separate from `_run` on purpose: `git merge-base --is-ancestor` answers
    entirely through its exit status and prints nothing, so reading its stdout
    reports every commit as *not* an ancestor — including `origin/main` itself.
    Provenance is exactly the claim that must not be quietly wrong.
    """
    try:
        return subprocess.run(command, capture_output=True, timeout=60).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def _posture(model: str, witness_author: str | None) -> dict[str, Any]:
    """Read the posture + its hash from the adapter itself."""
    try:
        from stella_harbor import _benchmark_engine_posture  # type: ignore[attr-defined]
    except ImportError as error:
        return {"error": f"adapter not importable: {error}"}
    posture, normalized, digest = _benchmark_engine_posture(model, witness_author=witness_author)
    return {
        "posture": posture,
        "normalized_sha256": digest,
        "normalized_bytes": len(normalized.encode()),
    }


def _assurance(model: str, witness_author: str | None) -> dict[str, Any]:
    """Declare which verification tiers this run exercised.

    A scored run must not silently disable a tier (#1007). Before this block the
    authored witness was off on every trial and the only trace was a warning
    line inside each trajectory, so a reader of the manifest had no way to know
    the number came from a ladder with a rung missing. Read from the adapter for
    the same reason the posture is: a hand-written declaration can drift from
    what the agent actually ran.
    """
    try:
        from stella_harbor import (  # type: ignore[attr-defined]
            _benchmark_assurance_tiers,
        )
    except ImportError as error:
        return {"error": f"adapter not importable: {error}"}
    tiers, normalized, digest = _benchmark_assurance_tiers(model, witness_author=witness_author)
    return {
        "tiers": tiers,
        "normalized_sha256": digest,
        "normalized_bytes": len(normalized.encode()),
    }


def _harbor_version() -> str | None:
    try:
        import harbor

        return str(harbor.__version__)
    except Exception:  # noqa: BLE001 - absence is the datum
        return None


def _file_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--job-dir", required=True)
    parser.add_argument("--run-dir", required=True)
    parser.add_argument("--sut-commit", required=True)
    parser.add_argument("--binary-sha256", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--dataset", required=True)
    parser.add_argument("--tasks", type=int, required=True)
    parser.add_argument("--attempts", type=int, required=True)
    parser.add_argument("--concurrency", type=int, required=True)
    parser.add_argument("--budget-per-trial", required=True)
    parser.add_argument(
        "--witness-author",
        default=None,
        help=(
            "provider/model pinned as the witness/judge author (#1007). Omit for "
            "the control arm, in which every role inherits one model and the "
            "authored-witness tier is structurally off."
        ),
    )
    parser.add_argument("--prereg-url", default=None)
    parser.add_argument("--started-at", default=None)
    parser.add_argument("--finished-at", default=None)
    args = parser.parse_args()

    # `--witness-author "$STELLA_WITNESS_AUTHOR_MODEL"` from finalize.sh passes an
    # empty string on the control arm, and empty is not a model spec — without
    # this the control run, the one that must keep working untouched, would be
    # the only one that fails to produce a manifest.
    witness_author = (args.witness_author or "").strip() or None

    job_dir = Path(args.job_dir)
    run_dir = Path(args.run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)

    trial_dirs = sorted(path.name for path in (job_dir / "trials").glob("*") if path.is_dir())

    manifest: dict[str, Any] = {
        "schema": SCHEMA,
        # This is the sentence a reader needs before any number below it.
        "claim_eligibility": {
            "audited_public_row": False,
            "why_not": [
                "host is not a native x86_64 Linux Docker host: linux/amd64 containers "
                "run under Rosetta inside a colima VM on macOS",
                "credentials came from the ambient environment, the adapter's "
                "environment-fallback source, not a Management-API-minted spend-capped key",
                "no host attestation was collected or committed",
                "no six-comment intent ledger; a single human-readable preregistration instead",
                "launched through the plain adapter path, not secure_launcher.py",
                "no external Terminal-Bench maintainer trajectory review",
            ],
            "what_it_is": (
                "a self-reported development baseline, adequate as the held-out number "
                "the self-improvement track needs and as a falsifiable check on Stella's "
                "own claims; not adequate for a leaderboard row or a competitor comparison "
                "without restating every caveat above"
            ),
        },
        "system_under_test": {
            "commit": args.sut_commit,
            "describe": _run("git", "describe", "--tags", args.sut_commit),
            "is_ancestor_of_main": _succeeds(
                "git", "merge-base", "--is-ancestor", args.sut_commit, "origin/main"
            ),
            "binary_sha256": args.binary_sha256,
            "binary_sha256_note": (
                "host-specific: release builds bake in the builder's cargo/rustup paths. "
                "The authoritative identity is the STELLA_BUILD_GIT_SHA stamp, which the "
                "adapter verifies against the uploaded binary on every trial."
            ),
            "build_stamp_env": f"STELLA_BUILD_GIT_SHA={args.sut_commit}",
            "target": "x86_64-unknown-linux-gnu, glibc 2.17 floor",
        },
        "benchmark": {
            "dataset": args.dataset,
            "tasks_declared": args.tasks,
            "verifier": "the dataset's own, unmodified",
            "harbor_version": _harbor_version(),
            "adapter": "bench/harbor_adapter (stella_harbor:StellaAgent)",
        },
        "sampling": {
            "attempts_per_task": args.attempts,
            "n_concurrent": args.concurrency,
            "max_retries": 0,
            "task_filters": None,
            "excluded_tasks": None,
        },
        "engine": {
            "model": args.model,
            "witness_author_model": witness_author,
            "budget_usd_per_trial": args.budget_per_trial,
            "reflection_disabled": True,
            **_posture(args.model, witness_author),
        },
        "assurance": _assurance(args.model, witness_author),
        "host": {
            "kernel": platform.platform(),
            "machine": platform.machine(),
            "container_platform": "linux/amd64 (DOCKER_DEFAULT_PLATFORM)",
            "docker": _run("docker", "version", "--format", "{{.Server.Version}}"),
            "vm": _run("colima", "version"),
            "note": (
                "shared developer workstation, not a dedicated benchmark host. "
                "Concurrent unrelated load was present; see wall-clock fields per trial."
            ),
        },
        "timing": {"started_at": args.started_at, "finished_at": args.finished_at},
        "preregistration_url": args.prereg_url,
        "trials": {"count": len(trial_dirs), "ids": trial_dirs},
        "artifacts": {},
    }

    # Digest whatever the run directory already holds so the committed evidence
    # is self-verifying.
    for path in sorted(run_dir.rglob("*")):
        if path.is_file() and path.name != "run-manifest.json":
            manifest["artifacts"][path.relative_to(run_dir).as_posix()] = _file_digest(path)

    out = run_dir / "run-manifest.json"
    out.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    print(f"wrote {out}")
    print(f"manifest sha256 = {_file_digest(out)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
