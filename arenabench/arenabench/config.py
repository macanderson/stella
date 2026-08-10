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
from .model import (
    ARENA_COLORS,
    RESPONSIBILITIES,
    Contestant,
    Engine,
    MatchSpec,
    ResponsibilityConfig,
    RoleConfig,
    declared_cap_keys,
    declared_flag,
    is_credential_name,
)

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


def _declared_env(
    raw: dict[str, Any], problems: list[str], where: str
) -> tuple[str, ...]:
    """The ``required = [...]`` declaration of a ``[contestant.env]`` table.

    Names only, never values: any other key is a value smuggled into a file
    that promises to carry none (see module docstring), and a name outside
    :func:`.model.screen_env`'s credential shapes would let a committed
    template pull an arbitrary host variable into a seat's subprocess. Both
    are template faults, reported like any other — loudly, before any
    container starts (#1777).
    """
    names: tuple[str, ...] = ()
    for key, value in raw.items():
        if key != "required":
            problems.append(
                f"{where}: [contestant.env] carries {key!r} — names only, never "
                'values; declare `required = ["NAME"]` and supply the value in '
                "the environment (or the saved credential set) at launch"
            )
            continue
        if not isinstance(value, list) or not all(
            isinstance(item, str) for item in value
        ):
            problems.append(f"{where}: env.required must be a list of variable names")
            continue
        bad = [item for item in value if not is_credential_name(item)]
        if bad:
            problems.append(
                f"{where}: env.required names {', '.join(map(repr, bad))} are not "
                "credential-shaped and would never be honoured (see screen_env)"
            )
            continue
        names = tuple(dict.fromkeys(value))
    return names


def _engine_from_toml(raw: dict[str, Any], problems: list[str], where: str) -> Engine:
    # A per-trial ceiling is refused outright, not dropped. Dropping it would
    # run the match the author wanted UNCAPPED while their template still said
    # otherwise, and the two would disagree about what was measured — the
    # failure mode #2411 documents, in reverse. Name the key and say what to do
    # instead, because the author had a real worry and it has a real answer.
    for key in declared_cap_keys(raw):
        problems.append(
            f"{where}: engine.{key} is refused — a per-trial ceiling stops the "
            "agent where the work finishes, so the score reports our limit as "
            "its capability (#2411). Bound the spend at the provider key, "
            "which fails a run visibly instead of truncating a trial into a loss"
        )
    # Refused rather than coerced: a human wrote `bare_loop = "no"` and meant
    # something by it, and the one outcome a selector must never have is to
    # silently run the arm it was spelled to decline. `declared_flag` returns
    # None for exactly the values `bool()` would have read backwards.
    bare_loop = declared_flag(raw.get("bare_loop", False))
    if bare_loop is None:
        problems.append(
            f"{where}: engine.bare_loop must be a boolean — "
            f"{raw.get('bare_loop')!r} declares neither arm"
        )
        bare_loop = False

    # The sibling field, same reader, same refusal (#2334). Its fallback is
    # True where `bare_loop`'s is False: `reasoning` defaults ON, so the
    # shipping configuration is the one an unreadable value must not leave.
    reasoning = declared_flag(raw.get("reasoning", True))
    if reasoning is None:
        problems.append(
            f"{where}: engine.reasoning must be a boolean — "
            f"{raw.get('reasoning')!r} declares neither"
        )
        reasoning = True

    roles_raw = raw.get("roles")
    roles: dict[str, RoleConfig] = {}
    if isinstance(roles_raw, dict):
        for name, entry in roles_raw.items():
            if not isinstance(entry, dict):
                problems.append(f"{where}: role {name!r} must be a table")
                continue
            # The same refusal one level down. A role table is the older way to
            # spell an output ceiling and would otherwise be the hole left in
            # the wall the engine-level check just built.
            for key in declared_cap_keys(entry):
                problems.append(
                    f"{where}: engine.roles.{name}.{key} is refused for the same "
                    "reason as the engine-level key — no role runs under a "
                    "ceiling the comparator does not also run under (#2411)"
                )
            role = RoleConfig.from_json(entry)
            if not role.is_empty:
                roles[_role_key(name)] = role

    # `[contestant.engine.responsibilities.<name>]` (#2381). Refused rather
    # than dropped, for the reason the whole key exists: a template that
    # spells an ablation and silently does not get one produces a number
    # described by the wrong posture, which is worse than no number.
    responsibilities: dict[str, ResponsibilityConfig] = {}
    responsibilities_raw = raw.get("responsibilities")
    if responsibilities_raw is not None and not isinstance(responsibilities_raw, dict):
        problems.append(f"{where}: engine.responsibilities must be a table")
    elif isinstance(responsibilities_raw, dict):
        for name, entry in responsibilities_raw.items():
            if not isinstance(entry, dict):
                problems.append(f"{where}: responsibility {name!r} must be a table")
                continue
            if name not in RESPONSIBILITIES:
                problems.append(
                    f"{where}: {name!r} is not an ablatable responsibility — "
                    f"known ones are {', '.join(RESPONSIBILITIES)}"
                )
                continue
            if "enabled" in entry and declared_flag(entry["enabled"]) is None:
                problems.append(
                    f"{where}: responsibility {name!r} enabled must be a boolean — "
                    f"{entry['enabled']!r} declares neither arm"
                )
                continue
            row = ResponsibilityConfig.from_json(entry)
            if not row.is_empty:
                responsibilities[name] = row

    return Engine(
        api=str(raw.get("api") or "openrouter"),
        model=str(raw.get("model") or ""),
        reasoning=reasoning,
        effort=str(raw.get("effort") or "high"),
        base_url=(str(raw["base_url"]).strip() or None) if raw.get("base_url") else None,
        bare_loop=bare_loop,
        roles=roles,
        responsibilities=responsibilities,
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
        if engine.bare_loop and agent and agent != "stella":
            # Refused at parse for the same reason `sut_ref` is below (#2082):
            # a selector nothing reads is a template claiming to measure an arm
            # it never ran, and the whole point of declaring the loop on the
            # engine was that a match cannot lie about which one it used.
            problems.append(
                f"{where}: bare_loop applies only to a stella seat — "
                f"{agent!r} has no staged pipeline to switch off"
            )
        if engine.responsibilities and agent and agent != "stella":
            # The same refusal, for the same reason, on the knob that arrived
            # after it (#2381). A roster on a comparator seat parsed cleanly and
            # was dropped on the floor: the template said the arm ablated a
            # stage, the arm ran whole, and the published number described
            # neither. Asymmetry here was an oversight, not a policy — a
            # declaration that reaches nothing must fail the file, always.
            problems.append(
                f"{where}: engine.responsibilities applies only to a stella "
                f"seat — {agent!r} has no pipeline stages to reassign"
            )

        env_raw = entry.get("env")
        declared: tuple[str, ...] = ()
        if env_raw is not None:
            if not isinstance(env_raw, dict):
                problems.append(f"{where}: [contestant.env] must be a table")
            else:
                declared = _declared_env(env_raw, problems, where)

        seat_sut = entry.get("sut_ref")
        if seat_sut is not None:
            if not isinstance(seat_sut, str):
                problems.append(f"{where}: sut_ref must be a string git ref")
                seat_sut = None
            elif agent and agent != "stella":
                # Refused at parse for the same reason `MatchSpec.validate`
                # refuses it: a pin nothing reads is a template claiming to
                # race a build it never runs (#2082).
                problems.append(
                    f"{where}: sut_ref applies only to a stella seat — "
                    f"{agent!r} runs no Stella binary"
                )

        contestants.append(
            Contestant(
                id=str(entry.get("id") or f"seat{seat + 1}"),
                name=str(entry.get("name") or agent or f"seat {seat + 1}"),
                agent=agent or "stella",
                engine=engine,
                env={},  # never values from a file — see module docstring
                color=str(entry.get("color") or ARENA_COLORS[seat % len(ARENA_COLORS)]),
                required_env=declared,
                sut_ref=seat_sut.strip() if isinstance(seat_sut, str) else None,
            )
        )

    match_sut = match_table.get("sut_ref")
    if match_sut is not None and not isinstance(match_sut, str):
        problems.append("match.sut_ref must be a string git ref")
        match_sut = None

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
            "agent_timeout_multiplier": match_table.get("agent_timeout_multiplier", 1.0),
            "capture_snapshots": match_table.get("capture_snapshots", False),
            "snapshot_interval": match_table.get("snapshot_interval", 30.0),
            # Absent means `main` (MatchSpec.from_json's contract), never the
            # opt-out: a committed template that ran the project's default
            # branch before this key existed must keep meaning that (#2082).
            "sut_ref": match_sut,
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


def required_env(spec: MatchSpec) -> dict[str, list[str]]:
    """Environment variables each seat needs, by contestant id.

    Derived from each seat's provider unless the seat declared its own list
    (``[contestant.env] required = [...]``), which is then the seat's whole
    contract — declaring only the subscription token is how a template keeps a
    metered key out of a seat on purpose (#1777). This is what turns "no
    secrets in the file" from a restriction into a contract: the template
    still says exactly what must be supplied.
    """
    from .agents import credential_env_for, resolve_agent

    out: dict[str, list[str]] = {}
    for contestant in spec.contestants:
        if contestant.required_env:
            out[contestant.id] = list(contestant.required_env)
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
            "agent_timeout_multiplier": spec.agent_timeout_multiplier,
            "capture_snapshots": spec.capture_snapshots,
            "snapshot_interval": spec.snapshot_interval,
            # Written unconditionally: a template that omits the pin means
            # `main`, and the GUI's "download this match" dropping the pin was
            # exactly how a committed file silently ran a different Stella
            # than the match it came from (#2082).
            "sut_ref": spec.sut_ref,
        },
        "contestant": [
            {
                "id": c.id,
                "name": c.name,
                "agent": c.agent,
                "color": c.color,
                # The override round-trips only when the seat declared one:
                # `None` (inherit) has no TOML spelling, and inventing one
                # would freeze the match default into every seat.
                **({"sut_ref": c.sut_ref} if c.sut_ref is not None else {}),
                # The declaration round-trips; values never exist to round-trip.
                **({"env": {"required": list(c.required_env)}} if c.required_env else {}),
                "engine": {
                    **{
                        k: v
                        for k, v in c.engine.to_json().items()
                        if k not in ("roles", "responsibilities", "qualified_model")
                        and v is not None
                    },
                    "roles": {
                        name: {
                            k: v for k, v in role.to_json().items() if v is not None
                        }
                        for name, role in c.engine.roles.items()
                    },
                    # Rebuilt rather than passed through, so the inner `None`s
                    # are dropped the same way `roles` drops them. TOML has no
                    # spelling for one, and an inherited axis written out as a
                    # key would record a pin the arm never made.
                    "responsibilities": {
                        name: {k: v for k, v in row.to_json().items() if v is not None}
                        for name, row in c.engine.responsibilities.items()
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
        "# Scales the agent-EXECUTION budget (Terminal-Bench pins 900s/task).",
        "# Same multiplier for every arm keeps a head-to-head fair; disclose it",
        "# next to any number compared outside this arena.",
        _line("agent_timeout_multiplier", spec.agent_timeout_multiplier),
        "# Periodic workspace snapshots, so `arenabench flip` can find the moment",
        "# the tests started passing and how long the agent kept going afterwards.",
        _line("capture_snapshots", spec.capture_snapshots),
        _line("snapshot_interval", spec.snapshot_interval),
        "# Which Stella the seats under test run: a git ref, resolved to a commit",
        "# at launch and recorded in provenance.json. Pin a full commit id for a",
        '# result that names its own code; "" runs whatever STELLA_BINARY points',
        "# at, recorded as unverified. A seat may override this with its own",
        "# sut_ref line — that is how two Stella builds race the same tasks.",
        _line("sut_ref", spec.sut_ref),
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
        ]
        if contestant.sut_ref is not None:
            out += [
                "# This seat's own Stella build, overriding [match] sut_ref.",
                _line("sut_ref", contestant.sut_ref),
            ]
        out += [
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
        if engine.bare_loop:
            # Emitted only when true, so every existing template renders back
            # byte-identical and a saved match cannot gain an arm it never had.
            out.append("  " + _line("bare_loop", engine.bare_loop))

        if engine.roles:
            out.append("")
            out.append("  # Per-role overrides. Blank inherits the engine baseline above.")
            for name, role in engine.roles.items():
                out.append(f"    [contestant.engine.roles.{name}]")
                for key, value in role.to_json().items():
                    if value is not None:
                        out.append("    " + _line(key, value))
                out.append("")

        if engine.responsibilities:
            # The stage roster (#2381), on the same terms as `roles` above: only
            # the fields the arm set, and nothing at all when it ablated nothing,
            # so an existing template renders back byte-identical.
            #
            # Omitting this block was a silent drop on the one path whose entire
            # purpose is reproducibility: "download this match" would hand back a
            # .toml describing the full pipeline for a seat that had ablated a
            # stage, and `arenabench run it.toml` would then run a different
            # experiment than the one whose number was published.
            out.append("")
            out.append("  # Stage roster. Absent means the shipped binding, which is not `false`.")
            for name, row in engine.responsibilities.items():
                out.append(f"    [contestant.engine.responsibilities.{name}]")
                for key, value in row.to_json().items():
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
