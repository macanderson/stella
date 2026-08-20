# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""One-click head-to-head presets: Stella vs Claude Code on a frontier model.

A preset is a fully configured match the setup wizard can load the way it
loads a template — same JSON shape, same applier — so "run the Sonnet 5
head-to-head" is one button and a launch click rather than eleven form
fields. Three rules give the presets their meaning, and all three are
*constructed* here rather than merely written down, so no edit to one seat
can silently break the comparison:

- **The worker is the constant.** Stella's worker model, reasoning and effort
  are copied from the Claude Code seat's engine — the same values, by
  construction. The variable under test is the agent architecture, never the
  model.
- **The verifier crosses tiers, never down.** Stella's pipeline verifier is
  the strongest available model that is not the worker: Opus 5 verifies a
  Fable 5 worker, and Fable 5 verifies everyone else. A verifier weaker than
  its worker rubber-stamps; the tier map in :data:`_VERIFIER_FOR` is the
  whole policy and the test suite pins it.
- **The seats authenticate the way each agent survives.** Claude Code runs
  first-party on the operator's Claude subscription
  (``CLAUDE_CODE_OAUTH_TOKEN``, filled fresh from the local login by
  :mod:`.claude_oauth`); Stella runs metered on OpenRouter
  (``OPENROUTER_API_KEY``). Declared names only — a preset carries no values.

The ten-task panel is `matches/fable5-cc-vs-stella-10task.toml`'s, kept
byte-identical across the three presets so their numbers are comparable to
each other and to the committed template's history: 1 easy / 6 medium /
3 hard, seven categories, no [heavy] task that would serialise the panel.
"""

from __future__ import annotations

from typing import Any

from .config import required_env
from .model import MatchSpec

__all__ = [
    "PRESETS",
    "SUPPORTED_WORKERS",
    "ContestError",
    "contest_comparability_note",
    "contest_spec",
    "normalize_worker",
    "preset_listing",
]


class ContestError(ValueError):
    """A contest was asked for that cannot be measured as asked.

    Its own type because every raise site here is a *refusal to guess*: the
    alternative in each case is a match that runs, produces numbers, and
    means something other than what the operator asked for.
    """

#: The 10-task Terminal-Bench 2.1 panel shared by every preset.
PANEL_TASKS: tuple[str, ...] = (
    "fix-git",
    "regex-log",
    "sqlite-with-gcov",
    "large-scale-text-editing",
    "openssl-selfsigned-cert",
    "nginx-request-logging",
    "query-optimize",
    "cancel-async-tasks",
    "dna-assembly",
    "write-compressor",
)

#: Worker model -> the Stella pipeline verifier for that worker. The verifier
#: is never the worker (independence) and never a weaker tier (a weak judge
#: rubber-stamps a strong worker's mistakes).
_VERIFIER_FOR: dict[str, str] = {
    "claude-sonnet-5": "claude-fable-5",
    "claude-opus-5": "claude-fable-5",
    "claude-fable-5": "claude-opus-5",
}

#: Both arms at the same effort, and `medium` specifically: measured 2026-08-03,
#: xhigh timed out on every step budget it was given; medium finished.
_EFFORT = "medium"


#: The workers a contest can be run against — the keys of the verifier tier
#: map, because a worker with no declared verifier has no honest match.
SUPPORTED_WORKERS: tuple[str, ...] = tuple(sorted(_VERIFIER_FOR))


def normalize_worker(versus_model: str) -> str:
    """Accept ``anthropic/claude-sonnet-5`` or ``claude-sonnet-5``; return bare.

    Both spellings are things an operator legitimately has in hand — the
    OpenRouter catalog publishes the namespaced one and Anthropic's API takes
    the bare one, and this preset uses *both* for the two arms. Taking either
    at the door means the caller never has to know which arm needs which.

    An unknown worker is refused rather than defaulted. The verifier tier map
    is the whole meaning of the comparison (a verifier weaker than its worker
    rubber-stamps), so guessing one would produce a match that runs and
    measures the wrong thing — the one outcome worse than an error.
    """
    worker = versus_model.strip()
    if worker.startswith("anthropic/"):
        worker = worker[len("anthropic/") :]
    if worker not in _VERIFIER_FOR:
        raise ContestError(
            f"no verifier tier is declared for worker {versus_model!r}; "
            f"supported: {', '.join(SUPPORTED_WORKERS)}. Add it to "
            "_VERIFIER_FOR (naming a model that is not weaker than the "
            "worker) before running a contest against it."
        )
    return worker


def contest_spec(
    versus_model: str,
    *,
    num_tasks: int = len(PANEL_TASKS),
    concurrency: int = 1,
    attempts: int = 1,
    record_video: bool = True,
    name: str | None = None,
) -> MatchSpec:
    """A head-to-head contest: Stella vs Claude Code, both on ``versus_model``.

    The same construction the three presets use — deliberately, because the
    seats are where a head-to-head silently dies. A Claude Code seat pointed
    at OpenRouter holds a valid key, passes the credential check, and then
    fails every trial at turn 1 with ``apiKeySource: none``, scoring 0.0
    indistinguishably from a real loss. Building the seats in one place is
    what stops a parameterised contest from re-deriving that seat by hand.

    ``num_tasks`` slices :data:`PANEL_TASKS` from the front. The full panel is
    byte-identical across presets so their numbers are comparable to each
    other and to the committed template's history; **a slice is not** — it is
    a different, easier or harder, panel. The caller is told so by
    :func:`contest_comparability_note` rather than being stopped, because a
    short smoke contest is a legitimate thing to want.
    """
    worker = normalize_worker(versus_model)
    if num_tasks < 1:
        raise ContestError(f"num_tasks must be at least 1, got {num_tasks}")
    if num_tasks > len(PANEL_TASKS):
        raise ContestError(
            f"num_tasks={num_tasks} exceeds the {len(PANEL_TASKS)}-task panel. "
            "The panel is fixed so results stay comparable; widening it means "
            "choosing tasks and difficulty mix deliberately, which this entry "
            "point will not do for you."
        )
    if concurrency < 1:
        raise ContestError(f"concurrency must be at least 1, got {concurrency}")
    if attempts < 1:
        raise ContestError(f"attempts must be at least 1, got {attempts}")
    built = _preset(f"contest-{worker}", worker, f"{worker}: contest", "")["match"]
    built["name"] = name or f"Contest: Claude Code vs Stella on {worker}"
    built["tasks"] = list(PANEL_TASKS[:num_tasks])
    built["concurrency"] = concurrency
    built["attempts"] = attempts
    built["record_video"] = record_video
    return MatchSpec.from_json(built)


def contest_comparability_note(num_tasks: int) -> str | None:
    """Why a sliced panel's number is not the panel's number, or ``None``.

    Returned rather than printed so both the CLI and any other caller say the
    same sentence. Silence when the panel is whole is the point: a warning
    that fires every time is a warning nobody reads.
    """
    if num_tasks >= len(PANEL_TASKS):
        return None
    return (
        f"panel sliced to the first {num_tasks} of {len(PANEL_TASKS)} tasks — "
        "this result is NOT comparable to the full-panel history, because a "
        "prefix is a different difficulty mix, not a sample of the same one"
    )


def _preset(key: str, worker: str, title: str, blurb: str) -> dict[str, Any]:
    """One preset, as the wizard's parsed-template JSON shape.

    ``worker`` is the bare Anthropic model id (``claude-sonnet-5``). The
    Claude Code seat runs it first-party; the Stella seat runs the same model
    through OpenRouter, whose catalog publishes it as ``anthropic/<worker>``.
    """
    verifier = _VERIFIER_FOR[worker]
    cc_engine = {
        "api": "anthropic",
        "model": worker,
        "reasoning": True,
        "effort": _EFFORT,
        # No base_url: first-party Anthropic, authenticated by the
        # subscription token. Claude Code honours no spend cap, and on the plan
        # the quota is the ceiling.
    }
    stella_engine = {
        "api": "openrouter",
        "model": f"anthropic/{worker}",
        # The parity invariant, by construction rather than by transcription.
        "reasoning": cc_engine["reasoning"],
        "effort": cc_engine["effort"],
        # Neither seat carries a ceiling, and this preset is where that stopped
        # being true by accident: the comparator above has never had one, while
        # this seat used to declare `max_tokens = 128000` and
        # `budget_usd = 12.0` — a handicap written three lines under the
        # sentence explaining that the other side has none (#2411).
        "roles": {
            "verifier": {
                "model": f"anthropic/{verifier}",
                "effort": "high",
                "reasoning": "on",
            },
            "triage": {
                # Classification only, never touches the workspace.
                "model": "anthropic/claude-haiku-4.5",
                "effort": "low",
                "reasoning": "off",
            },
        },
    }
    spec = MatchSpec.from_json(
        {
            "name": title,
            "dataset": "terminal-bench-2.1",
            "tasks": list(PANEL_TASKS),
            "attempts": 1,
            # One task slot is two contestant containers racing; a laptop-class
            # Docker allocation serialises honestly at 1. The wizard field is
            # editable after loading for a rig that can take more.
            "concurrency": 1,
            "record_video": True,
            # Claude Code installs itself per trial: apt-get plus the
            # claude.ai installer, measured at 229s against a 360s default.
            "setup_timeout_multiplier": 4.0,
            "contestants": [
                {
                    "id": "claude-code",
                    "name": f"Claude Code ({title.split(':')[0]})",
                    "agent": "claude-code",
                    "engine": cc_engine,
                    "env": {"required": ["CLAUDE_CODE_OAUTH_TOKEN"]},
                    "color": "#FF6B9D",
                },
                {
                    "id": "stella",
                    "name": f"Stella ({title.split(':')[0]} pipeline)",
                    "agent": "stella",
                    "engine": stella_engine,
                    "env": {"required": ["OPENROUTER_API_KEY"]},
                    "color": "#EFC53F",
                },
            ],
        }
    )
    return {
        "key": key,
        "title": title,
        "blurb": blurb,
        "match": spec.to_json(),
        "required_env": required_env(spec),
    }


def preset_listing() -> list[dict[str, Any]]:
    """Every preset, freshly built — cheap enough to construct per request."""
    return [
        _preset(
            "sonnet5-h2h",
            "claude-sonnet-5",
            "Sonnet 5: Claude Code vs Stella",
            "10-task TB2.1 panel, both arms on Sonnet 5 at medium effort; "
            "Stella verifies with Fable 5.",
        ),
        _preset(
            "opus5-h2h",
            "claude-opus-5",
            "Opus 5: Claude Code vs Stella",
            "10-task TB2.1 panel, both arms on Opus 5 at medium effort; "
            "Stella verifies with Fable 5.",
        ),
        _preset(
            "fable5-h2h",
            "claude-fable-5",
            "Fable 5: Claude Code vs Stella",
            "10-task TB2.1 panel, both arms on Fable 5 at medium effort; "
            "Stella verifies with Opus 5.",
        ),
    ]


PRESETS: tuple[str, ...] = ("sonnet5-h2h", "opus5-h2h", "fable5-h2h")
