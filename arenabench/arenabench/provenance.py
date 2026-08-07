# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""What a result set was measured *with* — so two of them are never silently mixed.

A score is only meaningful next to another score from the same apparatus. Two
things define that apparatus, and both can change under you:

* **the dataset digest** — the tasks themselves. Already pinned in a
  :class:`~arenabench.registry.Dataset`, but a *result* has to carry the digest
  it actually ran, because the registry entry can be re-pinned tomorrow.
* **the Harbor version** — the grading stack. This is the one that bites.
  Harbor 0.6.1 and 0.20.0 will both run Terminal-Bench 2.1 and both produce a
  number, and those numbers are not comparable: a Harbor that predates a task
  setting drops it (pydantic ``extra="ignore"``) and grades against a
  different container topology. Nothing errors. Nothing looks wrong.

So every match writes a :class:`Provenance` beside its jobs, and the pair
``(dataset_digest, harbor_version)`` is its :attr:`~Provenance.comparability_key`.
Two matches with the same key may be compared; two with different keys may be
*shown together*, but only labelled, never summed.

# Why history is labelled rather than guessed

**Harbor records its own version nowhere** — not in ``config.json``, not in
``result.json``. For a match that ran before this module existed there is no
version to recover, and inventing one would defeat the entire purpose: a
fabricated "0.6.1" is indistinguishable from a measured one, which is exactly
the confusion this file exists to prevent.

What *is* recoverable is a bound. Harbor's own ``JobConfig`` grew fields over
time, and the config it wrote is on disk: a job config with no ``install_only``
or ``extra_instruction_paths`` key was written by a Harbor that did not have
them, which is every Harbor before 0.20.0. So a backfilled record carries
``harbor_version = None`` and ``harbor_bound = "<0.20.0"``, with
:attr:`source` saying it was inferred. "Unknown, but provably older than X" is
a true statement; "0.6.1" would not be.
"""

from __future__ import annotations

import json
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

__all__ = [
    "PROVENANCE_FILE",
    "Provenance",
    "backfill_match",
    "read_match",
    "write_match",
]

#: Sits beside ``jobs/`` in a match directory.
PROVENANCE_FILE = "provenance.json"

#: Fields Harbor's ``JobConfig`` gained by 0.20.0. A config written without
#: them predates that release — the only version statement recoverable from a
#: match that did not record one.
_POST_0_20_JOB_FIELDS = ("install_only", "extra_instruction_paths")

#: What a record's numbers came from: this process measured it, or it was
#: reconstructed afterwards from what Harbor happened to leave on disk.
SOURCE_RECORDED = "recorded"
SOURCE_BACKFILLED = "backfilled"


@dataclass
class Provenance:
    """The apparatus one match's numbers were produced by."""

    #: Bumped when a field's meaning changes, never when one is added.
    schema: int = 1
    #: Registry key, e.g. ``terminal-bench-2.1``.
    dataset_key: str = ""
    #: The full ``name@sha256:…`` ref, as handed to Harbor.
    dataset_ref: str = ""
    #: Just the digest, which is the half that defines the task corpus.
    dataset_digest: str = ""
    #: The Harbor that graded it. ``None`` means genuinely unknown — see the
    #: module docstring on why that is never filled in with a guess.
    harbor_version: str | None = None
    #: A true statement about an unknown version, e.g. ``"<0.20.0"``.
    harbor_bound: str | None = None
    #: The Harbor binary's path, for a machine running more than one.
    harbor_bin: str = ""
    #: The git ref the operator pinned the system under test to, e.g. ``main``.
    #: Empty means the match ran unpinned — see :attr:`sut_commit`.
    sut_ref: str = ""
    #: The **commit** that ref resolved to at launch, which is the half that
    #: identifies the code. A ref is a moving pointer: recording only "main"
    #: dates a result to whenever someone happens to read the record, which is
    #: the same reason :mod:`arenabench.registry` pins datasets by digest.
    #: Empty means genuinely unknown, never a guess — an unpinned match ran a
    #: binary nobody can attribute, and saying so is the honest record.
    sut_commit: str = ""
    #: SHA-256 of the exact binary that ran. Two builds of one commit on
    #: different toolchains are not the same artifact, and this is what tells
    #: them apart after the fact.
    sut_sha256: str = ""
    arenabench_version: str = ""
    recorded_at: float = field(default_factory=time.time)
    source: str = SOURCE_RECORDED

    @property
    def comparability_key(self) -> str:
        """What must match for two result sets to be comparable.

        Short and human-readable on purpose: it is printed next to results, and
        a key nobody reads is a key nobody notices differing. An unknown Harbor
        keeps its bound in the key rather than collapsing to the same string as
        a measured one — the whole point is that those two are not the same.
        """
        digest = (self.dataset_digest or "unknown").removeprefix("sha256:")[:8]
        if self.harbor_version:
            grader = f"harbor{self.harbor_version}"
        elif self.harbor_bound:
            grader = f"harbor?{self.harbor_bound}"
        else:
            grader = "harbor?"
        # The SUT belongs in the key for the same reason the dataset digest
        # does: two runs of different Stella revisions answer different
        # questions, and a key that hides that invites them into one average.
        sut = f"stella{self.sut_commit[:8]}" if self.sut_commit else "stella?"
        return f"{self.dataset_key or 'unknown'}@{digest}/{grader}/{sut}"

    @property
    def measured(self) -> bool:
        """Whether the grading stack is known, rather than merely bounded."""
        return self.harbor_version is not None

    def to_json(self) -> dict[str, Any]:
        out = asdict(self)
        # Derived, but written out so anything reading the file — a script, a
        # spreadsheet, a future version of this tool — gets the grouping key
        # without having to reimplement how it is built.
        out["comparability_key"] = self.comparability_key
        out["measured"] = self.measured
        return out

    @classmethod
    def from_json(cls, payload: dict[str, Any]) -> Provenance:
        known = {f for f in cls.__dataclass_fields__}
        return cls(**{k: v for k, v in payload.items() if k in known})


def write_match(match_dir: Path, provenance: Provenance) -> None:
    """Record the apparatus beside the match's jobs.

    Best-effort: a match that cannot write this file still runs. Losing the
    label is bad, but refusing to benchmark because a directory is read-only
    would be worse, and the absence is itself detectable (a match with no
    record reads as unknown, never as "same as the last one").
    """
    try:
        match_dir.mkdir(parents=True, exist_ok=True)
        (match_dir / PROVENANCE_FILE).write_text(
            json.dumps(provenance.to_json(), indent=2) + "\n", encoding="utf-8"
        )
    except OSError:
        pass


def read_match(match_dir: Path) -> Provenance | None:
    """The recorded apparatus, or ``None`` when the match predates recording."""
    path = match_dir / PROVENANCE_FILE
    try:
        return Provenance.from_json(json.loads(path.read_text(encoding="utf-8")))
    except (OSError, ValueError, TypeError):
        return None


def _job_configs(match_dir: Path) -> list[dict[str, Any]]:
    """Every Harbor ``config.json`` under a match's job directories."""
    found: list[dict[str, Any]] = []
    for config in sorted(match_dir.glob("jobs/*/config.json")):
        try:
            found.append(json.loads(config.read_text(encoding="utf-8")))
        except (OSError, ValueError):
            continue
    return found


def _key_for_name(name: str) -> str:
    """Which registered dataset a Harbor dataset name belongs to.

    Matches on the digest-stripped name, because the *name* identifies the
    corpus while the digest identifies the version of it — and a match that
    ran an older digest of Terminal-Bench is still a Terminal-Bench match. The
    digest is recorded separately and is what actually gates comparability.
    """
    from .registry import DEFAULT_REGISTRY

    bare = name.partition("@")[0]
    for dataset in DEFAULT_REGISTRY.datasets.values():
        if bare == dataset.name_only:
            return dataset.key
    return ""


def _key_for_path(path: str) -> str:
    """Which registered dataset a local export directory belongs to.

    An export lands at ``<root>/<key>/<package_dir>``, so both halves of the
    tail identify it. Matching on either is deliberate: the layout has changed
    once already, and a record that silently says "unknown" because a
    directory moved is worth less than one that recognises the corpus.
    """
    from .registry import DEFAULT_REGISTRY

    tail = Path(path)
    leaf, parent = tail.name, tail.parent.name
    for dataset in DEFAULT_REGISTRY.datasets.values():
        if leaf == dataset.package_dir or parent == dataset.key:
            return dataset.key
    return ""


def backfill_match(match_dir: Path, dataset_key: str = "") -> Provenance | None:
    """Reconstruct what can be known about a match that recorded nothing.

    Reads Harbor's own per-job ``config.json``, which carries the dataset it
    was pointed at. The Harbor *version* is not in there and is not guessed:
    the record comes back with :attr:`Provenance.harbor_version` ``None`` and a
    bound inferred from which fields that config has.

    Returns ``None`` when there is nothing on disk to read, so a caller can
    tell "no jobs here" from "jobs, but nothing recoverable".
    """
    configs = _job_configs(match_dir)
    if not configs:
        return None

    # Harbor keeps the dataset's name and its digest in *separate* fields
    # (`name` = "terminal-bench/terminal-bench-2-1", `ref` = "sha256:…"), and
    # a run from a local export sets neither — only `path`. All three shapes
    # appear in real history, so all three are read.
    name = ""
    digest = ""
    path_hint = ""
    for config in configs:
        for dataset in config.get("datasets") or []:
            name = name or (dataset.get("name") or "")
            raw_ref = dataset.get("ref") or ""
            if raw_ref and not digest:
                # `ref` is sometimes the bare digest and sometimes name@digest.
                digest = raw_ref.partition("@")[2] if "@" in raw_ref else raw_ref
            path_hint = path_hint or (dataset.get("path") or "")
        if name or digest or path_hint:
            break
    if digest and not digest.startswith("sha256:"):
        # Only a content digest is provenance. Anything else here is a tag,
        # which can be re-pointed, so it is not recorded as one.
        digest = ""
    ref = f"{name}@{digest}" if name and digest else (name or "")
    if not dataset_key and name:
        dataset_key = _key_for_name(name)

    # A dataset run from a local export carries no digest in Harbor's config —
    # only the directory it was read from. The directory names *which* corpus
    # it was, so the registry can recover the key; it cannot recover the
    # digest, and the registry's current pin is not evidence of what ran back
    # then. So the key is resolved and the digest is left unknown, which is
    # the true statement rather than the flattering one.
    if not dataset_key and path_hint:
        dataset_key = _key_for_path(path_hint)
    if not ref and path_hint:
        ref = f"path:{Path(path_hint).name}"

    # The version bound: a config lacking every field 0.20.0 has was written
    # by something older. If the fields ARE present we still do not know the
    # version, only that it is recent — so no bound is claimed.
    first = configs[0]
    bound = None
    if not any(key in first for key in _POST_0_20_JOB_FIELDS):
        bound = "<0.20.0"

    from . import __version__

    return Provenance(
        dataset_key=dataset_key or "",
        dataset_ref=ref,
        dataset_digest=digest,
        harbor_version=None,
        harbor_bound=bound,
        harbor_bin="",
        arenabench_version=__version__,
        source=SOURCE_BACKFILLED,
    )
