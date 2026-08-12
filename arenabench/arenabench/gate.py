# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The banned-behavior gate: a subprocess contract, deliberately not an import.

A bench run pays real money for every trial, and the most expensive way to
spend it is to discover — at fetch time, hours later — that the very first
trial exhibited a defect that was already fixed on another branch. The run was
answering a question nobody has any more. This module is the seam that lets a
*finished* trial be inspected the moment it settles, so the remaining N-1
trials can be cancelled instead of billed.

**The seam is a subprocess, and that is the whole design.** ArenaBench seats
``claude-code`` and ``stella`` alike, is stdlib-only by contract, and is being
prepared for ejection into its own repository; the detector ledger it has to
consult is agent-specific by construction — Stella's lives in
``bench/trace_triage/`` and every one of its detectors cites a Stella crate
path. An import would put agent-specific analysis inside an agent-agnostic
package *and* break the ejection, so the two halves meet through
``argv``/exit code/stdout instead. That boundary, and the alternatives that
were rejected, are argued in issue #3024.

The contract in full, because a checker on the far side of a process boundary
cannot be type-checked into behaving:

* **argv** — ``<command...> <trial-dir> [--allow CODE]...``. The trial
  directory is one settled trial's fetched artifacts. Each ``--allow`` names a
  behavior code the operator has waived for this run.
* **exit 0** — clear. Nothing banned was observed.
* **exit 9** (:data:`BANNED_EXIT`) — a banned behavior was observed. The run
  is aborted.
* **any other exit** — *the gate itself failed*. The run continues.
* **stdout** — the report, shown to the operator verbatim. If it happens to be
  JSON, the behavior codes are lifted out of it; if it is not, the exit code
  still decides and the text is still kept.

That third rule is the one that matters most and is the easiest to get wrong.
A checker that crashes has said **nothing** about the trial, and inferring
"banned" from its silence would kill healthy runs. A gate that kills healthy
runs is switched off by the second operator it inconveniences, and a gate
nobody arms catches nothing — so a broken gate is loud, repeatedly, and
harmless. The failure is printed on every occurrence rather than once, because
the alternative (dedupe the noise) is how a permanently broken checker becomes
invisible.

The verdict is **never re-derived from the codes**. A waiver is passed *to* the
gate as ``--allow`` and the gate decides; ArenaBench does not subtract allowed
codes from a parsed list and downgrade a ban to clear, because that path turns
an unparseable report into a silent pass — the one direction this module must
never fail in.
"""

from __future__ import annotations

import shlex
import subprocess
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

__all__ = [
    "BANNED_EXIT",
    "GATE_ENV_VAR",
    "GateNotArmed",
    "GateOutcome",
    "GateSpec",
    "arm_gate",
    "gate_argv",
    "register_arguments",
    "resolve_gate",
    "run_gate",
]

#: The exit code a gate command uses to say "a banned behavior was observed".
#: Anything else that is not 0 means the gate failed, so this has to be a value
#: an interpreter is unlikely to produce by accident — 1 and 2 are what a
#: crashed script and an argparse error already return.
BANNED_EXIT = 9

#: Environment variable naming the gate command when no ``--gate`` is passed.
GATE_ENV_VAR = "ARENABENCH_GATE"

#: How long a gate may take on one trial before it is called failed. A checker
#: reading one trial's artifacts is a seconds-scale job; two minutes is slack
#: for a cold filesystem, not a budget for real analysis.
DEFAULT_TIMEOUT = 120.0


class GateNotArmed(Exception):
    """No gate resolved and the operator did not explicitly waive one.

    Raised by :func:`arm_gate` so the caller can refuse to start rather than
    submit an unguarded run. Its message names all three ways out, because an
    operator who hits this is mid-launch and should not have to read source to
    get moving.
    """


@dataclass(frozen=True)
class GateSpec:
    """A resolved gate: what to run, what has been waived, whether it is on.

    ``allowed`` holds behavior codes the operator has waived *for this run*.
    They are forwarded to the gate command rather than applied here — see the
    module docstring on why the verdict is never re-derived locally.
    """

    command: tuple[str, ...]
    allowed: frozenset[str] = field(default_factory=frozenset)
    enabled: bool = True

    def described(self) -> str:
        """The command as an operator would retype it, for the run header."""
        return shlex.join(self.command)


@dataclass(frozen=True)
class GateOutcome:
    """What one gate invocation concluded about one trial.

    ``banned`` and ``error`` are never both set: a gate that failed has made no
    claim about the trial. The default — every field empty — is the outcome of
    a disabled gate, which is also the only outcome that costs nothing.
    """

    banned: bool = False
    codes: tuple[str, ...] = ()
    report: str = ""
    error: str = ""


def gate_argv(spec: GateSpec, trial_dir: Path) -> tuple[str, ...]:
    """The exact argv a gate command is invoked with.

    Pure, and separate from :func:`run_gate`, so the wire shape of the contract
    is assertable without spawning anything. Waivers are sorted so two runs
    with the same waivers produce the same command line — a gate that wants to
    cache on its arguments can, and a log line is comparable between runs.
    """
    argv = [*spec.command, str(trial_dir)]
    for code in sorted(spec.allowed):
        argv += ["--allow", code]
    return tuple(argv)


def run_gate(
    spec: GateSpec,
    trial_dir: Path,
    *,
    runner: Callable[..., Any] = subprocess.run,
    timeout: float = DEFAULT_TIMEOUT,
) -> GateOutcome:
    """Run the gate over one settled trial's artifacts and read its verdict.

    ``runner`` is the seam that keeps this testable without a real checker: it
    has :func:`subprocess.run`'s shape and is called with ``capture_output`` and
    ``text`` set, so a fake is a two-line callable returning a
    :class:`subprocess.CompletedProcess`.

    Every way the gate can fail to answer — a non-{0,9} exit, a timeout, no
    such executable, an OS refusal — lands in :attr:`GateOutcome.error` and
    leaves ``banned`` false. See the module docstring: a checker that crashed
    has made no claim, and a run killed by a broken checker is how a gate stops
    being armed.
    """
    if not spec.enabled:
        return GateOutcome()
    argv = gate_argv(spec, trial_dir)
    try:
        done = runner(list(argv), capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return GateOutcome(
            error=f"gate timed out after {timeout:.0f}s: {shlex.join(argv)}"
        )
    except (OSError, subprocess.SubprocessError) as exc:
        return GateOutcome(error=f"gate could not run ({exc}): {shlex.join(argv)}")

    report = done.stdout or ""
    if done.returncode == BANNED_EXIT:
        return GateOutcome(banned=True, codes=_codes_from(report), report=report)
    if done.returncode == 0:
        return GateOutcome(codes=(), report=report)
    stderr = (done.stderr or "").strip()
    return GateOutcome(
        report=report,
        error=(
            f"gate exited {done.returncode} (expected 0=clear or "
            f"{BANNED_EXIT}=banned): {shlex.join(argv)}"
            + (f"\n{stderr}" if stderr else "")
        ),
    )


def _codes_from(stdout: str) -> tuple[str, ...]:
    """Behavior codes lifted out of a gate's stdout, best-effort.

    Three shapes are tolerated because three are plausible and none is worth an
    argument with a checker author: ``{"codes": [...]}``, a bare list of
    strings, and any list of objects carrying a ``code`` key (under ``codes``
    or ``findings``). Anything else yields ``()`` — the codes are a convenience
    for the abort record, and the verdict never depends on parsing them.
    """
    import json

    try:
        body = json.loads(stdout)
    except (ValueError, TypeError):
        return ()
    if isinstance(body, dict):
        for key in ("codes", "findings", "banned"):
            entries = body.get(key)
            if isinstance(entries, list):
                return _codes_from_list(entries)
        return ()
    if isinstance(body, list):
        return _codes_from_list(body)
    return ()


def _codes_from_list(entries: list[Any]) -> tuple[str, ...]:
    codes: list[str] = []
    for entry in entries:
        if isinstance(entry, str):
            codes.append(entry)
        elif isinstance(entry, dict) and isinstance(entry.get("code"), str):
            codes.append(entry["code"])
    return tuple(codes)


def resolve_gate(
    *,
    command: str | None,
    env: Mapping[str, str],
    disabled: bool,
    allowed: Sequence[str] = (),
) -> GateSpec | None:
    """Decide which gate, if any, this run is guarded by.

    Order: an explicit ``--gate`` beats :data:`GATE_ENV_VAR` beats nothing.
    ``disabled`` (``--no-gate``) short-circuits both, so a waiver is always a
    deliberate act at the command line and can never be *un*-waived by an
    environment variable a shell profile happens to export.

    ``None`` means "no gate resolved" and is not by itself an error — the
    refusal to start unguarded lives in :func:`arm_gate`, so this function
    stays a pure resolution that other callers can reuse.
    """
    if disabled:
        return None
    raw = command if command is not None else env.get(GATE_ENV_VAR)
    if raw is None or not raw.strip():
        return None
    return GateSpec(
        command=tuple(shlex.split(raw)),
        allowed=frozenset(allowed),
        enabled=True,
    )


def register_arguments(parser: Any) -> None:
    """Attach ``--gate`` / ``--no-gate`` / ``--gate-allow`` to a parser.

    Defined once and called from both places that need the trio: ``cloud
    run``'s parser and ``contest``'s, since ``contest --submit`` hands off by
    building ``cloud run``'s namespace **by hand** and an attribute added to
    one parser and not the other is an ``AttributeError`` raised after the
    contest TOML has already been written. One definition is the only version
    of this that cannot drift.
    """
    parser.add_argument(
        "--gate",
        help="banned-behavior gate command, run on each trial as it settles: "
             "`CMD <trial-dir> [--allow CODE]...`, exit 0 clear / "
             f"{BANNED_EXIT} banned / anything else the gate itself failed. A "
             f"ban cancels the rest of the run. Defaults to ${GATE_ENV_VAR}",
    )
    parser.add_argument(
        "--no-gate",
        action="store_true",
        help="run with no banned-behavior gate — a deliberate waiver, printed "
             "in the run header",
    )
    parser.add_argument(
        "--gate-allow",
        action="append",
        metavar="CODE",
        help="waive one behavior code for this run (repeatable); forwarded to "
             "the gate command as --allow CODE",
    )


def arm_gate(
    *,
    command: str | None,
    disabled: bool,
    allowed: Sequence[str] = (),
    env: Mapping[str, str] | None = None,
) -> tuple[GateSpec | None, str]:
    """Resolve the gate for a run, or refuse to let the run start.

    Returns ``(spec, header_line)``; the header line is printed with the rest
    of the run header so the transcript of every run records which gate was
    armed — or that it was waived, and by whom, which is the part a later
    dispute needs.

    Raises :class:`GateNotArmed` when nothing resolved and no waiver was
    passed. Defaulting to "unguarded" would be the expedient here: it makes
    every existing invocation keep working, and it means the one run that
    needed the gate is the run that did not have it.
    """
    if env is None:
        import os

        env = os.environ
    spec = resolve_gate(
        command=command, env=env, disabled=disabled, allowed=allowed
    )
    if spec is not None:
        waived = f"  (waived: {', '.join(sorted(spec.allowed))})" if spec.allowed else ""
        return spec, f"gate      : {spec.described()}{waived}"
    if disabled:
        return None, "gate      : DISABLED (--no-gate)"
    raise GateNotArmed(
        "no banned-behavior gate is armed — a run costs real money and an "
        "unguarded one can spend all of it reproducing a defect that is "
        "already fixed. Arm one with `--gate CMD`, export "
        f"{GATE_ENV_VAR}=CMD, or waive it deliberately with `--no-gate`."
    )
