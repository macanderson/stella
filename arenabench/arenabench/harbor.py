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
"""

from __future__ import annotations

import functools
import os
import re
import shutil
import subprocess

__all__ = [
    "HARBOR_ENV",
    "HarborTooOldError",
    "HarborUnavailableError",
    "dataset_harbor_env",
    "harbor_bin",
    "harbor_version",
    "parse_version",
    "require_for_dataset",
    "supports_agent_import_path",
]

#: Points ArenaBench at one specific Harbor, rather than whatever is on PATH.
HARBOR_ENV = "ARENABENCH_HARBOR"


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


def harbor_bin(dataset_key: str | None = None) -> str:
    """The Harbor to run, honouring the per-dataset override first.

    Resolution order: ``ARENABENCH_HARBOR_<DATASET>`` (when ``dataset_key`` is
    given), then the global :data:`HARBOR_ENV`, then PATH. Raises
    :class:`HarborUnavailableError` with the ways to fix it, rather than
    letting a missing binary surface as a subprocess error deep in a launch.
    """
    if dataset_key:
        name = dataset_harbor_env(dataset_key)
        override = os.environ.get(name)
        if override:
            if not os.path.isfile(override) or not os.access(override, os.X_OK):
                raise HarborUnavailableError(
                    f"{name}={override} is not an executable file. Point it at "
                    "a harbor binary (e.g. .venv/bin/harbor), or unset it to "
                    "fall back to the global resolution."
                )
            return override
    override = os.environ.get(HARBOR_ENV)
    if override:
        if not os.path.isfile(override) or not os.access(override, os.X_OK):
            raise HarborUnavailableError(
                f"{HARBOR_ENV}={override} is not an executable file. Point it at a "
                "harbor binary (e.g. .venv/bin/harbor), or unset it to use PATH."
            )
        return override
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
        f"{dataset_title} needs harbor >= {minimum}, but {resolved} is "
        f"{installed}. An older Harbor does not merely fail here — it drops "
        f"task settings it does not recognise and grades against the wrong "
        f"container topology, so the run would finish and report a wrong "
        f"score. Install a newer Harbor, or set {HARBOR_ENV} (or the "
        f"per-dataset override this module documents) to a virtualenv that "
        f"has one (e.g. `uv pip install 'harbor[modal]=={minimum}'`)."
    )
