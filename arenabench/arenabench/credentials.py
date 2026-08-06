# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""A saved credential set, so a seat's ``.env`` does not need re-pasting.

Per-seat credentials are deliberately file-free everywhere else in ArenaBench
— see :mod:`.config`'s "no secrets, ever" and :class:`.model.Contestant`'s own
docstring. This module is the one deliberate exception, for the same reason
:mod:`.pricing` keeps a price table at ``~/.arenabench``: an operator running
matches from this machine, over and over, against the same handful of
providers, ends up retyping or re-pasting the same keys every single time.

``credentials.env`` holds them once, at the location ArenaBench already treats
as "this machine's own state" — the same ``~/.arenabench`` directory
:func:`~.server.default_workspace` resolves to, following the same
``ARENABENCH_HOME`` override. That is what makes "gitignored" automatic rather
than a rule someone has to remember: the file is never inside a git checkout
in the first place.

**Never a source of truth by itself.** A value here is only a *default* a seat
falls back to; anything actually supplied for this run — a template's
``run``, or a contestant's pasted ``.env`` in the UI — always wins. That
ordering is what lets an operator override one key for one match (testing a
second account, routing around a rate limit) without touching the saved file,
and it is why :func:`apply_saved_credentials` only ever fills gaps, never
overwrites.
"""

from __future__ import annotations

import os
from dataclasses import replace
from pathlib import Path

from .config import required_env
from .model import Contestant, MatchSpec, parse_dotenv, screen_env

__all__ = ["apply_saved_credentials", "credentials_path", "load_credentials"]


def credentials_path() -> Path:
    """Where the saved credential set lives.

    ``ARENABENCH_CREDENTIALS`` names an exact file — useful for tests, or an
    operator who keeps it somewhere else on purpose. Otherwise it sits beside
    the rest of this machine's ArenaBench state, honouring the same
    ``ARENABENCH_HOME`` override :func:`~.server.default_workspace` does, so
    relocating one relocates the other.
    """
    override = os.environ.get("ARENABENCH_CREDENTIALS")
    if override:
        return Path(override).expanduser()
    home = os.environ.get("ARENABENCH_HOME")
    base = Path(home).expanduser() if home else Path.home() / ".arenabench"
    return base / "credentials.env"


def load_credentials() -> dict[str, str]:
    """The saved credential set, or ``{}`` if none has been saved yet.

    Parsed with :func:`.model.parse_dotenv` — the same forgiving format (quotes,
    ``export`` prefixes, multi-line values) as a seat's pasted ``.env`` in the
    UI, so a file copied from one place to the other needs no editing — and
    screened with :func:`.model.screen_env` on the way out, the same trust
    boundary a pasted ``.env`` crosses, so a stray line in a hand-edited file
    cannot hand a subprocess something that was never a credential.
    """
    try:
        text = credentials_path().read_text(encoding="utf-8")
    except OSError:
        return {}
    allowed, _ignored = screen_env(parse_dotenv(text))
    return allowed


def apply_saved_credentials(spec: MatchSpec) -> MatchSpec:
    """Layer the saved credential file under each seat's own ``.env``.

    Only the variable names that seat's provider and agent actually declare
    (:func:`~.config.required_env`) are pulled from the saved file, so a
    contestant never receives a credential for a provider it is not
    configured against — the saved file may hold keys for several providers
    at once, but a seat still only sees its own, the same per-seat isolation
    :class:`~.model.Contestant` promises for a pasted ``.env``. Whatever the
    seat's own environment already set wins.
    """
    saved = load_credentials()
    if not saved:
        return spec
    needed = required_env(spec)

    def seeded(contestant: Contestant) -> Contestant:
        candidates = needed.get(contestant.id, [])
        fallback = {name: saved[name] for name in candidates if name in saved}
        if not fallback:
            return contestant
        return replace(contestant, env={**fallback, **contestant.env})

    return replace(spec, contestants=tuple(seeded(c) for c in spec.contestants))
