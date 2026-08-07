"""The pre-agent git baseline for bare Terminal-Bench task directories.

On a plain directory Stella's diff probe is structurally blind (#973) and
isolation / witness authoring / the mutation audit all degrade; the baseline
turns those channels on (issue #1211 item 4). These tests pin its properties
at two levels: the composed script's guards as text, and — because the guards
live in the shell so they hold on the container's actual filesystem — the
script's real behavior against real directories with real git.

The states asserted here are the states the adapter publishes in trial
metadata and in the public ATIF trajectory, so each one has to be the truth
about the workspace the script found. `test_adapter.py::TestWorkspaceGitBaseline`
runs `install()` end to end against one real directory, which is the only
place a second baseline step could contradict this one.

Deliberately loads ``stella_harbor/git_baseline.py`` by path (the module is
stdlib-only), so this file runs even where the pinned ``harbor`` package is
absent — importing through the package would pull in the adapter root, which
imports harbor.
"""

from __future__ import annotations

import asyncio
import importlib.util
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

_MODULE_PATH = Path(__file__).resolve().parents[1] / "stella_harbor" / "git_baseline.py"
_spec = importlib.util.spec_from_file_location(
    "stella_harbor_git_baseline", _MODULE_PATH
)
# Module-scope assert, deliberately: an import precondition, not a constant
# check — without it there is nothing to collect (see #1452's sweep).
assert _spec is not None and _spec.loader is not None
_git_baseline = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_git_baseline)

GIT_BASELINE_SIZE_CAP_KB = _git_baseline.GIT_BASELINE_SIZE_CAP_KB
GIT_BASELINE_MARKER = _git_baseline.GIT_BASELINE_MARKER
GIT_BASELINE_COMMIT_MESSAGE = _git_baseline.GIT_BASELINE_COMMIT_MESSAGE
GIT_BASELINE_REF = _git_baseline.GIT_BASELINE_REF
git_baseline_script = _git_baseline.git_baseline_script
parse_git_baseline_report = _git_baseline.parse_git_baseline_report
run_git_baseline = _git_baseline.run_git_baseline


class _StubAgent:
    """Duck-typed stand-in for StellaAgent: the two members the helper uses."""

    def __init__(self, env: dict[str, str], exec_fn) -> None:
        self._env = env
        self._exec_fn = exec_fn
        self.calls: list[str] = []

    def _configured_value(self, key: str, default: str | None = None) -> str | None:
        return self._env.get(key, default)

    async def exec_as_agent(
        self, environment: object, *, command: str, timeout_sec: int
    ) -> SimpleNamespace:
        self.calls.append(command)
        return await self._exec_fn(command)


def _environment(workdir: str = "/workspace") -> SimpleNamespace:
    return SimpleNamespace(task_env_config=SimpleNamespace(workdir=workdir))


class TestScriptText:
    def test_script_carries_every_guard_and_a_synthetic_identity(self) -> None:
        script = git_baseline_script("/workspace")

        assert script.startswith("cd /workspace || ")
        # No git in the image → say so: the adapter provisions no utilities
        # beyond the best-effort `ensure_git`.
        assert "command -v git >/dev/null" in script
        # An existing repo — including fix-git's broken one and a parent-dir
        # repo — is the task's own state and must not be touched. The
        # `safe.directory` override keeps a dubious-ownership repo readable,
        # because a false "not a repository" is what would overwrite it.
        assert "[ -e .git ] || git -c safe.directory='*' rev-parse" in script
        # Dataset-heavy images must not have their disk doubled by objects.
        assert "du -sk" in script
        assert str(GIT_BASELINE_SIZE_CAP_KB) in script
        # HEAD must exist even in an empty workspace, unsigned, without hooks,
        # and under an identity written to no config file.
        assert "--allow-empty" in script
        assert "--no-verify" in script
        assert "--no-gpg-sign" in script
        assert "-c user.email=stella-harbor@bench.invalid" in script
        assert "git config" not in script
        # Stella's own state stays out of the baseline and every later diff.
        assert ".git/info/exclude" in script
        assert ".stella/" in script
        # Every branch reports through the one marker the parser reads, and
        # the script itself never fails the exec.
        assert script.count(GIT_BASELINE_MARKER) == 6
        assert script.count("exit 0") == 1

    def test_workdir_is_shell_quoted_and_optional(self) -> None:
        assert "cd '/work dir/task'" in git_baseline_script("/work dir/task")
        assert not git_baseline_script(None).startswith("cd ")


def _run(script: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["sh", "-c", script], capture_output=True, text=True, timeout=60
    )


def _git(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(cwd), *args], capture_output=True, text=True, timeout=60
    )


def _report_for(workdir: Path) -> dict[str, str]:
    done = _run(git_baseline_script(str(workdir)))
    assert done.returncode == 0, done.stderr
    return parse_git_baseline_report(done.stdout)


class TestScriptBehavior:
    """The script against real directories — what actually runs in-container."""

    def test_bare_directory_gets_one_synthetic_baseline_commit(
        self, tmp_path: Path
    ) -> None:
        (tmp_path / "file.txt").write_text("hello\n")
        (tmp_path / "sub").mkdir()
        (tmp_path / "sub" / "data.txt").write_text("world\n")

        report = _report_for(tmp_path)
        assert report["state"] == "created"
        assert report["commit"] == _git(tmp_path, "rev-parse", "HEAD").stdout.strip()

        log = _git(tmp_path, "log", "--format=%an <%ae> %s")
        assert log.stdout.strip() == (
            f"stella-harbor <stella-harbor@bench.invalid> {GIT_BASELINE_COMMIT_MESSAGE}"
        )
        # The witness-baseline pin (#2067): `verify_done` resolves this ref in
        # preference to a HEAD the agent's own commits have advanced past.
        pinned = _git(tmp_path, "rev-parse", "--verify", GIT_BASELINE_REF)
        assert pinned.returncode == 0
        assert pinned.stdout.strip() == report["commit"]
        # The agent's later edits are what a diff shows — the tree is clean now.
        assert _git(tmp_path, "status", "--short").stdout == ""
        # …and Stella's own state stays invisible to that diff, without adding
        # a tracked .gitignore a verifier would enumerate.
        stella_dir = tmp_path / ".stella" / "private"
        stella_dir.mkdir(parents=True)
        (stella_dir / "store.db").write_bytes(b"db")
        assert _git(tmp_path, "status", "--short").stdout == ""
        assert not (tmp_path / ".gitignore").exists()

    def test_an_existing_repository_is_left_untouched(self, tmp_path: Path) -> None:
        _git(tmp_path, "init", "-q")
        (tmp_path / "f").write_text("x\n")

        assert _report_for(tmp_path) == {"state": "preexisting"}
        # No commit was added: the repo still has no HEAD.
        assert _git(tmp_path, "rev-parse", "HEAD").returncode != 0

    def test_a_broken_repository_is_left_untouched(self, tmp_path: Path) -> None:
        # fix-git's shape: a .git that exists but is corrupt IS the task.
        (tmp_path / ".git").mkdir()
        (tmp_path / ".git" / "HEAD").write_text("junk\n")

        assert _report_for(tmp_path) == {"state": "preexisting"}
        assert (tmp_path / ".git" / "HEAD").read_text() == "junk\n"

    def test_an_empty_directory_still_gets_a_head(self, tmp_path: Path) -> None:
        assert _report_for(tmp_path)["state"] == "created"
        assert _git(tmp_path, "rev-parse", "HEAD").returncode == 0

    def test_an_oversized_workspace_is_a_reported_skip(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # The cap exists so git objects cannot double a dataset-heavy image's
        # disk usage. Lowered rather than filled: half a gigabyte of fixture
        # would make the assertion about disk, not about the guard.
        monkeypatch.setattr(_git_baseline, "GIT_BASELINE_SIZE_CAP_KB", 0)
        (tmp_path / "data.bin").write_bytes(b"x" * 4096)

        report = _report_for(tmp_path)
        assert report["state"] == "skipped"
        assert report["detail"] == "workspace-over-size-cap"
        assert report["cap_kb"] == "0"
        assert not (tmp_path / ".git").exists()

    def test_a_missing_workdir_is_a_reported_error(self, tmp_path: Path) -> None:
        assert _report_for(tmp_path / "does-not-exist") == {
            "state": "error",
            "detail": "workdir-unavailable",
        }

    def test_a_git_less_image_reports_unavailable(self, tmp_path: Path) -> None:
        done = subprocess.run(
            ["/bin/sh", "-c", git_baseline_script(str(tmp_path))],
            capture_output=True,
            text=True,
            env={"PATH": "/nonexistent"},
            timeout=60,
        )
        assert done.returncode == 0
        assert parse_git_baseline_report(done.stdout) == {
            "state": "unavailable",
            "detail": "git-not-installed",
        }


class TestReportParser:
    def test_the_last_marker_wins_and_empty_values_are_dropped(self) -> None:
        stdout = (
            "unrelated diagnostics\n"
            f"{GIT_BASELINE_MARKER} state=failed detail=git-command-failed\n"
            f"{GIT_BASELINE_MARKER} state=created commit=\n"
        )
        # Last marker wins; the empty commit= token (a failed rev-parse) must
        # yield no key rather than an empty string a reader could mistake for
        # a digest.
        assert parse_git_baseline_report(stdout) == {"state": "created"}

    def test_an_absent_marker_is_itself_a_state(self) -> None:
        assert parse_git_baseline_report("no marker here") == {"state": "unreported"}
        assert parse_git_baseline_report(None) == {"state": "unreported"}
        assert parse_git_baseline_report("") == {"state": "unreported"}


class TestRunGitBaseline:
    def test_env_kill_switch_skips_the_exec_and_says_so(self) -> None:
        async def _exec(command: str) -> SimpleNamespace:
            raise AssertionError("must not exec when disabled")

        agent = _StubAgent({"STELLA_TB_GIT_BASELINE": "0"}, _exec)
        report = asyncio.run(run_git_baseline(agent, _environment()))
        # "Turned off" is a posture, and it must not be reported as a
        # workspace that simply had nothing done to it.
        assert report == {"state": "disabled"}
        assert agent.calls == []

    def test_default_on_sends_the_composed_script_and_returns_the_report(self) -> None:
        async def _exec(command: str) -> SimpleNamespace:
            return SimpleNamespace(
                return_code=0,
                stdout=f"{GIT_BASELINE_MARKER} state=created commit={'a' * 40}\n",
            )

        agent = _StubAgent({}, _exec)
        report = asyncio.run(run_git_baseline(agent, _environment("/workspace")))
        assert agent.calls == [git_baseline_script("/workspace")]
        assert report == {"state": "created", "commit": "a" * 40}

    def test_a_failed_exec_is_not_fatal_and_is_disclosed(self) -> None:
        async def _exec(command: str) -> SimpleNamespace:
            raise RuntimeError("compose exec unavailable in this test")

        agent = _StubAgent({}, _exec)
        # Must not raise — a baseline aid can never fail a trial.
        report = asyncio.run(run_git_baseline(agent, _environment()))
        assert report == {
            "state": "error",
            "detail": "compose exec unavailable in this test",
        }
        assert len(agent.calls) == 1


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q"]))
