"""The runner's instance-netns claim: reclaim a dead trial's namespace, refuse a live one.

Batch places trials on warm instances, and every trial on an instance shares
one network namespace (ECS ``networkMode: host``). The claim has to separate
two states that look identical from the outside — leftover Docker chains from a
trial that already exited, versus a trial running right now — because getting
it wrong is expensive in both directions:

* Treating residue as a live neighbour refuses the trial. That is what took out
  **157 of 178 trials** in the 2026-08-11 panel: the 16 placed first, each on a
  fresh instance, ran; every trial placed on a reused instance died in ~5s.
* Treating a live neighbour as residue would flush a running dockerd's chains
  and break the trial that legitimately owns the instance.

The discriminator is an exclusive lock on the instance-wide scratch volume,
which the kernel releases when its holder dies. These exercise the lock
protocol itself with real ``flock`` processes; the iptables half needs a
namespace to manipulate and is covered by the smoke job.
"""

from __future__ import annotations

import os
import re
import signal
import subprocess
import sys
from pathlib import Path

import pytest

ENTRYPOINT = Path(__file__).resolve().parents[1] / "infra" / "runner" / "entrypoint.sh"

#: The lock-protocol tests drive real ``flock(1)``, a util-linux tool absent on
#: macOS. Scoped to those three deliberately: the wiring assertions below read
#: the script and must run on every platform, or a local run would report six
#: skips and prove nothing about the fix.
needs_flock = pytest.mark.skipif(
    sys.platform == "darwin", reason="flock(1) is a util-linux tool; the runner is Linux"
)


def _claim(scratch: Path) -> subprocess.CompletedProcess[str]:
    """Run the claim's lock step exactly as the entrypoint does."""
    script = f"""
    set -eu
    exec 9>>"{scratch}/.instance-netns.lock"
    if ! flock -n 9; then echo REFUSED; exit 1; fi
    echo CLAIMED
    """
    return subprocess.run(
        ["sh", "-c", script], capture_output=True, text=True, timeout=30
    )


@needs_flock
def test_a_free_instance_is_claimed(tmp_path: Path) -> None:
    assert "CLAIMED" in _claim(tmp_path).stdout


@needs_flock

def _spawn_holder(lock: Path) -> subprocess.Popen[bytes]:
    """A process that really holds ``lock``, and can really be killed.

    ``flock <file> <cmd>`` opens the file, takes the lock, and runs ``cmd`` as a
    **child** that inherits the open descriptor. So killing the `flock` pid
    alone leaves that child alive still holding the descriptor — and therefore
    still holding the lock. A test that did so would observe REFUSED after the
    "holder" had died and read it as the guard being wrong, when what actually
    survived was the test's own orphan. It also strands a `sleep` for the CI
    runner to reap, which is the visible symptom.

    So the holder gets its own session and is killed by process group.
    """
    return subprocess.Popen(
        ["flock", str(lock), "sleep", "30"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )


def _kill_holder(holder: "subprocess.Popen[bytes]") -> None:
    """Kill the holder *and* the child holding the descriptor."""
    try:
        os.killpg(os.getpgid(holder.pid), signal.SIGKILL)
    except ProcessLookupError:
        pass
    holder.wait(timeout=10)


def test_a_lock_held_by_a_live_process_is_refused(tmp_path: Path) -> None:
    lock = tmp_path / ".instance-netns.lock"
    lock.touch()
    # A neighbour that is genuinely still running: holds the lock and sleeps.
    holder = _spawn_holder(lock)
    try:
        # `flock` needs a moment to actually acquire before we race it.
        subprocess.run(["sleep", "0.5"], check=True)
        result = _claim(tmp_path)
        assert result.returncode != 0, "a live neighbour must be refused"
        assert "REFUSED" in result.stdout
    finally:
        _kill_holder(holder)


@needs_flock
def test_a_lock_whose_holder_died_is_reclaimed(tmp_path: Path) -> None:
    """The case the old guard got wrong.

    The previous trial exited. Its lock file still exists — and under the old
    logic its leftover iptables chains would have refused this trial — but the
    kernel dropped the lock when the holder died, so the instance is free.
    """
    lock = tmp_path / ".instance-netns.lock"
    holder = _spawn_holder(lock)
    subprocess.run(["sleep", "0.5"], check=True)
    _kill_holder(holder)

    assert lock.exists(), "the file outlives the holder; only the lock is dropped"
    assert "CLAIMED" in _claim(tmp_path).stdout


class TestEntrypointWiring:
    """The script says what these tests assume it says."""

    def test_the_claim_replaced_the_residue_only_guard(self) -> None:
        source = ENTRYPOINT.read_text()
        assert "claim_instance_netns" in source
        assert "assert_sole_dockerd" not in source, (
            "the old guard refused on residue alone; it must not linger as a "
            "second, contradictory path"
        )

    def test_the_lock_outlives_the_claim_function(self) -> None:
        # Releasing at function exit would admit a neighbour while our dockerd
        # is still running — the failure the guard exists to prevent.
        source = ENTRYPOINT.read_text()
        assert re.search(r"exec 9>>.*instance-netns\.lock", source)

    def test_chains_are_only_discarded_under_the_lock(self) -> None:
        source = ENTRYPOINT.read_text()
        body = source.split("claim_instance_netns()", 1)[1]
        flock_at = body.index("flock -n 9")
        discard_at = body.index("discard_dead_docker_chains")
        assert flock_at < discard_at, (
            "chains must only be flushed after the lock proves nobody else is "
            "using this namespace"
        )
