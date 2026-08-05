# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""``arenabench.toml`` — a match as a file.

A match is otherwise a thing you assemble in a browser and lose. This module
makes it a document: readable, diffable, committable, and runnable without a
UI at all. That is what CI needs — a repository of pre-agreed matches that run
on a schedule and always mean the same thing.

**No secrets, ever.** A template names the environment variables each seat
*requires*; it never carries their values. That is the property that makes the
file safe to commit, and it fails loudly (before any container starts) when a
variable is missing rather than quietly running an unauthenticated arm whose
zero score looks like a result. Values come from the process environment, a
`.env` the operator pastes into the UI, or CI's own secret store.

**Terminology.** The file says ``verifier`` for the role that checks the
worker's output, and so does Stella's own engine config
(``pipeline_verifier_model``) — the two vocabularies agree, so there is nothing
to translate. Templates written against the older spelling ``judge`` still
load: :data:`_ROLE_ALIASES` accepts it on the way in. Nothing writes it back
out, so a match dumped today spells the role the way :data:`.ROLES` does.

Round trip::

    spec = load_match(Path("matches/glm-headtohead.toml"))
    text = dump_match(spec)          # back to TOML, comments and all
"""

from __future__ import annotations

import tomllib
from dataclasses import replace
from pathlib import Path
from typing import Any

from .agents import resolve_agent
from .model import ARENA_COLORS, Contestant, Engine, MatchSpec, RoleConfig

__all__ = [
    "MatchTemplateError",
    "dump_match",
    "load_match",
    "match_from_toml",
    "match_to_toml_dict",
    "required_env",
]

#: Retired role spellings accepted on read, mapped to their member of
#: :data:`.ROLES`. Read-only on purpose: a committed template that predates the
#: rename must keep loading, but nothing should keep *emitting* a name the rest
#: of the codebase no longer uses, or the old spelling never dies.
_ROLE_ALIASES = {"judge": "verifier"}


class MatchTemplateError(ValueError):
    """A template that cannot be turned into a runnable match.

    Carries every problem at once rather than the first one: an operator
    fixing a committed file should see the whole list in one CI run.
    """

    def __init__(self, problems: list[str]) -> None:
        self.problems = problems
        super().__init__("; ".join(problems))


# --------------------------------------------------------------------------
# reading
# --------------------------------------------------------------------------


def _role_key(name: str) -> str:
    """Normalise a role name from a template to its member of :data:`.ROLES`."""
    return _ROLE_ALIASES.get(name.strip().lower(), name.strip().lower())


def _engine_from_toml(raw: dict[str, Any], problems: list[str], where: str) -> Engine:
    roles_raw = raw.get("roles")
    roles: dict[str, RoleConfig] = {}
    if isinstance(roles_raw, dict):
        for name, entry in roles_raw.items():
            if not isinstance(entry, dict):
                problems.append(f"{where}: role {name!r} must be a table")
                continue
            role = RoleConfig.from_json(entry)
            if not role.is_empty:
                roles[_role_key(name)] = role

    return Engine(
        api=str(raw.get("api") or "openrouter"),
        model=str(raw.get("model") or ""),
        reasoning=bool(raw.get("reasoning", True)),
        effort=str(raw.get("effort") or "high"),
        base_url=(str(raw["base_url"]).strip() or None) if raw.get("base_url") else None,
        budget_usd=(
            float(raw["budget_usd"]) if raw.get("budget_usd") not in (None, "") else None
        ),
        max_tokens=(
            int(raw["max_tokens"]) if raw.get("max_tokens") not in (None, "") else None
        ),
        roles=roles,
    )


def match_from_toml(data: dict[str, Any], *, match_id: str | None = None) -> MatchSpec:
    """Build a :class:`MatchSpec` from parsed TOML.

    Raises :class:`MatchTemplateError` listing every problem found, so a bad
    committed template reports all of its faults in one CI run.
    """
    problems: list[str] = []

    match_table = data.get("match") if isinstance(data.get("match"), dict) else data
    dataset = str(match_table.get("dataset") or "").strip()
    if not dataset:
        problems.append("match.dataset is required")

    raw_contestants = data.get("contestant")
    if isinstance(raw_contestants, dict):  # a single [contestant] table
        raw_contestants = [raw_contestants]
    if not isinstance(raw_contestants, list) or not raw_contestants:
        problems.append("at least one [[contestant]] is required")
        raw_contestants = []

    contestants: list[Contestant] = []
    for seat, entry in enumerate(raw_contestants):
        if not isinstance(entry, dict):
            problems.append(f"contestant {seat + 1} must be a table")
            continue
        where = f"contestant {seat + 1}"
        agent = str(entry.get("agent") or "").strip()
        if not agent:
            problems.append(f"{where}: agent is required")
        else:
            try:
                resolve_agent(agent)
            except KeyError as exc:
                problems.append(f"{where}: {exc.args[0] if exc.args else exc}")

        engine_raw = entry.get("engine")
        if not isinstance(engine_raw, dict):
            problems.append(f"{where}: an [contestant.engine] table is required")
            engine_raw = {}
        engine = _engine_from_toml(engine_raw, problems, where)
        if not engine.model:
            problems.append(f"{where}: engine.model is required")

        contestants.append(
            Contestant(
                id=str(entry.get("id") or f"seat{seat + 1}"),
                name=str(entry.get("name") or agent or f"seat {seat + 1}"),
                agent=agent or "stella",
                engine=engine,
                env={},  # never from a file — see module docstring
                color=str(entry.get("color") or ARENA_COLORS[seat % len(ARENA_COLORS)]),
            )
        )

    if problems:
        raise MatchTemplateError(problems)

    spec = MatchSpec.from_json(
        {
            "id": match_id,
            "name": match_table.get("name"),
            "dataset": dataset,
            "tasks": list(match_table.get("tasks") or []),
            "contestants": [],  # filled below, already validated
            "attempts": match_table.get("attempts", 1),
            "concurrency": match_table.get("concurrency", 1),
            "record_video": match_table.get("record_video", False),
            "setup_timeout_multiplier": match_table.get("setup_timeout_multiplier", 1.0),
        }
    )
    return replace(spec, contestants=tuple(contestants))


def load_match(path: Path, *, match_id: str | None = None) -> MatchSpec:
    """Read and validate an ``arenabench.toml``."""
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except OSError as exc:
        raise MatchTemplateError([f"cannot read {path}: {exc}"]) from exc
    except tomllib.TOMLDecodeError as exc:
        raise MatchTemplateError([f"{path} is not valid TOML: {exc}"]) from exc
    return match_from_toml(data, match_id=match_id)


def required_env(
    spec: MatchSpec, declared: dict[str, list[str]] | None = None
) -> dict[str, list[str]]:
    """Environment variables each seat needs, by contestant id.

    Derived from each seat's provider unless the template declared its own
    list. This is what turns "no secrets in the file" from a restriction into
    a contract: the template still says exactly what must be supplied.
    """
    from .agents import credential_env_for, resolve_agent

    out: dict[str, list[str]] = {}
    for contestant in spec.contestants:
        explicit = (declared or {}).get(contestant.id)
        if explicit:
            out[contestant.id] = list(explicit)
            continue
        candidates = list(credential_env_for(contestant.engine.api))
        agent_spec = resolve_agent(contestant.agent)
        for name in agent_spec.token_env + agent_spec.alt_credential_env:
            if name not in candidates:
                candidates.append(name)
        out[contestant.id] = candidates
    return out


# --------------------------------------------------------------------------
# writing
# --------------------------------------------------------------------------


def _toml_value(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return repr(value)
    if isinstance(value, (list, tuple)):
        if not value:
            return "[]"
        inner = ", ".join(_toml_value(v) for v in value)
        return f"[{inner}]" if len(inner) <= 68 else "[\n  " + ",\n  ".join(
            _toml_value(v) for v in value
        ) + ",\n]"
    text = str(value).replace("\\", "\\\\").replace('"', '\\"')
    return f'"{text}"'


def _line(key: str, value: Any) -> str:
    return f"{key} = {_toml_value(value)}"


def match_to_toml_dict(spec: MatchSpec) -> dict[str, Any]:
    """The template as plain data — useful for tests and JSON round trips."""
    return {
        "match": {
            "name": spec.name,
            "dataset": spec.dataset,
            "tasks": list(spec.tasks),
            "attempts": spec.attempts,
            "concurrency": spec.concurrency,
            "record_video": spec.record_video,
            "setup_timeout_multiplier": spec.setup_timeout_multiplier,
        },
        "contestant": [
            {
                "id": c.id,
                "name": c.name,
                "agent": c.agent,
                "color": c.color,
                "engine": {
                    **{
                        k: v
                        for k, v in c.engine.to_json().items()
                        if k not in ("roles", "qualified_model") and v is not None
                    },
                    "roles": {
                        name: {
                            k: v for k, v in role.to_json().items() if v is not None
                        }
                        for name, role in c.engine.roles.items()
                    },
                },
            }
            for c in spec.contestants
        ],
    }


def dump_match(spec: MatchSpec, env_by_seat: dict[str, list[str]] | None = None) -> str:
    """Render a match as commented, hand-editable ``arenabench.toml``.

    Written by hand rather than by a library because ``tomllib`` is read-only
    and ArenaBench takes no dependencies — and because a generated file people
    are meant to *edit* benefits from the comments a serializer would strip.
    """
    env_by_seat = env_by_seat or required_env(spec)
    out: list[str] = [
        "# arenabench match template",
        "#",
        "# Run it:      arenabench run this-file.toml",
        "# Or upload it in the web UI instead of walking the wizard.",
        "#",
        "# This file contains NO secrets. Each seat declares the environment",
        "# variables it needs by name; values come from the environment (or CI's",
        "# secret store) at launch. A missing one fails the match immediately",
        "# rather than running an unauthenticated arm that scores zero.",
        "",
        "[match]",
        _line("name", spec.name),
        _line("dataset", spec.dataset),
    ]
    out.append("# Empty list = run the whole dataset.")
    out.append(_line("tasks", list(spec.tasks)))
    out += [
        _line("attempts", spec.attempts),
        "# Concurrent trials PER CONTESTANT. Each task container wants ~2GB, so",
        "# 2 seats at concurrency 2 needs ~8GB of Docker memory before recorders.",
        _line("concurrency", spec.concurrency),
        "# Real MP4 screen capture per trial (needs the recorder image built).",
        _line("record_video", spec.record_video),
        "# Scales the agent-INSTALL budget. Agents that npm-install themselves",
        "# into an emulated container need >1 here or they never start.",
        _line("setup_timeout_multiplier", spec.setup_timeout_multiplier),
        "",
    ]

    for contestant in spec.contestants:
        spec_agent = resolve_agent(contestant.agent)
        out += [
            "[[contestant]]",
            _line("id", contestant.id),
            _line("name", contestant.name),
            _line("agent", contestant.agent),
            _line("color", contestant.color),
            "",
            "  [contestant.engine]",
        ]
        engine = contestant.engine
        out.append("  " + _line("api", engine.api))
        out.append("  " + _line("model", engine.model))
        out.append("  " + _line("reasoning", engine.reasoning))
        out.append("  " + _line("effort", engine.effort))
        if engine.base_url:
            out.append("  " + _line("base_url", engine.base_url))
        elif getattr(spec_agent, "base_url_env", None):
            # This agent *can* be routed off its default endpoint, so leave the
            # knob visible-but-commented rather than silently absent.
            out.append('  # base_url = "https://..."   # route this seat elsewhere')
        if engine.max_tokens is not None:
            out.append("  " + _line("max_tokens", engine.max_tokens))
        if engine.budget_usd is not None:
            out.append("  " + _line("budget_usd", engine.budget_usd))

        if engine.roles:
            out.append("")
            out.append("  # Per-role overrides. Blank inherits the engine baseline above.")
            for name, role in engine.roles.items():
                out.append(f"    [contestant.engine.roles.{name}]")
                for key, value in role.to_json().items():
                    if value is not None:
                        out.append("    " + _line(key, value))
                out.append("")

        needed = env_by_seat.get(contestant.id) or []
        out += [
            "",
            "  [contestant.env]",
            "  # Names only — never values. Supplied at launch.",
            "  " + _line("required", needed),
            "",
        ]

    return "\n".join(out).rstrip() + "\n"
