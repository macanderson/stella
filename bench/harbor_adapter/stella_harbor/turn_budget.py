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

## Why the agent kwarg alone was not enough (#2135)

The two inputs above are both things a *caller* has to remember to supply, and
in match `cc00894779ff` neither was: every trial's `config.json` records
`agent.kwargs: {}`, and ArenaBench's `harbor run` command line
(`arenabench/arenabench/runner.py`) passes no `--agent-kwarg` at all. So
`resolve_turn_budget(None, None)` returned `None`, `--turn-budget` was omitted
from argv, `Config::turn_budget` stayed `None`, and
`stella_cli::runtime::one_shot_budget_guard` never called
`BudgetGuard::set_task_deadline`. The whole #1481/#1503/#1507 mechanism was
built, wired and armed-capable, and simply never armed — every timeout in that
match was Harbor's own `AgentTimeoutError` SIGKILL at 900.0s.

A supplied kwarg is therefore treated as the operator's explicit statement, not
as the only channel. Harbor already writes the resolved trial configuration to
`config.json` at the trial root — one level above the agent's `logs_dir`, as
the first statement of `Trial.run()`, before the agent is started — and that
document plus the task's own `task.toml` carry every input Harbor's own
deadline arithmetic uses. [`discover_trial_deadline`] recomputes it from those
two files, so the deadline is per-trial (the shape requirement above) and no
runner has to remember anything.

That recomputation mirrors `harbor/trial/trial.py` and is therefore coupled to
a version of Harbor. It **fails open**, always: any missing, unreadable, or
mis-shaped input reads as "no deadline known" and leaves the flag off, which is
exactly the behaviour that shipped before this module existed. Guessing a
deadline is the one outcome that is worse than not having one.

## The cost of that fail-open, and the one thing that makes it survivable (#2957)

Failing open is right, and it is also **silent** — which is how this module
spent a whole benchmark run arming nothing at all. `task.path` pointed into
Harbor's download *cache*, where a content digest sits between the task and its
definition (`<task>/<sha>/task.toml`); [`_task_agent_timeout`] read only the
*export* shape (`<task>/task.toml`), the open raised `OSError`, and the
fail-open did exactly what it says. Every trial ran with no wall clock of its
own and the run died on Harbor's SIGKILL instead of stopping itself.

The measurement, either side of the break:

* 2026-08-09, three `fix-git` trials: **130 of 130** `budget_tick` events
  carried `deadline_remaining_ms`, and one stopped itself with
  `task deadline exceeded by 18.1s — stopping with partial work`.
* 2026-08-11, the twenty pipeline trials of `w10p`: **0 of 1087**.

Both layouts are now read. The lesson worth keeping is not "handle two paths",
it is that a fail-open on a *derived* value needs the derived value **recorded**:
what made this diagnosable at all was `stella_turn_budget_sec` in the adapter's
own result, which said `"unarmed"` in plain text. Without that field the
investigation is a bisect. Any future input added here should be recorded the
same way.
"""

from __future__ import annotations

import json
import tomllib
from collections.abc import Mapping
from pathlib import Path
from typing import Any

# Harbor's own filenames. `config.json` is the resolved trial configuration
# (`TrialPaths.config_path`); `task.toml` is the task definition it points at.
TRIAL_CONFIG_FILENAME = "config.json"
TASK_CONFIG_FILENAME = "task.toml"

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


def coerce_timeout_multiplier(raw: Any) -> float | None:
    """Parse a stated timeout multiplier, or ``None`` when none is stated.

    Deliberately not [`coerce_timeout_seconds`]: Harbor selects a multiplier
    with ``is not None``, so a stated ``0`` genuinely means "no time at all",
    while a deadline of ``0`` means "no deadline known". Collapsing the two
    would arm a full-length deadline against a trial Harbor kills on the
    instant. Non-finite and unparseable still read as unstated — there is no
    sane deadline to derive from a NaN.
    """
    if raw is None or isinstance(raw, bool):
        return None
    try:
        value = float(str(raw).strip())
    except (TypeError, ValueError):
        return None
    if value != value or value in (float("inf"), float("-inf")):
        return None
    return value


def trial_agent_deadline(
    trial_config: Mapping[str, Any],
    task_agent_timeout_sec: Any,
) -> float | None:
    """Recompute the wall clock Harbor will kill this trial's agent at. Pure.

    A transcription of `harbor/trial/trial.py`'s own arithmetic::

        base       = agent.override_timeout_sec or task.agent.timeout_sec
        cap        = agent.max_timeout_sec or inf
        multiplier = agent_timeout_multiplier ?? timeout_multiplier
        deadline   = min(base, cap) * multiplier

    Stated as one pure function over two already-loaded documents so the
    arithmetic is testable without a trial directory, a task on disk, or
    Harbor installed at all — and so the I/O half ([`discover_trial_deadline`])
    holds nothing but reading and fail-open.

    ``None`` whenever no base timeout is stated. An absent multiplier is
    ``1.0`` because that is Harbor's own default for both fields; an absent
    *deadline* is never defaulted.
    """
    agent = trial_config.get("agent")
    agent = agent if isinstance(agent, Mapping) else {}
    base = coerce_timeout_seconds(agent.get("override_timeout_sec"))
    if base is None:
        base = coerce_timeout_seconds(task_agent_timeout_sec)
    if base is None:
        return None
    cap = coerce_timeout_seconds(agent.get("max_timeout_sec"))
    if cap is not None:
        base = min(base, cap)
    multiplier = coerce_timeout_multiplier(trial_config.get("agent_timeout_multiplier"))
    if multiplier is None:
        multiplier = coerce_timeout_multiplier(trial_config.get("timeout_multiplier"))
    if multiplier is None:
        multiplier = 1.0
    return base * multiplier


def discover_trial_deadline(logs_dir: Any) -> float | None:
    """Read the deadline Harbor resolved for THIS trial off the trial tree.

    ``logs_dir`` is what Harbor hands the agent — ``TrialPaths.agent_dir`` —
    and the trial's ``config.json`` is its immediate parent. Only that one
    location is consulted: walking further up would eventually find *some*
    other trial's or job's configuration and silently arm a deadline belonging
    to a different run. A multi-step trial relocates the agent directory
    afterwards, so its later layout is simply not discoverable here, and reads
    as no deadline rather than as a wrong one.

    Fail-open by construction. Every failure mode — no ``logs_dir``, no
    ``config.json``, malformed JSON or TOML, a task path that no longer exists,
    a Harbor whose schema has moved — returns ``None``, which omits the flag
    and restores exactly the pre-#1161 behaviour.
    """
    if not logs_dir:
        return None
    try:
        raw = (Path(logs_dir).parent / TRIAL_CONFIG_FILENAME).read_text(
            encoding="utf-8"
        )
        trial_config = json.loads(raw)
    except (OSError, ValueError):
        return None
    if not isinstance(trial_config, Mapping):
        return None
    return trial_agent_deadline(trial_config, _task_agent_timeout(trial_config))


def _task_agent_timeout(trial_config: Mapping[str, Any]) -> Any:
    """The per-task ``[agent] timeout_sec`` the trial config points at.

    ``None`` when the trial names no local task path — a dataset resolved from
    a registry ref rather than an offline export has ``task.path: null``, and
    the task definition is then not on this host at all.

    **Harbor materialises a task in two layouts, and both have to be read.** An
    *export* puts the definition directly at ``<task>/task.toml``; the download
    *cache* interposes a content digest, ``<task>/<sha>/task.toml``. Reading
    only the first is what silently disarmed the deadline: `task.path` pointed
    into the cache, the open raised ``OSError``, the fail-open returned ``None``,
    and every trial ran with no wall clock of its own while the adapter recorded
    ``stella_turn_budget_sec: "unarmed"``. Measured either side of it — 130 of
    130 `budget_tick` events armed on 2026-08-09, then 0 of 1087 on 2026-08-11.

    ArenaBench already knew about both shapes and enumerates them for exactly
    this reason (``arenabench/arenabench/registry.py::_iter_task_tomls``); this
    is the same knowledge, applied on the deadline path where it was missing.
    Deliberately **not** shared code: `stella_harbor` is the installed Harbor
    adapter and does not import `arenabench` (the dependency runs the other
    way), so the alternative to a second reader is a coupling that would make
    the adapter unusable without ArenaBench.
    """
    task = trial_config.get("task")
    path = task.get("path") if isinstance(task, Mapping) else None
    if not path:
        return None
    for candidate in _task_config_candidates(Path(path)):
        try:
            with open(candidate, "rb") as handle:
                task_config = tomllib.load(handle)
        except (OSError, ValueError):
            continue
        agent = task_config.get("agent")
        timeout = agent.get("timeout_sec") if isinstance(agent, Mapping) else None
        if timeout is not None:
            return timeout
    return None


def _task_config_candidates(root: Path) -> list[Path]:
    """Every place a task definition may sit under ``root``, nearest first.

    The export layout is tried before the cache one so a directory holding both
    resolves to the export — the same precedence ``resolve_turn_budget`` uses
    throughout: the most specific statement of the deadline wins.

    A digest directory is found by globbing rather than by pattern-matching the
    name, because the digest's spelling is Harbor's business and a run that
    changed it must not silently stop arming the deadline again. Sorted so the
    choice is deterministic when a cache holds more than one digest for a task;
    every candidate is then tried in turn, so an unreadable first entry costs
    nothing.
    """
    candidates = [root / TASK_CONFIG_FILENAME]
    try:
        candidates.extend(sorted(root.glob("*/" + TASK_CONFIG_FILENAME)))
    except OSError:
        # An unreadable or non-existent root is "no deadline known", the same
        # fail-open every other failure here takes.
        pass
    return candidates


def resolve_turn_budget(
    agent_timeout_sec: Any = None,
    env_value: Any = None,
    trial_logs_dir: Any = None,
) -> str | None:
    """Return the ``--turn-budget`` argv value, or ``None`` to omit the flag.

    Precedence is deadline-first, most-specific-source-first:

    1. ``agent_timeout_sec`` — the operator said it outright, via
       ``--agent-kwarg agent_timeout_sec=<seconds>``.
    2. The deadline discovered from this trial's own Harbor configuration
       (``trial_logs_dir``). Same authority as (1) — it *is* Harbor's number —
       but derived rather than stated, so an explicit kwarg still wins.
    3. ``STELLA_TURN_BUDGET`` — a static operator fallback, last because one
       value cannot be right for a dataset whose tasks carry different
       timeouts.

    ``None`` leaves the continuation allowance a pure count — exactly the
    behaviour before #1161 — so a caller with no deadline is unaffected.

    A deadline at or under the reserve also yields ``None`` rather than zero or
    a negative. Zero would make every continuation unaffordable and turn a
    whole run into truthful partials, which reads as a deliberate policy rather
    than the misconfiguration it is.
    """
    deadline = coerce_timeout_seconds(agent_timeout_sec)
    if deadline is None:
        deadline = discover_trial_deadline(trial_logs_dir)
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
