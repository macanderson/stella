"""A container rejecting the SUT binary must not be scored as an agent failure.

Harbor records the exception class raised during agent setup into the trial's
``result.json``. On 2026-07-31 five trials whose binary could not execute were
recorded as ``NonZeroAgentExitCodeError`` — the same class a genuine nonzero
agent exit carries — and scored 0.0. Under a fixed denominator that is
arithmetically indistinguishable from Stella failing those tasks.

These tests pin the two halves of the fix: a loader diagnostic is reclassified,
and anything else keeps the class it already had. Both matter — reclassifying
every install failure would hide real ones just as thoroughly as the bug being
fixed here.

Kept out of ``test_adapter.py`` so the ELF-level tests in ``test_portability``
stay runnable without Harbor, and so this feature's tests live together.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from types import SimpleNamespace

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from harbor.agents.installed.base import NonZeroAgentExitCodeError  # noqa: E402

from stella_harbor import StellaAgent, _sha256_file  # noqa: E402
from stella_harbor.portability import (  # noqa: E402
    BinaryIncompatibleWithContainerError,
)
from tests.test_portability import build_elf  # noqa: E402

# Verbatim from
# rig-runs/jobs/dev2-armA-stella/qemu-alpine-ssh__Q2td85H/exception.txt.
_GLIBC_REJECTION = (
    "Command failed (exit 1): sha256sum /tmp/stella-upload && cp "
    "/tmp/stella-upload /usr/local/bin/stella && chmod +x "
    "/usr/local/bin/stella && /usr/local/bin/stella --version\n"
    "stdout: 852f8cfd308ee063e3ad46b556483ae1fd9c591cfc89a0d9d2defbe51eef3687  "
    "/tmp/stella-upload\n"
    "/usr/local/bin/stella: /lib/x86_64-linux-gnu/libc.so.6: version "
    "`GLIBC_2.34' not found (required by /usr/local/bin/stella)"
)


class _Environment:
    """Enough of a Harbor environment for ``install`` to reach its assertions."""

    task_env_config = SimpleNamespace(workdir="/workspace")

    async def upload_file(self, source: str, destination: str) -> None:
        return None


def _agent() -> StellaAgent:
    """A StellaAgent bypassing Harbor's base ``__init__``, as test_adapter does."""
    agent = StellaAgent.__new__(StellaAgent)
    agent._extra_env = {}
    return agent


def _install_raising(message: str):
    async def _exec_as_root(
        environment: object, *, command: str, timeout_sec: int
    ) -> SimpleNamespace:
        raise NonZeroAgentExitCodeError(message)

    return _exec_as_root


class TestLoaderRejectionIsNotAnAgentFailure:
    def test_glibc_rejection_raises_its_own_class(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        binary = tmp_path / "stella"
        binary.write_bytes(build_elf(requirements={"libc.so.6": ["GLIBC_2.34"]}))
        binary.chmod(0o755)
        monkeypatch.setenv("STELLA_BINARY", str(binary))

        agent = _agent()
        agent.exec_as_root = _install_raising(_GLIBC_REJECTION)

        with pytest.raises(BinaryIncompatibleWithContainerError) as caught:
            asyncio.run(agent.install(_Environment()))

        error = caught.value
        assert not isinstance(error, NonZeroAgentExitCodeError), (
            "the point is that Harbor records a different class for this trial"
        )
        message = str(error)
        assert "GLIBC_2.34" in message
        assert "not the agent failing the task" in message
        assert "build_sut.sh" in message
        # The host-side profile of what was actually uploaded is what turns a
        # container-side mystery into an actionable operator message.
        assert "max GLIBC_2.34" in message

    def test_wrong_architecture_is_also_classified(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        binary = tmp_path / "stella"
        binary.write_bytes(build_elf(machine=183, requirements={"libc.so.6": []}))
        binary.chmod(0o755)
        monkeypatch.setenv("STELLA_BINARY", str(binary))

        agent = _agent()
        agent.exec_as_root = _install_raising(
            "Command failed (exit 126): /usr/local/bin/stella: cannot execute "
            "binary file: Exec format error"
        )
        with pytest.raises(BinaryIncompatibleWithContainerError) as caught:
            asyncio.run(agent.install(_Environment()))
        assert "aarch64" in str(caught.value)

    @pytest.mark.parametrize(
        "message",
        [
            "Command failed (exit 1): cp: cannot create regular file "
            "'/usr/local/bin/stella': Read-only file system",
            "Command failed (exit 1): sha256sum: /tmp/stella-upload: "
            "No space left on device",
        ],
    )
    def test_unrelated_install_failures_keep_their_class(
        self, message: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        binary = tmp_path / "stella"
        binary.write_bytes(build_elf(requirements={"libc.so.6": ["GLIBC_2.17"]}))
        binary.chmod(0o755)
        monkeypatch.setenv("STELLA_BINARY", str(binary))

        agent = _agent()
        agent.exec_as_root = _install_raising(message)
        with pytest.raises(NonZeroAgentExitCodeError):
            asyncio.run(agent.install(_Environment()))


class TestProfilingNeverBreaksInstall:
    def test_a_non_elf_binary_installs_exactly_as_before(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The blocking check lives in env.sh; nothing here may become fatal.

        A development binary, a Mach-O build from a macOS tree, or the short
        byte strings the adapter's other tests write must all install unchanged.
        """
        binary = tmp_path / "stella"
        binary.write_bytes(b"\xcf\xfa\xed\xfe not an elf")
        binary.chmod(0o755)
        monkeypatch.setenv("STELLA_BINARY", str(binary))

        agent = _agent()

        async def _exec_as_root(
            environment: object, *, command: str, timeout_sec: int
        ) -> SimpleNamespace:
            return SimpleNamespace(
                return_code=0,
                stdout=f"{_sha256_file(binary)}  /tmp/stella-upload\nstella 0.6.24",
            )

        agent.exec_as_root = _exec_as_root
        asyncio.run(agent.install(_Environment()))
        assert agent._binary_sha256_verified is True

    def test_an_unreadable_binary_does_not_mask_the_loader_diagnosis(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """Even with no profile to report, the failure is still reclassified."""
        binary = tmp_path / "stella"
        binary.write_bytes(b"not an elf")
        binary.chmod(0o755)
        monkeypatch.setenv("STELLA_BINARY", str(binary))

        agent = _agent()
        agent.exec_as_root = _install_raising(_GLIBC_REJECTION)
        with pytest.raises(BinaryIncompatibleWithContainerError) as caught:
            asyncio.run(agent.install(_Environment()))
        assert "GLIBC_2.34" in str(caught.value)
