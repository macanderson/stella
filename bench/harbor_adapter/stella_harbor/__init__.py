"""Harbor installed-agent adapter for the Stella coding CLI.

Implements Harbor's ``BaseInstalledAgent`` so the ``stella`` binary can be
benchmarked on Terminal-Bench and SWE-bench head-to-head with Claude Code,
Terminus, Codex CLI, and any other Harbor-supported agent, under the same
verifier and container.

Harbor is the runner behind Terminal-Bench 2.x. This adapter is a *third-party*
agent: Harbor loads it by import path, not from its built-in agent registry.
The repository's ``bench/harbor_adapter/README.md#run`` section is the single
canonical claim command; keeping executable launch policy there prevents
examples in code and overview documentation from drifting apart.

How it works
------------
Stella is a Rust binary distributed as a standalone executable. This adapter:

1. Locates the compiled ``stella`` binary on the host (``STELLA_BINARY`` /
   ``PATH`` / ``./target/release/stella``).
2. Uploads it into the task container and installs it as
   ``/usr/local/bin/stella``.
3. Runs Stella one-shot in the task working directory, headless, emitting the
   stable ``--output-format stream-json`` interface to stdout and a flushed
   mounted-log sink.
4. Reconstructs complete or interrupted envelopes for Harbor's result context
   (cost, tokens, model, accounting completeness).
5. Converts the instruction and event stream to a validated ATIF-v1.7
   ``trajectory.json`` for public Terminal-Bench trajectory review.

Model selection
---------------
Harbor's literal ``--model provider/model`` reaches the agent as
``self.model_name`` and is forwarded verbatim to Stella's ``--model`` flag.
The claim launcher requires one or more explicit models using one provider.

Environment
-----------
Stella is BYOK (bring-your-own-key). The secure launcher stores exactly the
selected provider credential in one unlinked host descriptor, removes all
copies from Harbor's environment, and the adapter sends it to Stella through
inherited anonymous stdin. Only registered budget/reflection settings and
launcher-owned controls enter the container. Unregistered ``STELLA_*`` knobs
or arbitrary Harbor agent extras abort a claim run.
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import math
import os
import re
import shlex
import sys
import uuid
from collections.abc import Sequence
from importlib.metadata import PackageNotFoundError
from importlib.metadata import version as distribution_version
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit, urlunsplit

import harbor
from harbor.agents.installed.base import (
    BaseInstalledAgent,
    NonZeroAgentExitCodeError,
)
from harbor.environments.base import BaseEnvironment, ExecResult
from harbor.models.agent.context import AgentContext

from .atif import envelope_accounting, envelope_to_trajectory
from .credential_bundle import (
    ENV_CREDENTIAL_SOURCE,
    HOST_CREDENTIAL_BUNDLE_FD_ENV,
    HOST_CREDENTIAL_SOURCE,
    PROVIDER_CREDENTIAL_SPECS,
    SelectedProviderCredentials,
    read_bundle_from_environment,
    select_route_credentials,
)
from .exit_cause import (
    EXIT_CAUSE_LOG_NAME,
    SIGKILL_EXIT_CODE,
    build_exit_cause_report,
    probe_sigkill_cause,
)
from .git_baseline import ensure_git, run_git_baseline
from .portability import raise_for_loader_failure
from .posture import (
    _ASSURANCE_TIERS_VERSION,
    _CANDIDATES_ENV,
    _ENGINE_POSTURE_VERSION,
    _VERIFIER_EVIDENCE_DEMAND_ENV,
    _MAX_REVISIONS_ENV,
    _MODEL_TIMEOUT_ENV,
    _TRIAGE_MODEL_ENV,
    _WITNESS_AUTHOR_ENV,
    _WORKER_EFFORT_ENV,
    PostureBuilder,
    _benchmark_assurance_tiers,
    _benchmark_engine_posture,
    fold_witness_observations,
    resolve_candidates,
    resolve_verifier_evidence_demand,
    resolve_max_revisions,
    resolve_model_timeout,
    resolve_triage_model,
    resolve_worker_effort,
)
from .turn_budget import (
    TURN_BUDGET_ENV as _TURN_BUDGET_ENV,
)
from .turn_budget import (
    resolve_turn_budget as _resolve_turn_budget,
)

try:
    # Harbor renders prompt templates onto the ``instruction`` argument of a
    # ``run`` decorated with this helper. It is optional sugar: with no
    # template configured it is a pass-through, so a version of Harbor that
    # relocates or renames it must not break the adapter.
    from harbor.agents.installed.base import with_prompt_template
except ImportError:  # pragma: no cover - depends on the installed Harbor version

    def with_prompt_template(fn: Any) -> Any:
        """Fallback no-op used when Harbor does not export the real helper."""
        return fn


# Installation paths (inside the task container).
_BINARY_NAME = "stella"
_INSTALL_PATH = "/usr/local/bin/stella"
_REMOTE_TMP = "/tmp/stella-upload"

# Filenames written under ``self.logs_dir`` (host side) for durability/debug.
_RUN_JSON_NAME = "stella-run.json"
_RUN_STDOUT_NAME = "stella-run.stdout.txt"
_RUN_STDERR_NAME = "stella-run.stderr.txt"
_STREAM_EVENTS_NAME = "stella-events.jsonl"
_STREAM_EVENTS_PATH = f"/logs/agent/{_STREAM_EVENTS_NAME}"
_TRAJECTORY_NAME = "trajectory.json"

# The one log line that means telemetry was actually lost: a host-side
# artifact write failed AND no copy of the artifact exists on disk. Grep for
# this token during run validation (PROMPT-3 step 5). It is deliberately NOT
# the old "could not write" wording — that line also fired when a complete
# container copy was already present (11 false alarms in one 2026-07-31 run,
# zero bytes lost), which trained readers to ignore it.
_TELEMETRY_INCOMPLETE = "stella-adapter: TELEMETRY-INCOMPLETE"

# Workspace git baseline (#1211 §6 item 4; blindness documented in #973).
# Terminal-Bench task images are plain directories, so on exactly this
# benchmark the diff probe can never read a tree, candidate isolation is
# unavailable, the witness author has nothing to snapshot, and the mutation
# audit cannot restore. One repository with one commit, created before Stella
# starts, turns all of those channels on. The marker line is the whole
# contract between the in-container script and the host-side parser; the
# parsed report is disclosed verbatim in trial metadata and the public ATIF
# trajectory, so a reviewer sees exactly what the adapter did to the
# workspace. A workspace that is already a repository is left untouched —
# committing on top of task-owned history would change the task.
_GIT_BASELINE_MARKER = "stella-git-baseline:"
_GIT_BASELINE_TIMEOUT_SEC = 180
_GIT_BASELINE_COMMIT_MESSAGE = "stella-harbor: task workspace baseline"
_GIT_BASELINE_IDENT = "stella-harbor"
_GIT_BASELINE_EMAIL = "stella-harbor@bench.invalid"

# Defaults when neither Harbor nor the environment specify a value.
_DEFAULT_MODEL = "anthropic/claude-fable-5"
_DEFAULT_BUDGET = "5.0"

# What a run with no per-trial spend cap discloses as its cap. A word rather
# than an omission, because the metadata dict drops `None` values entirely and
# a published manifest would then spell "no cap" and "this adapter never
# recorded one" the same way. Deliberately not numeric: nothing downstream
# should be able to read it as a ceiling that was in force.
_NO_BUDGET_CAP = "unbounded"
_DEFAULT_DISABLE_REFLECTION = "1"
_DEFAULT_OPENROUTER_BASE_URL = "https://openrouter.ai/api/v1"
_ADAPTER_VERSION = "0.6.0"

_HANDOFF_FD_ENV = "STELLA_CREDENTIAL_HANDOFF_FD"
_HANDOFF_TARGET_ENV = "STELLA_CREDENTIAL_HANDOFF_TARGET"
_DURABLE_STREAM_ENV = "STELLA_DURABLE_STREAM_JSON_PATH"
_ENGINE_CONFIG_ENV = "STELLA_ENGINE_CONFIG_JSON"
_HANDOFF_MODE = "anonymous-fd"

# Apply these *after* every ambient/Harbor-provided extra. A benchmark task is
# untrusted input: it must not load a repository .env, opt itself into trusted
# project settings/hooks, or route paid provider traffic through a task-chosen
# proxy. Empty proxy values override any image/service defaults inherited by
# ``docker compose exec``; NO_PROXY=* makes the policy explicit for clients
# that consult bypass variables before proxy variables.
_LAUNCHER_ENV_OVERRIDES: tuple[tuple[str, str], ...] = (
    ("STELLA_NO_ENV_FILE", "1"),
    ("STELLA_NO_SETTINGS", "1"),
    ("STELLA_TRUST_PROJECT", "0"),
    ("STELLA_PROJECT_HOOKS", "0"),
    ("STELLA_CATALOG_AUTO_REFRESH", "0"),
    ("HTTP_PROXY", ""),
    ("HTTPS_PROXY", ""),
    ("ALL_PROXY", ""),
    ("http_proxy", ""),
    ("https_proxy", ""),
    ("all_proxy", ""),
    ("NO_PROXY", "*"),
    ("no_proxy", "*"),
)

_LAUNCHER_CONTROLS: dict[str, str] = {
    "process_invocation": "docker-compose-direct-argv",
    "repository_env_file": "disabled",
    "project_env_files": "disabled",
    "filesystem_settings": "disabled",
    "filesystem_credentials": "disabled",
    "subprocess_credential_scrub": "enabled",
    "project_trust": "disabled",
    "project_hooks": "disabled",
    "catalog_auto_refresh": "disabled",
    "provider_proxy": "disabled",
    "base_url_authority": "validated-cli-argument",
    "engine_config_authority": "trusted-launcher-json",
}

_CLAIM_CONTAINER_ENV = frozenset(
    {
        "STELLA_BUDGET",
        "STELLA_DISABLE_REFLECTION",
    }
)
_HOST_ONLY_STELLA_ENV = frozenset(
    {
        "STELLA_BINARY",
        "STELLA_SOURCE_COMMIT",
        "STELLA_MODEL",
        "STELLA_BASE_URL",
        # The turn deadline (#1161/#1170/#1173). Host-only like STELLA_MODEL:
        # read here, expressed as argv, never forwarded. Registered because the
        # ambient check below fails closed — unregistered, exporting it refused
        # the run rather than enabling the policy. See `turn_budget`.
        _TURN_BUDGET_ENV,
        _WITNESS_AUTHOR_ENV,
        # Worker effort and triage author: host-only like the witness author,
        # reaching Stella only inside the hashed posture. Registering them is
        # load-bearing, not tidy — the ambient check fails closed, and an
        # unlisted `STELLA_TURN_BUDGET` killed all ten trials of a run.
        _WORKER_EFFORT_ENV,
        _TRIAGE_MODEL_ENV,
        # The attempt-count selectors (#1211 §6.7, §6.8). Same bucket and same
        # reason as the three above: read on the host, expressed only inside
        # the hashed posture, never forwarded into the container. Registering
        # them is the load-bearing half — the ambient check fails closed, so an
        # unlisted selector refuses the run instead of enabling the arm.
        _MAX_REVISIONS_ENV,
        _CANDIDATES_ENV,
        _VERIFIER_EVIDENCE_DEMAND_ENV,
        # The third coupled ceiling (#1211 §6.2). Same bucket again: read on the
        # host, reaching Stella only inside the hashed posture.
        _MODEL_TIMEOUT_ENV,
        # The portability target triple and glibc floor (#1018). `env.sh` exports
        # both so `build_sut.sh` builds to the same floor `preflight` asserts
        # against — keeping them apart is what let a glibc-2.35 binary reach five
        # trials. But every script in `bench/evidence/run/` sources `env.sh`
        # *before* invoking Harbor, so both are ambient in the adapter's process
        # and the fail-closed ambient check rejected the run outright.
        #
        # Registered host-only rather than added to `_CLAIM_CONTAINER_ENV`: they
        # describe how the binary was built and verified *on the host*, and mean
        # nothing inside a task container. Host-only is the honest bucket, and it
        # keeps the container environment exactly as narrow as it was.
        "STELLA_TARGET_TRIPLE",
        "STELLA_GLIBC_FLOOR",
    }
)

_ENV_PREFIX = "STELLA_"

# A benchmark run has one selected model provider. Forward exactly what that
# provider's route needs over the anonymous-FD handoff; never spray every host
# provider key into the task container. Most providers need one value; Bedrock
# needs an access key id plus a secret access key (and a session token when the
# credentials are temporary), which is what #1301 opened the handoff up for.
#
# The declarations themselves live in `credential_bundle` so the launcher and
# the adapter cannot disagree about what a route requires — a second copy here
# is how a launcher sends two secrets to an adapter expecting one.
_PROVIDER_CREDENTIAL_ENV: dict[str, tuple[str, ...]] = {
    provider: spec.ordered_names()
    for provider, spec in PROVIDER_CREDENTIAL_SPECS.items()
}

_PROVIDER_ADDRESS_ENV_VARS: tuple[str, ...] = (
    "ZAI_GLM_CODING_PLAN",
    "VERTEX_PROJECT_ID",
    "VERTEX_LOCATION",
    "GOOGLE_CLOUD_PROJECT",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
)

# Environment inherited from the task image or Compose service is outside the
# registered benchmark posture.  ``docker compose exec -e`` overrides names it
# is given, but silently preserves every other Config.Env entry.  Reject the
# names below in the main task container before the credential handoff so an
# image cannot select a second provider route, credential source, proxy, shell
# startup hook, or dynamic loader.  This is intentionally narrower than
# ``_is_credential_env_name``: benchmark tasks may legitimately define their
# own application-level DATABASE_PASSWORD or TEST_API_KEY variables.
_PROVIDER_CREDENTIAL_CONFIG_ENV = frozenset(
    name for names in _PROVIDER_CREDENTIAL_ENV.values() for name in names
) | frozenset(
    {
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_SECURITY_TOKEN",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_PROFILE",
        "AWS_DEFAULT_PROFILE",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "AZURE_FEDERATED_TOKEN_FILE",
    }
)
_PROVIDER_ROUTE_CONFIG_ENV = frozenset(_PROVIDER_ADDRESS_ENV_VARS) | frozenset(
    {
        "ANTHROPIC_BASE_URL",
        "OPENAI_BASE_URL",
        "OPENROUTER_BASE_URL",
        "XAI_BASE_URL",
        "DEEPSEEK_BASE_URL",
        "ZAI_BASE_URL",
        "GEMINI_BASE_URL",
        "GOOGLE_BASE_URL",
        "VERTEX_BASE_URL",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "BASH_ENV",
        "ENV",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "LD_AUDIT",
    }
)
_COMPOSE_SERVICE_LABEL = "com.docker.compose.service"


def _is_truthy(value: str | None) -> bool:
    """Return whether a string environment variable represents truth."""
    if not value:
        return False
    return value.strip().lower() in ("1", "true", "yes", "on")


def _validated_public_base_url(value: str) -> str:
    """Reject credential-bearing endpoints and return a public route identity."""
    parsed = urlsplit(value)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise ValueError("STELLA_BASE_URL must be an absolute HTTP(S) URL")
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError(
            "STELLA_BASE_URL must not contain userinfo, a query, or a fragment"
        )
    return urlunsplit((parsed.scheme, parsed.netloc, parsed.path, "", ""))


def _is_credential_env_name(name: str) -> bool:
    """Mirror Stella's subprocess credential policy for adapter boundaries."""
    upper = name.upper()
    if upper in {"API_KEY", "TOKEN", "PASSWORD", "SECRET"}:
        return True
    if upper.endswith(("_API_KEY", "_TOKEN", "_PASSWORD", "_SECRET")):
        return True
    return upper in {
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_SECURITY_TOKEN",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_PROFILE",
        "AWS_DEFAULT_PROFILE",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "AZURE_FEDERATED_TOKEN_FILE",
    }


def _sanitize_compose_project_name(name: str) -> str:
    """Match Harbor's Docker Compose project-name normalization."""
    normalized = name.lower()
    if not re.match(r"^[a-z0-9]", normalized):
        normalized = "0" + normalized
    return re.sub(r"[^a-z0-9_-]", "-", normalized)


def _apply_launcher_env_controls(env: dict[str, str]) -> dict[str, str]:
    """Return ``env`` with benchmark safety controls authoritatively pinned."""
    controlled = dict(env)
    controlled.update(_LAUNCHER_ENV_OVERRIDES)
    return controlled


def _compose_base_argv(environment: BaseEnvironment) -> list[str]:
    """Return Harbor's exact Docker Compose project selector argv."""
    required = (
        "session_id",
        "environment_dir",
        "_docker_compose_paths",
        "_env_vars",
        "task_env_config",
    )
    missing = [name for name in required if not hasattr(environment, name)]
    if missing:
        raise RuntimeError(
            "secure credential handling requires Harbor's Docker environment "
            f"(missing {', '.join(missing)})"
        )
    compose = [
        "docker",
        "compose",
        "--project-name",
        _sanitize_compose_project_name(str(environment.session_id)),
        "--project-directory",
        str(Path(environment.environment_dir).resolve().absolute()),
    ]
    for path in environment._docker_compose_paths:
        compose.extend(["-f", str(Path(path).resolve().absolute())])
    return compose


def _compose_host_environment(environment: BaseEnvironment) -> dict[str, str]:
    """Reproduce Harbor's Compose env without credentials or host FD controls."""
    host_env = environment._env_vars.to_env_dict(include_os_env=True)
    host_env.update(getattr(environment, "_compose_task_env", {}) or {})
    host_env.update(getattr(environment, "_persistent_env", {}) or {})
    host_env = {
        key: str(value)
        for key, value in host_env.items()
        if not _is_credential_env_name(key) and key != HOST_CREDENTIAL_BUNDLE_FD_ENV
    }
    return _apply_launcher_env_controls(host_env)


def _contains_credential(value: Any, credential: str) -> bool:
    """Search a decoded Docker Config without rendering matching material."""
    if isinstance(value, str):
        return credential in value
    if isinstance(value, dict):
        return any(
            _contains_credential(key, credential)
            or _contains_credential(item, credential)
            for key, item in value.items()
        )
    if isinstance(value, list):
        return any(_contains_credential(item, credential) for item in value)
    return False


def _main_container_config_env_names(config: dict[str, Any]) -> set[str]:
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


def _forbidden_main_container_env_names(config: dict[str, Any]) -> list[str]:
    """Return only forbidden variable names, never their potentially secret values."""
    names = _main_container_config_env_names(config)
    return sorted(
        name
        for name in names
        if name.startswith(_ENV_PREFIX)
        or name.upper() in _PROVIDER_CREDENTIAL_CONFIG_ENV
        or name.upper() in _PROVIDER_ROUTE_CONFIG_ENV
    )


async def _captured_process(argv: list[str], env: dict[str, str]) -> tuple[int, bytes]:
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


async def _verify_compose_containers_exclude_credentials(
    environment: BaseEnvironment, credentials: Sequence[str]
) -> None:
    """Fail unless every project container Config excludes every selected key.

    This runs after Harbor has created and health-checked the environment but
    before Stella binary upload/install, then again immediately before the
    anonymous-stdin handoff. It inspects all project containers (not only
    ``main``) and the complete Docker ``Config`` object, which covers Env,
    Cmd, Entrypoint, Labels, and any future config field.

    Takes the whole set rather than one value: a Bedrock route has two or three
    secrets, and checking only the first would leave the others free to sit in
    a container's environment while the check reported clean.  The routing
    region is deliberately not checked — it is disclosed evidence, not a secret.
    """
    if not credentials:
        raise RuntimeError("credential-absence check requires at least one credential")
    hook = getattr(environment, "_stella_verify_no_container_credential", None)
    if callable(hook):
        for credential in credentials:
            if await hook(credential=credential) is not True:
                raise RuntimeError("project container credential-absence check failed")
        return

    compose = _compose_base_argv(environment)
    host_env = _compose_host_environment(environment)
    if any(
        credential in value
        for value in host_env.values()
        for credential in credentials
    ):
        raise RuntimeError(
            "selected provider credential remains in Docker's host environment"
        )

    return_code, output = await _captured_process([*compose, "ps", "-aq"], host_env)
    if return_code != 0:
        raise RuntimeError("could not enumerate benchmark project containers")
    container_ids = [
        line.strip().decode("ascii", errors="strict") for line in output.splitlines()
    ]
    if not container_ids or any(
        re.fullmatch(r"[0-9a-f]{12,64}", container_id) is None
        for container_id in container_ids
    ):
        raise RuntimeError("benchmark Compose project has no verifiable containers")

    return_code, output = await _captured_process(
        ["docker", "inspect", *container_ids], host_env
    )
    if return_code != 0:
        raise RuntimeError(
            "could not inspect benchmark project container configuration"
        )
    try:
        inspected = json.loads(output)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RuntimeError(
            "Docker returned invalid container configuration JSON"
        ) from exc
    if not isinstance(inspected, list) or len(inspected) != len(container_ids):
        raise RuntimeError("Docker returned incomplete container configuration")
    main_configs: list[dict[str, Any]] = []
    for container in inspected:
        config = container.get("Config") if isinstance(container, dict) else None
        if not isinstance(config, dict):
            raise RuntimeError("Docker container configuration omitted Config")
        if any(_contains_credential(config, secret) for secret in credentials):
            raise RuntimeError(
                "selected provider credential detected in project container "
                "configuration; refusing handoff"
            )
        labels = config.get("Labels")
        if isinstance(labels, dict) and labels.get(_COMPOSE_SERVICE_LABEL) == "main":
            main_configs.append(config)

    if len(main_configs) != 1:
        raise RuntimeError(
            "benchmark Compose project must expose exactly one main service container"
        )
    forbidden_names = _forbidden_main_container_env_names(main_configs[0])
    if forbidden_names:
        raise RuntimeError(
            "main benchmark container defines forbidden inherited environment names: "
            + ", ".join(forbidden_names)
        )


async def _probe_sigkill_cause(environment: BaseEnvironment) -> dict[str, Any]:
    """Evidence gathering for a 137 exit (#1178) — see ``exit_cause``.

    The logic lives in :mod:`stella_harbor.exit_cause`; only the Compose and
    Docker plumbing is bound here, because those helpers encode this module's
    credential-scrubbing rules and importing them back from ``exit_cause``
    would be a cycle.
    """
    return await probe_sigkill_cause(
        environment,
        compose_base_argv=_compose_base_argv,
        compose_host_environment=_compose_host_environment,
        captured_process=_captured_process,
        service_label=_COMPOSE_SERVICE_LABEL,
    )


async def _secure_exec_with_credential_fd(
    environment: BaseEnvironment,
    *,
    command: list[str],
    env: dict[str, str],
    credentials: Sequence[str],
    verifier: str | None = None,
    expected_posture_json: str | None = None,
    posture_builder: PostureBuilder | None = None,
) -> ExecResult:
    """Execute in Harbor Docker with the credentials only on anonymous stdin.

    Harbor 0.6's regular ``environment.exec(..., env=...)`` translates every
    environment value into ``docker compose exec -e NAME=value`` arguments.
    That makes a provider key visible in the host process table before Stella
    can scrub child environments. The official Terminal-Bench runner uses
    Harbor's Docker environment, so build the equivalent Compose invocation
    here and deliver the secrets through the subprocess stdin pipe. Compose
    invokes ``stella`` with a literal argv, so no launcher shell parses the
    instruction and repository-controlled ``BASH_ENV`` is never consulted.
    Stella consumes/closes fd 0 at startup.

    One value per line, in the order named by
    ``STELLA_CREDENTIAL_HANDOFF_TARGET``; Stella pairs them positionally. Most
    routes send exactly one line, which is the format byte-for-byte as it was
    before Bedrock needed a set (#1301).

    A deliberately named hook keeps unit tests independent of Docker; there is
    no insecure environment-variable fallback for production environments.
    """
    if not command or command[0] != _INSTALL_PATH:
        raise RuntimeError("secure benchmark runner only accepts direct stella argv")

    # Defense in depth: run() already receives the controlled environment from
    # _forwarded_env(), but re-apply here so no future caller can append an
    # unsafe override between collection and the process boundary.
    env = _apply_launcher_env_controls(env)
    try:
        command_model = command[command.index("--model") + 1]
    except (ValueError, IndexError) as exc:
        raise RuntimeError("secure benchmark runner requires a pinned --model") from exc
    # Recomputed here from the canonical builder rather than trusted from the
    # caller, so no future caller can append an override between collection and
    # the process boundary. `verifier` must ride along: recomputing
    # without it would rebuild the *control* posture and ship it to a trial
    # whose metadata records the treatment arm — a silently disabled tier,
    # which is the whole of #1007. The equality check makes that divergence a
    # refused run instead of an unlabelled one.
    #
    # `posture_builder` is the agent class's own canonical builder, never a
    # value the call site computed. A non-claim subclass may declare a
    # different frozen posture, and the boundary must recompute from *that*
    # declaration or reject every such run. "Recompute independently, never
    # trust a passed-in string" is untouched: the class pins the builder, and
    # the claim launcher pins the class.
    builder = posture_builder or _benchmark_engine_posture
    _, normalized_posture, _ = builder(command_model, verifier=verifier)
    if (
        expected_posture_json is not None
        and normalized_posture != expected_posture_json
    ):
        raise RuntimeError(
            "benchmark engine posture recomputed at the process boundary does "
            "not match the posture recorded for this trial"
        )
    env[_ENGINE_CONFIG_ENV] = normalized_posture
    test_hook = getattr(environment, "_stella_secure_exec_with_stdin", None)
    if not credentials:
        raise RuntimeError("secure benchmark runner requires at least one credential")
    if any(not value or "\n" in value or "\r" in value for value in credentials):
        # A newline inside a value would silently re-frame the pipe, handing
        # Stella a different value under a name it trusts.
        raise RuntimeError("provider credential is empty or contains a line break")
    wire = bytearray("\n".join(credentials).encode("utf-8"))
    wire.append(ord("\n"))
    try:
        if callable(test_hook):
            return await test_hook(command=command, env=env, stdin=bytes(wire))

        compose = _compose_base_argv(environment)
        compose.extend(["exec", "-T"])

        cwd = getattr(environment.task_env_config, "workdir", None)
        if cwd:
            compose.extend(["-w", str(cwd)])
        for key, value in env.items():
            if _is_credential_env_name(key):
                raise RuntimeError(
                    f"refusing to place credential variable {key} in Docker exec argv"
                )
            compose.extend(["-e", f"{key}={value}"])

        resolver = getattr(environment, "_resolve_user", None)
        user = resolver(None) if callable(resolver) else None
        if user is not None:
            compose.extend(["-u", str(user)])
        compose.extend(["main", *command])

        # Reproduce Harbor's Compose substitution environment, then remove all
        # host credentials. Docker itself still receives DOCKER_HOST/CONFIG and
        # ordinary build settings, but its `/proc/<pid>/environ` cannot expose
        # the benchmark provider key either.
        host_env = _compose_host_environment(environment)
        if any(
            credential in value
            for value in host_env.values()
            for credential in credentials
        ):
            raise RuntimeError(
                "selected provider credential remains in Docker's host environment"
            )

        process = await asyncio.create_subprocess_exec(
            *compose,
            env=host_env,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.STDOUT,
        )
        try:
            stdout_bytes, _ = await process.communicate(input=wire)
        except BaseException:
            if process.returncode is None:
                process.terminate()
                try:
                    await asyncio.wait_for(process.wait(), timeout=5)
                except TimeoutError:
                    process.kill()
                    await process.wait()
            raise
        stdout = stdout_bytes.decode(errors="replace") if stdout_bytes else None
        return ExecResult(
            stdout=stdout,
            stderr=None,
            return_code=process.returncode or 0,
        )
    finally:
        for index in range(len(wire)):
            wire[index] = 0


def _cached_binary(name: str) -> Path | None:
    """Return the first executable named ``name`` found on ``PATH``, or None."""
    for path in os.environ.get("PATH", "").split(os.pathsep):
        if not path:
            continue
        candidate = Path(path) / name
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
    return None


def _sha256_file(path: Path) -> str:
    """Return the full SHA-256 digest of a host binary."""
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _source_tree_sha256(root: Path, *, domain: str) -> str:
    """Hash executable Python sources with an unambiguous canonical framing."""
    digest = hashlib.sha256()
    digest.update(domain.encode("utf-8") + b"\0")
    sources = sorted(
        path for path in root.rglob("*.py") if "__pycache__" not in path.parts
    )
    if not sources:
        raise RuntimeError(f"no Python sources found under {root}")
    for path in sources:
        relative = path.relative_to(root).as_posix().encode("utf-8")
        content = path.read_bytes()
        digest.update(len(relative).to_bytes(8, "big"))
        digest.update(relative)
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    return digest.hexdigest()


def _adapter_content_sha256() -> str:
    return _source_tree_sha256(
        Path(__file__).resolve().parent,
        domain="stella-harbor-adapter-source-v1",
    )


def _harbor_content_sha256() -> str:
    roots = list(getattr(harbor, "__path__", []))
    if len(roots) != 1:
        raise RuntimeError(f"expected one Harbor package root, observed {roots!r}")
    return _source_tree_sha256(
        Path(roots[0]).resolve(),
        domain="harbor-python-source-v1",
    )


def _harbor_version() -> str:
    try:
        return distribution_version("harbor")
    except PackageNotFoundError as error:  # pragma: no cover - Harbor is required.
        raise RuntimeError("cannot determine installed Harbor version") from error


def _embedded_source_commit(version_text: str) -> str | None:
    """Extract the full compile-time STELLA_BUILD_GIT_SHA from `--version`."""
    match = re.search(r"-dev\.([0-9a-fA-F]{40})(?:\s|$)", version_text)
    return match.group(1).lower() if match is not None else None


def _locate_binary() -> Path:
    """Find the Stella binary on the host.

    Resolution order: explicit ``STELLA_BINARY`` env var, then ``stella`` on
    ``PATH``, then ``./target/release/stella`` walking up from the current
    working directory. Never imported at module load — only ``install`` calls
    this, so importing the adapter never requires Stella to be present.
    """
    explicit = os.environ.get("STELLA_BINARY")
    if explicit:
        binary = Path(explicit)
        if not binary.is_file():
            raise FileNotFoundError(
                f"STELLA_BINARY={explicit!r} does not point at a file"
            )
        return binary

    on_path = _cached_binary(_BINARY_NAME)
    if on_path is not None:
        return on_path

    cwd = Path.cwd()
    while True:
        candidate = cwd / "target" / "release" / _BINARY_NAME
        if candidate.is_file():
            return candidate
        if cwd == cwd.parent:  # reached filesystem root
            break
        cwd = cwd.parent

    raise FileNotFoundError(
        f"cannot find the {_BINARY_NAME!r} binary. For development, build "
        "with `cargo build --release -p stella-cli` (produces "
        "./target/release/stella) or put it on PATH. For a claim run, use "
        "the README's full-SHA-stamped x86_64 Linux build and set "
        "STELLA_BINARY=/path/to/stella."
    )


def _sum_step_usage(events: list[Any]) -> dict[str, int]:
    """Aggregate token usage across a turn's ``step_usage`` events.

    Each committed model call emits one ``{"type": "step_usage", ...}`` event
    carrying the normalized usage envelope (stella-protocol ``AgentEvent``).
    Summing them yields the turn totals. Fully defensive: an unexpected shape
    contributes nothing rather than raising.
    """
    totals = {"input": 0, "output": 0, "cache": 0}
    for event in events:
        if not isinstance(event, dict) or event.get("type") != "step_usage":
            continue
        for src, dst in (
            ("input_tokens", "input"),
            ("output_tokens", "output"),
            ("cached_input_tokens", "cache"),
        ):
            value = event.get(src)
            if isinstance(value, (int, float)):
                totals[dst] += int(value)
    return totals


def _valid_nonnegative_number(value: Any) -> bool:
    """Return whether ``value`` is finite, numeric, and non-negative."""
    return (
        not isinstance(value, bool)
        and isinstance(value, (int, float))
        and math.isfinite(float(value))
        and value >= 0
    )


def _valid_nonnegative_integer(value: Any) -> bool:
    """Return whether ``value`` is a non-negative integer telemetry value."""
    return _valid_nonnegative_number(value) and float(value).is_integer()


def _extract_metrics(stdout: str | None) -> dict[str, Any]:
    """Parse a strict Stella result envelope into a metrics dict.

    Returns keys ``cost_usd`` (float | None), ``n_input_tokens`` /
    ``n_output_tokens`` / ``n_cache_tokens`` (int | None), ``status`` /
    ``model`` (str | None), and ``steps`` (int | None). Never raises: a
    missing or malformed envelope yields all-None so a benchmark run is never
    aborted by a metadata-parsing edge case.
    """
    empty: dict[str, Any] = {
        "cost_usd": None,
        "n_input_tokens": None,
        "n_output_tokens": None,
        "n_cache_tokens": None,
        "status": None,
        "model": None,
        "steps": None,
    }
    if not stdout or not stdout.strip():
        return empty

    envelope = _load_json_object(stdout)
    if envelope is None:
        # Last resort: the envelope's total cost is a stable, greppable key
        # even if the surrounding JSON did not parse (e.g. truncated output).
        match = re.search(r'"cost_usd"\s*:\s*([0-9]+(?:\.[0-9]+)?)', stdout)
        if match:
            return {**empty, "cost_usd": float(match.group(1))}
        return empty

    metrics = dict(empty)
    cost = envelope.get("cost_usd")
    if _valid_nonnegative_number(cost):
        metrics["cost_usd"] = float(cost)

    status = envelope.get("status")
    if isinstance(status, str):
        metrics["status"] = status
    model = envelope.get("model")
    if isinstance(model, str):
        metrics["model"] = model

    events = envelope.get("events")
    if isinstance(events, list):
        step_events = [
            e for e in events if isinstance(e, dict) and e.get("type") == "step_usage"
        ]
        if step_events:
            metrics["steps"] = len(step_events)
            totals = _sum_step_usage(step_events)
            # A real zero is reportable, but a missing/malformed value must
            # remain unknown. Never turn an incomplete per-call field into an
            # apparently exact total by silently substituting zero.
            for source, destination, metric_key in (
                ("input_tokens", "input", "n_input_tokens"),
                ("output_tokens", "output", "n_output_tokens"),
                ("cached_input_tokens", "cache", "n_cache_tokens"),
            ):
                if all(
                    _valid_nonnegative_integer(event.get(source))
                    for event in step_events
                ):
                    metrics[metric_key] = totals[destination]

    return metrics


def _load_json_object(text: str) -> dict[str, Any] | None:
    """Best-effort parse of a JSON object from ``text``.

    Tries the whole string first. If diagnostics leaked before or after the
    envelope, incrementally decode every complete top-level object and select
    the candidate that most resembles Stella's result envelope. This is more
    robust than slicing from the first ``{`` to the last ``}``: a trailing
    diagnostic can legitimately contain its own JSON-like tool arguments.
    Returns None on failure.
    """
    text = text.strip()
    try:
        parsed = json.loads(text)
        return parsed if isinstance(parsed, dict) else None
    except json.JSONDecodeError:
        pass

    decoder = json.JSONDecoder()
    candidates: list[tuple[int, int, dict[str, Any]]] = []
    position = 0
    while True:
        start = text.find("{", position)
        if start == -1:
            break
        try:
            parsed, end = decoder.raw_decode(text, start)
        except json.JSONDecodeError:
            position = start + 1
            continue
        if isinstance(parsed, dict):
            candidates.append((_envelope_score(parsed), end - start, parsed))
        position = max(end, start + 1)

    if not candidates:
        return None
    # Score known envelope fields first, then prefer the larger complete object
    # over a small JSON argument embedded in a subsequent diagnostic.
    return max(candidates, key=lambda candidate: (candidate[0], candidate[1]))[2]


def _envelope_score(candidate: dict[str, Any]) -> int:
    """Rank a decoded object by its resemblance to a Stella run envelope."""
    score = 0
    if isinstance(candidate.get("events"), list):
        score += 16
    for key in ("status", "model", "text", "reason", "task_class", "verdict"):
        if key in candidate:
            score += 2
    if isinstance(candidate.get("cost_usd"), (int, float)):
        score += 4
    return score


def _json_dicts_from_line(line: str) -> list[dict[str, Any]]:
    """Decode complete JSON objects from one otherwise noisy stream line."""
    stripped = line.strip()
    if not stripped:
        return []
    try:
        value = json.loads(stripped)
    except json.JSONDecodeError:
        value = None
    if isinstance(value, dict):
        return [value]
    if value is not None:
        return []

    decoder = json.JSONDecoder()
    objects: list[dict[str, Any]] = []
    position = 0
    while True:
        start = stripped.find("{", position)
        if start < 0:
            break
        try:
            candidate, end = decoder.raw_decode(stripped, start)
        except json.JSONDecodeError:
            position = start + 1
            continue
        if isinstance(candidate, dict):
            objects.append(candidate)
        position = max(end, start + 1)
    return objects


def _stream_to_envelope(
    text: str | None,
    *,
    process_returned: bool = False,
) -> dict[str, Any] | None:
    """Build a best-effort Stella envelope from durable stream-json output.

    Non-JSON diagnostics and a truncated final line are ignored, but counted.
    Only top-level objects with a string ``type`` are Stella events. A process
    that did not return normally is explicitly marked interrupted unless a
    ``complete`` event proves completion; no missing terminal values are
    inferred.
    """
    if not text:
        return None

    events: list[dict[str, Any]] = []
    diagnostic_lines = 0
    ignored_json_objects = 0
    for line in text.splitlines():
        line_events = 0
        objects = _json_dicts_from_line(line)
        for candidate in objects:
            if isinstance(candidate.get("type"), str):
                events.append(candidate)
                line_events += 1
            else:
                ignored_json_objects += 1
        if line.strip() and line_events == 0:
            diagnostic_lines += 1

    if not events:
        return None

    last_terminal: dict[str, Any] | None = None
    last_error: dict[str, Any] | None = None
    last_text: str | None = None
    last_model: str | None = None
    usage_costs: list[float] = []
    usage_cost_complete = True
    usage_count = 0
    complete_count = 0
    error_count = 0

    for event in events:
        event_type = event.get("type")
        if event_type == "step_usage":
            usage_count += 1
            model = event.get("model")
            if isinstance(model, str) and model:
                last_model = model
            cost = event.get("cost_usd")
            if _valid_nonnegative_number(cost):
                usage_costs.append(float(cost))
            else:
                usage_cost_complete = False
        elif event_type == "text":
            fragment = event.get("delta")
            if fragment is None:
                fragment = event.get("text")
            if isinstance(fragment, str):
                last_text = fragment
        elif event_type == "error":
            error_count += 1
            last_error = event
            last_terminal = event
        elif event_type == "complete":
            complete_count += 1
            last_terminal = event
            model = event.get("model")
            if isinstance(model, str) and model:
                last_model = model

    terminal_type = last_terminal.get("type") if last_terminal else None
    stream_complete = terminal_type == "complete" or (
        process_returned and terminal_type == "error"
    )
    if terminal_type == "complete":
        status = "completed"
    elif process_returned and terminal_type == "error":
        status = "aborted"
    else:
        status = "interrupted"

    complete_cost = (
        last_terminal.get("cost_usd")
        if terminal_type == "complete" and last_terminal is not None
        else None
    )
    if _valid_nonnegative_number(complete_cost):
        total_cost: float | None = float(complete_cost)
        cost_source = "complete_event"
    elif usage_count and usage_cost_complete:
        total_cost = sum(usage_costs)
        cost_source = "summed_step_usage"
    else:
        total_cost = None
        cost_source = "unknown"

    reason = None
    if last_error is not None and isinstance(last_error.get("message"), str):
        reason = last_error["message"]

    # Stella's own account of its verification ladder, folded into fields an
    # analysis can read. See `posture.fold_witness_observations`.
    witness = fold_witness_observations(events)

    return {
        "status": status,
        "text": last_text,
        "cost_usd": total_cost,
        "reason": reason,
        "model": last_model,
        "events": events,
        "_stella_stream": {
            "event_count": len(events),
            "diagnostic_lines": diagnostic_lines,
            "ignored_json_objects": ignored_json_objects,
            "terminal_event": terminal_type,
            "stream_complete": stream_complete,
            "process_returned": process_returned,
            "step_usage_count": usage_count,
            "complete_event_count": complete_count,
            "error_event_count": error_count,
            "cost_source": cost_source,
            **witness,
        },
    }


def _git_baseline_script(workdir: str | None) -> str:
    """Build the POSIX-sh script that establishes the workspace git baseline.

    Always exits 0 and reports through one greppable marker line, because an
    evidence-channel setup step must never be able to fail a trial — a
    workspace it could not baseline runs exactly as every published number
    did (diff-blind), it just says so in metadata instead of silently.

    Branch order is load-bearing:

    - ``[ -e .git ]`` runs before ``rev-parse`` so a repository git refuses
      to read (dubious ownership) still reads as *preexisting* rather than
      falling through to ``git init`` — reinitializing and committing inside
      a task-owned repository would rewrite task state.
    - ``rev-parse`` runs under ``safe.directory='*'`` for the same reason:
      a false "not a repository" is the one probe result that must not
      happen, and it covers the workspace-nested-inside-a-parent-repo case
      ``[ -e .git ]`` cannot see.
    - ``.stella/`` is excluded via ``.git/info/exclude``, never a tracked
      ``.gitignore`` — the exclude file lives inside ``.git``, so the
      baseline adds zero entries to the tree a verifier might enumerate.
    - ``--allow-empty`` guarantees a baseline commit exists even for an
      empty task directory, so "repository with no HEAD" is not a state the
      diff probe can encounter.
    """
    prologue = ""
    if workdir:
        prologue = (
            f"cd {shlex.quote(workdir)} || {{ "
            f"echo '{_GIT_BASELINE_MARKER} state=error detail=workdir-unavailable'; "
            "exit 0; }; "
        )
    ident = (
        f"-c user.name={shlex.quote(_GIT_BASELINE_IDENT)} "
        f"-c user.email={shlex.quote(_GIT_BASELINE_EMAIL)}"
    )
    return (
        f"{prologue}"
        "if ! command -v git >/dev/null 2>&1; then "
        f"echo '{_GIT_BASELINE_MARKER} state=unavailable detail=git-not-installed'; "
        "elif [ -e .git ] || git -c safe.directory='*' rev-parse "
        "--is-inside-work-tree >/dev/null 2>&1; then "
        f"echo '{_GIT_BASELINE_MARKER} state=preexisting'; "
        "elif git init -q >/dev/null 2>&1 "
        "&& mkdir -p .git/info "
        "&& printf '%s\\n' '.stella/' >> .git/info/exclude "
        "&& git add -A >/dev/null 2>&1 "
        f"&& git {ident} commit -q --allow-empty --no-verify "
        f"-m {shlex.quote(_GIT_BASELINE_COMMIT_MESSAGE)} >/dev/null 2>&1; then "
        f'echo "{_GIT_BASELINE_MARKER} state=created '
        'commit=$(git rev-parse HEAD 2>/dev/null)"; '
        "else "
        f"echo '{_GIT_BASELINE_MARKER} state=failed detail=git-command-failed'; "
        "fi"
    )


def _parse_git_baseline_report(stdout: str | None) -> dict[str, str]:
    """Parse the marker line into a report; absence is itself a finding.

    The last marker line wins (a retried step reports its final state), and
    only non-empty ``key=value`` tokens are kept, so a failed
    ``git rev-parse HEAD`` yields no ``commit`` key rather than an empty one.
    """
    if stdout:
        for line in reversed(stdout.splitlines()):
            stripped = line.strip()
            if not stripped.startswith(_GIT_BASELINE_MARKER):
                continue
            report: dict[str, str] = {}
            for token in stripped[len(_GIT_BASELINE_MARKER) :].split():
                key, sep, value = token.partition("=")
                if sep and key and value:
                    report[key] = value
            if report.get("state"):
                return report
            break
    return {"state": "unreported"}


class StellaAgent(BaseInstalledAgent):
    """Run the Stella coding CLI as a Harbor installed agent."""

    SUPPORTS_ATIF: bool = True

    # Metrics captured during run() and consumed by populate_context_post_run().
    _metrics: dict[str, Any] | None
    _return_code: int | None
    _envelope: dict[str, Any] | None
    _instruction: str
    _session_id: str
    _binary_sha256: str
    _binary_sha256_verified: bool
    _source_commit: str | None
    _source_commit_verified: bool
    _adapter_sha256: str
    _harbor_version_value: str
    _harbor_sha256: str
    _base_url: str | None
    _provider_route_policy: str | None
    _disable_reflection: str
    # `None` is "no per-trial cap", which is a real posture and not a missing
    # value — see `_configured_budget`.
    _budget_usd: str | None
    _credential_handoff_mode: str
    _host_credential_source: str
    _host_credential_name: str | None
    _container_credential_absence_verified: bool
    _engine_posture: dict[str, Any]
    _engine_posture_json: str
    _engine_posture_sha256: str
    _verifier_model_value: str | None
    _assurance_tiers: dict[str, Any]
    _assurance_tiers_json: str
    _assurance_tiers_sha256: str
    _workspace_git_baseline: dict[str, str]

    def __init__(self, *args: Any, agent_timeout_sec: Any = None, **kwargs: Any):
        """Accept Harbor's per-trial agent deadline, and keep it off the base.

        Harbor 0.6.1 hands ``agent_timeout_sec`` to the *oracle* agent only, so
        an installed agent is never told the deadline it runs against — the
        reason the continuation policy shipped inert. The supported route is
        the agent kwarg (``--agent-kwarg agent_timeout_sec=<seconds>``), as
        Harbor's own Cline adapter takes it. Popping it is required, not
        stylistic: ``BaseInstalledAgent`` forwards unrecognized kwargs to
        ``BaseAgent.__init__``, which rejects it. Why it matters:
        :mod:`stella_harbor.turn_budget`.
        """
        # Raw: validation belongs at the point of use, since `__init__` is not
        # the only way this is set (the suite uses `__new__`).
        self._agent_timeout_sec = agent_timeout_sec
        super().__init__(*args, **kwargs)

    @staticmethod
    def name() -> str:
        return "stella"

    def get_version_command(self) -> str | None:
        # Overriding this (base default returns None) enables Harbor's
        # post-install version auto-detection.
        return f"{_INSTALL_PATH} --version"

    def version(self) -> str | None:
        """Return Stella's version with the exact uploaded build identity."""
        base_version = getattr(self, "_version", None)
        binary_sha256 = getattr(self, "_binary_sha256", None)
        if not binary_sha256:
            return base_version
        base = base_version or "stella unknown"
        return f"{base} [binary-sha256:{binary_sha256}]"

    def parse_version(self, stdout: str) -> str:
        """Parse ``stella --version`` output (e.g. ``stella 0.3.0``)."""
        for line in stdout.strip().splitlines():
            stripped = line.strip()
            if stripped:
                return stripped
        return stdout.strip()

    async def install(self, environment: BaseEnvironment) -> None:
        """Upload the Stella binary and install it as ``stella`` on PATH."""
        # In claim-eligible benchmark mode the provider key never entered
        # Harbor's environment. After Harbor has started/health-checked the task
        # environment but before Stella binary upload/install, inspect every
        # project container's complete Config object and refuse to continue
        # if the exact selected key appears anywhere.
        if HOST_CREDENTIAL_BUNDLE_FD_ENV in os.environ:
            selected = self._selected_provider_credentials()
            await _verify_compose_containers_exclude_credentials(
                environment, selected.secret_values()
            )
            self._container_credential_absence_verified = True
        else:
            self._host_credential_source = ENV_CREDENTIAL_SOURCE
            self._container_credential_absence_verified = False
        await ensure_git(self, environment)
        binary_path = _locate_binary()
        self._binary_sha256 = _sha256_file(binary_path)
        self._binary_sha256_verified = False
        self._source_commit = None
        self._source_commit_verified = False
        self._adapter_sha256 = _adapter_content_sha256()
        self._harbor_version_value = _harbor_version()
        self._harbor_sha256 = _harbor_content_sha256()

        await environment.upload_file(str(binary_path), _REMOTE_TMP)
        # `--version` is the first command that touches the container's dynamic
        # loader, so a binary that cannot run here dies at this exec and Harbor
        # attributes the raw nonzero exit to the agent. Classify the loader's
        # own diagnostic before that happens.
        try:
            install_result = await self.exec_as_root(
                environment,
                command=(
                    f"sha256sum {_REMOTE_TMP} && "
                    f"cp {_REMOTE_TMP} {_INSTALL_PATH} && "
                    f"chmod +x {_INSTALL_PATH} && "
                    f"{_INSTALL_PATH} --version"
                ),
                timeout_sec=120,
            )
        except NonZeroAgentExitCodeError as error:
            raise_for_loader_failure(error, binary_path)
            raise
        installed_stdout = getattr(install_result, "stdout", None) or ""
        remote_digest_match = re.search(r"(?m)^([0-9a-f]{64})\s+", installed_stdout)
        remote_digest = remote_digest_match.group(1) if remote_digest_match else None
        if remote_digest != self._binary_sha256:
            raise RuntimeError(
                "uploaded Stella binary SHA-256 did not match the host binary: "
                f"host={self._binary_sha256}, uploaded={remote_digest or 'unknown'}"
            )
        self._binary_sha256_verified = True

        version_line = next(
            (
                line.strip()
                for line in installed_stdout.splitlines()
                if line.strip().startswith("stella ")
            ),
            "",
        )
        embedded_commit = _embedded_source_commit(version_line)
        configured_commit = self._configured_value("STELLA_SOURCE_COMMIT")
        if configured_commit is not None:
            configured_commit = configured_commit.strip().lower()
            if re.fullmatch(r"[0-9a-f]{40}", configured_commit) is None:
                raise RuntimeError(
                    "STELLA_SOURCE_COMMIT must be a full 40-character Git commit"
                )
            if configured_commit != embedded_commit:
                raise RuntimeError(
                    "configured Stella source commit does not match the commit "
                    "embedded in `stella --version`: "
                    f"configured={configured_commit}, embedded="
                    f"{embedded_commit or 'missing'}"
                )
        self._source_commit = embedded_commit
        self._source_commit_verified = embedded_commit is not None
        if version_line:
            self._version = version_line

        # Baseline before the code graph, so `.stella/` is excluded before
        # `stella init` writes its DB into the workspace (see git_baseline).
        await run_git_baseline(self, environment)
        await self._build_code_graph(environment)
        await self._establish_workspace_git_baseline(environment)

    async def _build_code_graph(self, environment: BaseEnvironment) -> None:
        """Index the task workspace so ``graph_query`` is offered at all.

        The tool is registered only when ``.stella/private/codegraph.db``
        exists (`stella-tools` registry), and a fresh task checkout has never
        been initialized — so without this the agent is never offered code
        intelligence and falls back to grep/glob for every discovery step.

        `stella init` resolves no config and needs no credential: with no
        provider it takes the directory heuristic for domains and builds the
        graph regardless ("the code graph needs no provider"). Best-effort by
        construction — a workspace with nothing indexable (or no tree-sitter
        grammar for its language) is the normal case on this benchmark, not a
        failure, and must never block the run.
        """
        cwd = getattr(environment.task_env_config, "workdir", None)
        command = f"cd {shlex.quote(str(cwd))} && " if cwd else ""
        command += f"{_INSTALL_PATH} init"
        try:
            result = await self.exec_as_agent(
                environment, command=command, timeout_sec=300
            )
        except Exception as exc:  # noqa: BLE001 - discovery aid, never fatal
            print(f"stella-adapter: code graph unavailable: {exc}", file=sys.stderr)
            return
        summary = getattr(result, "stdout", None) or ""
        for line in summary.splitlines():
            if "code graph:" in line:
                self._code_graph_summary = line.strip()
                break

    async def _establish_workspace_git_baseline(
        self, environment: BaseEnvironment
    ) -> None:
        """Give the task workspace a git baseline before Stella starts.

        Runs once per task environment, after ``stella init`` (whose
        ``.stella/`` state directory the script excludes from the baseline),
        as the same in-container user the turn runs as — so the repository
        the diff probe later reads is owned by the user reading it.
        Best-effort by construction: every outcome, including "could not
        even exec", lands in ``self._workspace_git_baseline`` and from there
        in trial metadata and the ATIF trajectory, never in a raised error.
        """
        task_env_config = getattr(environment, "task_env_config", None)
        cwd = getattr(task_env_config, "workdir", None)
        script = _git_baseline_script(str(cwd) if cwd else None)
        try:
            result = await self.exec_as_agent(
                environment, command=script, timeout_sec=_GIT_BASELINE_TIMEOUT_SEC
            )
        except Exception as exc:  # noqa: BLE001 - evidence setup, never fatal
            self._workspace_git_baseline = {"state": "error", "detail": str(exc)}
            print(
                f"stella-adapter: workspace git baseline unavailable: {exc}",
                file=sys.stderr,
            )
            return
        self._workspace_git_baseline = _parse_git_baseline_report(
            getattr(result, "stdout", None)
        )

    @with_prompt_template
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        """Run Stella one-shot on the task, headless, in stream-json mode.

        Invoked as direct ``stella`` argv with the selected provider credential
        carried only on inherited fd 0. Stella consumes/closes that anonymous
        pipe before config resolution and writes each stream-json line itself
        to ``/logs/agent/stella-events.jsonl``.

        The command runs in the container's default working directory (the task
        repo). Each event is copied immediately into Harbor's mounted log path,
        so completed-call telemetry survives an outer timeout or cancellation.
        A nonzero Stella exit is raised *after* its telemetry is persisted and
        context/ATIF are populated; Harbor therefore records the official error
        class and still runs the benchmark verifier.
        """
        self._instruction = instruction
        self._session_id = str(uuid.uuid4())
        # One installed-agent instance can serve multiple Harbor task steps.
        # Never let a later timeout inherit a prior step's return code/envelope.
        self._return_code = None
        self._envelope = None
        self._metrics = None
        configured_model = self._effective_model()
        env = self._forwarded_env()
        self._disable_reflection = env["STELLA_DISABLE_REFLECTION"]
        self._budget_usd = self._configured_budget()
        verifier = self._verifier_model()
        self._verifier_model_value = verifier
        (
            self._engine_posture,
            self._engine_posture_json,
            self._engine_posture_sha256,
        ) = self._build_engine_posture(configured_model, verifier=verifier)
        (
            self._assurance_tiers,
            self._assurance_tiers_json,
            self._assurance_tiers_sha256,
        ) = _benchmark_assurance_tiers(configured_model, verifier=verifier)
        # Highest precedence, after ambient and all Harbor extra env. A task
        # cannot replace this with its own routing or request-effort config.
        env[_ENGINE_CONFIG_ENV] = self._engine_posture_json
        self._base_url = self._effective_base_url(configured_model)
        self._provider_route_policy = (
            "openrouter-auto"
            if configured_model.split("/", 1)[0].strip().lower() == "openrouter"
            else "direct"
        )
        command = self._build_command(
            instruction,
            model=configured_model,
            base_url=self._base_url,
        )

        selected = self._selected_provider_credentials()
        if self._host_credential_source == HOST_CREDENTIAL_SOURCE:
            await _verify_compose_containers_exclude_credentials(
                environment, selected.secret_values()
            )
            self._container_credential_absence_verified = True
        else:
            self._container_credential_absence_verified = False
        env[_HANDOFF_FD_ENV] = "0"
        # One name per value, in the order the values are written to the pipe.
        # Stella pairs them positionally, so this ordering is the contract.
        env[_HANDOFF_TARGET_ENV] = ",".join(selected.credentials)
        # Routing rides as ordinary registered container environment: a region
        # is disclosed in the manifest, not a secret to keep out of `/proc`.
        env.update(selected.routing)
        env[_DURABLE_STREAM_ENV] = _STREAM_EVENTS_PATH
        self._credential_handoff_mode = _HANDOFF_MODE
        self._provider_routing = dict(selected.routing)

        # Do not use environment.exec(env={credential: ...}): Harbor renders
        # those values into `docker compose exec -e` argv. The secure runner
        # feeds one selected key through an anonymous stdin pipe and invokes
        # Stella directly, leaving neither a shell parent nor a tee process.
        result = await _secure_exec_with_credential_fd(
            environment,
            command=command,
            env=env,
            credentials=tuple(selected.credentials.values()),
            verifier=verifier,
            expected_posture_json=self._engine_posture_json,
            posture_builder=self._build_engine_posture,
        )

        stdout = getattr(result, "stdout", None)
        stderr = getattr(result, "stderr", None)
        self._return_code = getattr(result, "return_code", None)
        durable_stream = self._read_log(_STREAM_EVENTS_NAME)
        if durable_stream is None:
            # Fake and non-mounted environments do not expose the container's
            # internal sink yet. Preserve captured stdout without overwriting
            # a mounted file that may be more complete than ExecResult.
            self._write_log(_STREAM_EVENTS_NAME, stdout)
            durable_stream = stdout
        self._envelope = _stream_to_envelope(
            durable_stream,
            process_returned=True,
        )
        self._metrics = _extract_metrics(
            json.dumps(self._envelope) if self._envelope is not None else None
        )

        # Preserve exact process output and a strict synthetic envelope before
        # raising on a nonzero exit. The stream file normally already exists
        # through Stella's internal sink; fake/non-mounted environments
        # received the captured fallback above, while a mounted source of
        # truth is never overwritten.
        self._write_log(_RUN_STDOUT_NAME, stdout)
        if self._envelope is not None:
            self._write_log(
                _RUN_JSON_NAME,
                json.dumps(self._envelope, indent=2, ensure_ascii=False),
            )
        if stderr:
            self._write_log(_RUN_STDERR_NAME, stderr)

        # Populate now so Harbor keeps metrics even though it deliberately
        # skips a second post-run call once AgentContext is non-empty.
        self.populate_context_post_run(context)

        if isinstance(self._return_code, int) and self._return_code != 0:
            # A 137 is SIGKILL — the one exit that is not Stella's own and
            # used to carry no explanation at all (#1178). Diagnose it now,
            # while the container still exists, so the exception line names
            # OOM vs external teardown instead of leaving the trial as the
            # failure everyone steps around.
            exit_cause_note = ""
            if self._return_code == SIGKILL_EXIT_CODE:
                report = await self._record_exit_cause(environment)
                if report is not None:
                    exit_cause_note = (
                        f" Exit 137 is SIGKILL, not a Stella exit path; "
                        f"verdict: {report['verdict']} ({report['detail']}) — "
                        f"evidence in {EXIT_CAUSE_LOG_NAME}."
                    )
            raise NonZeroAgentExitCodeError(
                f"Stella exited with code {self._return_code}; stream telemetry "
                f"was preserved in {_STREAM_EVENTS_PATH}.{exit_cause_note} stderr: "
                f"{self._truncate_output(stderr)}"
            )

    async def _record_exit_cause(
        self, environment: BaseEnvironment
    ) -> dict[str, Any] | None:
        """Diagnose a SIGKILL exit and persist the evidence (#1178).

        Writes ``stella-exit-cause.json`` beside the event stream, carrying
        the container's Docker ``State``, its cgroup ``memory.events``
        counters, and whether the event stream stopped mid-step or at a step
        boundary — the three signals that separate an OOM kill from an
        external teardown. Best-effort: any failure is printed and swallowed,
        because the diagnosis must never displace the official
        ``NonZeroAgentExitCodeError`` Harbor scores the trial by.
        """
        try:
            probe = await _probe_sigkill_cause(environment)
            events: list[dict[str, Any]] | None = None
            if self._envelope is not None:
                raw_events = self._envelope.get("events")
                if isinstance(raw_events, list):
                    events = raw_events
            report = build_exit_cause_report(
                return_code=SIGKILL_EXIT_CODE,
                container_state=probe.get("container_state"),
                container_absent=bool(probe.get("container_absent")),
                memory_events_text=probe.get("memory_events_text"),
                events=events,
                probe_errors=list(probe.get("errors") or []),
            )
            self._write_log(
                EXIT_CAUSE_LOG_NAME,
                json.dumps(report, indent=2, ensure_ascii=False),
            )
            return report
        except Exception as exc:  # noqa: BLE001 - never displace the scored error
            print(
                f"stella-adapter: exit-cause diagnosis unavailable: {exc}",
                file=sys.stderr,
            )
            return None

    def _effective_model(self) -> str:
        """Return the model selected by Harbor, configuration, or the default."""
        return (
            getattr(self, "model_name", None)
            or self._configured_value("STELLA_MODEL")
            or _DEFAULT_MODEL
        )

    def _build_engine_posture(
        self, model: str, *, verifier: str | None
    ) -> tuple[dict[str, Any], str, str]:
        """Return ``(posture, canonical_json, sha256)`` for this trial.

        The one seam a *non-claim* harness may override to run a different
        frozen engine configuration — a head-to-head varying effort across
        arms, say, which the benchmark posture deliberately holds constant.
        Safe by construction, not convention: the claim launcher pins
        ``--agent-import-path`` to ``stella_harbor:StellaAgent`` and rejects
        any other agent, so a subclass cannot reach a claim run, and whatever
        it returns is hashed into the same ``stella_engine_posture*`` metadata
        — a changed posture is a changed digest, visible in the manifest.

        The base implementation is the frozen benchmark posture and must stay
        that way: a claim run's posture is part of its identity. The worker
        effort, triage author and attempt counts are resolved here rather than
        at the call site so the exec boundary, which recomputes through this
        same bound method, resolves them identically — a selected arm must
        produce the same posture at collection and at the process boundary or
        the run is refused.
        """
        return _benchmark_engine_posture(
            model,
            verifier=verifier,
            worker_effort=resolve_worker_effort(
                self._configured_value(_WORKER_EFFORT_ENV)
            ),
            triage_model=resolve_triage_model(
                self._configured_value(_TRIAGE_MODEL_ENV)
            ),
            max_revisions=resolve_max_revisions(
                self._configured_value(_MAX_REVISIONS_ENV)
            ),
            candidates=resolve_candidates(self._configured_value(_CANDIDATES_ENV)),
            verifier_evidence_demand=resolve_verifier_evidence_demand(
                self._configured_value(_VERIFIER_EVIDENCE_DEMAND_ENV)
            ),
            model_timeout_secs=resolve_model_timeout(
                self._configured_value(_MODEL_TIMEOUT_ENV)
            ),
        )

    def _verifier_model(self) -> str | None:
        """Return the pinned witness/verifier author, or ``None`` for the control arm.

        Unset defaults to the control arm, which keeps every published number
        and every registered posture hash exactly as it was — the treatment arm
        has to be asked for, so this can never change a run's meaning by
        arriving in the tree.
        """
        value = self._configured_value(_WITNESS_AUTHOR_ENV)
        if value is None:
            return None
        stripped = value.strip()
        return stripped or None


    def _effective_base_url(self, model: str) -> str | None:
        """Resolve and validate the authoritative provider endpoint."""
        configured = self._configured_value("STELLA_BASE_URL")
        provider = model.split("/", 1)[0].strip().lower()
        if provider == "openrouter":
            if configured:
                validated = _validated_public_base_url(configured)
                if validated != _DEFAULT_OPENROUTER_BASE_URL:
                    raise RuntimeError(
                        "OpenRouter benchmark runs require the canonical provider "
                        "endpoint; refusing a configured STELLA_BASE_URL"
                    )
            return _validated_public_base_url(_DEFAULT_OPENROUTER_BASE_URL)
        if configured:
            return _validated_public_base_url(configured)
        return None

    def _build_command(
        self,
        instruction: str,
        *,
        model: str | None = None,
        base_url: str | None = None,
    ) -> list[str]:
        """Build the headless one-shot Stella argument vector.

        Global flags (``--model``, ``--budget``, ``--base-url``,
        ``--output-format``) precede the ``run`` subcommand — they are top-level
        CLI flags in Stella, not flags of ``run``. Returning an argv preserves
        the instruction as one literal argument without shell quoting/parsing.
        """
        model = model or self._effective_model()
        budget = self._configured_budget()
        turn_budget = self._configured_turn_budget()
        base_url = base_url or self._effective_base_url(model)
        parts = [
            _INSTALL_PATH,
            "--model",
            model,
            # Omitted entirely when there is no cap. `--budget ""` is not "no
            # budget" to clap, it is a malformed number, and it exits 2 before
            # the turn starts.
            *(["--budget", budget] if budget is not None else []),
            # Omit-when-absent, same discipline and same reason as `--budget`:
            # an absent flag is "no deadline", an empty value exits 2.
            *(["--turn-budget", turn_budget] if turn_budget is not None else []),
            "--output-format",
            "stream-json",
        ]
        if base_url:
            base_url = _validated_public_base_url(base_url)
            parts += ["--base-url", base_url]
        # `--` ends option parsing so a task instruction that begins with a
        # dash (a markdown list, a CLI transcript) binds to the positional
        # instead of parsing as a flag. Without it, `stella run '- foo'`
        # exits 2 before the agent starts — one 2026-07-31 trial
        # (pytorch-model-recovery) died exactly this way and scored 0.
        parts += ["run", "--", instruction]
        return parts

    def _selected_provider_credentials(self) -> SelectedProviderCredentials:
        """Resolve every credential and routing value the model provider needs.

        Most providers resolve to exactly one credential; Bedrock resolves to
        an access key id, a secret access key, an optional session token, and a
        region (#1301).  The policy itself lives in :mod:`credential_bundle`,
        beside the specs it reads, so the launcher and the adapter cannot
        disagree about what a route requires; only the Harbor-shaped inputs are
        bound here.
        """
        model = (
            getattr(self, "model_name", None)
            or self._configured_value("STELLA_MODEL")
            or _DEFAULT_MODEL
        )
        # Every place a value could have been copied to under a name of the
        # copier's choosing.  A secure launch must not retain a duplicate of a
        # bundled key anywhere ambient; the descriptor is the sole source.
        ambient_values = list(os.environ.values())
        for attr in ("_resolved_env_vars", "extra_env", "_extra_env"):
            extra = getattr(self, attr, None)
            if isinstance(extra, dict):
                ambient_values.extend(str(item) for item in extra.values())

        resolution = select_route_credentials(
            model,
            lookup=self._configured_value,
            bundled=read_bundle_from_environment(os.environ),
            ambient_values=ambient_values,
        )
        self._host_credential_source = resolution.source
        self._host_credential_name = resolution.handoff_target
        return resolution.selected

    def _configured_value(self, key: str, default: str | None = None) -> str | None:
        """Resolve one env value with Harbor's per-run overrides taking priority."""
        value = os.environ.get(key, default)
        for attr in ("_resolved_env_vars", "extra_env", "_extra_env"):
            extra = getattr(self, attr, None)
            if isinstance(extra, dict) and key in extra:
                value = str(extra[key])
        return value

    def _configured_budget(self) -> str | None:
        """Resolve the per-trial spend cap, where **empty means no cap**.

        ``--budget`` is ``Option<f64>`` in the CLI and is documented as "omit to
        meter spend for the cost summary without ever blocking", so "no cap" is
        a state the contract already has — reachable only by *omitting* the
        flag. ``_configured_value`` cannot express it: ``os.environ.get`` treats
        an exported empty string as a present value, so ``_DEFAULT_BUDGET``
        never fires and the empty string is forwarded as though it were a
        number. It then reaches clap's ``parse_budget``, which rejects it
        (```` is not a number``) and exits 2 **before the turn starts** — a
        failure Harbor can only record as ``NonZeroAgentExitCodeError``, which
        is arithmetically indistinguishable from the agent failing the task.

        A head-to-head against a comparator that has no spend ceiling has to be
        able to say "no ceiling" on this side too, or the cap is a difference
        between the agents that is not the one being measured. Empty resolves to
        ``None`` and every caller omits: the flag, the forwarded variable, and
        the recorded value alike. ``None`` in the manifest reads as "no cap",
        which is the truth; ``_DEFAULT_BUDGET`` there would be a number no run
        ever enforced.
        """
        budget = self._configured_value("STELLA_BUDGET", _DEFAULT_BUDGET)
        if budget is None or not str(budget).strip():
            return None
        return budget

    def _configured_turn_budget(self) -> str | None:
        """Resolve the turn deadline in seconds, or ``None`` for no deadline.

        Policy and parsing live in :mod:`stella_harbor.turn_budget`; this
        supplies the two inputs and nothing else.

        ``getattr`` rather than attribute access: Harbor constructs this class,
        but the suite builds instances without running ``__init__`` — the same
        reason ``_configured_value`` reaches for ``_resolved_env_vars`` this
        way — and an adapter that only works when fully constructed is an
        adapter whose deadline handling is untested.
        """
        return _resolve_turn_budget(
            getattr(self, "_agent_timeout_sec", None),
            self._configured_value(_TURN_BUDGET_ENV),
        )

    def _forwarded_env(self) -> dict[str, str]:
        """Build the registered minimal claim environment.

        Provider credentials are intentionally absent; exactly one is resolved
        separately and sent over the inherited anonymous FD. Unregistered
        Stella knobs and arbitrary Harbor extras fail closed rather than
        silently changing the SUT or exposing host metadata.
        """
        forwarded: dict[str, str] = {}
        trusted_transport = {
            _HANDOFF_FD_ENV,
            _HANDOFF_TARGET_ENV,
            _DURABLE_STREAM_ENV,
            _ENGINE_CONFIG_ENV,
            HOST_CREDENTIAL_BUNDLE_FD_ENV,
        }
        pinned_names = {name for name, _ in _LAUNCHER_ENV_OVERRIDES}
        recognized_host_names = (
            _CLAIM_CONTAINER_ENV
            | _HOST_ONLY_STELLA_ENV
            | trusted_transport
            | pinned_names
        )
        unexpected_ambient = sorted(
            key
            for key in os.environ
            if key.startswith(_ENV_PREFIX)
            and not _is_credential_env_name(key)
            and key not in recognized_host_names
        )
        if unexpected_ambient:
            raise RuntimeError(
                "claim benchmark environment contains unregistered STELLA_* knobs: "
                + ", ".join(unexpected_ambient)
            )

        # Harbor-resolved declared env vars and per-run --env overrides are not
        # part of the registered SUT. Credential-shaped values are handled by
        # the separate bundle path; pinned controls are overwritten below.
        for attr in ("_resolved_env_vars", "extra_env", "_extra_env"):
            extra = getattr(self, attr, None)
            if isinstance(extra, dict):
                unexpected_extra = sorted(
                    str(key)
                    for key in extra
                    if not _is_credential_env_name(str(key))
                    and str(key) not in _CLAIM_CONTAINER_ENV
                    and str(key) not in trusted_transport
                    and str(key) not in pinned_names
                )
                if unexpected_extra:
                    raise RuntimeError(
                        "claim benchmark rejects unregistered Harbor agent extras: "
                        + ", ".join(unexpected_extra)
                    )

        # Not forwarded at all when there is no cap: the CLI declares `--budget`
        # with `env = "STELLA_BUDGET"`, so an empty value in the container's
        # environment fails the same parse the omitted flag was avoiding.
        budget = self._configured_budget()
        if budget is not None:
            forwarded["STELLA_BUDGET"] = budget
        reflection = self._configured_value(
            "STELLA_DISABLE_REFLECTION", _DEFAULT_DISABLE_REFLECTION
        )
        if reflection is not None:
            forwarded["STELLA_DISABLE_REFLECTION"] = reflection

        # Headless benchmark trials are ephemeral and should not spend an
        # unreported post-turn model call. This is a disclosed benchmark
        # configuration, not a telemetry workaround; callers can explicitly
        # set 0/false to restore reflection for an experiment.
        return _apply_launcher_env_controls(forwarded)

    def populate_context_post_run(self, context: AgentContext) -> None:
        """Populate Harbor context and emit an ATIF-v1.7 trajectory."""
        envelope = getattr(self, "_envelope", None)
        if envelope is None:
            # On an outer timeout/cancellation, run() never receives an
            # ExecResult. Stella's flushed stream file is nevertheless already
            # mounted (or downloaded by Harbor before this hook), so reconstruct
            # a partial envelope using only events that reached durable storage.
            raw_stream = self._read_log(_STREAM_EVENTS_NAME)
            if raw_stream:
                envelope = _stream_to_envelope(
                    raw_stream,
                    process_returned=getattr(self, "_return_code", None) is not None,
                )

        if envelope is None:
            # Backward-compatible fallback for runs created by adapter 0.4.x.
            raw_envelope = self._read_log(_RUN_JSON_NAME)
            envelope = _load_json_object(raw_envelope) if raw_envelope else None

        if envelope is not None:
            self._envelope = envelope
            self._write_log(
                _RUN_JSON_NAME,
                json.dumps(envelope, indent=2, ensure_ascii=False),
            )

        metrics = getattr(self, "_metrics", None)
        if metrics is None:
            metrics = _extract_metrics(
                json.dumps(envelope) if envelope is not None else None
            )
            self._metrics = metrics

        if metrics.get("cost_usd") is not None:
            context.cost_usd = metrics["cost_usd"]
        if metrics.get("n_input_tokens") is not None:
            context.n_input_tokens = metrics["n_input_tokens"]
        if metrics.get("n_output_tokens") is not None:
            context.n_output_tokens = metrics["n_output_tokens"]
        if metrics.get("n_cache_tokens") is not None:
            context.n_cache_tokens = metrics["n_cache_tokens"]

        return_code = getattr(self, "_return_code", None)
        binary_sha256 = getattr(self, "_binary_sha256", None)
        binary_sha256_verified = getattr(self, "_binary_sha256_verified", None)
        source_commit = getattr(self, "_source_commit", None)
        source_commit_verified = getattr(self, "_source_commit_verified", False)
        adapter_sha256 = getattr(self, "_adapter_sha256", None)
        harbor_version_value = getattr(self, "_harbor_version_value", None)
        harbor_sha256 = getattr(self, "_harbor_sha256", None)
        base_url = getattr(self, "_base_url", None)
        provider_route_policy = getattr(self, "_provider_route_policy", None)
        reflection = getattr(self, "_disable_reflection", None)
        if reflection is None:
            reflection = self._configured_value(
                "STELLA_DISABLE_REFLECTION",
                _DEFAULT_DISABLE_REFLECTION,
            )
        budget_usd = getattr(self, "_budget_usd", None)
        if budget_usd is None:
            budget_usd = self._configured_budget()
        if budget_usd is None:
            budget_usd = _NO_BUDGET_CAP
        credential_handoff_mode = getattr(
            self, "_credential_handoff_mode", _HANDOFF_MODE
        )
        host_credential_source = getattr(
            self, "_host_credential_source", ENV_CREDENTIAL_SOURCE
        )
        host_credential_name = getattr(self, "_host_credential_name", None)
        # The comma-joined target list is one string on purpose: it is the exact
        # value of STELLA_CREDENTIAL_HANDOFF_TARGET, so a reader comparing the
        # manifest against the wire sees the same bytes. The count is derived
        # from it rather than hard-coded to 1 — a Bedrock route carries two or
        # three, and a manifest that always said "1" would be false.
        host_credential_bundle_count = (
            len(host_credential_name.split(",")) if host_credential_name else 0
        )
        provider_routing = getattr(self, "_provider_routing", None) or None
        container_credential_absence_verified = getattr(
            self, "_container_credential_absence_verified", False
        )
        engine_posture = getattr(self, "_engine_posture", None)
        engine_posture_json = getattr(self, "_engine_posture_json", None)
        engine_posture_sha256 = getattr(self, "_engine_posture_sha256", None)
        verifier_model = getattr(self, "_verifier_model_value", None)
        assurance_tiers = getattr(self, "_assurance_tiers", None)
        assurance_tiers_json = getattr(self, "_assurance_tiers_json", None)
        assurance_tiers_sha256 = getattr(self, "_assurance_tiers_sha256", None)
        # An outer timeout can reach this hook before run() built the posture.
        # Reconstruct the declaration from the same inputs rather than leaving
        # the arm unstated on exactly the trials most likely to be re-read.
        if assurance_tiers is None:
            try:
                (
                    assurance_tiers,
                    assurance_tiers_json,
                    assurance_tiers_sha256,
                ) = _benchmark_assurance_tiers(
                    self._effective_model(),
                    verifier=self._verifier_model(),
                )
            except (ValueError, RuntimeError):
                assurance_tiers = None
        assurance_arm = (
            assurance_tiers.get("arm") if isinstance(assurance_tiers, dict) else None
        )
        stream_view = envelope.get("_stella_stream") if envelope is not None else None
        witness_authored_state = (
            stream_view.get("witness_authored_state")
            if isinstance(stream_view, dict)
            else None
        ) or "not_reported"
        # Stella's own verdict on its own work, as a top-level field for the
        # same reason the witness state is one: the A/B this exists for (#1284)
        # compares it against the external grader's reward, and a comparison
        # that has to reach into a nested blob is one an analysis quietly skips.
        # A trial with no stream made no claim — "not_reported", never "failed".
        self_verdict_state = (
            stream_view.get("self_verdict_state")
            if isinstance(stream_view, dict)
            else None
        ) or "not_reported"
        # Absent (adapter predates install, or install never ran) is a real
        # state and must not be conflated with a step that ran and reported.
        workspace_git_baseline = getattr(
            self, "_workspace_git_baseline", {"state": "not_attempted"}
        )

        extra: dict[str, Any] = {
            "stella_status": metrics.get("status"),
            "stella_model": metrics.get("model"),
            "stella_steps": metrics.get("steps"),
            "stella_return_code": return_code,
            "stella_return_code_state": (
                "known" if return_code is not None else "unknown"
            ),
            "stella_binary_sha256": binary_sha256,
            "stella_binary_sha256_verified_in_container": binary_sha256_verified,
            "stella_source_commit": source_commit,
            "stella_source_commit_verified_in_binary": source_commit_verified,
            "stella_agent_version": self.version(),
            "stella_adapter_version": _ADAPTER_VERSION,
            "stella_adapter_sha256": adapter_sha256,
            "stella_harbor_version": harbor_version_value,
            "stella_harbor_sha256": harbor_sha256,
            "stella_base_url": base_url,
            "stella_provider_route_policy": provider_route_policy,
            "stella_budget_usd": budget_usd,
            "stella_output_format": "stream-json",
            "stella_disable_reflection": reflection,
            "stella_reflection_policy": (
                "disabled_for_ephemeral_benchmark"
                if _is_truthy(reflection)
                else "explicitly_enabled"
            ),
            "stella_credential_handoff": credential_handoff_mode,
            "stella_host_credential_source": host_credential_source,
            "stella_host_credential_name": host_credential_name,
            "stella_host_credential_bundle_count": host_credential_bundle_count,
            # Non-secret route addressing (AWS_REGION today). Disclosed, not
            # scrubbed: a Bedrock number nobody can attribute to a region is a
            # number nobody can reproduce.
            "stella_provider_routing": provider_routing,
            "stella_container_credential_absence_verified": (
                container_credential_absence_verified
            ),
            "stella_launcher_controls": dict(_LAUNCHER_CONTROLS),
            "stella_engine_posture_version": _ENGINE_POSTURE_VERSION,
            "stella_engine_posture": engine_posture,
            "stella_engine_posture_json": engine_posture_json,
            "stella_engine_posture_sha256": engine_posture_sha256,
            # Which tiers this posture *declares*, and what the run's own proof
            # stream *observed*. Both first-class, so an analysis reports
            # `witness_authored: false` from metadata rather than inferring it
            # from a warning line in a trajectory (#1007).
            "stella_assurance_tiers_version": _ASSURANCE_TIERS_VERSION,
            "stella_assurance_arm": assurance_arm,
            "stella_assurance_tiers": assurance_tiers,
            "stella_assurance_tiers_json": assurance_tiers_json,
            "stella_assurance_tiers_sha256": assurance_tiers_sha256,
            "stella_verifier_model": verifier_model,
            "stella_witness_authored_state": witness_authored_state,
            "stella_self_verdict_state": self_verdict_state,
            "stella_workspace_git_baseline": workspace_git_baseline,
        }
        if envelope is not None:
            extra["stella_accounting"] = envelope_accounting(envelope)
            stream_metadata = envelope.get("_stella_stream")
            if isinstance(stream_metadata, dict):
                extra["stella_stream"] = stream_metadata
        extra = {k: v for k, v in extra.items() if v is not None}
        if extra:
            context.metadata = {**(context.metadata or {}), **extra}

        instruction = getattr(self, "_instruction", None)
        if envelope is None or not isinstance(instruction, str):
            return

        try:
            trajectory = envelope_to_trajectory(
                envelope,
                instruction=instruction,
                session_id=getattr(self, "_session_id", None) or str(uuid.uuid4()),
                agent_version=self.version() or "unknown",
                default_model=(
                    getattr(self, "model_name", None)
                    or self._configured_value("STELLA_MODEL")
                    or _DEFAULT_MODEL
                ),
                return_code=return_code,
                binary_sha256=binary_sha256,
                binary_sha256_verified=binary_sha256_verified,
                source_commit=source_commit,
                source_commit_verified=source_commit_verified,
                disable_reflection=reflection,
                adapter_version=_ADAPTER_VERSION,
                adapter_sha256=adapter_sha256,
                harbor_version=harbor_version_value,
                harbor_sha256=harbor_sha256,
                base_url=base_url,
                provider_route_policy=provider_route_policy,
                budget_usd=budget_usd,
                credential_handoff=credential_handoff_mode,
                host_credential_source=host_credential_source,
                host_credential_name=host_credential_name,
                host_credential_bundle_count=host_credential_bundle_count,
                container_credential_absence_verified=(
                    container_credential_absence_verified
                ),
                launcher_controls=dict(_LAUNCHER_CONTROLS),
                engine_posture_version=_ENGINE_POSTURE_VERSION,
                engine_posture=engine_posture,
                engine_posture_json=engine_posture_json,
                engine_posture_sha256=engine_posture_sha256,
                assurance_tiers_version=_ASSURANCE_TIERS_VERSION,
                assurance_arm=assurance_arm,
                assurance_tiers=assurance_tiers,
                assurance_tiers_json=assurance_tiers_json,
                assurance_tiers_sha256=assurance_tiers_sha256,
                verifier_model=verifier_model,
                witness_authored_state=witness_authored_state,
                workspace_git_baseline=workspace_git_baseline,
            )
            self._write_log(
                _TRAJECTORY_NAME,
                json.dumps(
                    trajectory.to_json_dict(),
                    indent=2,
                    ensure_ascii=False,
                ),
            )
        except Exception as exc:  # noqa: BLE001 - metadata must not fail a trial
            # Serialization failures only: `_write_log` handles (and honestly
            # classifies) filesystem failures itself and does not raise.
            print(
                f"stella-adapter: could not build {_TRAJECTORY_NAME}: {exc}",
                file=sys.stderr,
            )

    # -- host-side log helpers ------------------------------------------------

    def _log_path(self, name: str) -> Path | None:
        logs_dir = getattr(self, "logs_dir", None)
        if logs_dir is None:
            return None
        return Path(logs_dir) / name

    def _write_log(self, name: str, content: str | None) -> None:
        """Persist ``content`` under ``logs_dir`` without clobbering — or
        false-alarming about — a copy that is already there.

        The events file arrives by two racing paths: Stella's in-container
        sink (downloaded by Harbor, often root-owned) and this host-side
        write. Whichever lost the race used to hit the other's file and print
        "could not write" — eleven times in one run, with zero bytes lost,
        and the error anti-correlated with actual loss. Now: an existing
        file is left alone (whatever is there is the container's own artifact
        or an earlier successful write — both authoritative over a host-side
        reconstruction); a failed write re-checks the filesystem and stays
        silent when a non-empty copy exists (the race was lost, nothing was);
        and only "nothing exists and nothing could be written" is loud, with
        the greppable ``TELEMETRY-INCOMPLETE`` marker run validation looks
        for.
        """
        if not content:
            return
        path = self._log_path(name)
        if path is None:
            return
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            if path.exists():
                # Non-destructive by design; never replace an existing
                # artifact with a reconstruction.
                return
            path.write_text(content, encoding="utf-8")
        except OSError as exc:
            if self._artifact_present(path):
                # Lost the race to the container download: the artifact is
                # present and non-empty, so nothing was lost and nothing is
                # printed.
                return
            print(
                f"{_TELEMETRY_INCOMPLETE} {name}: no copy exists and the host "
                f"write failed: {exc}",
                file=sys.stderr,
            )

    @staticmethod
    def _artifact_present(path: Path) -> bool:
        """Whether a non-empty file exists at ``path``. Checked *after* a
        failed write, so a lost race reads as presence rather than loss; a
        root-owned unreadable file still counts (stat needs only the parent)."""
        try:
            return path.stat().st_size > 0
        except OSError:
            return False

    def _read_log(self, name: str) -> str | None:
        path = self._log_path(name)
        if path is None or not path.is_file():
            return None
        try:
            return path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return None
