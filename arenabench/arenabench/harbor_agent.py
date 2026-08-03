# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""ArenaBench's Harbor agent for Stella.

Subclasses the ``stella_harbor`` adapter for exactly one reason: to replace its
frozen benchmark posture with the contestant's engine configuration. Credential
handling, ATIF trajectory writing, telemetry reconstruction, container
verification — all of it is inherited untouched, because none of it is what a
contest varies.

**This is deliberately not a claim-run adapter.** Stella's secure launcher pins
``--agent-import-path`` to the canonical ``stella_harbor:StellaAgent`` and
refuses anything else, so this class cannot enter a claim run even by accident.
That is the point: an arena match is exploratory, and the freedom to vary
effort per arm is precisely what a published claim must not have.

The posture still gets hashed and published through the same
``stella_engine_posture*`` metadata keys, so an arena run says exactly which
configuration produced its numbers.
"""

from __future__ import annotations

import hashlib
import json
import os
from typing import Any

from .model import Engine

__all__ = ["ARENA_ENGINE_ENV", "ArenaStellaAgent", "arena_posture"]

#: Environment variable carrying the contestant's :class:`~.model.Engine` as
#: JSON. Set by the runner on the ``harbor run`` subprocess, read here.
#:
#: An environment variable rather than a constructor argument because Harbor
#: instantiates the agent class itself; this is the one channel that survives
#: that boundary without ArenaBench having to fork Harbor's factory.
ARENA_ENGINE_ENV = "ARENABENCH_ENGINE_JSON"

#: Root keys Stella's trusted-launcher seam accepts. Anything else makes the
#: engine reject the config outright (it fails closed on unknown root fields),
#: so this list is a hard schema boundary and not a style preference.
_ENGINE_ROOT_FIELDS = frozenset(
    {
        "default_model",
        "pipeline_judge_model",
        "pipeline_worker_model",
        "pipeline_triage_model",
        "allowed_models",
        "auto_mode",
        "effort_auto",
        "reasoning_auto",
        "headless_scope_bypass",
        "agents",
    }
)

_ROLE_TO_ROOT_MODEL_KEY = {
    "worker": "pipeline_worker_model",
    "judge": "pipeline_judge_model",
    "triage": "pipeline_triage_model",
}


def _canonical(posture: dict[str, Any]) -> tuple[str, str]:
    """Canonical JSON and its SHA-256, byte-identical to the base adapter's."""
    normalized = json.dumps(
        posture, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    )
    return normalized, hashlib.sha256(normalized.encode("utf-8")).hexdigest()


def _qualify(model: str, *, like: str) -> str:
    """Give ``model`` a provider prefix if it lacks one, borrowing ``like``'s.

    A role override is usually typed as a bare slug (``deepseek/deepseek-v4``)
    while the worker carries its route (``openrouter/deepseek/deepseek-v4``).
    Pinning a role to an unrouted slug silently drops the pin, so infer the
    provider from the model that *is* routed rather than failing on a papercut.
    """
    model = model.strip()
    if not model:
        return ""
    provider = like.split("/", 1)[0] if "/" in like else ""
    if provider and not model.startswith(f"{provider}/"):
        return f"{provider}/{model}"
    return model


def arena_posture(
    model: str, engine: Engine | None = None
) -> tuple[dict[str, Any], str, str]:
    """Build a frozen engine posture from a contestant's engine config.

    Shares the benchmark posture's *shape* — every automatic selector off, an
    explicit setting per role, nothing left to drift between Stella versions —
    while letting the values vary, which is the whole reason a contest exists.

    ``headless_scope_bypass`` stays on for the same reason the benchmark
    posture sets it: a task container is disposable and the budget cap is the
    real guard, so scope review has nothing to protect and nobody to ask. Left
    off, any plan over five steps ends the run and the contestant scores zero
    for a reason that has nothing to do with its ability.
    """
    selected = model.strip()
    if not selected or "/" not in selected:
        raise ValueError("arena model must be a non-empty provider/model spec")
    engine = engine or Engine()

    allowed = [selected]
    posture: dict[str, Any] = {
        "default_model": selected,
        "auto_mode": "off",
        "effort_auto": "off",
        "reasoning_auto": "off",
        "headless_scope_bypass": "on",
    }

    agents: dict[str, dict[str, Any]] = {}
    for role in ("default", "worker", "judge", "triage"):
        override = engine.role(role)
        resolved = engine.effective_role(role)
        if role == "triage" and override.effort is None and override.reasoning is None:
            # Triage does not inherit the engine baseline. It emits a
            # three-line classification and never touches the workspace, so
            # raising it changes what the agent *is* rather than what it was
            # allowed to spend — and at a high tier it silently bills a
            # reasoning pass per turn for a decision that needs none. An
            # operator who genuinely wants an expensive triage still gets it
            # by setting the role explicitly; this only governs the default.
            entry: dict[str, Any] = {"effort": "low", "reasoning": "off"}
        else:
            entry = {
                "effort": resolved.effort or "high",
                "reasoning": "on" if resolved.reasoning else "off",
            }
        if resolved.max_tokens:
            entry["params"] = {"max_tokens": int(resolved.max_tokens)}
        agents[role] = entry

        override_model = engine.role(role).model
        if override_model:
            qualified = _qualify(override_model, like=selected)
            root_key = _ROLE_TO_ROOT_MODEL_KEY.get(role)
            if root_key:
                # The flat root key, not `agents.<role>.model` — both resolve,
                # but the flat one is what `stella config` reports as the
                # role's origin, so the disclosed posture and the engine's own
                # account of its wiring name the same field.
                posture[root_key] = qualified
            if qualified not in allowed:
                allowed.append(qualified)

    posture["agents"] = agents
    posture["allowed_models"] = allowed

    unknown = set(posture) - _ENGINE_ROOT_FIELDS
    if unknown:  # pragma: no cover - guards a future edit, not a runtime path
        raise ValueError(
            "arena posture emitted root keys Stella's trusted seam rejects: "
            + ", ".join(sorted(unknown))
        )

    normalized, digest = _canonical(posture)
    return posture, normalized, digest


def _engine_from_env() -> Engine | None:
    """Parse the contestant engine the runner handed us, or ``None``.

    ``None`` means "no arena configuration present", and the class then behaves
    exactly like the stock benchmark adapter. Importing this module must never
    change a run that did not ask for it.
    """
    raw = os.environ.get(ARENA_ENGINE_ENV)
    if not raw:
        return None
    try:
        parsed = json.loads(raw)
    except ValueError:
        return None
    return Engine.from_json(parsed) if isinstance(parsed, dict) else None


def _load_base_agent() -> type:
    """Import ``stella_harbor.StellaAgent`` with a legible failure.

    Deferred to call time so ``import arenabench`` works on a machine that has
    never built Stella — the web UI needs to list Stella as an available agent
    long before anything tries to launch it.
    """
    try:
        from stella_harbor import StellaAgent
    except ImportError as exc:  # pragma: no cover - environment-dependent
        raise ImportError(
            "arenabench's Stella contestant needs the `stella_harbor` adapter "
            "on PYTHONPATH. Point ARENABENCH_STELLA_ADAPTER at "
            "<stella>/bench/harbor_adapter, or install it with "
            "`uv sync --project <stella>/bench/harbor_adapter`."
        ) from exc
    return StellaAgent


try:  # pragma: no cover - exercised by whichever environment imports this
    _Base = _load_base_agent()
except ImportError:  # pragma: no cover
    _Base = object


class ArenaStellaAgent(_Base):  # type: ignore[misc,valid-type]
    """Stella, configured by a contest rather than by the frozen protocol.

    Overrides exactly one method. Everything that makes the base adapter
    trustworthy — the anonymous-fd credential handoff, the container config
    verification, the refusal to forward unregistered knobs — is inherited,
    including the exec-boundary check that recomputes this posture and refuses
    the run if it does not match what was recorded for the trial.
    """

    def _build_engine_posture(
        self, model: str, *, witness_author: str | None
    ) -> tuple[dict[str, Any], str, str]:
        engine = _engine_from_env()
        if engine is None:
            # No arena config: defer to the frozen benchmark posture, so this
            # class is a no-op rather than a silent reconfiguration.
            return super()._build_engine_posture(model, witness_author=witness_author)
        posture, normalized, digest = arena_posture(model, engine)
        if witness_author is not None:
            # A witness author pinned by the host still wins the judge seat:
            # it is the only way to run the authored-witness tier, and the base
            # adapter has already validated that it is a real, independent,
            # same-provider model.
            posture["pipeline_judge_model"] = witness_author
            allowed = list(posture.get("allowed_models") or [])
            if witness_author not in allowed:
                allowed.append(witness_author)
            posture["allowed_models"] = allowed
            normalized, digest = _canonical(posture)
        return posture, normalized, digest
