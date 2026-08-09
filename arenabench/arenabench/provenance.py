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
* **each contestant's own product version.** The system under test was always
  pinned — :attr:`Provenance.sut_commit` names the Stella that ran — but the
  *opponent* was not, and an opponent is a moving target too. Claude Code ships
  most days; it gets better and worse exactly like we do, and a scoreboard that
  labels our commit while leaving theirs blank is measuring one variable and
  reporting two.

So every match writes a :class:`Provenance` beside its jobs, and the tuple
``(dataset_digest, harbor_version, sut, agent products)`` is its
:attr:`~Provenance.comparability_key`. Two matches with the same key may be
compared; two with different keys may be *shown together*, but only labelled,
never summed.

# Why the opponent's version is observed rather than resolved

Stella's identity is resolved before launch: ArenaBench builds the binary, so
it knows the commit and can hash the artifact. Every other contestant installs
itself **inside the task container**, per trial, from its vendor's installer —
Harbor's Claude Code agent runs ``curl -fsSL https://claude.ai/install.sh``
with no version pin unless one is given. The host therefore cannot know the
opponent's version at launch, and only learns it from what the trials left
behind: Harbor's ATIF trajectory records ``agent.version`` for every agent that
supports the format.

That makes agent versions a **second write**. :func:`write_match` records the
apparatus before the first trial, because a match killed mid-run must still be
attributable; :func:`stamp_agent_versions` adds what only the finished trials
could say. Both are best-effort, and a missing stamp reads as unknown.

This is not hypothetical drift. At the time this was written the local archive
held Claude Code arms spanning **eight** product versions (2.1.220 through
2.1.226) with nothing recording any of them, and match ``f23fbcd61adc`` ran
*two* versions inside a single arm — a release landed between two trials of one
job, so half its tasks were graded against a product the other half never saw.
That match's solve rate is a blend of two agents, and until :attr:`mixed_version_seats`
existed nothing on the scoreboard could say so.

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
    "AGENT_SOURCE_OBSERVED",
    "AGENT_SOURCE_PINNED",
    "PROVENANCE_FILE",
    "Provenance",
    "backfill_match",
    "observe_agent_versions",
    "read_match",
    "stamp_agent_versions",
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

#: How a seat's product version was learned. ``observed`` means it was read
#: back from what the trials wrote, which is the only channel available for an
#: agent that installs itself in the container. ``pinned`` means the operator
#: named the version and the installer was told to fetch exactly it — the
#: stronger fact, because it holds for trials that have not run yet.
AGENT_SOURCE_OBSERVED = "observed"
AGENT_SOURCE_PINNED = "pinned"

#: Where an agent that speaks ATIF records the product that ran.
_TRAJECTORY_NAME = "agent/trajectory.json"


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
    #: binary nobody can attribute, and saying so is the honest record. When
    #: seats pin *different* commits (:attr:`sut_seats`), no single value is
    #: true and this stays empty — per-seat truth lives in ``sut_seats``.
    sut_commit: str = ""
    #: SHA-256 of the exact binary that ran. Two builds of one commit on
    #: different toolchains are not the same artifact, and this is what tells
    #: them apart after the fact. Like :attr:`sut_commit`, empty when the
    #: seats diverge.
    sut_sha256: str = ""
    #: Per-seat SUT identity, keyed by contestant id: ``{"name", "ref",
    #: "commit", "sha256"}`` for every Stella seat. This is what makes a
    #: champion-vs-challenger match recordable at all — the single fields
    #: above assume one SUT per match, which was defect 3 of #2082. Empty for
    #: records written before per-seat pins existed and for matches with no
    #: Stella seat; readers fall back to the single fields then.
    sut_seats: dict[str, dict[str, str]] = field(default_factory=dict)
    #: Per-seat **product** identity for contestants ArenaBench does not build,
    #: keyed by contestant id: ``{"agent", "name", "versions", "source",
    #: "trials", "unversioned"}``. ``versions`` is a sorted list because one
    #: seat can genuinely run more than one — an agent that installs itself per
    #: trial picks up whatever its vendor published that hour — and collapsing
    #: that to a single value would erase the very fact worth recording.
    #:
    #: Stella seats are deliberately absent: their identity is the SUT pin in
    #: :attr:`sut_seats`, which is finer-grained (a commit and a binary hash)
    #: than any version string, and recording it twice invites the two copies
    #: to disagree.
    agent_seats: dict[str, dict[str, Any]] = field(default_factory=dict)
    arenabench_version: str = ""
    recorded_at: float = field(default_factory=time.time)
    source: str = SOURCE_RECORDED

    def _key(self, sut: str) -> str:
        digest = (self.dataset_digest or "unknown").removeprefix("sha256:")[:8]
        if self.harbor_version:
            grader = f"harbor{self.harbor_version}"
        elif self.harbor_bound:
            grader = f"harbor?{self.harbor_bound}"
        else:
            grader = "harbor?"
        return f"{self.dataset_key or 'unknown'}@{digest}/{grader}/{sut}"

    @staticmethod
    def _product_label(seat: dict[str, Any]) -> str:
        """One seat's product as a key component, e.g. ``claude-code2.1.226``.

        A seat that ran several versions names them all, joined by ``+``, for
        the same reason :attr:`comparability_key` names several SUT commits: a
        blend is its own apparatus and must never collapse into the key of
        either half. A seat with no observed version gets a trailing ``?`` —
        unknown, and visibly so.

        A seat whose **pin was violated** is labelled by what ran, never by
        what was asked for. The pin is the intent and the observation is the
        apparatus; a key that reported the intent would group a match with
        others that genuinely ran that release, which is the precise error the
        pin was added to prevent.
        """
        agent = str(seat.get("agent") or "agent")
        source = seat.get("versions") or []
        if seat.get("pin_violated"):
            source = seat.get("observed") or seat["pin_violated"]
        versions = [str(v) for v in source if v]
        if not versions:
            return f"{agent}?"
        label = f"{agent}{'+'.join(sorted(versions))}"
        # A seat that also produced trials with no version at all is only
        # partly known, and saying "2.1.226" flat would overclaim.
        return f"{label}+?" if seat.get("unversioned") else label

    @property
    def agent_products(self) -> tuple[str, ...]:
        """Every non-Stella seat's product label, sorted by contestant id."""
        return tuple(
            self._product_label(self.agent_seats[seat_id])
            for seat_id in sorted(self.agent_seats)
        )

    @property
    def mixed_version_seats(self) -> tuple[str, ...]:
        """Contestant ids whose arm ran more than one product version.

        Such an arm's numbers are a blend, and no single version label is true
        of it. Surfaced as its own property rather than left implicit in the
        key because the key is a grouping token an operator compares, while
        this is a warning an operator has to *read* — the failure it catches
        looks exactly like a normal result until someone asks which product
        produced it.
        """
        return tuple(
            seat_id
            for seat_id in sorted(self.agent_seats)
            if len(self._observed_versions(self.agent_seats[seat_id])) > 1
        )

    @property
    def violated_pins(self) -> dict[str, list[str]]:
        """Seats told to run one release that ran another, and what they ran.

        Empty is the normal case, including for every unpinned seat — a seat
        with no pin cannot violate one. Non-empty means a match believed it was
        holding the opponent fixed and was not, which is worse than never
        pinning, because the belief is what makes the comparison quotable.
        """
        return {
            seat_id: list(seat["pin_violated"])
            for seat_id, seat in sorted(self.agent_seats.items())
            if seat.get("pin_violated")
        }

    @staticmethod
    def _observed_versions(seat: dict[str, Any]) -> set[str]:
        """What a seat actually ran, preferring evidence over intent."""
        source = seat.get("observed") or seat.get("versions") or []
        return {str(v) for v in source if v}

    @property
    def sut_commits(self) -> tuple[str, ...]:
        """Distinct per-seat SUT commits, sorted; the legacy single field when
        no per-seat record exists. ``""`` members mean a seat whose commit is
        genuinely unknown — kept, never dropped, because unknown is not zero.
        """
        if self.sut_seats:
            return tuple(
                sorted({seat.get("commit") or "" for seat in self.sut_seats.values()})
            )
        return (self.sut_commit,)

    @property
    def comparability_key(self) -> str:
        """What must match for two result sets to be comparable.

        Short and human-readable on purpose: it is printed next to results, and
        a key nobody reads is a key nobody notices differing. An unknown Harbor
        keeps its bound in the key rather than collapsing to the same string as
        a measured one — the whole point is that those two are not the same.

        The SUT belongs in the key for the same reason the dataset digest
        does: two runs of different Stella revisions answer different
        questions, and a key that hides that invites them into one average. A
        match whose seats pin *different* commits names them all — a two-twin
        match is its own apparatus, never averageable into a single-SUT
        series; :meth:`comparability_key_for` is the arm-level key.
        """
        commits = self.sut_commits
        if len(commits) == 1:
            sut = f"stella{commits[0][:8]}" if commits[0] else "stella?"
        else:
            sut = "stella" + "+".join(c[:8] if c else "?" for c in commits)
        key = self._key(sut)
        # Appended only when something is known, so every key written before
        # agent versions were recorded keeps its exact spelling and the whole
        # archive stays groupable against itself. A match that DID record the
        # opponent is a different apparatus from one that could not, and the
        # longer key says so rather than pretending they are the same.
        products = self.agent_products
        return f"{key}/{'+'.join(products)}" if products else key

    def comparability_key_for(self, contestant_id: str) -> str:
        """The key for one arm of the match.

        Two arms — from the same match or different ones — are comparable
        exactly when their keys differ in nothing but the product half, which
        is the variable under test. Everything else differing (dataset digest,
        grader) makes them different measurements, and this key is what refuses
        to let that difference hide (#2082).

        Which product that is depends on the seat. A Stella seat is identified
        by the commit ArenaBench built; every other seat by the version its
        vendor shipped. Before agent versions were recorded this returned
        ``stella?`` for a Claude Code arm — a key that names the wrong product
        entirely, so two Claude Code arms four releases apart grouped as one
        series and their difference read as noise.
        """
        agent_seat = self.agent_seats.get(contestant_id)
        if agent_seat is not None:
            return self._key(self._product_label(agent_seat))
        seat = self.sut_seats.get(contestant_id)
        commit = (seat.get("commit") or "") if seat else self.sut_commit
        return self._key(f"stella{commit[:8]}" if commit else "stella?")

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
        seats = {*self.sut_seats, *self.agent_seats}
        if seats:
            out["comparability_key_by_seat"] = {
                seat_id: self.comparability_key_for(seat_id)
                for seat_id in sorted(seats)
            }
        if self.agent_seats:
            out["agent_products"] = list(self.agent_products)
            out["mixed_version_seats"] = list(self.mixed_version_seats)
            out["violated_pins"] = self.violated_pins
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


def _seat_of(job_dir: Path) -> dict[str, Any] | None:
    """The launch record beside a job directory, or ``None``.

    Reuses the runner's existing seat manifest (``<job>.seat.json``) rather
    than parsing the job's name. The name is ``<match id>-<contestant slug>``
    and a slug may itself contain a dash, so splitting it is ambiguous exactly
    where seats are most likely to be named descriptively; the manifest states
    the contestant id outright.
    """
    path = job_dir.with_name(job_dir.name + ".seat.json")
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return loaded if isinstance(loaded, dict) else None


def observe_agent_versions(match_dir: Path) -> dict[str, dict[str, Any]]:
    """Which product each non-Stella seat actually ran, read off its trials.

    ATIF is the channel: every Harbor agent that opts into the format writes
    ``agent.version`` into ``agent/trajectory.json``, and for a self-installing
    CLI that is the only statement of the version that exists anywhere on the
    host. Trials are counted as well as versioned, so ``unversioned`` can say
    how much of the arm the answer covers — a seat whose version is known for
    two of eight trials is not the same fact as one known for all eight, and
    :meth:`Provenance._product_label` marks the difference.

    Stella seats are skipped: :attr:`Provenance.sut_seats` already identifies
    them by commit, and a version string derived from the same build would be
    a second copy of a fact that is better recorded once.

    Best-effort throughout. A trial with no trajectory, a torn JSON file, a job
    with no seat manifest — each subtracts from what is known and none of them
    raises, because a benchmark that refuses to report because one artifact is
    unreadable is worse than one that reports what it has and says so.
    """
    seats: dict[str, dict[str, Any]] = {}
    for job_dir in sorted(match_dir.glob("jobs/*")):
        if not job_dir.is_dir():
            continue
        record = _seat_of(job_dir)
        agent = str((record or {}).get("agent") or "")
        if agent == "stella":
            continue
        seat_id = str((record or {}).get("contestant") or "") or job_dir.name
        name = str((record or {}).get("name") or "")

        versions: set[str] = set()
        trials = 0
        unversioned = 0
        for trajectory_path in sorted(job_dir.glob(f"*/{_TRAJECTORY_NAME}")):
            payload = _load_trajectory(trajectory_path)
            if payload is None:
                continue
            trials += 1
            version = str(payload.get("version") or "")
            if version and version.lower() != "unknown":
                versions.add(version)
            else:
                unversioned += 1
            # An archive with no seat manifest still knows what ran, because
            # ATIF names the agent. Trusted only as a fallback: the manifest is
            # what the launcher *asked for*, which outranks what the artifact
            # reports about itself.
            agent = agent or str(payload.get("name") or "")

        if not trials:
            continue
        # The Stella skip has to be re-tested here, not only against the
        # manifest. Every archive predating the seat manifest has no manifest
        # to skip on, so the check above passes an empty `agent` for both arms
        # and a Stella seat would be recorded as a product version — competing
        # with the SUT pin that already identifies it, in a spelling
        # (`stella 0.6.76 [binary-sha256:…]`) that no key should ever carry.
        if agent == "stella":
            continue
        seats[seat_id] = {
            "agent": agent or "agent",
            "name": name,
            "versions": sorted(versions),
            "source": AGENT_SOURCE_OBSERVED,
            "trials": trials,
            "unversioned": unversioned,
        }
    return seats


def _load_trajectory(path: Path) -> dict[str, Any] | None:
    """The ``agent`` block of an ATIF trajectory, or ``None``."""
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(loaded, dict):
        return None
    agent = loaded.get("agent")
    return agent if isinstance(agent, dict) else None


def stamp_agent_versions(match_dir: Path) -> Provenance | None:
    """Fold observed agent versions into a match's record, in place.

    The second of provenance's two writes. :func:`write_match` runs before the
    first trial so a killed match is still attributable; this runs after the
    trials, because an agent that installs itself in the container cannot be
    interrogated any earlier. Idempotent — re-reading finished trials yields
    the same answer, so a match may be stamped again after a partial run
    completes.

    Returns the updated record, or ``None`` when there is no record to update
    or nothing new to say. Never *downgrades*: a re-stamp against a
    half-deleted archive must not turn a known apparatus into an unknown one.

    A **pinned** seat keeps its pin but is still checked against what ran. A
    pin is an instruction to an installer, not evidence — the installer can
    fall back, resolve a range, or fail to a cached build — and a pin believed
    without checking is worth less than no pin at all, because it is believed.
    A disagreement is recorded on the seat as ``pin_violated`` rather than
    quietly resolved in either direction.
    """
    record = read_match(match_dir)
    if record is None:
        return None
    observed = observe_agent_versions(match_dir)
    if not observed:
        return None
    changed = False
    for seat_id, seat in observed.items():
        existing = record.agent_seats.get(seat_id)
        if existing and existing.get("source") == AGENT_SOURCE_PINNED:
            pinned = {str(v) for v in (existing.get("versions") or []) if v}
            actually = {str(v) for v in (seat.get("versions") or []) if v}
            violated = sorted(actually - pinned)
            if violated and existing.get("pin_violated") != violated:
                existing["pin_violated"] = violated
                existing["observed"] = sorted(actually)
                changed = True
            continue
        if existing == seat:
            continue
        record.agent_seats[seat_id] = seat
        changed = True
    if not changed:
        return record
    write_match(match_dir, record)
    return record


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
        # Unlike the Harbor version, the opponent's product IS recoverable from
        # history: ATIF has carried `agent.version` all along and nothing read
        # it. So a backfill recovers the one half of the apparatus that was
        # always on disk, and the whole existing archive becomes groupable by
        # the product that produced it rather than by our commit alone.
        agent_seats=observe_agent_versions(match_dir),
        arenabench_version=__version__,
        source=SOURCE_BACKFILLED,
    )
