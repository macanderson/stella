# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Fold a fetched cloud run back into the **one match** it was submitted as.

``arenabench cloud run`` fans a match out into ``attempts x tasks x seats``
independent Batch jobs (see :mod:`.cloud`), each executing a one-trial slice of
the original template. That is the right shape for the substrate — a container
is the unit of isolation — and the wrong shape for the operator: a 10-task,
2-attempt, one-seat run fetches down as twenty separate contests, and the 89-task
two-arm shape is 178 of them. A scoreboard that has to be read 178 times is not
a scoreboard.

**The assembled match is not invented; it is the submitted one.**
``runs/<id>/match.toml`` in S3 holds the whole spec as submitted, and the
per-trial ``match.toml`` files under ``runs/<id>/trials/`` are slices of it
(``cloud.slice_spec``). So assembly restores the spec that was always there and
files each trial's artifacts back into the cell they were sliced out of.

**Assembly is a local fold, so it does not live in the S3 adapter.** Every
input is a file the fetch already wrote; nothing here knows AWS exists. Three
things follow, and each is the reason to keep it separate rather than an
afterthought of doing so:

* it is **testable without a network** — the tests build a fetched tree in a
  ``tmp_path`` and run the real function over it;
* it is **re-runnable** on a run fetched months ago, which is the whole
  migration story for the runs already sitting in ``arenabench-cloud/``: they
  are not corrupted and not rewritten, they are simply assembled in place; and
* it is **re-runnable after a bug in this file is fixed**, without paying for
  the download again.

``arenabench cloud fetch`` calls it at the end, so the operator still types one
command.

**By default nothing is copied and nothing is moved.** The assembled match is
a directory of **relative symlinks** into the per-trial artifact trees the
fetch wrote — so ``agent/stella-events.jsonl``, ``result.json``, ``verifier/``
and the rest stay exactly where they landed, reachable both by their original
path and through the match. That is deliberate: every bench conclusion this
project makes comes from the traces, and an assembler that summarised them
into a merged JSON and left the originals behind would be the summary layer
CLAUDE.md forbids concluding from. ``assembly.json`` records, per link, which
Batch job and which attempt it came from, so the provenance of a cell is one
file lookup rather than an inference from a directory name.

**A match meant to outlive its fetch directory is assembled with ``--copy``.**
Nothing owns a fetch directory — it is scratch output — and a symlink-only
match dies silently and totally when it is cleaned up: two loaded matches were
found with every one of their 85/88/122 trial links dangling, still *listing*
their arms so every read returned nothing rather than an error (#3261).
``--copy`` materialises the trial trees inside the match itself, spending disk
to buy a match that is actually a match; either way ``assembly.json`` records
the source run directory and run id, so a dangling match can name its own
recovery (`arenabench cloud fetch <run-id> --artifacts` + re-assemble — the
artifacts stay in S3).

The result is the **local** layout (``<workspace>/matches/<id>/``), which means
``arenabench serve`` opens it with no new server code: ``MatchRunner.restore_
from_disk`` finds it, ``Match.trial_dirs`` collapses the attempts of a task to
one representative by the same measured-beats-void rule a local run uses, and
the grid comes out one row per task and one column per seat.
"""

from __future__ import annotations

import json
import os
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any

from .model import MatchSpec, slugify

__all__ = [
    "ASSEMBLY_NAME",
    "AssembleError",
    "Assembly",
    "TrialArtifact",
    "assemble_run",
    "discover_trials",
    "link_name",
    "match_id_for",
    "register_cli",
    "seat_for_job_dir",
]

#: The manifest written beside the assembled match, naming every cell's source.
ASSEMBLY_NAME = "assembly.json"

#: Where the fetch puts the whole-match spec (``runs/<id>/match.toml``).
MATCH_TEMPLATE_NAME = "match.toml"

#: Harbor's per-seat launch manifest, a sibling of the job directory.
SEAT_MANIFEST_SUFFIX = ".seat.json"

PROVENANCE_NAME = "provenance.json"

#: Where a fetched trial keeps the workspace its container ran in. The glob is
#: the one :mod:`bench.trace_triage.run_trace` already reads a cloud run with,
#: so both readers agree about what a trial directory is.
_TRIAL_GLOB = "*/ws/matches/*/jobs/*/*"


class AssembleError(RuntimeError):
    """A run directory could not be assembled; the message says why."""


@dataclass(frozen=True)
class TrialArtifact:
    """One Harbor trial directory inside a fetched cloud run.

    ``task`` is recovered from the directory name the way every other reader in
    the tree recovers it (``rsplit("__", 1)``) — Harbor mints the suffix per
    invocation, so two attempts of one task are two directory names that share
    a task and agree about nothing else.
    """

    #: The Batch job directory the trial was fetched into — the run's own id
    #: for this trial, and the join key back to ``jobs.json``.
    job_id: str
    #: The seat this trial belongs to, as a contestant slug.
    seat_slug: str
    #: The trial directory itself, absolute.
    path: Path
    #: Harbor's per-seat manifest for the job this trial ran under, if present.
    seat_manifest: Path | None = None
    #: The provenance record of the slice this trial ran under, if present.
    provenance: Path | None = None

    @property
    def task(self) -> str:
        return self.path.name.rsplit("__", 1)[0]


@dataclass
class Assembly:
    """What one :func:`assemble_run` produced, for printing and for tests."""

    match_id: str
    match_dir: Path
    tasks: tuple[str, ...]
    #: Seat slug -> how many trial directories were linked into that column.
    linked: dict[str, int] = field(default_factory=dict)
    #: Human-readable problems that did not stop the assembly.
    warnings: list[str] = field(default_factory=list)

    @property
    def trials(self) -> int:
        return sum(self.linked.values())


# --------------------------------------------------------------------------
# pure decisions
# --------------------------------------------------------------------------


def match_id_for(run_dir: Path) -> str:
    """The assembled match's id: the run directory's own name, slugified.

    Deliberately **not** the spec id carried in ``match.toml``. Two runs of one
    template share that id, and two matches sharing a directory name in one
    workspace is one of them silently shadowing the other. The run directory is
    what the operator typed and what the artifacts sit under, so it is the name
    that can never collide with a run they still have.
    """
    name = slugify(run_dir.resolve().name)
    if not name:
        raise AssembleError(f"cannot derive a match id from {run_dir}")
    return name


def seat_for_job_dir(job_dir_name: str, slugs: Sequence[str]) -> str | None:
    """Which contestant slug a fetched Harbor job directory belongs to.

    A job directory is named ``<match-id>-<slug>`` by the container that ran
    it. Matching by suffix rather than by parsing the prefix is what keeps this
    correct for a slug that itself contains a dash, and the **longest** match
    wins so that ``stella`` never claims a directory belonging to
    ``stella-bare``.
    """
    matched = [
        slug
        for slug in slugs
        if job_dir_name == slug or job_dir_name.endswith(f"-{slug}")
    ]
    return max(matched, key=len) if matched else None


def link_name(taken: Iterable[str], trial_dir_name: str) -> str:
    """A collision-free name for a trial directory inside one job directory.

    Harbor's ``<task>__<suffix>`` is unique within the invocation that minted
    it and carries no guarantee across the separate containers a cloud run
    fans out to. A collision must not drop a trial and must not change its
    task, so the disambiguator is appended **after** the suffix — every reader
    recovers the task with ``rsplit("__", 1)``, which is unaffected.
    """
    taken = set(taken)
    if trial_dir_name not in taken:
        return trial_dir_name
    index = 2
    while f"{trial_dir_name}x{index}" in taken:
        index += 1
    return f"{trial_dir_name}x{index}"


def discover_trials(run_dir: Path, slugs: Sequence[str]) -> list[TrialArtifact]:
    """Every Harbor trial directory a fetched run contains, sorted.

    Reads the filesystem and nothing else — in particular not ``jobs.json``,
    which records what was *submitted*. What was submitted and what came back
    are different questions, and the second one is the one an assembler must
    answer: a job that never produced a trial has nothing to file into a cell,
    and a trial present on disk belongs in the match whether or not the
    submission record survived.
    """
    trials: list[TrialArtifact] = []
    for path in sorted(run_dir.glob(_TRIAL_GLOB)):
        if not path.is_dir():
            continue
        if not (path / "agent").is_dir() and not (path / "verifier").is_dir():
            # Harbor writes siblings beside its trial directories; a directory
            # with neither half is not a trial and must not become a cell.
            continue
        job_dir = path.parent
        seat = seat_for_job_dir(job_dir.name, slugs)
        if seat is None:
            continue
        match_root = job_dir.parent.parent
        manifest = job_dir.parent / f"{job_dir.name}{SEAT_MANIFEST_SUFFIX}"
        record = match_root / PROVENANCE_NAME
        trials.append(
            TrialArtifact(
                job_id=_job_id_of(run_dir, path),
                seat_slug=seat,
                path=path,
                seat_manifest=manifest if manifest.is_file() else None,
                provenance=record if record.is_file() else None,
            )
        )
    return trials


def _job_id_of(run_dir: Path, trial: Path) -> str:
    """The fetched Batch job directory a trial sits under."""
    return trial.relative_to(run_dir).parts[0]


#: Batch states that mean a job may yet upload the artifact it is missing.
#: Mirrored from :mod:`.cloud`'s split rather than imported: assembly is a
#: pure local fold and must keep working on a machine with no cloud module
#: concerns at all — and these strings are Batch's public contract, not ours.
_LIVE_BATCH_STATES = frozenset(
    {"SUBMITTED", "PENDING", "RUNNABLE", "STARTING", "RUNNING"}
)


def _sidecar_task_states(run_dir: Path) -> dict[str, list[str]]:
    """Per-task Batch states from the fetch's ``jobs.json`` sidecar.

    Empty on a fetch that predates the sidecar — the warning then degrades to
    its old, state-blind wording rather than failing the fold (#3218).
    """
    try:
        record = json.loads((run_dir / "jobs.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    states: dict[str, list[str]] = {}
    for job in record.get("jobs", []) if isinstance(record, dict) else []:
        task = job.get("task") if isinstance(job, dict) else None
        if task:
            states.setdefault(str(task), []).append(str(job.get("state") or ""))
    return states


def _missing_note(task: str, states: dict[str, list[str]]) -> str:
    """Why this task has no artifact, as far as the fetch-time states can say."""
    observed = [state for state in states.get(task, []) if state]
    if not observed:
        return ""
    live = sorted({s for s in observed if s in _LIVE_BATCH_STATES})
    if live:
        return f" (still {'/'.join(live)} in Batch at fetch time — re-fetch)"
    return " (terminal, no artifact uploaded)"


# --------------------------------------------------------------------------
# the filesystem side
# --------------------------------------------------------------------------


def _load_spec(run_dir: Path, match_id: str) -> MatchSpec:
    from .config import MatchTemplateError, load_match

    template = run_dir / MATCH_TEMPLATE_NAME
    if not template.is_file():
        raise AssembleError(
            f"{template} does not exist — the whole-match spec is what makes "
            "assembly a restoration rather than a guess. Re-fetch the run with "
            "`arenabench cloud fetch <run-id> --artifacts`."
        )
    try:
        spec = load_match(template, match_id=match_id)
    except MatchTemplateError as exc:
        raise AssembleError(
            f"{template} is not a loadable match: {'; '.join(exc.problems)}"
        ) from exc
    # Both, deliberately: the keyword only supplies an id the template lacks,
    # and a template that carries its own would otherwise keep it — which is
    # the spec-id collision `match_id_for` exists to avoid.
    return replace(spec, id=match_id)


def _clear_links(directory: Path) -> None:
    """Drop this directory's symlinks so a re-assembly is idempotent.

    Only symlinks: a real file or directory under here was put there by
    somebody else, and an assembler that deletes those is a data-loss bug
    wearing a convenience.
    """
    if not directory.is_dir():
        return
    for entry in directory.iterdir():
        if entry.is_symlink():
            entry.unlink()


def _link(source: Path, destination: Path, symlink: Callable[..., None]) -> None:
    """Point ``destination`` at ``source`` with a **relative** symlink.

    Relative so the whole ``arenabench-cloud/`` tree survives being moved or
    copied to another machine, which is exactly what happens when a run is
    handed to somebody else to read.
    """
    target = os.path.relpath(source, destination.parent)
    symlink(target, destination)


def _materialise(source: Path, destination: Path) -> None:
    """Copy ``source`` into the match instead of linking to it (#3261).

    A destination that already exists as a real entry is left alone — it was
    materialised by a previous assembly of the same run, and re-copying would
    only spend disk to overwrite it with itself. Internal symlinks are
    preserved as symlinks: the copy's job is to own the trial tree, not to
    reinterpret it.
    """
    if destination.exists() and not destination.is_symlink():
        return
    if destination.is_symlink():
        # A previous symlink-mode assembly of the same cell: the copy takes
        # its place, upgrading the match to a durable one.
        destination.unlink()
    if source.is_dir():
        import shutil

        shutil.copytree(source, destination, symlinks=True)
    else:
        import shutil

        shutil.copy2(source, destination, follow_symlinks=False)


def assemble_run(
    run_dir: Path,
    *,
    workspace: Path | None = None,
    match_id: str | None = None,
    copy: bool = False,
    symlink: Callable[..., None] = os.symlink,
) -> Assembly:
    """Assemble one fetched cloud run into a single match under ``workspace``.

    ``workspace`` defaults to the run directory itself, so
    ``arenabench serve --workspace <run-dir>`` opens exactly one contest and
    nothing outside the run directory is written. Pointing it at the shared
    parent (``arenabench-cloud/``) instead collects every assembled run into
    one arena, one match each.
    """
    run_dir = run_dir.expanduser().resolve()
    if not run_dir.is_dir():
        raise AssembleError(f"{run_dir} is not a directory")
    match_id = match_id or match_id_for(run_dir)
    workspace = (workspace or run_dir).expanduser().resolve()

    spec = _load_spec(run_dir, match_id)
    slugs = [c.slug for c in spec.contestants]
    trials = discover_trials(run_dir, slugs)

    assembly = Assembly(
        match_id=match_id,
        match_dir=workspace / "matches" / match_id,
        tasks=spec.tasks,
    )
    if not trials:
        assembly.warnings.append(
            f"no trial artifacts under {run_dir} — a results-only fetch carries "
            "no traces to assemble. Re-fetch with `--artifacts`."
        )

    match_dir = assembly.match_dir
    jobs_root = match_dir / "jobs"
    jobs_root.mkdir(parents=True, exist_ok=True)
    (match_dir / "spec.json").write_text(
        json.dumps(spec.to_json(), indent=2) + "\n", encoding="utf-8"
    )

    cells: list[dict[str, Any]] = []
    provenance_seen: set[bytes] = set()
    for slug in slugs:
        job_dir = jobs_root / f"{match_id}-{slug}"
        job_dir.mkdir(parents=True, exist_ok=True)
        _clear_links(job_dir)
        taken: set[str] = set()
        seat_trials = [t for t in trials if t.seat_slug == slug]
        for trial in seat_trials:
            name = link_name(taken, trial.path.name)
            taken.add(name)
            target = job_dir / name
            if copy:
                _materialise(trial.path, target)
            elif target.exists() and not target.is_symlink():
                # A previous --copy assembly already owns this cell; a
                # symlink re-assembly must not fail on it, and must not
                # replace a durable copy with a link that can dangle.
                pass
            else:
                _link(trial.path, target, symlink)
            cells.append(
                {
                    "seat": slug,
                    "task": trial.task,
                    "job_id": trial.job_id,
                    "trial": name,
                    # The path an operator opens to read the trace by hand, and
                    # the one the postmortem reads by machine.
                    "artifacts": os.path.relpath(trial.path, match_dir),
                }
            )
        assembly.linked[slug] = len(seat_trials)
        manifest = next(
            (t.seat_manifest for t in seat_trials if t.seat_manifest), None
        )
        if manifest is not None:
            link = jobs_root / f"{match_id}-{slug}{SEAT_MANIFEST_SUFFIX}"
            if link.is_symlink():
                link.unlink()
            if copy:
                _materialise(manifest, link)
            elif not link.exists():
                _link(manifest, link, symlink)
        for trial in seat_trials:
            if trial.provenance is not None:
                provenance_seen.add(trial.provenance.read_bytes())

    if provenance_seen:
        # Every slice recorded the same apparatus, or the run is not one match
        # in the only sense that matters. Disagreement is surfaced rather than
        # resolved: picking a winner would publish one arm's provenance over
        # another's, which is the confound `arenabench.provenance` exists to
        # refuse.
        first = sorted(provenance_seen)[0]
        (match_dir / PROVENANCE_NAME).write_bytes(first)
        if len(provenance_seen) > 1:
            assembly.warnings.append(
                f"{len(provenance_seen)} distinct provenance records across the "
                "run's trials — the slices did not all measure the same "
                "apparatus. Read them before quoting a number."
            )

    missing = sorted(set(spec.tasks) - {cell["task"] for cell in cells})
    if missing:
        # "No artifact" is two different facts wearing one warning: a trial
        # that will never produce one, and a trial that has not finished yet.
        # The h2h891 head-to-head was analyzed on a match missing two Claude
        # Code results that were still RUNNING at fetch time, and the short
        # match read as complete (#3218). The fetch leaves the per-job Batch
        # state it observed in a `jobs.json` sidecar, so assembly — still a
        # pure local fold — can name which kind each miss is.
        states = _sidecar_task_states(run_dir)
        described = [task + _missing_note(task, states) for task in missing[:8]]
        assembly.warnings.append(
            f"{len(missing)} task(s) produced no trial artifact: "
            + ", ".join(described)
            + (" …" if len(missing) > 8 else "")
        )

    # The recovery record (#3261): a symlink-mode match whose fetch directory
    # is later deleted still lists its arms while every read returns nothing,
    # so the manifest names where the cells came from and — when the fetch
    # left its `jobs.json` sidecar — the run id whose S3 artifacts restore
    # them (`arenabench cloud fetch <run-id> --artifacts`, then re-assemble).
    source_run_id = ""
    try:
        sidecar = json.loads((run_dir / "jobs.json").read_text(encoding="utf-8"))
        source_run_id = str(sidecar.get("run_id") or "")
    except (OSError, ValueError):
        pass

    (match_dir / ASSEMBLY_NAME).write_text(
        json.dumps(
            {
                "schema": 1,
                "match_id": match_id,
                "run_dir": os.path.relpath(run_dir, match_dir),
                "copied": copy,
                "source": {
                    "run_dir": str(run_dir),
                    "run_id": source_run_id,
                    "recover": (
                        f"arenabench cloud fetch {source_run_id} --artifacts"
                        if source_run_id
                        else ""
                    ),
                },
                "tasks": list(spec.tasks),
                "seats": {slug: assembly.linked[slug] for slug in slugs},
                "cells": cells,
                "warnings": assembly.warnings,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    return assembly


# --------------------------------------------------------------------------
# CLI verb
# --------------------------------------------------------------------------


def _cmd_assemble(args: Any) -> int:
    import sys

    run_dir = Path(args.run_dir)
    try:
        assembly = assemble_run(
            run_dir,
            workspace=Path(args.workspace) if args.workspace else None,
            match_id=args.match_id,
            copy=args.copy,
        )
    except AssembleError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    print(f"match     : {assembly.match_id}")
    print(f"tasks     : {len(assembly.tasks) or 'all'}")
    for slug, count in assembly.linked.items():
        print(f"  {slug:40} {count} trial(s)")
    print(f"assembled : {assembly.match_dir}")
    for warning in assembly.warnings:
        print(f"  !! {warning}")
    workspace = assembly.match_dir.parent.parent
    print(f"open with : arenabench serve --workspace {workspace}")
    return 0


def register_cli(subparsers: Any) -> None:
    """Attach the ``assemble`` verb to the main CLI's subparsers."""
    parser = subparsers.add_parser(
        "assemble",
        help="fold a fetched cloud run into one match `arenabench serve` opens",
    )
    parser.add_argument("run_dir", help="a directory fetched by `cloud fetch`")
    parser.add_argument(
        "--workspace",
        help="arena workspace to write the match into (default: the run "
             "directory, so `serve --workspace <run-dir>` shows one contest)",
    )
    parser.add_argument(
        "--match-id",
        help="id for the assembled match (default: the run directory's name)",
    )
    parser.add_argument(
        "--copy",
        action="store_true",
        help="materialise trial trees inside the match instead of symlinking "
             "into the fetch directory — costs disk, survives the fetch "
             "directory being cleaned up",
    )
    parser.set_defaults(func=_cmd_assemble)
