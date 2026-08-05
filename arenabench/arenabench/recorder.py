# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Real MP4 screen recording of a running trial.

Not a terminal-cast format. Each trial gets a sidecar container running a
virtual X display (Xvfb), a real terminal emulator (xterm) rendering the live
event stream, and ``ffmpeg -f x11grab`` capturing that framebuffer to H.264.
The bytes in the file are pixels that were actually composited by X.

**Why a sidecar rather than recording inside the task container.** A
Terminal-Bench task image is canonical — the whole comparability argument rests
on every agent getting the identical utility set. Installing Xvfb, xterm and
ffmpeg into it would change what the agent can see and do, and in most images
would fail outright (no network, no package manager). The sidecar shares
exactly one thing with the trial: a **read-only** bind mount of the log
directory the agent is already writing. It cannot influence the run it films.

**What is on screen.** In a benchmark, Stella runs headless and one-shot inside
the task container, with no TTY and no interface — there is no native GUI to
film. What the recorder shows is ArenaBench's own live renderer
(``recorder/render.py``) drawing that agent's real event stream in a real
terminal. Stated plainly because the distinction matters: the video is genuine
screen capture of genuine live data, not a replay of the agent's own UI.

Recording is opt-in per match and degrades to nothing. A host with no Docker,
no image, or a failing container produces a match with no videos and a warning
— never a failed run. Filming must not be able to cost you a benchmark.
"""

from __future__ import annotations

import hashlib
import logging
import os
import shutil
import subprocess
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

__all__ = [
    "IMAGE_TAG",
    "RecorderSupervisor",
    "build_image",
    "docker_available",
    "image_present",
]

log = logging.getLogger("arenabench.recorder")

IMAGE_TAG = "arenabench/recorder:1"

#: Give ffmpeg time to write the moov atom. Docker's default 10s stop timeout
#: is usually enough, but a long recording on a loaded host is not, and the
#: failure mode is a truncated unplayable file rather than a loud error.
STOP_TIMEOUT_S = 30


def docker_available() -> bool:
    if shutil.which("docker") is None:
        return False
    try:
        return (
            subprocess.run(
                ["docker", "info"],
                capture_output=True,
                timeout=20,
            ).returncode
            == 0
        )
    except (OSError, subprocess.SubprocessError):
        return False


def image_present(tag: str = IMAGE_TAG) -> bool:
    try:
        return (
            subprocess.run(
                ["docker", "image", "inspect", tag],
                capture_output=True,
                timeout=30,
            ).returncode
            == 0
        )
    except (OSError, subprocess.SubprocessError):
        return False


def build_image(context: Path | None = None, tag: str = IMAGE_TAG) -> bool:
    """Build the recorder image. Returns ``True`` on success."""
    context = context or (Path(__file__).resolve().parent.parent / "recorder")
    if not (context / "Dockerfile").is_file():
        log.error("recorder Dockerfile not found under %s", context)
        return False
    log.info("building %s from %s (one-time, ~1 minute)", tag, context)
    try:
        completed = subprocess.run(
            ["docker", "build", "-t", tag, str(context)],
            capture_output=True,
            text=True,
            timeout=1800,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        log.error("recorder image build failed: %s", exc)
        return False
    if completed.returncode != 0:
        log.error("recorder image build failed:\n%s", completed.stderr[-2000:])
        return False
    return True


@dataclass
class _Recording:
    container: str
    trial_dir: Path
    started_at: float


@dataclass
class RecorderSupervisor:
    """Starts and stops one recorder container per trial, automatically.

    Harbor creates trial directories as it goes and never announces them, so
    this watches the jobs tree instead of being told. A directory that has
    acquired an ``agent/`` subdirectory is a trial that has begun; the
    appearance of ``result.json`` is the trial ending. Both signals are files
    the run already produces, which keeps the recorder outside the run's
    control flow entirely.
    """

    jobs_root: Path
    #: Job directory names to watch — one per contestant.
    jobs: list[str]
    #: Label a trial's video with which contestant produced it.
    contestant_by_job: dict[str, str] = field(default_factory=dict)
    width: int = 1440
    height: int = 900
    fps: int = 10
    poll_interval: float = 2.0
    #: Stop trying to film a trial after this many consecutive start failures.
    max_start_attempts: int = 3

    _active: dict[Path, _Recording] = field(default_factory=dict, init=False)
    _finished: set[Path] = field(default_factory=set, init=False)
    _failures: dict[Path, int] = field(default_factory=dict, init=False)
    _thread: threading.Thread | None = field(default=None, init=False)
    _stop: threading.Event = field(default_factory=threading.Event, init=False)
    _lock: threading.Lock = field(default_factory=threading.Lock, init=False)

    # -- lifecycle --------------------------------------------------------

    def start(self) -> None:
        if self._thread is not None:
            return
        self._stop.clear()
        self._thread = threading.Thread(
            target=self._watch, name="arena-recorder", daemon=True
        )
        self._thread.start()

    def stop(self, timeout: float = 60.0) -> None:
        """Stop watching and finalise every recording still in flight."""
        self._stop.set()
        thread = self._thread
        if thread is not None:
            thread.join(timeout=timeout)
        self._thread = None
        with self._lock:
            active = list(self._active.items())
            self._active.clear()
        for trial_dir, recording in active:
            self._stop_container(recording, trial_dir)

    @property
    def active_count(self) -> int:
        with self._lock:
            return len(self._active)

    # -- internals --------------------------------------------------------

    def _watch(self) -> None:
        while not self._stop.is_set():
            try:
                self._sweep()
            except Exception:  # pragma: no cover - a watcher must never die
                log.exception("recorder sweep failed")
            self._stop.wait(self.poll_interval)

    def _sweep(self) -> None:
        for job in self.jobs:
            job_dir = self.jobs_root / job
            if not job_dir.is_dir():
                continue
            for trial_dir in sorted(p for p in job_dir.iterdir() if p.is_dir()):
                self._consider(trial_dir, job)

    def _consider(self, trial_dir: Path, job: str) -> None:
        if trial_dir in self._finished:
            return
        with self._lock:
            recording = self._active.get(trial_dir)

        finished = (trial_dir / "result.json").exists()
        if recording is not None:
            if finished:
                # Let the renderer draw the final frames before cutting.
                time.sleep(1.0)
                self._stop_container(recording, trial_dir)
                with self._lock:
                    self._active.pop(trial_dir, None)
                self._finished.add(trial_dir)
            return

        if finished:
            # Already over by the time we noticed — nothing to film.
            self._finished.add(trial_dir)
            return
        if not (trial_dir / "agent").is_dir():
            return  # not started yet
        self._start_container(trial_dir, job)

    def _container_name(self, trial_dir: Path) -> str:
        digest = hashlib.sha256(str(trial_dir).encode("utf-8")).hexdigest()[:12]
        return f"arena-rec-{digest}"

    @staticmethod
    def _docker_env() -> dict[str, str]:
        """Docker environment for the recorder, with the task platform pin removed.

        A benchmark host sets ``DOCKER_DEFAULT_PLATFORM=linux/amd64`` because
        Terminal-Bench task images publish amd64 only. The recorder is not a
        task image — it is built locally, for the host's own architecture, and
        it never runs task code. Inheriting the pin makes Docker look for an
        amd64 variant of an arm64 image, fail to find it locally, and then try
        to *pull* ``arenabench/recorder:1`` from a registry that has no such
        repository. The error reads like a missing image; the cause is an
        inherited environment variable.
        """
        env = dict(os.environ)
        env.pop("DOCKER_DEFAULT_PLATFORM", None)
        return env

    def _record_failure(self, trial_dir: Path, detail: str) -> None:
        """Log a start failure once, then stop retrying this trial.

        Without a cap the watcher retries every poll for the lifetime of the
        trial. That is noisy in a log an operator is reading during a live run,
        and when the failure is a registry lookup it is also a network request
        every two seconds for something that will never succeed.
        """
        count = self._failures.get(trial_dir, 0) + 1
        self._failures[trial_dir] = count
        if count == 1:
            log.warning("recorder start failed for %s: %s", trial_dir.name, detail)
        if count >= self.max_start_attempts:
            log.warning(
                "giving up recording %s after %d attempts; the match continues "
                "without video for this trial",
                trial_dir.name,
                count,
            )
            self._finished.add(trial_dir)

    def _start_container(self, trial_dir: Path, job: str) -> None:
        agent_dir = trial_dir / "agent"
        out_dir = trial_dir / "arena"
        try:
            out_dir.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            log.warning("cannot create %s: %s", out_dir, exc)
            return

        name = self._container_name(trial_dir)
        contestant = self.contestant_by_job.get(job, job)
        task = trial_dir.name.rsplit("__", 1)[0]
        command = [
            "docker", "run", "--rm", "--detach",
            "--name", name,
            # No network at all: the recorder reads a file and writes a file.
            "--network", "none",
            "--memory", "512m",
            "--cpus", "0.5",
            "-v", f"{agent_dir}:/logs/agent:ro",
            "-v", f"{out_dir}:/out",
            "-e", f"ARENA_WIDTH={self.width}",
            "-e", f"ARENA_HEIGHT={self.height}",
            "-e", f"ARENA_FPS={self.fps}",
            "-e", f"ARENA_TASK={task}",
            "-e", f"ARENA_CONTESTANT={contestant}",
            IMAGE_TAG,
        ]
        try:
            completed = subprocess.run(
                command,
                capture_output=True,
                text=True,
                timeout=60,
                env=self._docker_env(),
            )
        except (OSError, subprocess.SubprocessError) as exc:
            self._record_failure(trial_dir, str(exc))
            return
        if completed.returncode != 0:
            self._record_failure(trial_dir, completed.stderr.strip()[:400])
            return
        self._failures.pop(trial_dir, None)
        with self._lock:
            self._active[trial_dir] = _Recording(name, trial_dir, time.time())
        log.info("recording %s (%s)", trial_dir.name, contestant)

    def _stop_container(self, recording: _Recording, trial_dir: Path) -> None:
        try:
            subprocess.run(
                ["docker", "stop", "--timeout", str(STOP_TIMEOUT_S), recording.container],
                capture_output=True,
                timeout=STOP_TIMEOUT_S + 30,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            log.warning("recorder stop failed for %s: %s", trial_dir.name, exc)
            return
        video = trial_dir / "arena" / "recording.mp4"
        if video.exists() and video.stat().st_size > 0:
            log.info(
                "recorded %s -> %s (%.1f MB)",
                trial_dir.name,
                video,
                video.stat().st_size / 1e6,
            )
        else:
            log.warning("recorder produced no video for %s", trial_dir.name)


def preflight(build_if_missing: bool = True) -> tuple[bool, str]:
    """Can this host record? Returns ``(ok, human explanation)``.

    Called before a match so the operator learns that recording is unavailable
    at setup time rather than discovering an empty video gallery an hour later.
    """
    if not docker_available():
        return False, "Docker is not available, so trials cannot be recorded."
    if image_present():
        return True, "recorder image ready"
    if not build_if_missing:
        return False, f"{IMAGE_TAG} is not built (run `arenabench build-recorder`)"
    if build_image():
        return True, f"built {IMAGE_TAG}"
    return False, f"could not build {IMAGE_TAG}; see the log for the build output"
