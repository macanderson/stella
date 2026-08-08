"""Metrics extraction from Stella's result envelope.

The strict envelope Stella prints — and, when diagnostics leaked around it,
the best top-level JSON object recovered from the output — is parsed here
into the flat metrics dict Harbor records for a trial. Defensive by
contract: a missing or malformed envelope yields all-``None`` values,
because a benchmark run must never be aborted by a metadata-parsing edge
case.

Names stay underscore-prefixed, like :mod:`.posture`'s, and are re-exported
through ``stella_harbor`` for the adapter and its tests: this is adapter
machinery, not a public API.
"""

from __future__ import annotations

import json
import math
import re
from typing import Any


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
