# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The vocabulary of a contest.

A **match** is one benchmark run: a dataset, a chosen subset of its tasks, and
N **contestants**. A contestant is an *agent* (the CLI under test) plus an
*engine* (the API/provider/model/reasoning/effort it is pinned to) plus its own
environment. Every contestant runs the same tasks, so a match is a grid:
``len(tasks) x len(contestants)`` trials.

Nothing here does I/O. These are the types the HTTP API speaks, the runner
executes, and the scoreboard aggregates — kept separate from all three so the
contest's shape can be tested without Docker, a network, or a model key.

Design note on metrics: every scoreboard dimension declares its own
*direction*. "Winning" is not a property of a number, it is a property of a
number **and** what the number means — 2 million cache-read tokens is a triumph
and 2 million output tokens is a bill. :data:`DIMENSIONS` is the single place
that judgment lives, so the UI never has to re-derive it.
"""

from __future__ import annotations

import re
import uuid
from collections.abc import Iterable
from dataclasses import dataclass, field, replace
from typing import Any, Literal

__all__ = [
    "ARENA_COLORS",
    "DIMENSIONS",
    "DIMENSIONS_BY_KEY",
    "EFFORTS",
    "ROLES",
    "Contestant",
    "Dimension",
    "Engine",
    "MatchSpec",
    "RoleConfig",
    "parse_dotenv",
    "slugify",
]

# --------------------------------------------------------------------------
# Scoreboard dimensions
# --------------------------------------------------------------------------

Direction = Literal["higher", "lower", "neutral"]


@dataclass(frozen=True)
class Dimension:
    """One column of the scoreboard.

    ``direction`` is what makes a leaderboard possible: ``higher`` means a
    bigger number wins the dimension, ``lower`` means a smaller one does, and
    ``neutral`` means the number is reported but never crowns anyone (it is
    diagnostic, not competitive).
    """

    key: str
    label: str
    direction: Direction
    unit: str
    #: Short help shown under the metric in the UI.
    blurb: str

    def better(self, left: float, right: float) -> bool:
        """Is ``left`` a better score than ``right`` on this dimension?"""
        if self.direction == "higher":
            return left > right
        if self.direction == "lower":
            return left < right
        return False


#: The seven primary dimensions of a contest, in scoreboard order.
#:
#: ``cache_read`` is ``higher``-is-better on purpose: cache reads are prompt
#: tokens you did not pay full price for, so a contestant that reads more cache
#: is being *cheaper*, not lazier. ``cache_write`` is ``neutral`` — writes are a
#: real cost paid to make future reads possible, so neither more nor less is
#: self-evidently better, and crowning either direction would be a lie the
#: scoreboard tells confidently.
DIMENSIONS: tuple[Dimension, ...] = (
    Dimension(
        "solve_rate",
        "Solve Rate",
        "higher",
        "%",
        "verifier rewards / trials completed",
    ),
    Dimension("clock_time", "Clock Time", "lower", "s", "wall-clock across all trials"),
    Dimension("total_cost", "Total Cost", "lower", "$", "provider spend across all trials"),
    Dimension("tokens_in", "Tokens In", "lower", "tok", "uncached prompt tokens billed"),
    Dimension("tokens_out", "Tokens Out", "lower", "tok", "completion tokens billed"),
    Dimension("cache_read", "Cache Read", "higher", "tok", "prompt tokens served from cache"),
    Dimension("cache_write", "Cache Write", "neutral", "tok", "prompt tokens written to cache"),
)

DIMENSIONS_BY_KEY: dict[str, Dimension] = {d.key: d for d in DIMENSIONS}

#: Per-contestant accent colours, assigned by seat. Chosen to stay distinct
#: for the most common forms of colour blindness (no red/green-only pairs) and
#: to survive on the arena's near-black background.
ARENA_COLORS: tuple[str, ...] = (
    "#FFB000",  # amber
    "#37D5F2",  # cyan
    "#C77DFF",  # violet
    "#5CE68A",  # mint
    "#FF6B9D",  # rose
    "#FFE066",  # citron
    "#7AA2F7",  # periwinkle
    "#FF8C42",  # ember
)


# --------------------------------------------------------------------------
# Contestants
# --------------------------------------------------------------------------


def slugify(text: str) -> str:
    """A filesystem- and job-name-safe slug.

    Harbor job names become directory names, so this is deliberately strict:
    lowercase, ``[a-z0-9-]`` only, no leading/trailing or repeated dashes.
    """
    slug = re.sub(r"[^a-z0-9]+", "-", text.strip().lower()).strip("-")
    return slug or "unnamed"


#: Effort tiers, weakest to strongest. Ordered so a UI can render a slider and
#: a report can say "the arms differed by two tiers" rather than "high vs max".
EFFORTS: tuple[str, ...] = ("low", "medium", "high", "xhigh", "max")

#: The pipeline roles a contestant may configure independently. ``default`` is
#: the interactive/step loop; ``worker`` does the task; ``verifier`` verifies it;
#: ``triage`` classifies the request. Agents without a pipeline ignore all but
#: ``default`` and say so in :attr:`AgentSpec.honours`.
ROLES: tuple[str, ...] = ("default", "worker", "verifier", "triage")


@dataclass(frozen=True)
class RoleConfig:
    """Per-role overrides inside a pipeline.

    Every field is optional and ``None`` means *inherit* — from the engine's
    top-level setting, and ultimately from the provider default. That matters
    for honesty as much as ergonomics: a role that inherits is recorded as
    inheriting, so a posture never claims to pin something it left open.
    """

    model: str | None = None
    reasoning: bool | None = None
    effort: str | None = None
    max_tokens: int | None = None

    def to_json(self) -> dict[str, Any]:
        return {
            "model": self.model,
            "reasoning": self.reasoning,
            "effort": self.effort,
            "max_tokens": self.max_tokens,
        }

    @classmethod
    def from_json(cls, raw: dict[str, Any]) -> RoleConfig:
        def _opt_bool(value: Any) -> bool | None:
            if value in (None, "", "inherit"):
                return None
            if isinstance(value, str):
                return value.strip().lower() in ("on", "true", "yes", "1")
            return bool(value)

        def _opt_int(value: Any) -> int | None:
            if value in (None, ""):
                return None
            try:
                return int(value)
            except (TypeError, ValueError):
                return None

        return cls(
            model=(str(raw["model"]).strip() or None) if raw.get("model") else None,
            reasoning=_opt_bool(raw.get("reasoning")),
            effort=(str(raw["effort"]).strip() or None) if raw.get("effort") else None,
            max_tokens=_opt_int(raw.get("max_tokens")),
        )

    @property
    def is_empty(self) -> bool:
        return (
            self.model is None
            and self.reasoning is None
            and self.effort is None
            and self.max_tokens is None
        )


@dataclass(frozen=True)
class Engine:
    """How one contestant's model calls are pinned — the full engine config.

    The top-level ``api``/``model``/``reasoning``/``effort`` are the baseline
    every role inherits. ``roles`` then overrides individual pipeline stages,
    which is what makes a Stella contestant a *pipeline* configuration rather
    than a single model choice: worker on one model at ``xhigh``, verifier on a
    different family for independence, triage cheap and thinking-off.

    Agents that are a single model loop simply have no role overrides, and the
    UI hides the section for them.
    """

    #: Provider API family: ``openrouter``, ``anthropic``, ``zai``, ``openai``…
    api: str = "openrouter"
    #: Model id as the provider expects it, e.g. ``z-ai/glm-5.2``.
    model: str = ""
    #: Whether extended reasoning / thinking is requested at all.
    reasoning: bool = True
    #: Reasoning effort when ``reasoning`` is on.
    effort: str = "high"
    #: Explicit base URL override. ``None`` uses the api's default route.
    base_url: str | None = None
    #: Per-trial USD target handed to the agent, if it supports one.
    budget_usd: float | None = None
    #: Output-token ceiling per step. Left ``None`` to take the agent's own
    #: default — but see the Stella adapter's note on why a low cap plus high
    #: effort is the classic way to buy reasoning that cannot fit an answer.
    max_tokens: int | None = None
    #: Per-role overrides, keyed by a member of :data:`ROLES`.
    roles: dict[str, RoleConfig] = field(default_factory=dict)

    @property
    def qualified_model(self) -> str:
        """The ``provider/model`` string Harbor's ``--model`` expects.

        Idempotent: a model already carrying its api prefix is left alone, so
        an operator can paste either ``glm-5.2`` or
        ``openrouter/z-ai/glm-5.2`` and get the same run.
        """
        model = self.model.strip()
        if not model:
            return ""
        if model.startswith(f"{self.api}/"):
            return model
        return f"{self.api}/{model}"

    @property
    def bare_model(self) -> str:
        """The model id as the *provider* publishes it, with no api prefix.

        The inverse of :attr:`qualified_model`, and the form an agent needs
        when it has been pointed at a provider's endpoint directly rather than
        routed there by Harbor: z.ai is handed ``glm-5.2`` and has never heard
        of ``zai/glm-5.2``.

        Only the leading ``<api>/`` comes off. An OpenRouter model keeps its
        vendor segment — ``openrouter/z-ai/glm-5.2`` bares to ``z-ai/glm-5.2``,
        not ``glm-5.2`` — because that segment is part of the id the provider
        itself publishes, not a route ArenaBench added.
        """
        model = self.model.strip()
        prefix = f"{self.api}/"
        return model[len(prefix) :] if model.startswith(prefix) else model

    def role(self, name: str) -> RoleConfig:
        return self.roles.get(name, RoleConfig())

    def effective_role(self, name: str) -> RoleConfig:
        """A role with inheritance resolved against the engine baseline."""
        override = self.role(name)
        return RoleConfig(
            model=override.model or None,
            reasoning=self.reasoning if override.reasoning is None else override.reasoning,
            effort=override.effort or self.effort,
            max_tokens=override.max_tokens or self.max_tokens,
        )

    def to_json(self) -> dict[str, Any]:
        return {
            "api": self.api,
            "model": self.model,
            "qualified_model": self.qualified_model,
            "reasoning": self.reasoning,
            "effort": self.effort,
            "base_url": self.base_url,
            "budget_usd": self.budget_usd,
            "max_tokens": self.max_tokens,
            "roles": {name: role.to_json() for name, role in self.roles.items()},
        }

    @classmethod
    def from_json(cls, raw: dict[str, Any]) -> Engine:
        raw_roles = raw.get("roles") if isinstance(raw.get("roles"), dict) else {}
        roles: dict[str, RoleConfig] = {}
        for name in ROLES:
            entry = raw_roles.get(name)
            if isinstance(entry, dict):
                role = RoleConfig.from_json(entry)
                if not role.is_empty:
                    roles[name] = role
        return cls(
            api=str(raw.get("api") or "openrouter"),
            model=str(raw.get("model") or ""),
            reasoning=bool(raw.get("reasoning", True)),
            effort=str(raw.get("effort") or "high"),
            base_url=(str(raw["base_url"]) or None) if raw.get("base_url") else None,
            budget_usd=(
                float(raw["budget_usd"]) if raw.get("budget_usd") not in (None, "") else None
            ),
            max_tokens=(
                int(raw["max_tokens"]) if raw.get("max_tokens") not in (None, "") else None
            ),
            roles=roles,
        )

    @property
    def label(self) -> str:
        """A one-line human description, e.g. ``glm-5.2 · xhigh · +2 roles``."""
        short = self.model.rsplit("/", 1)[-1] or "unset"
        think = self.effort if self.reasoning else "no-reasoning"
        extra = f" · +{len(self.roles)} roles" if self.roles else ""
        return f"{short} · {think}{extra}"


@dataclass(frozen=True)
class Contestant:
    """One competitor in a match: an agent, an engine, and an environment.

    ``env`` is the parsed contents of the ``.env`` the operator pasted for this
    seat. It is per-contestant by design — a match routinely pits two different
    providers against each other, so a single shared credential set could not
    express it. It is also the reason nothing here is ever logged verbatim; see
    :meth:`redacted`.
    """

    id: str
    name: str
    #: Which agent adapter drives this seat: ``stella``, ``claude-code``, …
    agent: str
    engine: Engine
    env: dict[str, str] = field(default_factory=dict)
    color: str = ARENA_COLORS[0]

    @property
    def slug(self) -> str:
        return slugify(self.name)

    def redacted(self) -> dict[str, Any]:
        """JSON safe to hand to a browser or write into a manifest.

        Env **keys** are disclosed (an operator needs to see that
        ``ANTHROPIC_API_KEY`` was set for seat 2) but values never are.
        """
        return {
            "id": self.id,
            "name": self.name,
            "slug": self.slug,
            "agent": self.agent,
            "engine": self.engine.to_json(),
            "engine_label": self.engine.label,
            "env_keys": sorted(self.env),
            "color": self.color,
        }

    @classmethod
    def from_json(cls, raw: dict[str, Any], seat: int = 0) -> Contestant:
        name = str(raw.get("name") or "").strip()
        agent = str(raw.get("agent") or "stella").strip()
        env = raw.get("env")
        if isinstance(env, str):
            env = parse_dotenv(env)
        elif isinstance(env, dict):
            env = {str(k): str(v) for k, v in env.items()}
        else:
            env = {}
        engine = Engine.from_json(raw.get("engine") or {})
        return cls(
            id=str(raw.get("id") or uuid.uuid4().hex[:12]),
            name=name or f"{agent}-{seat + 1}",
            agent=agent,
            engine=engine,
            env=env,
            color=str(raw.get("color") or ARENA_COLORS[seat % len(ARENA_COLORS)]),
        )


@dataclass(frozen=True)
class MatchSpec:
    """A complete, launchable contest."""

    id: str
    name: str
    #: Registry key of the dataset, e.g. ``terminal-bench-2.1``.
    dataset: str
    #: Task names to run. Empty means "the whole dataset".
    tasks: tuple[str, ...]
    contestants: tuple[Contestant, ...]
    attempts: int = 1
    concurrency: int = 1
    #: Scales Harbor's agent-*setup* budget — installing the agent's own
    #: toolchain into each container, before the task begins.
    #:
    #: It is separate from the agent timeout because the two measure
    #: different things, and only one of them is about coding. An agent
    #: that installs a Node toolchain per trial needs the network for
    #: minutes before it starts; one that uploads a static binary needs
    #: seconds. On a slow link or a contended host the first blows the
    #: default budget and scores nothing, which is a fact about its
    #: installer rather than its ability — so the operator gets a knob
    #: instead of a mystery.
    setup_timeout_multiplier: float = 1.0
    #: Capture a real MP4 screen recording per trial (see :mod:`.recorder`).
    record_video: bool = False

    def to_json(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "name": self.name,
            "dataset": self.dataset,
            "tasks": list(self.tasks),
            "contestants": [c.redacted() for c in self.contestants],
            "attempts": self.attempts,
            "concurrency": self.concurrency,
            "setup_timeout_multiplier": self.setup_timeout_multiplier,
            "record_video": self.record_video,
        }

    @classmethod
    def from_json(cls, raw: dict[str, Any]) -> MatchSpec:
        contestants = tuple(
            Contestant.from_json(entry, seat)
            for seat, entry in enumerate(raw.get("contestants") or [])
        )
        contestants = _disambiguate(contestants)
        match_id = str(raw.get("id") or uuid.uuid4().hex[:12])
        return cls(
            id=match_id,
            name=str(raw.get("name") or "").strip() or f"match-{match_id}",
            dataset=str(raw.get("dataset") or ""),
            tasks=tuple(str(t) for t in (raw.get("tasks") or [])),
            contestants=contestants,
            attempts=max(1, int(raw.get("attempts") or 1)),
            concurrency=max(1, int(raw.get("concurrency") or 1)),
            setup_timeout_multiplier=max(
                1.0, float(raw.get("setup_timeout_multiplier") or 1.0)
            ),
            record_video=bool(raw.get("record_video")),
        )

    def validate(self) -> list[str]:
        """Every problem with this spec, so the UI can show them all at once."""
        problems: list[str] = []
        if not self.dataset:
            problems.append("no dataset selected")
        if not self.contestants:
            problems.append("a match needs at least one contestant")
        seen: set[str] = set()
        for contestant in self.contestants:
            if contestant.slug in seen:
                problems.append(f"duplicate contestant name: {contestant.name!r}")
            seen.add(contestant.slug)
            if not contestant.engine.model:
                problems.append(f"{contestant.name}: no model selected")
        return problems


def _disambiguate(contestants: Iterable[Contestant]) -> tuple[Contestant, ...]:
    """Make contestant slugs unique.

    Two seats named "stella" are a natural thing for an operator to do when
    comparing two *engines* of the same agent — but the slug becomes a Harbor
    job directory, and Harbor refuses to reuse one. Rather than reject the
    match, suffix the collisions.
    """
    out: list[Contestant] = []
    counts: dict[str, int] = {}
    for contestant in contestants:
        slug = contestant.slug
        counts[slug] = counts.get(slug, 0) + 1
        if counts[slug] > 1:
            contestant = replace(contestant, name=f"{contestant.name} {counts[slug]}")
        out.append(contestant)
    return tuple(out)


# --------------------------------------------------------------------------
# .env parsing
# --------------------------------------------------------------------------


def parse_dotenv(text: str) -> dict[str, str]:
    """Parse pasted ``.env`` contents into a plain mapping.

    Deliberately forgiving, because this is a textarea an operator pastes into
    and a parse error there is far more annoying than a dropped comment:

    * ``export FOO=bar`` is accepted (people paste from shell profiles);
    * single or double quotes around a value are stripped;
    * a double-quoted value may span lines, which is how PEM keys and JSON
      blobs survive the round trip;
    * blank lines, ``#`` comments, and lines with no ``=`` are skipped.

    Values are never expanded or interpolated: ``FOO=$BAR`` stays literal, so
    pasting a file cannot pull in a variable from the arena's own environment.
    """
    env: dict[str, str] = {}
    lines = text.splitlines()
    index = 0
    while index < len(lines):
        line = lines[index]
        index += 1
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped.startswith("export "):
            stripped = stripped[len("export ") :].lstrip()
        key, sep, value = stripped.partition("=")
        if not sep:
            continue
        key = key.strip()
        if not key or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key):
            continue
        value = value.strip()
        # A value that opens a double quote without closing it continues onto
        # following lines verbatim — the PEM-key case.
        if value.startswith('"') and not (value.endswith('"') and len(value) > 1):
            buffer = [value[1:]]
            while index < len(lines):
                nxt = lines[index]
                index += 1
                if nxt.rstrip().endswith('"'):
                    buffer.append(nxt.rstrip()[:-1])
                    break
                buffer.append(nxt)
            env[key] = "\n".join(buffer)
            continue
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        env[key] = value
    return env
