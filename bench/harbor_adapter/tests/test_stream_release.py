"""The exec-stream release when a task daemon outlives Stella (#2122).

``docker compose exec``'s output stream reaches EOF only when every fd holder
inside the container lets go. Several Terminal-Bench tasks require the agent
to leave a daemon running (``sshd``, ``nginx``); the daemon inherits the exec
stream, so a plain ``communicate()`` outlived a *finished* trial until
Harbor's agent timeout killed it. These tests pin the release helper's
contract with real local processes shaped like the two hangs seen in match
``14d0127b8ca7``: a live client blocked on a held stream, and an already-dead
client whose orphan shares the pipe directly.
"""

from __future__ import annotations

import asyncio
import time

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor import _communicate_with_completion_probe  # noqa: E402


async def _spawn(script: str) -> asyncio.subprocess.Process:
    return await asyncio.create_subprocess_exec(
        "sh",
        "-c",
        script,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
    )


class TestStreamRelease:
    def test_probe_releases_a_stream_held_by_a_live_client(self) -> None:
        """The docker shape: the client blocks on a stream that never EOFs.

        ``exec sleep`` keeps the direct child alive holding stdout, exactly
        like a compose exec client whose in-container daemon never closes the
        stream. Without the probe this communicate() outlives the trial; with
        it, the wait must return in seconds, keep the output written before
        the hold, and report the release.
        """

        async def scenario() -> tuple[bytes, bool]:
            process = await _spawn("echo held; exec sleep 30")
            return await _communicate_with_completion_probe(
                process,
                b"",
                lambda: True,
                poll_seconds=0.1,
                grace_seconds=0.2,
                drain_seconds=1.0,
            )

        started = time.monotonic()
        stdout, released = asyncio.run(asyncio.wait_for(scenario(), timeout=10))
        assert released is True
        assert stdout == b"held\n"
        assert time.monotonic() - started < 10

    def test_probe_releases_when_only_an_orphan_holds_the_pipe(self) -> None:
        """The nastier shape: the client already exited, an orphan holds on.

        ``sleep 30 &`` inherits stdout and survives ``sh``; there is nothing
        left to signal, so the helper must stop reading rather than block on
        an EOF that cannot come. Captured stdout is forfeit in this corner —
        the durable journal is the source of truth — but the wait must end.
        """

        async def scenario() -> tuple[bytes, bool]:
            process = await _spawn("sleep 30 & echo held")
            return await _communicate_with_completion_probe(
                process,
                b"",
                lambda: True,
                poll_seconds=0.1,
                grace_seconds=0.2,
                drain_seconds=0.5,
            )

        stdout, released = asyncio.run(asyncio.wait_for(scenario(), timeout=10))
        assert released is True
        assert stdout == b""

    def test_natural_eof_within_grace_is_not_a_release(self) -> None:
        """A stream that ends on its own wins the race; the client is spared."""

        async def scenario() -> tuple[bytes, bool]:
            process = await _spawn("sleep 0.3; echo done")
            return await _communicate_with_completion_probe(
                process,
                b"",
                lambda: True,
                poll_seconds=0.1,
                grace_seconds=5.0,
                drain_seconds=1.0,
            )

        stdout, released = asyncio.run(asyncio.wait_for(scenario(), timeout=10))
        assert released is False
        assert stdout == b"done\n"

    def test_unproven_completion_never_releases(self) -> None:
        """A probe that stays false leaves the wait alone — a hung pipeline
        must remain visible as a hang (#2110), never dressed as finished."""

        async def scenario() -> tuple[bytes, bool]:
            process = await _spawn("echo eof")
            return await _communicate_with_completion_probe(
                process,
                b"",
                lambda: False,
                poll_seconds=0.05,
                grace_seconds=0.1,
                drain_seconds=0.5,
            )

        stdout, released = asyncio.run(asyncio.wait_for(scenario(), timeout=10))
        assert released is False
        assert stdout == b"eof\n"

    def test_no_probe_matches_plain_communicate(self) -> None:
        """Callers that pass no probe get the exact pre-#2122 behavior."""

        async def scenario() -> tuple[bytes, bool]:
            process = await _spawn("cat")
            return await _communicate_with_completion_probe(
                process,
                b"echoed\n",
                None,
            )

        stdout, released = asyncio.run(asyncio.wait_for(scenario(), timeout=10))
        assert released is False
        assert stdout == b"echoed\n"
