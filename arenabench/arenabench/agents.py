# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Which agents can enter a contest, and how each one is launched.

Harbor already knows how to install and drive ~23 coding agents; ArenaBench
does not reimplement any of them. This module is a thin description layer that
answers three questions per agent:

1. **How does Harbor launch it?** Either ``--agent <slug>`` for a built-in, or
   ``--agent-import-path pkg:Class`` for one ArenaBench supplies.
2. **Which engine knobs does it actually honour?** An operator who sets
   ``effort: max`` on an agent with no effort control deserves to be told, not
   to discover it by comparing two runs that were secretly identical. That is
   what :attr:`AgentSpec.honours` is for, and the UI greys out the rest.
3. **Which credential does it need?** So the env-paste box can say
   "``ANTHROPIC_API_KEY`` is missing" before a match burns twenty minutes
   failing every trial.

The honest-reporting bias is deliberate. A contest between two agents is only
meaningful if you know which differences were real, so anything ArenaBench
cannot enforce it declares rather than silently drops.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from . import harbor
from .model import Contestant, Engine

__all__ = [
    "AGENTS",
    "API_CREDENTIALS",
    "AgentSpec",
    "credential_env_for",
    "launch_flags",
    "launch_model",
    "missing_credentials",
    "resolve_agent",
    "routes_directly",
]

#: Environment variable each provider API reads its key from, plus the base URL
#: variable where one is conventional. Used only to *check* an operator's
#: pasted env and to name what is missing — ArenaBench never invents a value.
API_CREDENTIALS: dict[str, tuple[str, ...]] = {
    "openrouter": ("OPENROUTER_API_KEY",),
    "anthropic": ("ANTHROPIC_API_KEY",),
    "openai": ("OPENAI_API_KEY",),
    "zai": ("ZAI_API_KEY",),
    "google": ("GEMINI_API_KEY", "GOOGLE_API_KEY"),
    "deepseek": ("DEEPSEEK_API_KEY",),
    "moonshot": ("MOONSHOT_API_KEY",),
    "xai": ("XAI_API_KEY",),
    "mistral": ("MISTRAL_API_KEY",),
}

#: Knob names used in :attr:`AgentSpec.honours`.
KNOB_MODEL = "model"
KNOB_REASONING = "reasoning"
KNOB_EFFORT = "effort"
KNOB_BASE_URL = "base_url"
KNOB_BUDGET = "budget"
KNOB_MAX_TOKENS = "max_tokens"
KNOB_ROLES = "roles"


@dataclass(frozen=True)
class AgentSpec:
    """A registered agent kind that can take a seat in a match."""

    slug: str
    title: str
    #: Harbor built-in agent name, e.g. ``claude-code``. Mutually exclusive
    #: with :attr:`import_path`.
    harbor_agent: str | None = None
    #: ``module:Class`` for an agent ArenaBench ships. Wins if both are set.
    import_path: str | None = None
    description: str = ""
    #: Engine knobs this agent genuinely applies. Anything absent is reported
    #: to the operator as "declared, not enforced".
    honours: frozenset[str] = field(default_factory=frozenset)
    #: ``True`` when the agent runs a multi-role pipeline worth configuring.
    has_pipeline: bool = False
    #: Extra environment this agent always needs, beyond the provider key.
    extra_env: dict[str, str] = field(default_factory=dict)
    #: Environment variable this agent reads a custom provider endpoint from,
    #: when it can be pointed at one at all. ``None`` means a base URL set on
    #: this seat is genuinely ignored, and the operator is told so.
    #:
    #: Stella deliberately leaves this ``None`` while still honouring
    #: ``base_url``: its endpoint travels through the adapter's own registered
    #: ``STELLA_BASE_URL`` seam rather than an environment variable Harbor
    #: hands to a CLI, so the two are not interchangeable.
    base_url_env: str | None = None
    #: Variables this agent will accept a bearer token in, most preferred
    #: first. Used to alias a provider-native key into the name the agent
    #: actually reads once it has been routed off its home provider.
    token_env: tuple[str, ...] = ()
    #: Variables that satisfy this agent's credential requirement but are
    #: never alias *targets*. A subscription token is not a provider bearer
    #: token: copying a provider key into it would produce a seat that
    #: cannot authenticate at all, so it counts as credentials present
    #: without ever being written to.
    alt_credential_env: tuple[str, ...] = ()

    def to_json(self) -> dict[str, Any]:
        return {
            "slug": self.slug,
            "title": self.title,
            "description": self.description,
            "honours": sorted(self.honours),
            "has_pipeline": self.has_pipeline,
            "builtin": self.harbor_agent is not None and self.import_path is None,
        }

    def unhonoured(self, engine: Engine) -> list[str]:
        """Knobs the operator set that this agent will quietly ignore.

        Only *set* knobs are reported. Telling someone that an agent ignores a
        setting they never touched is noise; telling them it ignores the one
        they just changed is the whole point.
        """
        missed: list[str] = []
        if engine.base_url and KNOB_BASE_URL not in self.honours:
            missed.append("base_url")
        if engine.budget_usd is not None and KNOB_BUDGET not in self.honours:
            missed.append("budget_usd")
        if engine.max_tokens is not None and KNOB_MAX_TOKENS not in self.honours:
            missed.append("max_tokens")
        if engine.roles and KNOB_ROLES not in self.honours:
            missed.append("per-role pipeline config")
        if not engine.reasoning and KNOB_REASONING not in self.honours:
            missed.append("reasoning:off")
        return missed


#: ArenaBench's own Stella adapter. It subclasses the AGPL ``stella_harbor``
#: adapter purely to swap the frozen benchmark posture for the contestant's
#: engine config; everything else — credential handling, ATIF, telemetry —
#: is inherited unchanged.
STELLA = AgentSpec(
    slug="stella",
    title="Stella",
    import_path="arenabench.harbor_agent:ArenaStellaAgent",
    description=(
        "Stella coding CLI, run one-shot and headless. Full pipeline "
        "configuration: per-role model, reasoning and effort."
    ),
    honours=frozenset(
        {
            KNOB_MODEL,
            KNOB_REASONING,
            KNOB_EFFORT,
            KNOB_BASE_URL,
            KNOB_BUDGET,
            KNOB_MAX_TOKENS,
            KNOB_ROLES,
        }
    ),
    has_pipeline=True,
)


def _builtin(
    slug: str,
    title: str,
    description: str = "",
    *,
    effort: bool = False,
    reasoning: bool = False,
    base_url_env: str | None = None,
    token_env: tuple[str, ...] = (),
    alt_credential_env: tuple[str, ...] = (),
) -> AgentSpec:
    """A Harbor built-in agent.

    Model selection is the one knob every Harbor agent honours, because Harbor
    passes ``--model`` itself. Effort and reasoning are opt-in per agent since
    most CLI agents expose neither.

    ``base_url_env`` implies :data:`KNOB_BASE_URL` rather than being declared
    alongside it. The two cannot drift apart that way, and drift is exactly the
    failure that matters here: an agent listed as honouring ``base_url`` with
    no variable to put it in would report a route it never took.
    """
    honours = {KNOB_MODEL}
    if effort:
        honours.add(KNOB_EFFORT)
    if reasoning:
        honours.add(KNOB_REASONING)
    if base_url_env:
        honours.add(KNOB_BASE_URL)
    return AgentSpec(
        slug=slug,
        title=title,
        harbor_agent=slug,
        description=description,
        honours=frozenset(honours),
        base_url_env=base_url_env,
        token_env=token_env,
        alt_credential_env=alt_credential_env,
    )


#: Every agent that may take a seat. Harbor built-ins come along for free; the
#: list is intentionally the same one Harbor's own factory exposes, so this
#: registry stays a description of Harbor rather than a fork of it.
AGENTS: dict[str, AgentSpec] = {
    spec.slug: spec
    for spec in (
        STELLA,
        _builtin(
            "claude-code",
            "Claude Code",
            "Anthropic's coding CLI. Routable at any Anthropic-shaped "
            "endpoint, so it can also be seated on a GLM or self-hosted model.",
            effort=True,
            reasoning=True,
            # Harbor's Claude Code agent already forwards ANTHROPIC_BASE_URL
            # into the task container and, when one is set, hands the model
            # name to that endpoint unchanged. That is what makes a non-
            # Anthropic seat possible at all.
            base_url_env="ANTHROPIC_BASE_URL",
            token_env=("ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"),
            # A Claude subscription. Harbor forwards this into the
            # container and the CLI prefers whichever auth is present, so
            # a seat carrying only this runs on the plan rather than on
            # metered credits.
            alt_credential_env=("CLAUDE_CODE_OAUTH_TOKEN",),
        ),
        _builtin("codex", "Codex CLI", "OpenAI's coding CLI.", effort=True),
        _builtin("gemini-cli", "Gemini CLI", "Google's coding CLI."),
        _builtin("opencode", "OpenCode", "Open-source multi-provider coding agent."),
        _builtin("qwen-coder", "Qwen Coder", "Alibaba's coding CLI."),
        _builtin("kimi-cli", "Kimi CLI", "Moonshot's coding CLI."),
        _builtin("cursor-cli", "Cursor CLI", "Cursor's headless agent."),
        _builtin("copilot-cli", "Copilot CLI", "GitHub Copilot in the terminal."),
        _builtin("aider", "Aider", "Git-native pair-programming agent."),
        _builtin("goose", "Goose", "Block's extensible coding agent."),
        _builtin("cline-cli", "Cline", "Cline's headless agent."),
        _builtin("rovodev-cli", "Rovo Dev", "Atlassian's coding CLI."),
        _builtin("trae-agent", "Trae", "ByteDance's coding agent."),
        _builtin("hermes", "Hermes", "Harbor's Hermes agent."),
        _builtin("mini-swe-agent", "Mini SWE-Agent", "Minimal SWE-bench scaffold."),
        _builtin("swe-agent", "SWE-Agent", "Princeton's SWE-agent scaffold."),
        _builtin("openhands", "OpenHands", "All-Hands OpenHands."),
        _builtin("terminus-2", "Terminus 2", "Harbor's reference terminal agent."),
        _builtin("nop", "No-op", "Does nothing. A free floor for the scoreboard."),
        _builtin("oracle", "Oracle", "Applies the reference solution. A ceiling."),
    )
}


def resolve_agent(slug: str) -> AgentSpec:
    """Look up an agent, defaulting to Stella for an unknown slug is *not*
    done on purpose — an unrecognised agent is an operator error worth an
    explicit failure, since the alternative is a contest that silently ran
    something other than what was asked for."""
    try:
        return AGENTS[slug]
    except KeyError:
        known = ", ".join(sorted(AGENTS))
        raise KeyError(f"unknown agent {slug!r}; registered agents: {known}") from None


def launch_flags(contestant: Contestant) -> list[str]:
    """The ``harbor run`` flags that select this contestant's agent.

    ``--agent-import-path`` was folded into ``--agent`` — which takes a
    built-in name *or* a ``module:Class`` path — in a Harbor newer than 0.6.1.
    Which flag to send is asked of the installed binary rather than derived
    from its version number, so this keeps working across the fold in both
    directions (:func:`arenabench.harbor.supports_agent_import_path`).
    """
    spec = resolve_agent(contestant.agent)
    if spec.import_path:
        if harbor.supports_agent_import_path():
            return ["--agent-import-path", spec.import_path]
        return ["--agent", spec.import_path]
    if spec.harbor_agent:
        return ["--agent", spec.harbor_agent]
    raise ValueError(f"agent {spec.slug!r} declares no launch mechanism")


def routes_directly(contestant: Contestant) -> bool:
    """Whether this seat talks to a provider endpoint of its own choosing.

    True only when the agent has somewhere to put a base URL *and* the
    operator set one. Both halves matter: an agent with no such variable
    cannot be rerouted, and an agent that was never given a URL is still on
    its home provider and must be addressed the way Harbor addresses it.
    """
    spec = resolve_agent(contestant.agent)
    return bool(spec.base_url_env and (contestant.engine.base_url or "").strip())


def launch_model(contestant: Contestant) -> str:
    """The value for ``harbor run --model``.

    Harbor routes most agents by a ``provider/model`` string and strips the
    prefix itself on the way to the provider. An agent pointed at an explicit
    endpoint is the exception: Harbor forwards the name to that endpoint
    verbatim, so an api prefix ArenaBench added would arrive at a provider
    that has never heard of it and fail *every* trial with a model-not-found —
    which reads on the scoreboard as an agent that cannot code.
    """
    engine = contestant.engine
    return engine.bare_model if routes_directly(contestant) else engine.qualified_model


def credential_env_for(api: str) -> tuple[str, ...]:
    """Candidate credential variable names for a provider API."""
    return API_CREDENTIALS.get(api.strip().lower(), ())


def missing_credentials(contestant: Contestant) -> list[str]:
    """Credential variables this contestant needs but does not have.

    Returns the *candidate set* when none of the alternatives is present — for
    Google that is ``["GEMINI_API_KEY", "GOOGLE_API_KEY"]``, meaning "any one
    of these". An empty list means the seat is credentialled.

    An agent that carries its token in its own variable adds those names to
    the set, because a seat routed off its home provider is legitimately
    credentialled either way: ``ZAI_API_KEY`` is what the provider calls it
    and ``ANTHROPIC_AUTH_TOKEN`` is what Claude Code reads it from, and
    warning about the one an operator did not use would be noise.
    """
    spec = resolve_agent(contestant.agent)
    candidates = list(credential_env_for(contestant.engine.api))
    for name in spec.token_env + spec.alt_credential_env:
        if name not in candidates:
            candidates.append(name)
    if not candidates:
        return []
    if any(contestant.env.get(name) for name in candidates):
        return []
    return candidates
