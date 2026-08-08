"""Releasing the exec stream when a task daemon outlives Stella (#2122).

``docker compose exec``'s output stream reaches EOF only when every fd holder
inside the container lets go. A daemon the agent legitimately started —
``sshd``, ``nginx``; several Terminal-Bench tasks require one — inherits the
stream and holds it open past Stella's exit, so a plain ``communicate()``
outlives a *finished* trial until Harbor's agent timeout kills it.

The release therefore consults evidence from outside the stream: Stella's own
durable journal, which is host-visible while the run is live. Only a probe
that proves completion releases the wait. A run that never proves completion
is deliberately NOT released — a hung pipeline must stay visible as a timeout
rather than be dressed up as a finished trial (#2110 stays loud).

Nothing inside the container is ever signalled: only the **client** process is
terminated, so daemons the verifier will probe stay up.
"""

from __future__ import annotations

import asyncio
import contextlib
from collections.abc import Callable

from harbor.environments.base import ExecResult

#: How often the completion probe is consulted while the exec stream is open.
_STREAM_PROBE_POLL_SECONDS = 5.0
#: How long a proven-finished stream may still end on its own before the
#: client is terminated. Generous enough for an ordinary EOF racing the probe,
#: short enough that a held stream costs seconds rather than the agent budget.
_STREAM_PROBE_GRACE_SECONDS = 10.0
#: Bytes of the journal's tail scanned for the terminal event. The event is
#: terminal and the compact serde encoding is byte-stable, so a tail scan is
#: both sufficient and bounded against a journal that grows all run.
_JOURNAL_TAIL_BYTES = 4096

#: Rides in ``ExecResult.stderr`` when the stream was released, so the fact
#: lands in the trial's stderr log through the existing write path.
STREAM_RELEASED_NOTE = (
    "stella-harbor: exec stream released after the journal's terminal event; "
    "a process the agent left running held the stream open past Stella's "
    "exit (#2122)"
)


def journal_reports_completion(stream: str | None) -> bool:
    """Whether the durable journal carries Stella's terminal ``complete`` event.

    The journal is host-visible while the run is live (it is what the arena
    streams), so the last line the pipeline writes before exiting can prove
    the run finished even when the exec stream cannot EOF.
    """
    return bool(stream) and '"type":"complete"' in stream[-_JOURNAL_TAIL_BYTES:]


async def communicate_with_completion_probe(
    process: asyncio.subprocess.Process,
    wire: bytes | bytearray,
    completion_probe: Callable[[], bool] | None,
    *,
    poll_seconds: float = _STREAM_PROBE_POLL_SECONDS,
    grace_seconds: float = _STREAM_PROBE_GRACE_SECONDS,
    drain_seconds: float = 5.0,
) -> tuple[bytes, bool]:
    """``communicate()``, released early once the run is proven finished.

    The probe consults evidence outside the stream (see the module docstring);
    the stream is granted ``grace_seconds`` to end on its own, then the client
    process is terminated.

    Returns the captured stdout and whether the wait was released by the probe
    rather than by stream EOF.
    """
    communicate = asyncio.ensure_future(process.communicate(input=wire))

    async def _finish(released: bool) -> tuple[bytes, bool]:
        stdout_bytes, _ = await communicate
        return stdout_bytes, released

    try:
        if completion_probe is None:
            return await _finish(False)
        while True:
            done, _ = await asyncio.wait({communicate}, timeout=poll_seconds)
            if done:
                return await _finish(False)
            if not completion_probe():
                continue
            done, _ = await asyncio.wait({communicate}, timeout=grace_seconds)
            if done:
                return await _finish(False)
            # The client may itself have exited already (only a grandchild
            # holds the pipe); a signal to a reaped pid is not an error here.
            with contextlib.suppress(ProcessLookupError):
                process.terminate()
            done, _ = await asyncio.wait({communicate}, timeout=drain_seconds)
            if not done:
                with contextlib.suppress(ProcessLookupError):
                    process.kill()
                done, _ = await asyncio.wait({communicate}, timeout=drain_seconds)
            if done:
                return await _finish(True)
            # Nothing left to signal and the pipe is still held (an orphan
            # shares our read end). Stop reading; the journal carries the
            # run's truth and the caller's cleanup owns the process.
            communicate.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await communicate
            return b"", True
    except BaseException:
        communicate.cancel()
        raise


async def communicate_with_release(
    process: asyncio.subprocess.Process,
    wire: bytes | bytearray,
    completion_probe: Callable[[], bool] | None,
) -> ExecResult:
    """Wait on the exec client and fold the outcome into an ``ExecResult``.

    Wraps :func:`communicate_with_completion_probe` with the teardown a caller
    must not skip, and with the result a release implies: the client was
    terminated by the release, so its return code is the signal, not Stella's.
    The journal — which the caller already treats as the source of truth over
    captured stdout — proved the run finished, and the note rides in stderr so
    the release lands in the trial's log rather than being silent.
    """
    try:
        stdout_bytes, released = await communicate_with_completion_probe(
            process, wire, completion_probe
        )
    except BaseException:
        if process.returncode is None:
            process.terminate()
            try:
                await asyncio.wait_for(process.wait(), timeout=5)
            except TimeoutError:
                process.kill()
                await process.wait()
        raise
    stdout = stdout_bytes.decode(errors="replace") if stdout_bytes else None
    if released:
        return ExecResult(stdout=stdout, stderr=STREAM_RELEASED_NOTE, return_code=0)
    return ExecResult(
        stdout=stdout,
        stderr=None,
        return_code=process.returncode or 0,
    )
