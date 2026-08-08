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
import shutil
import tempfile
from pathlib import Path

from .sut import arenabench_home

__all__ = [
    "PACKAGE_NAME",
    "AdapterUnavailableError",
    "adapter_root",
    "stage_adapter",
]

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
