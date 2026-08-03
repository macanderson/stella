"""Post-mortem classification of a SIGKILL'd trial (#1178).

Exit code 137 is not a Stella exit path: nothing in the CLI returns it. It is
128+9 — the kernel or the container supervisor killing the process — and it is
the one failure mode that used to produce **no explanation at all** (the
``loop-bench`` ``SILENT-DEATH`` shape, in the real harness). On the gate3c
Terminal-Bench 2.1 set, ``torch-tensor-parallelism`` ended exactly this way
and stayed unowned because nothing recorded which of the two candidates
killed it:

* **The OOM killer.** The kernel kills the biggest process in the task
  container's cgroup — Stella — and the container itself keeps running.
  Diagnosable *at the moment of death*: the cgroup's ``memory.events``
  ``oom_kill`` counter is non-zero, and ``docker inspect`` may show
  ``State.OOMKilled`` when the whole container was the victim.
* **A stop/kill on teardown.** Docker (or the harness) tears the container
  down while the agent is still writing. Nothing inside can record anything —
  which is itself the signature: the container is gone or exited, and the
  cgroup counters are unreadable.

The adapter probes both signals immediately after a 137 exit (see
``_probe_sigkill_cause`` in ``stella_harbor``) while the original container
still exists — ``docker inspect`` on a re-run cannot answer for a trial whose
container Harbor already removed. This module is the pure half: parsing and
classification, testable without Docker.

A third, independent discriminator comes from the event stream the trial
already wrote: whether ``stella-events.jsonl`` stops mid-step or at a step
boundary. An OOM lands mid-step (the kill interrupts a model call or tool
run); a teardown between steps lands on a boundary. Recorded alongside the
container evidence rather than folded into the verdict, because the two can
legitimately disagree and the disagreement is itself diagnostic.
"""

from __future__ import annotations

import json
from collections.abc import Awaitable, Callable, Sequence
from typing import Any

# The exit code this module exists for: 128 + SIGKILL(9).
SIGKILL_EXIT_CODE = 137

# Written under ``logs_dir`` next to ``stella-events.jsonl`` when a trial
# exits 137, and named in the ``NonZeroAgentExitCodeError`` message so
# ``exception.txt`` points at it.
EXIT_CAUSE_LOG_NAME = "stella-exit-cause.json"

# Event types that mark a step boundary in the stream-json vocabulary: a
# ``step_usage`` closes each engine step, and the two terminal events close
# the turn. Everything else (deltas, tool activity, stages) is mid-step work.
_BOUNDARY_EVENT_TYPES = frozenset({"step_usage", "complete", "error"})


def classify_stream_end(events: list[dict[str, Any]] | None) -> dict[str, Any]:
    """Say where the event stream stopped: mid-step or at a step boundary.

    ``events`` is the parsed event list from ``stella-events.jsonl`` (the
    envelope's ``events``). The classification deliberately reads only the
    *last* event: the stream is flushed per event into a mounted file, so its
    tail is the last thing the process lived to say.
    """
    if not events:
        return {
            "events_seen": 0,
            "last_event_type": None,
            "ended_at_step_boundary": False,
            "classification": "no-events",
        }
    last = events[-1]
    last_type = last.get("type")
    at_boundary = last_type in _BOUNDARY_EVENT_TYPES
    return {
        "events_seen": len(events),
        "last_event_type": last_type,
        "ended_at_step_boundary": at_boundary,
        "classification": "step-boundary" if at_boundary else "mid-step",
    }


def parse_memory_events(text: str | None) -> dict[str, int]:
    """Parse a cgroup-v2 ``memory.events`` file into its counters.

    The file is ``<name> <count>`` per line (``low``, ``high``, ``max``,
    ``oom``, ``oom_kill``, …). Unparseable lines are skipped rather than
    fatal: this runs on whatever a possibly-dying container handed back.
    """
    counters: dict[str, int] = {}
    for line in (text or "").splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        name, raw = parts
        try:
            counters[name] = int(raw)
        except ValueError:
            continue
    return counters


def sigkill_verdict(
    *,
    container_state: dict[str, Any] | None,
    memory_events: dict[str, int],
    container_absent: bool = False,
) -> tuple[str, str]:
    """Return ``(verdict, detail)`` for a 137 exit.

    Verdicts, most specific first:

    * ``oom-kill`` — the kernel's OOM killer acted in the trial container:
      either the container itself was the victim (``State.OOMKilled``) or the
      cgroup's ``oom_kill`` counter shows a process inside (Stella, as the
      biggest resident) was.
    * ``external-teardown`` — the container is exited, dead, or already gone,
      with no OOM evidence: something outside stopped it (a ``docker stop``,
      the harness reclaiming the trial).
    * ``unattributed`` — no evidence either way. Either the container is alive
      and healthy with zero OOM events, or the probe could not observe it at
      all. Still recorded, because "we looked and found nothing" beats silence.

    ``container_state`` is ``None`` for two very different reasons, and only
    the caller can tell them apart, so it must: ``container_absent=True`` means
    *the probe looked and the container was genuinely gone* — that is
    ``external-teardown``. ``container_absent=False`` with no state means *the
    probe could not see* (the daemon refused, ``inspect`` failed, the enumerate
    step errored). That is ``unattributed``, never a teardown: reporting a
    failed measurement as an affirmative cause would erase a real OOM from the
    record whenever Docker happens to be unwell, and the reader of the artifact
    has no way to tell the guess from an observation.
    """
    state = container_state or {}
    if state.get("OOMKilled") is True:
        return (
            "oom-kill",
            "docker reports State.OOMKilled on the task container",
        )
    oom_kills = memory_events.get("oom_kill", 0)
    if oom_kills > 0:
        return (
            "oom-kill",
            f"the container cgroup recorded oom_kill {oom_kills} — the kernel "
            "killed a process inside (the agent, as the largest resident) "
            "while the container itself kept running",
        )
    if container_absent:
        return (
            "external-teardown",
            "the task container no longer exists — it was removed while the "
            "agent was still running",
        )
    if not state:
        return (
            "unattributed",
            "the probe could not observe the task container (see "
            "probe_errors); with no observation there is no evidence of a "
            "teardown or an OOM either way",
        )
    if state.get("Running") is not True:
        status = state.get("Status", "unknown")
        return (
            "external-teardown",
            f"the task container is not running (status: {status}) and shows "
            "no OOM evidence — it was stopped from outside",
        )
    return (
        "unattributed",
        "the container is running with zero cgroup oom_kill events; the "
        "SIGKILL came from outside what this probe can observe",
    )


async def probe_sigkill_cause(
    environment: Any,
    *,
    compose_base_argv: Callable[[Any], list[str]],
    compose_host_environment: Callable[[Any], dict[str, str]],
    captured_process: Callable[
        [list[str], dict[str, str]], Awaitable[tuple[int, bytes]]
    ],
    service_label: str,
) -> dict[str, Any]:
    """Gather host-side evidence for a 137 exit, while it still exists.

    Runs immediately after the exec returns — before Harbor's verifier and
    teardown — because this is the only moment the question is answerable:
    ``docker inspect`` on a re-run cannot say anything about the container
    that actually died, and the original is gone once the trial is torn down.

    Two signals, matching the two candidates for a SIGKILL:

    * the main task container's Docker ``State`` (``OOMKilled``, ``Running``,
      ``Status``) — a container that is exited or missing points at an
      external stop;
    * the cgroup ``memory.events`` counters read from *inside* a
      still-running container — a non-zero ``oom_kill`` means the kernel
      killed a process in the container (the agent, as the largest resident)
      while the container survived, which ``State.OOMKilled`` alone misses.

    The Compose/Docker plumbing is injected rather than imported: the package
    ``__init__`` owns those helpers (they encode its credential-scrubbing
    rules), and importing back into the package from here would be a cycle.

    Best-effort throughout: every failure is recorded as a string in
    ``errors`` rather than raised, because a broken probe must never turn a
    scored trial into an adapter crash. The ``_stella_exit_cause_probe``
    hook keeps unit tests independent of Docker, mirroring
    ``_stella_secure_exec_with_stdin``.
    """
    hook = getattr(environment, "_stella_exit_cause_probe", None)
    if callable(hook):
        return await hook()

    errors: list[str] = []
    container_state: dict[str, Any] | None = None
    container_absent = False
    memory_events_text: str | None = None
    try:
        compose = compose_base_argv(environment)
        host_env = compose_host_environment(environment)
        return_code, output = await captured_process([*compose, "ps", "-aq"], host_env)
        container_ids = (
            [
                line.strip().decode("ascii", errors="replace")
                for line in output.splitlines()
                if line.strip()
            ]
            if return_code == 0
            else []
        )
        if return_code != 0:
            errors.append("could not enumerate the project's containers")
        elif not container_ids:
            # An answered question, not a failed one: the project was
            # enumerated and it is empty.
            container_absent = True
            errors.append("the compose project lists no containers")
        else:
            return_code, output = await captured_process(
                ["docker", "inspect", *container_ids], host_env
            )
            if return_code != 0:
                errors.append("docker inspect failed for the project containers")
            else:
                container_state, container_absent = _main_container_state(
                    json.loads(output), service_label=service_label, errors=errors
                )
        if container_state is not None and container_state.get("Running") is True:
            return_code, output = await captured_process(
                [*compose, "exec", "-T", "main", "cat", "/sys/fs/cgroup/memory.events"],
                host_env,
            )
            if return_code == 0:
                memory_events_text = output.decode("utf-8", errors="replace")
            else:
                errors.append(
                    "could not read /sys/fs/cgroup/memory.events from the "
                    "running container"
                )
    except Exception as exc:  # noqa: BLE001 - diagnosis must never fail the trial
        errors.append(f"exit-cause probe failed: {exc}")
    return {
        "container_state": container_state,
        "container_absent": container_absent,
        "memory_events_text": memory_events_text,
        "errors": errors,
    }


def _main_container_state(
    inspected: Any, *, service_label: str, errors: list[str]
) -> tuple[dict[str, Any] | None, bool]:
    """Pick the main service container's ``State`` out of ``docker inspect``.

    Returns ``(state, absent)``. ``absent`` is ``True`` only when the inspect
    succeeded and genuinely showed no main container — an observation. A
    malformed payload is a probe failure, not an absence, and leaves ``absent``
    ``False`` so the verdict stays ``unattributed``.
    """
    if not isinstance(inspected, Sequence):
        errors.append("docker inspect returned a non-list")
        return None, False
    for container in inspected:
        if not isinstance(container, dict):
            continue
        config = container.get("Config")
        labels = (config or {}).get("Labels") or {}
        if labels.get(service_label) == "main":
            state = container.get("State")
            if isinstance(state, dict):
                return state, False
            errors.append("the main service container reported no State object")
            return None, False
    errors.append("no main service container found to inspect")
    return None, True


def build_exit_cause_report(
    *,
    return_code: int,
    container_state: dict[str, Any] | None,
    memory_events_text: str | None,
    events: list[dict[str, Any]] | None,
    probe_errors: list[str],
    container_absent: bool = False,
) -> dict[str, Any]:
    """Assemble the ``stella-exit-cause.json`` artifact for one 137 exit.

    Everything observed goes in verbatim alongside the verdict, so a reader
    who disputes the classification can re-derive it — the artifact is
    evidence first, opinion second.
    """
    memory_events = parse_memory_events(memory_events_text)
    verdict, detail = sigkill_verdict(
        container_state=container_state,
        memory_events=memory_events,
        container_absent=container_absent,
    )
    return {
        "return_code": return_code,
        "signal": "SIGKILL" if return_code == SIGKILL_EXIT_CODE else None,
        "verdict": verdict,
        "detail": detail,
        "container_state": container_state,
        "container_absent": container_absent,
        "memory_events": memory_events or None,
        "stream_end": classify_stream_end(events),
        "probe_errors": probe_errors,
    }
