"""Credential-absence inspection, and the refusal it raises.

Two jobs that are the same job seen from either side. Before a benchmark
credential is handed to a container over an anonymous pipe, the adapter must
establish that the *same* material is not already reachable the ordinary way —
in Docker's host environment, or in any project container's decoded ``Config``.
When it is, the run must stop, and the stop must be actionable.

Both halves obey one rule: **names, never values.** The guard exists because
this material must not be rendered anywhere, and an error message an operator
pastes into a terminal is one of the places it must not be rendered — so the
functions here return variable names and match counts, and the refusal text
names the carrying variables without ever disclosing which credential matched.

Almost all of it is pure — dictionaries and decoded JSON in, names out. The one
exception is :func:`captured_process`, which is here because *not echoing what
it captured* is the same rule the rest of the module enforces. The Compose
plumbing that decides *which* command to run stays in :mod:`stella_harbor`,
because it encodes that module's credential-scrubbing rules and importing it
back here would be a cycle — the same split :mod:`stella_harbor.exit_cause`
already uses.
"""

from __future__ import annotations

import asyncio
from collections.abc import Collection, Iterable, Mapping, Sequence
from typing import Any


async def captured_process(argv: list[str], env: dict[str, str]) -> tuple[int, bytes]:
    """Run one host inspection command without ever echoing captured output."""
    process = await asyncio.create_subprocess_exec(
        *argv,
        env=env,
        stdin=asyncio.subprocess.DEVNULL,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    stdout, _ = await process.communicate()
    return process.returncode or 0, stdout


def credential_carrying_names(
    host_env: Mapping[str, str], credentials: Iterable[str]
) -> tuple[str, ...]:
    """Variable *names* in ``host_env`` whose value carries a live credential.

    Names only, never values, and never which credential matched: the point of
    the guard is that this material must not be rendered anywhere, and an error
    message is one of the places it must not be rendered.
    """
    secrets = tuple(credentials)
    return tuple(
        sorted(
            name
            for name, value in host_env.items()
            if any(secret in value for secret in secrets)
        )
    )


def credential_leak_message(carriers: Sequence[str]) -> str:
    """Name the offending variables and the fix, not just the symptom.

    This refusal is correct and load-bearing — it is the check that stops a
    provider key reaching a task container's ``/proc/<pid>/environ`` — but for
    as long as it said only *that* a credential remained, it did not say
    *which variable* held it, and every occurrence cost an operator a manual
    hunt through the host environment.

    The failure it actually catches is an **alias under a name nothing
    recognises**: ``VOYAGE_AI_KEY`` beside ``VOYAGE_API_KEY`` holds the same
    secret, but ends ``_AI_KEY``, so ``_is_credential_env_name`` does not
    strip it and the arena's ``_SCRUBBED_PREFIXES`` do not scrub it. The guard
    fires, correctly, on a variable the operator has to be told the name of.
    """
    names = ", ".join(carriers)
    return (
        "selected provider credential remains in Docker's host environment, "
        f"carried by: {names}. These variables hold a live credential under a "
        "name the credential-name policy does not recognise (an alias such as "
        "VOYAGE_AI_KEY beside VOYAGE_API_KEY is the usual cause). Unset them "
        "before launching, or rename them to the canonical *_API_KEY spelling "
        "so they are stripped from the Compose host environment."
    )


def contains_credential(value: Any, credential: str) -> bool:
    """Search a decoded Docker Config without rendering matching material."""
    if isinstance(value, str):
        return credential in value
    if isinstance(value, dict):
        return any(
            contains_credential(key, credential)
            or contains_credential(item, credential)
            for key, item in value.items()
        )
    if isinstance(value, list):
        return any(contains_credential(item, credential) for item in value)
    return False


def main_container_config_env_names(config: dict[str, Any]) -> set[str]:
    """Parse Docker Config.Env without retaining or reporting its values."""
    raw_env = config.get("Env")
    if raw_env is None:
        return set()
    if not isinstance(raw_env, list):
        raise RuntimeError("Docker main container Config.Env is not a list")
    names: set[str] = set()
    for item in raw_env:
        if not isinstance(item, str) or "=" not in item:
            raise RuntimeError("Docker main container Config.Env is malformed")
        name, _ = item.split("=", 1)
        if not name or name != name.strip():
            raise RuntimeError("Docker main container Config.Env is malformed")
        names.add(name)
    return names


def forbidden_main_container_env_names(
    config: dict[str, Any],
    *,
    env_prefix: str,
    credential_names: Collection[str],
    route_names: Collection[str],
) -> list[str]:
    """Return only forbidden variable names, never their potentially secret values.

    The three name sets are passed in rather than imported: they are
    :mod:`stella_harbor`'s declarations of what this benchmark's containers may
    inherit, and reading them from here would invert the dependency.
    """
    names = main_container_config_env_names(config)
    return sorted(
        name
        for name in names
        if name.startswith(env_prefix)
        or name.upper() in credential_names
        or name.upper() in route_names
    )
