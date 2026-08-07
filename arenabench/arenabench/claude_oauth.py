# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Fill a Claude Code seat from the operator's own local Claude Code login.

A Claude Code seat authenticating on a subscription needs
``CLAUDE_CODE_OAUTH_TOKEN``, and the only durable source of that token is the
Claude Code installation already logged in on this machine — the CLI rotates
its OAuth credential whenever any local session refreshes, and rotation
*revokes* the previous access token. A token pasted into a launcher script or
a saved credential file is therefore stale by construction: it dies mid-match
the moment the operator opens Claude Code somewhere else. Reading the live
credential store at match-creation time is not a convenience, it is the only
version of this seat that survives contact with its own provider.

Portability is the same property seen from another machine: any operator with
Claude Code installed and logged in gets a working seat from the same match
template, because the token comes from *their* login, not from a value someone
else exported.

Two hard rules, both load-bearing:

- **The token is read only for a seat that declared it.** A template saying
  ``required = ["CLAUDE_CODE_OAUTH_TOKEN"]`` is the committed, reviewed
  statement that this seat means to run on the local subscription — the same
  contract :func:`~.credentials.apply_ambient_credentials` honours for
  ambient names (#1777). Seats that declared nothing get nothing, and in
  particular a seat pointed at some *other* provider is never silently
  switched onto the operator's Claude plan by a token it did not ask for.
- **The value never touches disk.** It rides the in-memory spec exactly like
  a pasted ``.env`` value, is redacted everywhere the spec is persisted, and
  is re-read fresh on every match creation, so rotation costs the next match
  nothing.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from dataclasses import replace
from pathlib import Path

from .model import MatchSpec

__all__ = [
    "CLAUDE_OAUTH_ENV",
    "apply_local_claude_login",
    "local_claude_token",
]

#: The variable Claude Code reads a subscription token from.
CLAUDE_OAUTH_ENV = "CLAUDE_CODE_OAUTH_TOKEN"

#: macOS keychain service name Claude Code stores its credentials under.
_KEYCHAIN_SERVICE = "Claude Code-credentials"


def _parse_credentials(raw: str) -> str | None:
    """The live access token out of Claude Code's credential JSON, or ``None``.

    ``expiresAt`` is honoured when present (milliseconds since the epoch):
    handing a seat a token that is already expired would reproduce the exact
    dead-arm launch this module exists to prevent, with one extra step.
    """
    try:
        data = json.loads(raw)
    except ValueError:
        return None
    if not isinstance(data, dict):
        return None
    oauth = data.get("claudeAiOauth")
    if not isinstance(oauth, dict):
        return None
    token = oauth.get("accessToken")
    if not isinstance(token, str) or not token.strip():
        return None
    expires_at = oauth.get("expiresAt")
    if isinstance(expires_at, (int, float)) and expires_at / 1000.0 <= time.time():
        return None
    return token.strip()


def _from_keychain() -> str | None:
    try:
        out = subprocess.run(
            ["security", "find-generic-password", "-s", _KEYCHAIN_SERVICE, "-w"],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if out.returncode != 0:
        return None
    return _parse_credentials(out.stdout.strip())


def _from_credentials_file() -> str | None:
    path = Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude")
    try:
        raw = (path / ".credentials.json").read_text(encoding="utf-8")
    except OSError:
        return None
    return _parse_credentials(raw)


def local_claude_token() -> str | None:
    """The access token of this machine's Claude Code login, or ``None``.

    macOS keeps it in the keychain; every other platform (and a macOS install
    configured for plain files) keeps ``~/.claude/.credentials.json``. Both are
    tried in that order and every failure — no login, locked keychain, expired
    token, malformed store — degrades to ``None`` rather than an error, because
    "no local login" is an ordinary state the caller reports as a missing
    credential, not a crash.
    """
    if sys.platform == "darwin":
        token = _from_keychain()
        if token:
            return token
    return _from_credentials_file()


def apply_local_claude_login(spec: MatchSpec) -> tuple[MatchSpec, list[str]]:
    """Fill declared-but-unset ``CLAUDE_CODE_OAUTH_TOKEN`` seats from the local login.

    The last fill layer, after the ambient environment and the saved
    credential file, and gap-filling like both: a seat that already carries
    any of its declared credentials — pasted, ambient, or saved — is left
    alone. Returns the (possibly unchanged) spec and one note per seat that
    was filled, so the launch record can say where the credential came from
    without ever saying what it was.
    """
    wants = [
        contestant
        for contestant in spec.contestants
        if CLAUDE_OAUTH_ENV in contestant.required_env
        and not any(contestant.env.get(name) for name in contestant.required_env)
    ]
    if not wants:
        return spec, []
    token = local_claude_token()
    if not token:
        return spec, []
    notes: list[str] = []
    filled = []
    for contestant in spec.contestants:
        if contestant in wants:
            filled.append(
                replace(contestant, env={CLAUDE_OAUTH_ENV: token, **contestant.env})
            )
            notes.append(
                f"{contestant.name}: {CLAUDE_OAUTH_ENV} read fresh from this "
                "machine's Claude Code login (rotation-safe; never persisted)"
            )
        else:
            filled.append(contestant)
    return replace(spec, contestants=tuple(filled)), notes
