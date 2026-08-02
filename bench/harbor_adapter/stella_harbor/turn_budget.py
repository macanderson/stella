"""The turn deadline a trial runs under, and how it is derived.

Split out of the adapter package root for the same reason `posture.py` was:
that file is over its recorded ceiling (`scripts/check-file-size.sh`), and the
repository's rule is that separable new code becomes its own module rather than
more length on an already-oversized file. It is a genuine seam — everything
here is a pure function of a deadline and an environment value, with no Harbor,
Docker, or credential dependency — so the adapter and its tests exercise one
implementation rather than copies that can disagree.

## Why this exists at all

Stella's engine can decline to *start* a length continuation it cannot finish
(`driver::truncation::ContinuationBudget`, #1161), and #1173 made that decline
complete the turn with a truthful partial instead of aborting it. Both are
inert without a deadline, and the benchmark — the one caller that actually runs
under an external kill — had no way to supply one:

* Harbor 0.6.1 passes `agent_timeout_sec` to the **oracle** agent only
  (`trial.py`: `if config.agent.name == AgentName.ORACLE.value`), so an
  installed agent is never told the deadline it is running against.
* Exporting `STELLA_TURN_BUDGET` was worse than doing nothing: the adapter's
  fail-closed ambient check rejects any unregistered `STELLA_*` name, so it
  refused the run rather than enabling the policy.

## The two things this gets right that a constant cannot

**Shape.** Harbor's agent timeout is per-task — 900s and 1800s are both live in
Terminal-Bench 2.1 — so any single global value is wrong for most tasks. The
deadline has to come from the trial.

**Size.** The value used by hand on the bench box was 840s, against a
comparator whose winning `regex-chess` run took 2,746s of wall clock on this
same dataset. Stella was declining continuations roughly 3x too early, which
turns a policy meant to save turns into one that ends them.
"""

from __future__ import annotations

from typing import Any

# Host-side name for the deadline, forwarded to the CLI as `--turn-budget`.
# Registered host-only in the adapter: it is read on the host and expressed as
# argv, so the container environment stays exactly as narrow as it was and the
# empty-string parse trap that killed six trials on `--budget` cannot recur on
# a second variable.
TURN_BUDGET_ENV = "STELLA_TURN_BUDGET"

# Wall clock held back from the harness deadline so the turn ends as a result
# rather than as a kill.
#
# Subtracted, never multiplied. A fraction scales the reserve with the task and
# would throw away ~270s on an 1800s task — time the comparator spends
# finishing. What the engine needs is room to write its partial and exit, which
# is a constant.
TEARDOWN_SEC = 60.0


def coerce_timeout_seconds(raw: Any) -> float | None:
    """Parse a deadline in seconds, or ``None`` when there is not one.

    Harbor passes agent kwargs through as strings when they arrive from
    ``--agent-kwarg`` and as floats when they arrive from a config object, so
    both have to parse. Anything absent, blank, unparseable, non-finite, or
    non-positive is ``None`` — "no deadline known" — and never a substituted
    default, because guessing a deadline is what makes the engine decline
    continuations it could afford.

    ``bool`` is rejected explicitly: it is an ``int`` subclass in Python, so
    ``float(True)`` is ``1.0`` and a stray flag would otherwise become a
    one-second deadline.
    """
    if raw is None or isinstance(raw, bool):
        return None
    try:
        seconds = float(str(raw).strip())
    except (TypeError, ValueError):
        return None
    # NaN fails every comparison, so test it as itself; inf would produce an
    # argv value clap cannot parse.
    if seconds != seconds or seconds in (float("inf"), float("-inf")):
        return None
    if seconds <= 0:
        return None
    return seconds


def resolve_turn_budget(
    agent_timeout_sec: Any = None,
    env_value: Any = None,
) -> str | None:
    """Return the ``--turn-budget`` argv value, or ``None`` to omit the flag.

    Precedence is deadline-first: Harbor's own per-trial ``agent_timeout_sec``
    wins because it *is* the constraint being modelled, and an operator-set
    ``STELLA_TURN_BUDGET`` is the fallback for a runner that cannot pass an
    agent kwarg.

    ``None`` leaves the continuation allowance a pure count — exactly the
    behaviour before #1161 — so a caller with no deadline is unaffected.

    A deadline at or under the reserve also yields ``None`` rather than zero or
    a negative. Zero would make every continuation unaffordable and turn a
    whole run into truthful partials, which reads as a deliberate policy rather
    than the misconfiguration it is.
    """
    deadline = coerce_timeout_seconds(agent_timeout_sec)
    if deadline is None:
        deadline = coerce_timeout_seconds(env_value)
    if deadline is None:
        return None
    usable = deadline - TEARDOWN_SEC
    if usable <= 0:
        return None
    # `%g` so a whole number of seconds is "840" rather than "840.0" — the
    # value lands in a recorded argv that a reader compares against the task's
    # own timeout.
    return f"{usable:g}"
