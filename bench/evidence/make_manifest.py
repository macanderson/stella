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

Every value that describes the run rather than this machine is taken from what
the trials recorded, and the operator-supplied arguments are cross-checked
against it. Running here and running on the host that produced the trials are
the same thing only while the run is still warm; afterwards, this checkout's
adapter, venv and platform are all somebody else's, and a manifest built from
them describes the wrong run with no outward sign that it does.

Usage::

    python make_manifest.py --run-dir <dir> \
        --sut-commit <sha> --binary-sha256 <sha> --model <spec> \
        --dataset <name@sha256:...> --tasks 89 --attempts 1 \
        --concurrency 3 --budget-per-trial 0.60

`--job-dir <dir>` names the trials from the Harbor job tree when it is still on
this machine; without it they come from `trials.jsonl`, so a manifest can be
rebuilt from committed evidence alone. `--host-json <file>` supplies the host the
trials ran on, and is required whenever this script is not running there.

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


def _drift(computed: dict[str, Any], recorded: str | None) -> dict[str, Any]:
    """Reconcile a value computed from this checkout against the run's own record.

    The working tree is the run's tree only while the run is still warm. Building
    a manifest for a finished run after the adapter has moved recomputes a
    posture no trial ever used — and it is emitted with exactly the same
    confidence as a correct one, which is the part that makes it dangerous. When
    the trials recorded a digest, that digest is what ran; a disagreement is
    reported rather than resolved, and the checkout's body is withdrawn instead
    of being left to read as this run's configuration.
    """
    if recorded is None:
        return computed
    if "error" in computed:
        # The adapter not being importable here says nothing about what ran.
        # Reporting only the import error would throw away the one authoritative
        # value we hold, and off-host is exactly when that value is all there is.
        return {
            "normalized_sha256": recorded,
            "source": "recorded per trial",
            "note": f"not cross-checked against a checkout: {computed['error']}",
        }
    if computed.get("normalized_sha256") == recorded:
        return {**computed, "source": "recorded per trial, and this checkout agrees"}
    return {
        "normalized_sha256": recorded,
        "source": "recorded per trial",
        "computed_from_this_checkout_sha256": computed.get("normalized_sha256"),
        "note": (
            "this checkout's adapter is NOT the one that produced this run, so the "
            "body it computes is withheld: the recorded digest is authoritative"
        ),
    }


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


def _host(host_json: Path | None) -> dict[str, Any]:
    """The host the trials actually ran on.

    Detected locally by default, which is correct while finalizing on the run
    host and wrong every other time: build a manifest on a laptop from a cloud
    run's job tree and it records the laptop, in the one block whose whole job is
    to say where the number came from. `--host-json` carries the real one,
    collected on that host.
    """
    if host_json is not None:
        supplied = json.loads(host_json.read_text())
        return {**supplied, "source": f"collected on the run host ({host_json.name})"}
    return {
        "kernel": platform.platform(),
        "machine": platform.machine(),
        "container_platform": "linux/amd64 (DOCKER_DEFAULT_PLATFORM)",
        "docker": _run("docker", "version", "--format", "{{.Server.Version}}"),
        "vm": _run("colima", "version"),
        "native_x86_64_linux": platform.system() == "Linux" and platform.machine() == "x86_64",
        "source": "detected on the machine that built this manifest",
    }


def _claim_eligibility(host: dict[str, Any]) -> dict[str, Any]:
    """Why this run is not a leaderboard row — for *this* run, not for a remembered one.

    These reasons were a fixed list, which read as thorough and was the opposite:
    it asserted Rosetta-under-colima-on-macOS for every run, so the first one to
    move to a native Linux host published a manifest that disclosed a defect it
    did not have. A disclosure block that cannot be wrong in the flattering
    direction can still be wrong, and a reader has no way to tell which entries
    were checked. Only the host-dependent reason is conditional; the rest hold on
    any host this path can run on, which is why this is still never a claim.
    """
    why_not = []
    if not host.get("native_x86_64_linux"):
        why_not.append(
            "host is not a native x86_64 Linux Docker host: linux/amd64 containers "
            "run under emulation (Rosetta/qemu) inside a VM"
        )
    why_not += [
        "credentials came from the ambient environment, the adapter's "
        "environment-fallback source, not a Management-API-minted spend-capped key",
        "no host attestation was collected or committed",
        "no six-comment intent ledger; a single human-readable preregistration instead",
        "launched through the plain adapter path, not secure_launcher.py",
        "no external Terminal-Bench maintainer trajectory review",
    ]
    return {
        "audited_public_row": False,
        "why_not": why_not,
        "what_it_is": (
            "a self-reported development baseline, adequate as the held-out number "
            "the self-improvement track needs and as a falsifiable check on Stella's "
            "own claims; not adequate for a leaderboard row or a competitor comparison "
            "without restating every caveat above"
        ),
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


def _trial_dirs(job_dir: Path | None, run_dir: Path) -> list[str]:
    """Trial directory names, under either Harbor layout — or from the evidence.

    Harbor 0.6.1 — the version pinned here as an audited constant — writes
    `<task>__<suffix>/result.json` directly under the job directory; later
    releases nest them under `trials/`. Reading only the nested layout is not a
    partial answer, it is a wrong one: a complete 89-trial run records
    `"trials": {"count": 0, "ids": []}`, and the count is the field a reader
    checks first to see whether the manifest describes the whole run. The
    sibling scorer already accepts both, so the two disagreed about the same
    job directory.

    The last fallback is `trials.jsonl` itself, because the multi-gigabyte job
    tree is deliberately never committed: rebuilding a manifest from published
    evidence — the thing this directory promises a reader can do — otherwise
    reports a run with no trials in it.
    """
    if job_dir is not None:
        nested = sorted(path.name for path in (job_dir / "trials").glob("*") if path.is_dir())
        if nested:
            return nested
        flat = sorted(path.parent.name for path in job_dir.glob("*/result.json"))
        if flat:
            return flat
    trials = run_dir / "trials.jsonl"
    if not trials.is_file():
        return []
    return sorted(
        str(row["trial_id"])
        for row in (json.loads(line) for line in trials.read_text().splitlines() if line.strip())
        if row.get("trial_id")
    )


def _recorded(run_dir: Path) -> dict[str, Any]:
    """What the trials themselves say they ran.

    `finalize.sh` writes `trials.jsonl` before calling this script, so the run's
    own account of its identity is already on disk and needs no new flag. Each
    field is collapsed to the set of distinct values across trials: one value is
    the run's identity, more than one means the run was not uniform and that is
    a finding, not something to average away.
    """
    trials = run_dir / "trials.jsonl"
    if not trials.is_file():
        return {}
    fields = (
        "engine_posture_sha256",
        "assurance_arm",
        "assurance_tiers_sha256",
        "binary_sha256",
        "source_commit",
        "harbor_version",
    )
    seen: dict[str, set[str]] = {name: set() for name in fields}
    rows = 0
    for line in trials.read_text().splitlines():
        if not line.strip():
            continue
        rows += 1
        row = json.loads(line)
        for name in fields:
            value = row.get(name)
            if value is not None:
                seen[name].add(str(value))
    if not rows:
        return {}
    out: dict[str, Any] = {"trials_read": rows}
    for name, values in seen.items():
        if len(values) == 1:
            out[name] = next(iter(values))
        elif values:
            # Never silently pick one. A run whose trials disagree about the
            # binary they executed is not one number.
            out[name] = {"DISAGREEMENT": sorted(values)}
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--job-dir",
        default=None,
        help=(
            "the Harbor job directory, when it is still on this machine. Optional: "
            "the job tree is never committed, so rebuilding a manifest from "
            "published evidence names its trials from trials.jsonl instead."
        ),
    )
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
    parser.add_argument(
        "--host-json",
        default=None,
        help=(
            "JSON describing the host the trials ran on, collected on that host. "
            "Required whenever this script does not run there — otherwise the "
            "manifest records the machine that built it. Set "
            "`native_x86_64_linux` in it: it decides whether the emulated-host "
            "entry appears in claim_eligibility.why_not."
        ),
    )
    parser.add_argument("--started-at", default=None)
    parser.add_argument("--finished-at", default=None)
    args = parser.parse_args()

    # `--witness-author "$STELLA_WITNESS_AUTHOR_MODEL"` from finalize.sh passes an
    # empty string on the control arm, and empty is not a model spec — without
    # this the control run, the one that must keep working untouched, would be
    # the only one that fails to produce a manifest.
    witness_author = (args.witness_author or "").strip() or None

    job_dir = Path(args.job_dir) if args.job_dir else None
    run_dir = Path(args.run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)

    trial_dirs = _trial_dirs(job_dir, run_dir)
    recorded = _recorded(run_dir)
    host = _host(Path(args.host_json) if args.host_json else None)

    manifest: dict[str, Any] = {
        "schema": SCHEMA,
        # This is the sentence a reader needs before any number below it.
        "claim_eligibility": _claim_eligibility(host),
        # What the trials themselves reported running, independent of any
        # argument passed here or any state of this checkout. Where it overlaps
        # the fields below it is the cross-check on them.
        "recorded_by_the_run": recorded,
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
            # The two identity fields above are operator-supplied. The trials
            # recorded what they actually executed, so the claim is checkable
            # rather than merely stated.
            "matches_what_the_trials_recorded": {
                "commit": recorded.get("source_commit") == args.sut_commit,
                "binary_sha256": recorded.get("binary_sha256") == args.binary_sha256,
            },
        },
        "benchmark": {
            "dataset": args.dataset,
            "tasks_declared": args.tasks,
            "verifier": "the dataset's own, unmodified",
            # The run's own record first: `_harbor_version()` reads whatever
            # venv this script is running in, which is the run's venv only while
            # finalizing on the run host.
            "harbor_version": recorded.get("harbor_version") or _harbor_version(),
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
            **_drift(
                _posture(args.model, witness_author),
                recorded.get("engine_posture_sha256"),
            ),
        },
        "assurance": _drift(
            _assurance(args.model, witness_author),
            recorded.get("assurance_tiers_sha256"),
        ),
        "host": host,
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
