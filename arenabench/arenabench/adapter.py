"""Staged harbor-adapter sources, so a match outlives its launch checkout.

The arena resolved the ``stella_harbor`` package out of the checkout the
server was launched from, and resolved it again at *every* trial's install.
In match ``cc00894779ff`` that checkout — a git worktree — was deleted while
the match was running. The four trials that installed afterwards each died in
agent setup naming a path that no longer existed, 0 steps and 0 tokens
apiece, and the dashboard scored all four as agent losses: ``solve_rate``
read 40% against a true agent record of 4/6 (#2127).

Staging removes the dependency rather than accounting for it. The sources are
copied once, on first use, into ``<arenabench home>/adapter/<digest>/``, and
every later trial in the process reads that copy. Two consequences worth
naming:

* A running server is immune to its own launch tree disappearing.
* A match measures exactly one adapter revision — which is what the
  provenance record already claims, and was not previously guaranteed, since
  an operator editing the adapter mid-match changed what later trials ran.

The digest is content-addressed, so an edited adapter stages a fresh
directory on the next match rather than being shadowed by a stale copy.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
from collections.abc import Callable
from pathlib import Path

from .sut import arenabench_home, stella_repo

__all__ = [
    "ADAPTER_ENV",
    "PACKAGE_NAME",
    "AdapterUnavailableError",
    "adapter_root",
    "adapter_source",
    "harbor_interpreter",
    "seat_import_roots",
    "stage_adapter",
    "stella_seat_problem",
    "stella_seat_problem_for",
]

#: Points the arena at the harbor adapter's sources.
ADAPTER_ENV = "ARENABENCH_STELLA_ADAPTER"

#: The package a Stella seat must be able to import. The adapter directory on
#: ``PYTHONPATH`` is the one that *contains* it.
PACKAGE_NAME = "stella_harbor"


class AdapterUnavailableError(RuntimeError):
    """The harbor adapter's sources cannot be found or staged.

    Named rather than a bare :class:`RuntimeError` so
    :data:`arenabench.telemetry.INFRASTRUCTURE_FAILURES` can recognise it: a
    trial that never reached a state where the agent could act is an
    operational abort, and must stay out of ``solve_rate``'s denominator.
    """


def adapter_root() -> Path:
    """Where staged adapter sources live, honouring ``ARENABENCH_HOME``."""
    return arenabench_home() / "adapter"


#: Staged roots already materialised by this process, keyed by resolved
#: source path **and** the arena home it was staged under. Process-scoped on
#: purpose: the point is that trial *n* does not re-read a source tree trial 1
#: already copied, so the copy has to outlive the source within one server run.
#:
#: The home is part of the key because it is read from the environment, and a
#: cache keyed on the source alone hands back a path under whichever home
#: happened to be set first — which in the test suite meant staging into the
#: developer's real ``~/.arenabench`` and then reusing it from a tmp-home test.
_staged: dict[tuple[Path, Path], Path] = {}


def _digest(source: Path) -> str:
    """A content digest over the adapter package.

    Every file under the package is folded in — not just ``*.py`` — because
    the adapter ships data beside its code, and a digest that ignored those
    would reuse a stale staging directory after they changed.
    """
    package = source / PACKAGE_NAME
    files = sorted(p for p in package.rglob("*") if p.is_file())
    if not files:
        raise AdapterUnavailableError(
            f"no {PACKAGE_NAME} sources under {package}. Point "
            "ARENABENCH_STELLA_ADAPTER at <stella>/bench/harbor_adapter."
        )
    digest = hashlib.sha256()
    for path in files:
        digest.update(str(path.relative_to(package)).encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()[:16]


def stage_adapter(source: Path) -> Path:
    """Stage ``source``'s adapter package and return the root to import from.

    Idempotent per process and per content digest. The returned path is a
    directory containing :data:`PACKAGE_NAME`, suitable for ``PYTHONPATH``.

    Raises :class:`AdapterUnavailableError` when the sources are absent and
    nothing has been staged for this source yet — the honest failure, because
    there is no adapter to run and every later step would misreport why.
    """
    source = source.expanduser().resolve()
    root = adapter_root()
    key = (source, root)
    cached = _staged.get(key)
    if cached is not None and (cached / PACKAGE_NAME).is_dir():
        return cached

    if not (source / PACKAGE_NAME).is_dir():
        raise AdapterUnavailableError(
            f"the harbor adapter is not at {source}: no {PACKAGE_NAME} "
            "directory. If the arena's checkout moved or was deleted mid-run, "
            "restart the server from a live checkout, or point "
            "ARENABENCH_STELLA_ADAPTER at <stella>/bench/harbor_adapter."
        )

    destination = root / _digest(source)
    marker = destination / PACKAGE_NAME
    if not marker.is_dir():
        destination.mkdir(parents=True, exist_ok=True)
        # Copy to a sibling first, then rename: a half-copied tree that a
        # concurrent match started importing would fail in a way that reads
        # like a corrupt adapter rather than a racing one.
        #
        # The sibling name must be unique per attempt, not per digest. The
        # server is a ThreadingHTTPServer, so two matches staging the same
        # digest run this concurrently in one process: with a shared name,
        # the second thread's rmtree deletes the first thread's finished copy
        # and the first thread's rename then publishes the second's
        # *in-flight* partial tree — permanently, since the cache check above
        # never revisits a digest that exists.
        pending = Path(
            tempfile.mkdtemp(dir=destination.parent, prefix=f"{destination.name}.pending-")
        )
        try:
            shutil.copytree(source / PACKAGE_NAME, pending / PACKAGE_NAME)
            try:
                (pending / PACKAGE_NAME).rename(marker)
            except OSError:
                # Another match staged the identical digest first. Its copy is
                # byte-identical by construction *and complete* — a unique
                # staging dir is what makes that second half true — so losing
                # the race is fine.
                if not marker.is_dir():
                    raise
        finally:
            shutil.rmtree(pending, ignore_errors=True)

    _staged[key] = destination
    return destination


def adapter_source() -> Path:
    """Where this rig's ``stella_harbor`` sources live.

    :data:`ADAPTER_ENV` wins; failing that, the adapter ships inside the same
    checkout :func:`~.sut.stella_repo` already found, so an arena running out
    of a clone wires itself rather than depending on an operator remembering
    one variable whose omission used to be invisible.

    Raises :class:`AdapterUnavailableError` when neither answers — the honest
    failure, since there is no adapter to run and every later step would
    misreport why.
    """
    configured = os.environ.get(ADAPTER_ENV)
    if configured:
        return Path(configured)
    repo = stella_repo()
    if repo is not None and (repo / "bench" / "harbor_adapter").is_dir():
        return repo / "bench" / "harbor_adapter"
    raise AdapterUnavailableError(
        "a Stella seat needs the harbor adapter, and neither "
        f"{ADAPTER_ENV} nor a Stella checkout names one. "
        f"Point {ADAPTER_ENV} at <stella>/bench/harbor_adapter."
    )


def seat_import_roots() -> list[str]:
    """``PYTHONPATH`` entries a Stella seat's Harbor subprocess needs.

    Two packages must be importable there: ``arenabench`` (Harbor loads
    ``arenabench.harbor_agent:ArenaStellaAgent`` by import path) and the
    ``stella_harbor`` base it subclasses. The second is *staged* rather than
    read from the source checkout at every trial — the launch worktree was
    deleted mid-match once and took four trials with it, each scored as an
    agent loss (#2127).
    """
    return [
        str(Path(__file__).resolve().parent.parent),
        str(stage_adapter(adapter_source())),
    ]


def _probe_roots() -> list[str]:
    """:func:`seat_import_roots`, answered from the source tree.

    A preflight asks whether an interpreter *can* import the seat, and the
    source tree and its staged copy are byte-identical by construction, so
    the answer is the same from either. Reading the source keeps the question
    free: staging copies the whole package to disk and is worth paying for
    once per match, never per health check.
    """
    return [
        str(Path(__file__).resolve().parent.parent),
        str(adapter_source()),
    ]


#: Shebang interpreters that are a wrapper around the real one rather than the
#: real one. Named rather than sniffed for: the question is "did a packager
#: write a re-exec stub here", and that set is small, stable and knowable.
_SHELLS = frozenset({"sh", "bash", "dash", "zsh", "ksh"})


def harbor_interpreter(binary: str) -> str:
    """The Python that will execute ``binary``.

    Read from the console script's shebang, because that is literally what the
    kernel will exec — not a guess from the path. A wheel-installed ``harbor``
    is a text stub beginning ``#!/…/.venv/bin/python``; the ``/usr/bin/env``
    form is resolved through ``PATH`` the same way exec would. Falls back to
    the interpreter beside the script, then to this process's own, so a probe
    is always possible even against a binary that is not a Python stub.

    **A shell shebang is not an answer to this question.** ``uv`` writes its
    console scripts with a portable wrapper — ``#!/bin/sh`` followed by a line
    that re-execs ``$(dirname $0)/python`` — so the shebang the kernel reads
    names a shell, and the Python that ultimately imports the seat is the one
    beside the script. Returning ``/bin/sh`` is not a wrong guess that degrades
    gracefully: the seat probe then runs ``/bin/sh -c "import
    arenabench.harbor_agent"``, which fails with ``import: command not found``,
    and every Stella seat is refused on a rig that would have run it. That is
    every Frontier-Bench match, because that dataset's ``min_harbor`` forces a
    uv-provisioned Harbor. So a shell shebang falls through to the
    beside-the-script lookup — the case the fallback already existed for and
    could never reach while ``/bin/sh`` counted as an absolute interpreter.
    """
    path = Path(binary)
    try:
        with path.open("rb") as handle:
            first = handle.readline(4096)
    except OSError:
        first = b""
    if first.startswith(b"#!"):
        line = first[2:].strip().decode("utf-8", "replace")
        parts = line.split()
        if parts:
            head = parts[0]
            if Path(head).name in {"env", "env.exe"} and len(parts) > 1:
                resolved = shutil.which(parts[1])
                if resolved:
                    return resolved
            elif Path(head).name in _SHELLS:
                pass  # a uv-style wrapper; the real interpreter sits beside it
            elif Path(head).is_absolute():
                return head
    for name in ("python3", "python"):
        beside = path.parent / name
        if beside.exists():
            return str(beside)
    return sys.executable


#: The exact command a seat's Harbor subprocess runs at agent construction,
#: reduced to its import. Everything the seat needs is reachable from here:
#: ``arenabench.harbor_agent`` imports ``stella_harbor``, which imports
#: ``harbor``.
_SEAT_IMPORT = "import arenabench.harbor_agent"


def _probe(interpreter: str, roots: list[str]) -> tuple[int, str]:
    """Ask ``interpreter`` whether it can build a Stella seat."""
    env = dict(os.environ)
    existing = env.get("PYTHONPATH", "")
    env["PYTHONPATH"] = os.pathsep.join([*roots, existing] if existing else roots)
    try:
        done = subprocess.run(
            [interpreter, "-c", _SEAT_IMPORT],
            capture_output=True,
            text=True,
            timeout=60,
            env=env,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        return 1, str(exc)
    return done.returncode, (done.stderr or done.stdout or "").strip()


#: Probe answers already paid for, keyed by interpreter and import roots. The
#: question is a property of an installation, and `serve()` plus every
#: `create_match` would otherwise each spend a subprocess on it.
_verdicts: dict[tuple[str, tuple[str, ...]], str | None] = {}


def stella_seat_problem(
    harbor_binary: str | None = None,
    *,
    probe: Callable[[str, list[str]], tuple[int, str]] | None = None,
) -> str | None:
    """Why a Stella seat launched from this rig would measure nothing.

    ``None`` means "go". A string is a refusal, phrased for an operator who
    now has to do something about it — the shape :func:`~.sut.sut_problem_for`
    already established for the SUT.

    **The interpreter asked is the one that will run the seat**, not this
    process's. Harbor is launched as a console script, so the Python behind
    *that* is what must import ``arenabench.harbor_agent`` — and through it
    ``stella_harbor``, and through that ``harbor`` itself. Asking
    ``sys.executable`` instead would be a proxy that is wrong in both
    directions: an arena served by an interpreter with no ``harbor`` can still
    launch seats perfectly well through a Harbor from another virtualenv.

    Without this, an arena would accept a Stella seat it could not run and the
    seat would report *done* having measured nothing: Harbor's exit status on
    that path is 0 (#2325). The named refusal in :func:`._unavailable_base`
    fixed the message; it did not stop the match from being accepted, and a
    refusal at trial time has already spent the container.

    ``harbor_binary`` should be the Harbor this match resolved when the caller
    knows it — datasets may pin their own, so a global lookup can answer about
    a different installation than the one that will grade. A caller that has
    no resolution yet may omit it; if Harbor cannot be found at all this
    returns ``None`` rather than a refusal, because "no Harbor" is a separate,
    already-reported condition (:meth:`~.runner.MatchRunner.start` carries it
    onto every seat) and answering it here would turn one machine-wide fact
    into a different refusal than the one an operator is used to reading.
    """
    from . import harbor as harbor_module

    try:
        roots = _probe_roots()
    except AdapterUnavailableError as exc:
        return str(exc)
    if harbor_binary is None:
        try:
            harbor_binary = harbor_module.harbor_bin()
        except harbor_module.HarborUnavailableError:
            return None
    interpreter = harbor_interpreter(harbor_binary)

    key = (interpreter, tuple(roots))
    if probe is None and key in _verdicts:
        return _verdicts[key]
    code, detail = (probe or _probe)(interpreter, roots)
    verdict: str | None = None
    if code != 0:
        tail = detail.strip().splitlines()
        verdict = (
            f"{interpreter} cannot build a Stella seat: "
            f"`{_SEAT_IMPORT}` failed with {tail[-1] if tail else 'no output'}. "
            "That interpreter is the one Harbor runs, so every Stella trial "
            "would die in agent setup. Install the adapter's dependencies into "
            "it — `uv sync --project <stella>/bench/harbor_adapter --locked "
            "--extra dev` — and point ARENABENCH_HARBOR at "
            "<stella>/bench/harbor_adapter/.venv/bin/harbor."
        )
    if probe is None:
        _verdicts[key] = verdict
    return verdict


def stella_seat_problem_for(spec: object, harbor_binary: str | None = None) -> str | None:
    """Why this match must not launch, as far as the Stella adapter is concerned.

    ``None`` means "go". Checked only when a Stella seat is present: a
    Claude-Code-vs-Codex contest loads none of our adapter and must not be
    blocked by the state of an installation it never touches — the same
    per-agent scoping :func:`~.sut.sut_problem_for` uses.
    """
    contestants = getattr(spec, "contestants", ())
    if not any(getattr(c, "agent", "") == "stella" for c in contestants):
        return None
    return stella_seat_problem(harbor_binary)
