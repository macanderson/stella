# SPDX-License-Identifier: AGPL-3.0-only
"""Resolving which Stella binary a trial uploads — and when not to.

Split out of ``__init__`` (which is closed to growth under the repository's
file-size ratchet) when the broken-pin refusal landed. Deliberately
package-import-free so the module can never participate in an import cycle
with the adapter root.

The resolution order is a deliberate gradient from explicit to ambient:
``STELLA_BINARY``, then ``stella`` on ``PATH``, then ``./target/release/stella``
walking up from the current working directory. The ambient tiers exist for
unpinned development runs and stay (#2051 tracks their visibility); what must
never happen is a *pinned* trial reaching them — see :func:`locate_binary`.
"""

from __future__ import annotations

import os
from pathlib import Path

#: The binary's name, both on the host and installed inside the container.
BINARY_NAME = "stella"


def _cached_binary(name: str) -> Path | None:
    """Return the first executable named ``name`` found on ``PATH``, or None."""
    for path in os.environ.get("PATH", "").split(os.pathsep):
        if not path:
            continue
        candidate = Path(path) / name
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
    return None


def locate_binary() -> Path:
    """Find the Stella binary on the host.

    Resolution order: explicit ``STELLA_BINARY`` env var, then ``stella`` on
    ``PATH``, then ``./target/release/stella`` walking up from the current
    working directory. Never imported at module load — only ``install`` calls
    this, so importing the adapter never requires Stella to be present.

    A trial whose environment carries ``ARENABENCH_SUT_COMMIT`` was *pinned*
    to a commit by its launcher, and the launcher always sets ``STELLA_BINARY``
    beside it. Reaching the ``PATH``/cwd tiers with the pin present is
    therefore always a broken pin, and it fails closed: those tiers exist for
    genuinely unpinned development runs, and falling through them once
    uploaded the operator's native macOS arm64 build into every Linux task
    container (#2098).
    """
    pinned = os.environ.get("ARENABENCH_SUT_COMMIT")
    explicit = os.environ.get("STELLA_BINARY")
    if explicit:
        binary = Path(explicit)
        if not binary.is_file():
            raise FileNotFoundError(
                f"STELLA_BINARY={explicit!r} does not point at a file"
            )
        return binary
    if pinned:
        raise FileNotFoundError(
            f"this trial is pinned to Stella commit {pinned[:12]} "
            "(ARENABENCH_SUT_COMMIT) but STELLA_BINARY is not set — a broken "
            "pin fails closed. Refusing to fall back to `stella` on PATH or "
            "a cwd walk: an ambient binary has no relation to the pinned "
            "commit, and that fallback once uploaded a host's native macOS "
            "build into Linux task containers (#2098)."
        )

    on_path = _cached_binary(BINARY_NAME)
    if on_path is not None:
        return on_path

    cwd = Path.cwd()
    while True:
        candidate = cwd / "target" / "release" / BINARY_NAME
        if candidate.is_file():
            return candidate
        if cwd == cwd.parent:  # reached filesystem root
            break
        cwd = cwd.parent

    raise FileNotFoundError(
        f"cannot find the {BINARY_NAME!r} binary. For development, build "
        "with `cargo build --release -p stella-cli` (produces "
        "./target/release/stella) or put it on PATH. For a claim run, use "
        "the README's full-SHA-stamped x86_64 Linux build and set "
        "STELLA_BINARY=/path/to/stella."
    )
