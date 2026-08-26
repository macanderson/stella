# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

"""Value-shape refusals shared by the secure launcher's validators.

Each function either returns the value in the shape the caller asked for, or
raises `RuntimeError` naming what was wrong. They read no clock, touch no
network, and hold no state, so a caller can test one refusal without building
a launch.

The refusal messages are part of the launcher's contract:
`bench/harbor_adapter/tests/test_secure_launcher.py` matches on them, so change
one only alongside the test that asserts it.
"""

from __future__ import annotations

import math
from datetime import datetime, timezone
from typing import Any

__all__ = [
    "aware_timestamp",
    "finite_nonnegative_number",
    "parse_github_timestamp",
    "require_exact_object",
    "utc_text",
]


def parse_github_timestamp(value: Any) -> datetime:
    """A GitHub server timestamp, which is canonical UTC or nothing."""
    if not isinstance(value, str) or not value.endswith("Z"):
        raise RuntimeError("GitHub comment timestamp is not canonical UTC")
    try:
        parsed = datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as exc:
        raise RuntimeError("GitHub comment timestamp is invalid") from exc
    if parsed.tzinfo is None:
        raise RuntimeError("GitHub comment timestamp lacks a timezone")
    return parsed.astimezone(timezone.utc)


def utc_text(value: datetime) -> str:
    """A datetime as the `Z`-suffixed microsecond ISO-8601 an attestation carries."""
    if value.tzinfo is None:
        raise RuntimeError("public-intent preflight clock lacks a timezone")
    return (
        value.astimezone(timezone.utc)
        .isoformat(timespec="microseconds")
        .replace("+00:00", "Z")
    )


def aware_timestamp(value: Any, *, label: str) -> datetime:
    """A recorded timestamp, refused unless it carries a timezone.

    A naive timestamp is refused rather than assumed to be UTC: the launcher
    orders events by these, and an assumption there orders them wrongly by
    however far the writer's clock was offset.
    """
    if not isinstance(value, str) or not value:
        raise RuntimeError(f"{label} is not a timezone-aware ISO-8601 timestamp")
    normalized = value[:-1] + "+00:00" if value.endswith("Z") else value
    try:
        parsed = datetime.fromisoformat(normalized)
    except ValueError as exc:
        raise RuntimeError(
            f"{label} is not a timezone-aware ISO-8601 timestamp"
        ) from exc
    if parsed.tzinfo is None:
        raise RuntimeError(f"{label} is not a timezone-aware ISO-8601 timestamp")
    return parsed.astimezone(timezone.utc)


def finite_nonnegative_number(value: Any, *, label: str) -> float:
    """A money or count field: a real number, at or above zero.

    `bool` is rejected before the numeric check, because `True` is an `int` in
    Python and would otherwise pass as one dollar.
    """
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise RuntimeError(f"{label} must be a finite nonnegative number")
    result = float(value)
    if not math.isfinite(result) or result < 0:
        raise RuntimeError(f"{label} must be a finite nonnegative number")
    return result


def require_exact_object(
    value: Any, fields: frozenset[str], *, label: str
) -> dict[str, Any]:
    """An object whose key set is exactly `fields` — no extras, none missing.

    Exact rather than a superset check, so a payload that grew a field is
    refused instead of parsed under an assumption about what the field means.
    """
    if not isinstance(value, dict) or set(value) != fields:
        raise RuntimeError(f"{label} differs from the exact v2 schema")
    return value
