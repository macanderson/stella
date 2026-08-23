# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Which ``harbor`` runs a match, and whether it is new enough for the dataset.

ArenaBench shells out to Harbor for every trial, so Harbor's version is part
of the measurement apparatus rather than an installation detail. Two facts
make that worth a module of its own.

**A too-old Harbor mis-grades silently.** Frontier-Bench's tasks declare
``environment_mode = "separate"`` under ``[verifier]``. Harbor 0.6.1's config
model has no such field, and pydantic's default ``extra="ignore"`` drops it
without a word: the run completes, produces rewards from the wrong container
topology, and looks exactly like a real result. There is no error to notice
and no number that looks wrong. A benchmark tool that can report a confidently
wrong score is worse than one that refuses to run, so a dataset declares the
Harbor floor it needs (:attr:`~arenabench.registry.Dataset.min_harbor`) and
:func:`require_for_dataset` refuses below it.

**The CLI moved between versions, so we detect rather than assume.**
``--agent-import-path`` (0.6.1) folded into ``--agent`` (0.20.0), which takes
a built-in name *or* an import path. Exactly which release folded it is not
something this file should guess, so :func:`supports_agent_import_path` asks
the installed binary by reading its own ``--help``. That help is rendered with
Rich, which leaves ANSI escapes **inside** flag names when it wraps, so the
text is scrubbed before it is searched — an unscrubbed grep finds nothing and
fails closed against a Harbor that is perfectly fine.

``ARENABENCH_HARBOR`` points at a specific binary. That is how one machine
runs a 0.6.1 lane and a 0.20.0 lane without either pretending to be the other
— the same per-lane pinning ``bench/evidence/frontier`` already does.

**One machine, one server, two Harbors.** The Terminal-Bench rig keeps 0.6.1
as its audited constant while Frontier-Bench refuses anything below 0.20.0,
and a single global override cannot serve both from one running server. So the
override is *per dataset* first: ``ARENABENCH_HARBOR_FRONTIER_BENCH`` (the
dataset key, uppercased, with every non-alphanumeric squashed to ``_``) names
the Harbor that runs that dataset's matches, falling back to the global
``ARENABENCH_HARBOR``, falling back to PATH. Every version- and capability-
probe here takes the *resolved binary*, not a fresh global lookup, so the
Harbor that is asked "do you still have ``--agent-import-path``?" is the one
that will actually be launched.

**A floor is a thing to satisfy, not only a thing to check.** Refusing to run
is the right answer when the only Harbor on the machine would mis-grade. It
was also, for a while, the answer when a perfectly good one sat one directory
away — the Frontier lane's own virtualenv — because resolution had exactly one
implicit source, ``PATH``, and PATH held an unrelated 0.6.1. A dataset that
declares a floor therefore goes through :func:`resolve_for_dataset`, which
looks for a Harbor that satisfies it and, finding none, installs one. The
refusal survives underneath as the last resort it was always meant to be.

Two rules keep that convenience from becoming its own hazard:

* **An explicit pin is never second-guessed.** A binary named by
  ``ARENABENCH_HARBOR`` or a per-dataset override is used, or refused with the
  variable's name — never quietly replaced. Pinning is how a lane declares its
  audited constant, and a search that overrode it would make that declaration
  a lie.
* **A dataset with no floor resolves exactly as it always did**: PATH, and
  nothing else. Without a floor every Harbor grades identically as far as this
  module can tell, so there is nothing to search *for* — and searching anyway
  is how Terminal-Bench's pinned 0.6.1 lane would silently start running some
  newer binary that happened to be lying around.
"""

from __future__ import annotations

import functools
import itertools
import os
import re
import shutil
import subprocess
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path

__all__ = [
    "HARBOR_ENV",
    "MANAGED_HOME_ENV",
    "NO_PROVISION_ENV",
    "HarborTooOldError",
    "HarborUnavailableError",
    "Resolution",
    "dataset_harbor_env",
    "harbor_bin",
    "harbor_version",
    "managed_root",
    "parse_version",
    "provision",
    "require_for_dataset",
    "resolve_for_dataset",
    "supports_agent_import_path",
]

#: Points ArenaBench at one specific Harbor, rather than whatever is on PATH.
HARBOR_ENV = "ARENABENCH_HARBOR"

#: Moves the directory :func:`provision` installs Harbors into.
MANAGED_HOME_ENV = "ARENABENCH_HARBOR_HOME"

#: Set truthy to make a missing Harbor a refusal instead of an installation.
#: A hermetic CI runner wants this; a laptop about to start a match does not.
NO_PROVISION_ENV = "ARENABENCH_HARBOR_NO_PROVISION"

#: How long one ``uv`` invocation gets. Generous because this is a cold
#: package download on a machine that may be running a benchmark at the same
#: time, and the cost of being wrong is a spurious failure at the start of a
#: paid run.
PROVISION_TIMEOUT = 900.0


def dataset_harbor_env(dataset_key: str) -> str:
    """The per-dataset override variable for ``dataset_key``.

    ``frontier-bench`` -> ``ARENABENCH_HARBOR_FRONTIER_BENCH``. Every run of
    non-alphanumerics squashes to one ``_`` so a key with a dot or dash cannot
    produce a name a shell refuses to export.
    """
    suffix = re.sub(r"[^A-Za-z0-9]+", "_", dataset_key).strip("_").upper()
    return f"{HARBOR_ENV}_{suffix}"

#: Strips the SGR escapes Rich leaves inside wrapped `--help` output.
_ANSI = re.compile(r"\x1b\[[0-9;]*m")


class HarborUnavailableError(RuntimeError):
    """No usable ``harbor`` binary was found."""


class HarborTooOldError(RuntimeError):
    """The installed Harbor predates what this dataset needs to grade correctly."""


def _executable(path: str | Path) -> bool:
    return os.path.isfile(path) and os.access(path, os.X_OK)


def _pinned(dataset_key: str | None) -> tuple[str, str] | None:
    """The Harbor an environment variable names, and which variable named it.

    ``None`` when no pin is set, which is the signal that resolution is free
    to look around. A pin that points at nothing is an error rather than a
    fallback: it means someone stated an intent that cannot be honoured, and
    quietly running a different Harbor is precisely what a pin exists to
    prevent.
    """
    names = []
    if dataset_key:
        names.append(dataset_harbor_env(dataset_key))
    names.append(HARBOR_ENV)
    for name in names:
        value = os.environ.get(name)
        if not value:
            continue
        if not _executable(value):
            raise HarborUnavailableError(
                f"{name}={value} is not an executable file. Point it at a "
                "harbor binary (e.g. .venv/bin/harbor), or unset it to fall "
                "back to the resolution this module documents."
            )
        return value, name
    return None


def harbor_bin(dataset_key: str | None = None) -> str:
    """The Harbor to run, honouring the per-dataset override first.

    Resolution order: ``ARENABENCH_HARBOR_<DATASET>`` (when ``dataset_key`` is
    given), then the global :data:`HARBOR_ENV`, then PATH. Raises
    :class:`HarborUnavailableError` with the ways to fix it, rather than
    letting a missing binary surface as a subprocess error deep in a launch.

    This answers "which Harbor, all else being equal". A caller that knows the
    dataset's floor wants :func:`resolve_for_dataset` instead, which can find
    or build one that clears it.
    """
    pin = _pinned(dataset_key)
    if pin is not None:
        return pin[0]
    found = shutil.which("harbor")
    if found is None:
        raise HarborUnavailableError(
            "`harbor` is not on PATH. Install it, or set "
            f"{HARBOR_ENV} to a virtualenv's harbor binary."
        )
    return found


def parse_version(raw: str) -> tuple[int, ...]:
    """``"0.20.0"`` -> ``(0, 20, 0)``.

    Compares numerically so ``0.20.0`` sorts above ``0.6.1`` — the exact
    comparison a string sort gets backwards, and the one this module exists to
    get right. Non-numeric trailing parts (``0.20.0rc1``) are dropped rather
    than guessed at.
    """
    parts: list[int] = []
    for chunk in raw.strip().split("."):
        match = re.match(r"\d+", chunk)
        if not match:
            break
        parts.append(int(match.group()))
    return tuple(parts)


@functools.lru_cache(maxsize=8)
def _version_of(binary: str) -> str:
    try:
        out = subprocess.run(
            [binary, "--version"],
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
    except OSError as exc:
        raise HarborUnavailableError(f"cannot run {binary}: {exc}") from exc
    text = _ANSI.sub("", (out.stdout or out.stderr or "")).strip()
    # Harbor prints a bare version; tolerate a "harbor, version X" shape too.
    match = re.search(r"\d+(?:\.\d+)+", text)
    if not match:
        raise HarborUnavailableError(f"{binary} did not report a version (said {text!r})")
    return match.group()


def harbor_version(binary: str | None = None) -> str:
    """A Harbor's version string, e.g. ``"0.20.0"``.

    ``binary`` defaults to the globally resolved Harbor. A caller that already
    resolved a per-dataset binary must pass it, or the version reported is for
    a Harbor other than the one about to run.
    """
    return _version_of(binary or harbor_bin())


@functools.lru_cache(maxsize=8)
def _run_help(binary: str) -> str:
    try:
        out = subprocess.run(
            [binary, "run", "--help"],
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
            # Rich wraps to the terminal width; a wide one keeps a flag name
            # from being split across lines where no grep would find it.
            env={**os.environ, "COLUMNS": "200", "TERM": "dumb"},
        )
    except OSError as exc:
        raise HarborUnavailableError(f"cannot run {binary}: {exc}") from exc
    return _ANSI.sub("", out.stdout or "")


def supports_agent_import_path(binary: str | None = None) -> bool:
    """Whether this Harbor still has the separate ``--agent-import-path`` flag.

    Asked of the binary rather than inferred from its version, because the
    release that folded the flag into ``--agent`` is not a fact this file
    should invent. ``False`` means "pass the import path to ``--agent``",
    which is what every Harbor new enough to have dropped the flag accepts.
    """
    return "--agent-import-path" in _run_help(binary or harbor_bin())


def require_for_dataset(
    minimum: str | None, dataset_title: str, binary: str | None = None
) -> None:
    """Refuse to launch when Harbor is too old to grade `dataset_title`.

    Refusing is the whole point: the failure this guards against produces a
    complete run with plausible numbers, so letting it proceed with a warning
    would mean publishing a score nobody can tell is wrong. ``binary`` is the
    Harbor actually about to launch; the check is meaningless against any
    other one.
    """
    if not minimum:
        return
    resolved = binary or harbor_bin()
    installed = harbor_version(resolved)
    if parse_version(installed) >= parse_version(minimum):
        return
    raise HarborTooOldError(
        _too_old(dataset_title, minimum, f"{resolved} is {installed}")
        + f" Install a newer Harbor, or set {HARBOR_ENV} (or the per-dataset "
        f"override this module documents) to a virtualenv that has one "
        f"(e.g. `uv pip install 'harbor[modal]=={minimum}'`)."
    )


def _too_old(dataset_title: str, minimum: str, found: str) -> str:
    """The single statement of why an old Harbor is refused, not warned about.

    ``found`` is the only part that differs between a pin that came up short
    and a whole search that did, so the explanation itself — the reason this
    is a refusal — has one copy and cannot drift into two tempers.
    """
    return (
        f"{dataset_title} needs harbor >= {minimum}, but {found}. An older "
        "Harbor does not merely fail here — it drops task settings it does "
        "not recognise and grades against the wrong container topology, so "
        "the run would finish and report a wrong score."
    )


@dataclass(frozen=True)
class Resolution:
    """The Harbor a match is about to launch, and how it came to be chosen.

    ``origin`` exists so the choice reaches the run's notes. Resolution can
    now end at a binary nobody named — one found in a lane's virtualenv, or
    one installed on the spot — and a measurement apparatus that reconfigures
    itself has to say so where the numbers are read, not only in a log.
    """

    binary: str
    version: str
    origin: str

    @property
    def note(self) -> str:
        """The one line a run records about which Harbor graded it."""
        return f"harbor {self.version} ({self.binary}) — {self.origin}"


def managed_root() -> Path:
    """Where ArenaBench keeps the Harbors it installed itself.

    One directory per exact version, because a machine running two datasets
    with two floors needs both at once and a single shared directory is how
    one of them would silently become the other. Overridable with
    :data:`MANAGED_HOME_ENV`, matching what
    :func:`~arenabench.registry.export_root` already offers for datasets.
    """
    override = os.environ.get(MANAGED_HOME_ENV)
    if override:
        return Path(override).expanduser()
    return Path.home() / ".arenabench" / "harbor"


def _version_or_none(binary: str) -> str | None:
    """This binary's version, or ``None`` when it cannot be asked for one.

    Discovery walks speculative paths, and a stale or half-built virtualenv on
    one of them has to drop out of the running rather than abort a search that
    would have found a good Harbor two entries later.

    Through :func:`harbor_version` rather than the cache behind it, so every
    version this module believes comes from the one public accessor.
    """
    try:
        return harbor_version(binary)
    except HarborUnavailableError:
        return None


def _meets(version: str | None, minimum: str | None) -> bool:
    if version is None:
        return False
    return not minimum or parse_version(version) >= parse_version(minimum)


def _repo_root() -> Path | None:
    """This checkout's root, or ``None`` when ArenaBench runs from elsewhere.

    Both starting points earn their place. ArenaBench is normally run straight
    out of a checkout with no install at all (``python -m arenabench`` with the
    project directory as the working directory), which only the working
    directory finds; an installed copy would not be found that way, and
    ``__file__`` is what still points into a tree then.
    """
    for start in (Path(__file__).resolve().parent, Path.cwd()):
        try:
            resolved = start.resolve()
        except OSError:  # a deleted working directory is not fatal here
            continue
        for candidate in (resolved, *resolved.parents):
            if (candidate / "bench").is_dir() and (candidate / "arenabench").is_dir():
                return candidate
    return None


def _discovered() -> Iterator[tuple[str, str]]:
    """Harbors worth trying once PATH's cannot grade the dataset.

    Ordered by how much ArenaBench knows about each: the ones it installed
    itself, newest first, then the per-lane virtualenvs of this checkout —
    ``bench/evidence/frontier/.venv`` being exactly where a correct Harbor was
    already sitting on the machine that motivated this search.
    """
    root = managed_root()
    if root.is_dir():
        versions = sorted(root.iterdir(), key=lambda p: parse_version(p.name), reverse=True)
        for child in versions:
            binary = child / "bin" / "harbor"
            if _executable(binary):
                yield str(binary), f"installed by arenabench in {root}"
    repo = _repo_root()
    if repo is None:
        return
    for pattern in ("bench/*/.venv/bin/harbor", "bench/evidence/*/.venv/bin/harbor"):
        for binary in sorted(repo.glob(pattern)):
            if _executable(binary):
                lane = binary.relative_to(repo).parents[2]
                yield str(binary), f"found in this checkout's {lane} virtualenv"


def _install(uv: str, venv: Path, version: str, timeout: float) -> None:
    """Build `venv` and put exactly ``harbor==version`` in it.

    ``--relocatable`` is required rather than tidy: :func:`provision`
    renames this directory into place when it is finished, and uv's ordinary
    console scripts hard-code the interpreter path they were created at, so a
    moved virtualenv's ``harbor`` would be a file that exists and cannot run.

    Plain ``harbor``, not ``harbor[modal]``: every match this resolves for
    launches with ``--env docker``, and the Modal extra is a large dependency
    tree for a scheduler that would not be used. A lane that wants Modal
    builds its own virtualenv and pins it, which is the path that already
    exists for saying so.
    """
    steps = (
        [uv, "venv", "--relocatable", str(venv)],
        [uv, "pip", "install", "--python", str(venv / "bin" / "python"), f"harbor=={version}"],
    )
    for argv in steps:
        try:
            out = subprocess.run(
                argv, capture_output=True, text=True, timeout=timeout, check=False
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            raise HarborUnavailableError(f"`{' '.join(argv)}` did not finish: {exc}") from exc
        if out.returncode != 0:
            noise = _ANSI.sub("", (out.stderr or out.stdout or "")).strip().splitlines()
            tail = " / ".join(noise[-3:]) if noise else f"exit {out.returncode}"
            raise HarborUnavailableError(f"`{' '.join(argv)}` failed: {tail}")


def provision(minimum: str, *, timeout: float = PROVISION_TIMEOUT) -> str:
    """Install exactly Harbor `minimum` under :func:`managed_root`, and return it.

    The install lands in a staging directory named for this process and is
    renamed into place only once it is complete, so a crash or a cancelled run
    can never leave a half-populated version directory for the next match to
    find, believe, and launch.

    That rename is also what serialises concurrent callers, which is the
    ordinary case here rather than a contrived one — this machine routinely
    has three arena servers up at once. Two processes may both build; exactly
    one rename can win, and the loser takes the winner's copy. It is
    identical by construction, since the version is pinned exactly, and
    replacing a directory another process may already be executing out of
    would be the more dangerous move.
    """
    target = managed_root() / minimum
    binary = target / "bin" / "harbor"

    def ready() -> bool:
        return _executable(binary) and _meets(_version_or_none(str(binary)), minimum)

    if ready():
        return str(binary)

    uv = shutil.which("uv")
    if uv is None:
        raise HarborUnavailableError(
            f"no harbor >= {minimum} was found, and `uv` is not installed so "
            "one cannot be built. Install uv (https://docs.astral.sh/uv/), or "
            f"set {HARBOR_ENV} to a virtualenv that has a new enough harbor."
        )

    target.parent.mkdir(parents=True, exist_ok=True)
    staging = target.with_name(f"{target.name}.staging.{os.getpid()}")
    shutil.rmtree(staging, ignore_errors=True)
    try:
        _install(uv, staging, minimum, timeout)
        try:
            os.rename(staging, target)
        except OSError:
            # Either a peer finished first, or an older build left a directory
            # here that is not usable. Take a good one; replace a bad one.
            _version_of.cache_clear()
            if not ready():
                shutil.rmtree(target, ignore_errors=True)
                os.rename(staging, target)
    finally:
        shutil.rmtree(staging, ignore_errors=True)

    _version_of.cache_clear()
    if not ready():
        raise HarborUnavailableError(
            f"installed harbor into {target}, but it does not report a "
            f"version of at least {minimum}"
        )
    return str(binary)


def _provision_allowed() -> bool:
    return os.environ.get(NO_PROVISION_ENV, "").strip().lower() not in ("1", "true", "yes", "on")


def resolve_for_dataset(
    dataset_key: str | None,
    minimum: str | None,
    dataset_title: str,
    *,
    allow_provision: bool | None = None,
) -> Resolution:
    """The Harbor that will grade this dataset — found, or installed if need be.

    The order is a policy, not a preference:

    1. **A pin wins outright.** Used if it clears the floor, refused by name if
       it does not. Never replaced — see this module's header.
    2. **PATH keeps its job whenever it can do it.** A machine whose ``harbor``
       already grades this dataset resolves exactly as it did before any of
       this existed.
    3. **Otherwise look**, through :func:`_discovered`, for one that clears the
       floor. This is the step that would have turned the failure this function
       was written for into a run: the right Harbor was already installed, one
       directory away, and nothing was looking.
    4. **Otherwise build one**, unless :data:`NO_PROVISION_ENV` says not to.
    5. **Otherwise refuse**, naming every Harbor that was tried and its
       version, because "too old" without "and here is what I looked at" is
       the message that sent someone hunting for a binary they already had.

    Steps 3 and 4 only ever happen when `minimum` is set. Without a floor
    there is nothing to search for, and the search would silently swap the
    binary under a lane whose claim rests on one audited version.
    """
    pin = _pinned(dataset_key)
    if pin is not None:
        binary, name = pin
        version = harbor_version(binary)
        if minimum and not _meets(version, minimum):
            raise HarborTooOldError(
                _too_old(dataset_title, minimum, f"{binary} is {version}")
                + f" That Harbor is pinned by {name}, and a pin is honoured "
                "rather than second-guessed, so this is for you to change: "
                f"point {name} at a newer virtualenv, or unset it to let "
                "arenabench find or install one."
            )
        return Resolution(binary, version, f"pinned by {name}")

    # Deliberately through `harbor_bin` and not a bare `shutil.which`: that is
    # the seam the rest of the codebase and its tests already treat as "the
    # ordinary lookup", and a second copy here would be a second thing to keep
    # true. Without a floor its answer is also the final answer.
    if not minimum:
        binary = harbor_bin(dataset_key)
        return Resolution(binary, harbor_version(binary), "found on PATH")
    try:
        on_path: str | None = harbor_bin(dataset_key)
    except HarborUnavailableError:
        on_path = None

    tried: list[str] = []
    candidates = itertools.chain(
        [(on_path, "found on PATH")] if on_path else [],
        _discovered(),
    )
    for binary, origin in candidates:
        version = _version_or_none(binary)
        if version is None:
            continue
        if _meets(version, minimum):
            return Resolution(binary, version, origin)
        tried.append(f"{binary} is {version}")

    if allow_provision is None:
        allow_provision = _provision_allowed()
    if not allow_provision:
        raise HarborTooOldError(
            _too_old(dataset_title, minimum, _summarise(tried))
            + f" Installing one automatically is off ({NO_PROVISION_ENV}), so "
            f"either unset that or set {HARBOR_ENV} to a virtualenv that has "
            f"harbor {minimum} or newer."
        )
    try:
        binary = provision(minimum)
    except HarborUnavailableError as exc:
        raise HarborTooOldError(
            _too_old(dataset_title, minimum, _summarise(tried))
            + f" Installing one automatically also failed: {exc}"
        ) from exc
    return Resolution(binary, harbor_version(binary), f"installed by arenabench for {dataset_title}")


def _summarise(tried: list[str]) -> str:
    """What the search actually saw, for a refusal that has to be actionable."""
    if not tried:
        return "no harbor was found at all"
    if len(tried) == 1:
        return tried[0]
    return "the only ones found were " + ", ".join(tried)
