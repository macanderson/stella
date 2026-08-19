# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""A seat that never loaded its adapter did not lose a contest.

Two defects with one consequence, both observed on real paid matches:

* The arena resolved the ``stella_harbor`` sources out of its launch checkout
  at **every** trial's install. Deleting that worktree mid-match killed the
  four trials of ``cc00894779ff`` that installed afterwards — 0 steps, 0
  tokens each — and the dashboard scored all four as agent losses, publishing
  ``solve_rate`` 40% against a true agent record of 4/6 (#2127).
* When the adapter was simply absent, ``harbor_agent`` fell back to
  ``_Base = object``, discarding the actionable ``ImportError`` it had just
  built. ``ArenaStellaAgent(...)`` then raised ``TypeError: takes no
  arguments`` and Harbor **exited 0**, so the seat reported *done* having run
  nothing (#2192).

Both now fail by name, and both names are classified as infrastructure, so
neither can re-enter ``solve_rate``'s denominator.
"""

from __future__ import annotations

import itertools
import shutil
import threading
import time
from pathlib import Path

import pytest

from arenabench import adapter
from arenabench.adapter import (
    PACKAGE_NAME,
    AdapterUnavailableError,
    adapter_root,
    stage_adapter,
)
from arenabench.harbor_agent import StellaAdapterMissingError, _unavailable_base
from arenabench.telemetry import INFRASTRUCTURE_FAILURES, VOID_SETUP


@pytest.fixture(autouse=True)
def _isolated_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """A private arena home, and an empty process cache, per test."""
    monkeypatch.setenv("ARENABENCH_HOME", str(tmp_path / "home"))
    monkeypatch.setattr(adapter, "_staged", {})
    yield


def _source_tree(root: Path) -> Path:
    """A minimal adapter checkout: the package, some code, and a data file."""
    package = root / PACKAGE_NAME
    (package / "sub").mkdir(parents=True)
    (package / "__init__.py").write_text("VERSION = '1'\n", encoding="utf-8")
    (package / "sub" / "posture.py").write_text("X = 1\n", encoding="utf-8")
    (package / "prices.json").write_text('{"a": 1}\n', encoding="utf-8")
    return root


def test_a_staged_adapter_outlives_the_checkout_it_was_copied_from(tmp_path: Path):
    """The #2127 witness: trial 1 stages, the worktree dies, trial 2 runs."""
    source = _source_tree(tmp_path / "worktree" / "bench" / "harbor_adapter")

    first = stage_adapter(source)
    assert (first / PACKAGE_NAME / "__init__.py").is_file()
    assert adapter_root() in first.parents

    # The launch worktree is deleted mid-match, exactly as in cc00894779ff.
    shutil.rmtree(tmp_path / "worktree")
    assert not source.exists()

    second = stage_adapter(source)
    assert second == first
    assert (second / PACKAGE_NAME / "sub" / "posture.py").read_text() == "X = 1\n"
    assert (second / PACKAGE_NAME / "prices.json").is_file()


def test_staging_is_content_addressed_so_an_edited_adapter_is_not_shadowed(
    tmp_path: Path,
):
    source = _source_tree(tmp_path / "adapter")
    before = stage_adapter(source)

    (source / PACKAGE_NAME / "sub" / "posture.py").write_text("X = 2\n", encoding="utf-8")
    adapter._staged.clear()  # a later match, same process
    after = stage_adapter(source)

    assert after != before, "an edited adapter must stage a fresh directory"
    assert (after / PACKAGE_NAME / "sub" / "posture.py").read_text() == "X = 2\n"


def test_a_data_file_change_alone_restages(tmp_path: Path):
    """The digest folds in every file, not just ``*.py``."""
    source = _source_tree(tmp_path / "adapter")
    before = stage_adapter(source)

    (source / PACKAGE_NAME / "prices.json").write_text('{"a": 2}\n', encoding="utf-8")
    adapter._staged.clear()

    assert stage_adapter(source) != before


def test_absent_sources_with_nothing_staged_refuse_by_name(tmp_path: Path):
    """The honest failure — and a name telemetry can classify."""
    with pytest.raises(AdapterUnavailableError) as caught:
        stage_adapter(tmp_path / "gone")
    assert "ARENABENCH_STELLA_ADAPTER" in str(caught.value)


def test_an_empty_package_directory_is_refused_rather_than_staged(tmp_path: Path):
    (tmp_path / "adapter" / PACKAGE_NAME).mkdir(parents=True)
    with pytest.raises(AdapterUnavailableError):
        stage_adapter(tmp_path / "adapter")


def test_concurrent_staging_never_publishes_a_half_copied_tree(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """The server is threaded, so two matches stage one digest at once.

    A staging directory named after the digest alone is shared, and the
    damage is permanent: thread B's ``rmtree`` deletes A's finished copy, A
    then renames B's *in-flight* tree into place, and the cache check never
    revisits a digest that already exists — every later trial in the process
    imports the partial adapter.

    The interleaving is forced rather than raced, so this fails
    deterministically against a shared staging name.
    """
    source = _source_tree(tmp_path / "adapter")
    real_copytree = shutil.copytree
    a_copied = threading.Event()
    b_mid_copy = threading.Event()
    a_done = threading.Event()
    calls = itertools.count()

    def sequenced(src, dst, *args, **kwargs):
        # ``copytree`` recurses through this same module attribute for every
        # subdirectory; only the top-level package copy is a staging attempt.
        if Path(dst).name != PACKAGE_NAME:
            return real_copytree(src, dst, *args, **kwargs)
        if next(calls) == 0:
            # A: copy in full, then hold before the rename while B runs.
            result = real_copytree(src, dst, *args, **kwargs)
            a_copied.set()
            assert b_mid_copy.wait(timeout=10), "B never reached its copy"
            return result
        # B: land one file, let A rename whatever is at the staging path,
        # then finish. Mid-copy is the state A must not be able to publish.
        src, dst = Path(src), Path(dst)
        dst.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src / "__init__.py", dst / "__init__.py")
        b_mid_copy.set()
        assert a_done.wait(timeout=10), "A never finished"
        for path in sorted(p for p in src.rglob("*") if p.is_file()):
            target = dst / path.relative_to(src)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
        return str(dst)

    monkeypatch.setattr(adapter.shutil, "copytree", sequenced)
    failures: list[BaseException] = []

    def stage(record_done: bool):
        try:
            stage_adapter(source)
        except BaseException as error:  # a thread's failure is asserted below
            failures.append(error)
        finally:
            if record_done:
                a_done.set()

    first = threading.Thread(target=stage, args=(True,))
    first.start()
    assert a_copied.wait(timeout=10), f"A never copied: {failures}"
    adapter._staged.clear()  # a second match, same process, same digest
    second = threading.Thread(target=stage, args=(False,))
    second.start()
    first.join(timeout=15)
    second.join(timeout=15)
    assert not first.is_alive() and not second.is_alive(), "staging wedged"
    assert not failures, f"staging raised: {failures}"

    published = adapter_root() / adapter._digest(source) / PACKAGE_NAME
    assert (published / "__init__.py").is_file()
    assert (published / "prices.json").is_file(), "published a half-copied tree"
    assert (published / "sub" / "posture.py").read_text() == "X = 1\n"


def test_a_missing_adapter_refuses_at_construction_naming_the_fix():
    """The #2192 witness.

    The old stand-in was ``object``; constructing against it raised
    ``TypeError: takes no arguments`` and Harbor exited 0.
    """
    base = _unavailable_base(
        StellaAdapterMissingError(
            "arenabench's Stella contestant needs the `stella_harbor` adapter "
            "on PYTHONPATH. Point ARENABENCH_STELLA_ADAPTER at "
            "<stella>/bench/harbor_adapter."
        )
    )

    class Seat(base):  # type: ignore[misc,valid-type]
        pass

    with pytest.raises(StellaAdapterMissingError) as caught:
        Seat("task-id", model="x")
    assert "ARENABENCH_STELLA_ADAPTER" in str(caught.value)
    assert not isinstance(caught.value, TypeError)


def test_both_failures_are_infrastructure_not_agent_losses():
    """The half that actually moves the published number."""
    for name in ("AdapterUnavailableError", "StellaAdapterMissingError"):
        assert name in INFRASTRUCTURE_FAILURES, f"{name} would score as a loss"
        assert name in VOID_SETUP, f"{name} needs an outcome-taxonomy bucket"


def test_the_named_error_is_still_an_import_error():
    """Existing ``except ImportError`` paths must keep catching it."""
    assert issubclass(StellaAdapterMissingError, ImportError)


class TestTheRigMustBeAbleToRunTheSeatItAccepts:
    """An arena that cannot run a Stella seat must not silently accept one.

    ``arenabench serve`` booted happily on an interpreter that cannot import
    ``stella_harbor``, and the server looked completely healthy. Every Stella
    seat it launched then died constructing the agent — and Harbor **exits 0**
    on that path, so the UI showed the seat as *done*. The match measured
    nothing and did not say so (#2325).

    The named refusal from #2192 fixed the *message*; it did not stop the match
    from being accepted, and a refusal at trial time has already spent the
    container. So the question is asked before anything launches — and asked of
    the interpreter that will actually run the seat, which is the one behind
    the resolved ``harbor`` console script, never ``sys.executable``.
    """

    def _fails(self, _interpreter: str, _roots: list[str]) -> tuple[int, str]:
        return 1, "ModuleNotFoundError: No module named 'harbor'"

    def _passes(self, _interpreter: str, _roots: list[str]) -> tuple[int, str]:
        return 0, ""

    def test_an_interpreter_that_cannot_import_the_seat_is_a_refusal(self) -> None:
        problem = adapter.stella_seat_problem("/venv/bin/harbor", probe=self._fails)
        assert problem
        assert "No module named 'harbor'" in problem
        # The message must name the fix, not merely the symptom: the whole
        # incident was an operator looking at a healthy-looking arena.
        assert "bench/harbor_adapter" in problem
        assert "uv sync" in problem

    def test_a_working_interpreter_is_no_refusal(self) -> None:
        assert adapter.stella_seat_problem("/venv/bin/harbor", probe=self._passes) is None

    def test_the_interpreter_asked_is_the_one_behind_the_harbor_script(
        self, tmp_path: Path
    ) -> None:
        """A console script's shebang is what the kernel will exec.

        Reading it is the difference between asking the interpreter that will
        run the seat and asking this process's — a proxy that is wrong in both
        directions, since an arena served by a Python with no ``harbor`` can
        still launch seats through a Harbor from another virtualenv.
        """
        venv = tmp_path / "adapter-venv" / "bin"
        venv.mkdir(parents=True)
        interpreter = venv / "python"
        interpreter.write_text("", encoding="utf-8")
        script = venv / "harbor"
        script.write_text(f"#!{interpreter}\n# console script\n", encoding="utf-8")
        assert adapter.harbor_interpreter(str(script)) == str(interpreter)

        asked: list[str] = []

        def probe(interp: str, _roots: list[str]) -> tuple[int, str]:
            asked.append(interp)
            return 0, ""

        adapter.stella_seat_problem(str(script), probe=probe)
        assert asked == [str(interpreter)]

    def test_a_uv_shell_wrapper_resolves_to_the_python_beside_it(
        self, tmp_path: Path
    ) -> None:
        """``uv`` writes a ``#!/bin/sh`` re-exec stub, not a Python shebang.

        Taking that shebang at face value hands the seat probe a shell, which
        answers ``import: command not found`` — so every Stella seat is refused
        on a rig that would have run it, while Claude Code's seat launches
        beside it and the match reports a one-sided result. Frontier-Bench
        makes this the only path there is: its ``min_harbor`` forces the
        uv-provisioned Harbor, whose console script is exactly this shape.
        """
        venv = tmp_path / "harbor-venv" / "bin"
        venv.mkdir(parents=True)
        interpreter = venv / "python"
        interpreter.write_text("", encoding="utf-8")
        script = venv / "harbor"
        script.write_text(
            "#!/bin/sh\n"
            "'''exec' \"$(dirname -- \"$(realpath -- \"$0\")\")\"/'python' "
            '"$0" "$@"\n'
            "' '''\n",
            encoding="utf-8",
        )
        assert adapter.harbor_interpreter(str(script)) == str(interpreter)

        asked: list[str] = []

        def probe(interp: str, _roots: list[str]) -> tuple[int, str]:
            asked.append(interp)
            return 0, ""

        adapter.stella_seat_problem(str(script), probe=probe)
        assert asked == [str(interpreter)]

    def test_a_non_stella_contest_is_never_blocked_by_our_adapter(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """A Claude-Code-vs-Codex match loads none of this and must still run."""
        from arenabench.model import Contestant, Engine, MatchSpec

        monkeypatch.setattr(
            adapter, "stella_seat_problem", lambda *_a, **_k: "would refuse"
        )
        spec = MatchSpec(
            id="m",
            name="m",
            dataset="terminal-bench-2.1",
            tasks=("fix-git",),
            contestants=(
                Contestant(
                    id="cc",
                    name="cc",
                    agent="claude-code",
                    engine=Engine(api="anthropic", model="claude-fable-5"),
                ),
            ),
        )
        assert adapter.stella_seat_problem_for(spec) is None

    def test_the_launch_refuses_the_seat_rather_than_exiting_zero(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The load-bearing witness: no container, and a named infrastructure
        failure instead of a seat that reports done having measured nothing."""
        import subprocess

        from arenabench.registry import DEFAULT_REGISTRY
        from arenabench.runner import MatchRunner

        monkeypatch.setattr(
            "arenabench.harbor.harbor_bin", lambda dataset_key=None: "/usr/bin/harbor"
        )
        monkeypatch.setattr(
            "arenabench.harbor.harbor_version", lambda binary=None: "0.20.0"
        )
        monkeypatch.setattr(
            "arenabench.harbor.supports_agent_import_path", lambda binary=None: False
        )
        monkeypatch.setattr(
            adapter,
            "stella_seat_problem",
            lambda binary=None, **_kw: "this rig cannot build a Stella seat",
        )

        def _never(*_args, **_kwargs):
            raise AssertionError("a refused seat must not start a Harbor subprocess")

        monkeypatch.setattr(subprocess, "Popen", _never)

        from arenabench.model import MatchSpec

        spec = MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "tasks": ["alpha"],
                "sut_ref": "",
                "contestants": [
                    {
                        "name": "s",
                        "agent": "stella",
                        "engine": {"api": "openrouter", "model": "x/y"},
                    }
                ],
            }
        )
        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        match = runner.create(spec)
        runner.start(match)

        (run,) = match.runs.values()
        assert "cannot build a Stella seat" in run.error
        assert run.process is None
        # And the match says it failed rather than reporting a finished contest
        # in which one seat happened to score nothing. `start` hands the
        # settlement to a watcher thread, so wait for it rather than racing it.
        deadline = time.monotonic() + 10.0
        while match.status == "running" and time.monotonic() < deadline:
            time.sleep(0.05)
        assert match.status == "failed"

    def test_the_refusal_name_is_classified_as_infrastructure(self) -> None:
        """`AdapterUnavailableError` is what the launch raises, and it must stay
        out of `solve_rate`'s denominator — a seat that never loaded did not
        lose."""
        assert AdapterUnavailableError.__name__ in INFRASTRUCTURE_FAILURES
        assert AdapterUnavailableError.__name__ in VOID_SETUP


class TestTheHealthProbeStaysCheap:
    """`/api/health` must answer inside the discovery timeout.

    `probe_arena` gives it 0.6 seconds. The seat preflight spawns an
    interpreter on its first call, so an answer computed inside the first
    request would time that probe out — another `serve` would read "nothing is
    listening", and two arenas would drive one workspace, which is exactly the
    conflation `find_running_arena` exists to prevent.

    The guarantee is a memo warmed before the socket opens, so this asserts the
    two halves that make it hold: the verdict is cached per interpreter, and
    `serve` warms it ahead of the bind.
    """

    def test_a_second_call_spends_no_subprocess(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setattr(adapter, "_verdicts", {})
        calls: list[str] = []

        def probe(interp: str, roots: list[str]) -> tuple[int, str]:
            calls.append(interp)
            return 0, ""

        monkeypatch.setattr(adapter, "_probe", probe)
        first = adapter.stella_seat_problem("/venv/bin/harbor")
        second = adapter.stella_seat_problem("/venv/bin/harbor")
        assert first is second is None
        assert len(calls) == 1, "the health endpoint would pay this on every request"

    def test_serve_warms_the_verdict_before_binding_the_socket(self) -> None:
        """A source assertion, because the ordering is the whole guarantee and
        there is no seam to observe it through: by the time a request can
        arrive the answer must already be memoized."""
        from pathlib import Path as _Path

        import arenabench.server as server_module

        source = _Path(server_module.__file__).read_text(encoding="utf-8")
        body = source[source.index("def serve("):]
        warm = body.index("unrunnable = stella_seat_problem()")
        bind = body.index("httpd = ThreadingHTTPServer(")
        assert warm < bind, (
            "serve must warm the seat verdict before it binds, or the first "
            "/api/health blows probe_arena's 0.6s timeout"
        )
