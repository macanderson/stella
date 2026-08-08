"""Reaping the agent when Harbor's per-trial deadline cancels the run (#2131).

Harbor enforces the agent deadline by cancelling the coroutine that awaits the
exec client. Cancellation reaches the ``await`` and nothing else: the
``docker compose exec`` client is a *proxy*, and a Docker exec process is not
signalled when its client goes away. So on every timeout trial the Stella
process kept running inside the container, unsupervised, while Harbor started
the verifier — 414 seconds of it in match ``cc00894779ff``, including two
further billed model calls and a 103.8s container command, all of it
concurrent with the scoring of the same filesystem it was still writing to.

Two things follow from that, and this module owns both:

* **The agent is reaped in the container**, not merely disconnected from. The
  reap is a second, short-lived Compose exec that finds the agent by its
  installed binary and escalates ``SIGTERM`` → ``SIGKILL`` against its process
  group with a bounded wait in between.
* **The journal becomes final.** The adapter reconstructs a timed-out trial's
  usage by re-reading ``stella-events.jsonl`` after ``run()`` unwinds
  (``populate_context_post_run``); it under-reported by 16–35% only because the
  orphan kept appending after the snapshot. Killing the writer first is what
  makes that re-read the truth rather than a moving target.

This is the **timeout** branch. :mod:`stella_harbor.stream_release` owns the
**completion** branch, and the two are deliberately opposite: a run that
proved it finished releases the stream without signalling anything inside the
container, because the daemons a finished agent left behind are what the
verifier is about to probe (#2122). A run that never finished has no such
claim on them, so its process group goes.

The group, not the process tree: ``docker exec`` starts the agent as its own
process-group leader, so the group holds Stella plus the shells it spawned for
its tools — the work that must stop — while a service the agent properly
daemonized (``setsid``, and every real ``sshd``/``nginx`` daemon mode does)
has left that group and survives the signal. That is the line this module
draws, and it is drawn where the kernel already drew one.

Every step here is best-effort by contract: a reap that fails must never
displace the timeout Harbor is in the middle of reporting.
"""

from __future__ import annotations

import asyncio
import contextlib
import shlex
import sys
from collections.abc import Callable, Mapping, Sequence

from harbor.environments.base import ExecResult

from .stream_release import communicate_with_release, terminate_client

#: Compose service the benchmark task runs in. Matches the run's own argv.
_SERVICE = "main"
#: Wall clock the whole container-side reap may take, including Docker's own
#: exec setup. Comfortably above the in-container escalation below, and small
#: enough that a Docker daemon that has itself wedged cannot hold the trial's
#: timeout open.
_REAP_BUDGET_SECONDS = 40.0
#: Seconds the in-container escalation waits between ``SIGTERM`` and
#: ``SIGKILL``. Polled once a second, so a Stella that exits on the term signal
#: costs about a second, not the whole grace.
_TERM_GRACE_SECONDS = 8

#: Greppable prefix on every line the reap prints, so the fact that a trial was
#: reaped (and which pids it took) is recoverable from the trial's stderr log.
REAP_LOG_PREFIX = "stella-harbor-reap:"


def container_reap_program(agent_path: str) -> str:
    """The POSIX ``sh`` program that reaps the agent inside the container.

    Deliberately free of ``pkill``/``pgrep``/``ps``: Terminal-Bench images are
    minimal and several ship none of them, and a reap that silently no-ops on
    the images most likely to time out would be worse than no reap at all.
    ``/proc`` is the one interface every Linux container has.

    Identification is by ``/proc/<pid>/exe``, which the kernel resolves — a
    ``cmdline`` match would also accept a task's own process that merely
    mentions the path in an argument. The trailing-space case catches the
    ``"<path> (deleted)"`` form the kernel reports when the binary was replaced
    while running.

    Liveness is read from the state field of ``/proc/<pid>/stat``, not from the
    existence of ``/proc/<pid>``: a reaped-but-unwaited process keeps its
    directory for as long as its parent ignores it, so a presence check would
    report a zombie as a survivor and spend the whole grace waiting for a
    process that is already dead.

    No ``cut``, ``ps`` or ``awk``: the fields come out of ``set --``, so the
    program depends on nothing but the shell itself.
    """
    quoted = shlex.quote(agent_path)
    return f"""set -u
agent={quoted}
# Echoes the subset of $pids that is still running — zombies excluded.
running() {{
    result=''
    for pid in $pids; do
        stat=$(cat "/proc/$pid/stat" 2>/dev/null) || continue
        # A process that vanished mid-read yields an empty line, and `set -u`
        # would abort the whole reap on the "$1" below rather than skip it.
        [ -n "$stat" ] || continue
        set -- ${{stat##*') '}}
        [ "$1" = Z ] || result="$result $pid"
    done
    echo "$result"
}}
pids=''
for entry in /proc/[0-9]*; do
    exe=$(readlink "$entry/exe" 2>/dev/null) || continue
    case $exe in
        "$agent"|"$agent "*) pids="$pids ${{entry#/proc/}}" ;;
    esac
done
if [ -z "$pids" ]; then
    echo '{REAP_LOG_PREFIX} no agent process is running'
    exit 0
fi
groups=''
for pid in $pids; do
    stat=$(cat "/proc/$pid/stat" 2>/dev/null) || continue
    [ -n "$stat" ] || continue
    set -- ${{stat##*') '}}
    [ $# -ge 3 ] || continue
    pgid=$3
    case " $groups " in
        *" $pgid "*) ;;
        *) groups="$groups $pgid" ;;
    esac
done
echo '{REAP_LOG_PREFIX} signalling agent pids'"$pids"' in groups'"$groups"
for pgid in $groups; do
    kill -TERM "-$pgid" 2>/dev/null || true
done
waited=0
while [ "$waited" -lt {_TERM_GRACE_SECONDS} ]; do
    [ -n "$(running)" ] || break
    sleep 1
    waited=$((waited + 1))
done
for pgid in $groups; do
    kill -KILL "-$pgid" 2>/dev/null || true
done
# SIGKILL is delivered, not completed: give the kernel a moment before
# reporting, so the report is the outcome and not the race.
settled=0
while [ "$settled" -lt 2 ]; do
    [ -n "$(running)" ] || break
    sleep 1
    settled=$((settled + 1))
done
survivors=$(running)
echo '{REAP_LOG_PREFIX} survivors:'"${{survivors:- none}}"
exit 0
"""


def container_reap_argv(compose_base: Sequence[str], agent_path: str) -> list[str]:
    """The Compose argv that runs :func:`container_reap_program`.

    Runs as uid 0 so the escalation can reach an agent started as another user;
    carries no credential, so unlike the run itself it needs no stdin handoff.
    """
    return [
        *compose_base,
        "exec",
        "-T",
        "-u",
        "0",
        _SERVICE,
        "sh",
        "-c",
        container_reap_program(agent_path),
    ]


async def reap_container_agent(
    compose_base: Sequence[str],
    host_env: Mapping[str, str],
    agent_path: str,
    *,
    budget_seconds: float = _REAP_BUDGET_SECONDS,
) -> str | None:
    """Kill and reap the agent inside the container; report what it took.

    Returns the reap's own output, or ``None`` when it could not be run. The
    output is a diagnosis, never a control signal: the caller re-raises the
    timeout either way.
    """
    process = await asyncio.create_subprocess_exec(
        *container_reap_argv(compose_base, agent_path),
        env=dict(host_env),
        stdin=asyncio.subprocess.DEVNULL,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
        # Its own session, so the escalation below signals this client's group
        # and never the harness's.
        start_new_session=True,
    )
    try:
        stdout, _ = await asyncio.wait_for(
            process.communicate(), timeout=budget_seconds
        )
    except (TimeoutError, asyncio.CancelledError):
        await terminate_client(process)
        return None
    return stdout.decode(errors="replace") if stdout else ""


async def exec_with_agent_reap(
    argv: Sequence[str],
    compose_base: Sequence[str],
    host_env: Mapping[str, str],
    wire: bytes | bytearray,
    completion_probe: Callable[[], bool] | None,
    agent_path: str,
) -> ExecResult:
    """Run the agent's exec client, reaping the agent if the wait is cut short.

    ``argv`` is the full Compose invocation for the run; ``compose_base`` is
    the project-scoped prefix the reap's own exec is built from.

    The client is spawned into its own session so that a teardown can signal
    the client's process group without reaching the harness — see
    :func:`stella_harbor.stream_release.terminate_client`, which performs that
    teardown for both branches.

    ``BaseException`` and not ``Exception``: the case this exists for is
    ``CancelledError``, which is neither. The reap is awaited before the
    exception continues, so the container is quiet before Harbor moves on to
    the verifier, and the durable journal has stopped growing before the
    adapter re-reads it for the trial's usage.
    """
    process = await asyncio.create_subprocess_exec(
        *argv,
        env=dict(host_env),
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
        start_new_session=True,
    )
    try:
        return await communicate_with_release(process, wire, completion_probe)
    except BaseException:
        # Nothing in here may replace the exception being reported: a trial
        # that timed out must still be recorded as a trial that timed out, and
        # a Docker daemon wedged enough to refuse the reap is precisely when
        # that matters most.
        report: str | None = None
        with contextlib.suppress(Exception, asyncio.CancelledError):
            report = await reap_container_agent(compose_base, host_env, agent_path)
        detail = (report or "").strip() or "the reap could not be run"
        print(f"{REAP_LOG_PREFIX} the run was cut short; {detail}", file=sys.stderr)
        raise
