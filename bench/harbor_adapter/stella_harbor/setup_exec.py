"""The credential-bearing exec for *setup* commands — ``stella init``.

A sibling of the run path's ``_secure_exec_with_credential_fd``, not a
widening of it. The run path hard-requires a pinned ``--model`` and recomputes
the engine posture at the process boundary; both are required there and
neither has any meaning for ``stella init``, so relaxing them to fit a setup
command would weaken a guard that is doing real work on the path it was
written for. What this shares instead is every property that is about *the
credential*: direct stella argv, no credential name rendered into ``docker
compose exec -e`` (Harbor puts those in the host process table), values
validated against the empty/newline framing attack, the host environment
checked for leaked material, and the wire buffer zeroed in a ``finally``.

Why setup needs a credential at all: ``stella init`` is where the semantic
index is *built*, and ``stella-embed`` reads ``VOYAGE_API_KEY`` ambiently
(``EmbedderEnv::from_process``). Without it, init logs "semantic index:
skipped — no embedding backend configured" and the code graph is indexed but
never embedded. The credential the run path hands over later cannot repair
that: by then there are no vectors to rank, so ``search`` silently degrades to
lexical for the whole trial. The handoff is consumed in ``main`` *before* clap
dispatch, and ``credential_handoff.rs`` deliberately re-exports this one
target into the process environment, so ``init`` honours it exactly as ``run``
does.

The Compose plumbing arrives as injected callables rather than as imports:
those helpers encode :mod:`stella_harbor`'s credential-scrubbing rules, and
importing them back from here would be a cycle — the same split
:mod:`stella_harbor.exit_cause` already uses.
"""

from __future__ import annotations

from collections.abc import Callable

from harbor.environments.base import BaseEnvironment, ExecResult

from .credential_guard import credential_carrying_names, credential_leak_message
from .timeout_reap import exec_with_agent_reap


async def secure_setup_exec_with_credential_fd(
    environment: BaseEnvironment,
    *,
    command: list[str],
    env: dict[str, str],
    credentials: dict[str, str],
    install_path: str,
    handoff_fd_env: str,
    handoff_target_env: str,
    compose_base_argv: Callable[[BaseEnvironment], list[str]],
    compose_host_environment: Callable[[BaseEnvironment], dict[str, str]],
    apply_launcher_env_controls: Callable[[dict[str, str]], dict[str, str]],
    is_credential_env_name: Callable[[str], bool],
    timeout_sec: int | None = None,
) -> ExecResult:
    """Run a *setup* stella command with credentials only on anonymous stdin."""
    if not command or command[0] != install_path:
        raise RuntimeError("secure setup runner only accepts direct stella argv")
    if not credentials:
        raise RuntimeError("secure setup runner requires at least one credential")

    values = tuple(credentials.values())
    if any(not value or "\n" in value or "\r" in value for value in values):
        # A newline inside a value would silently re-frame the pipe, handing
        # Stella a different value under a name it trusts.
        raise RuntimeError("setup credential is empty or contains a line break")

    env = apply_launcher_env_controls(dict(env))
    env[handoff_fd_env] = "0"
    # One name per value, in the order the values are written to the pipe.
    # Stella pairs them positionally, so this ordering is the contract.
    env[handoff_target_env] = ",".join(credentials)

    test_hook = getattr(environment, "_stella_secure_exec_with_stdin", None)
    wire = bytearray("\n".join(values).encode("utf-8"))
    wire.append(ord("\n"))
    try:
        if callable(test_hook):
            return await test_hook(command=command, env=env, stdin=bytes(wire))

        compose_base = compose_base_argv(environment)
        compose = [*compose_base, "exec", "-T"]

        cwd = getattr(environment.task_env_config, "workdir", None)
        if cwd:
            compose.extend(["-w", str(cwd)])
        for key, value in env.items():
            if is_credential_env_name(key):
                raise RuntimeError(
                    f"refusing to place credential variable {key} in Docker exec argv"
                )
            compose.extend(["-e", f"{key}={value}"])

        resolver = getattr(environment, "_resolve_user", None)
        user = resolver(None) if callable(resolver) else None
        if user is not None:
            compose.extend(["-u", str(user)])
        compose.extend(["main", *command])

        host_env = compose_host_environment(environment)
        carriers = credential_carrying_names(host_env, values)
        if carriers:
            raise RuntimeError(credential_leak_message(carriers))

        return await exec_with_agent_reap(
            compose, compose_base, host_env, wire, None, install_path
        )
    finally:
        for index in range(len(wire)):
            wire[index] = 0
