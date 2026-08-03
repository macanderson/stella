"""The pre-agent git baseline for bare Terminal-Bench task directories.

On a plain directory Stella's diff probe is structurally blind (#973) and
isolation / witness authoring / the mutation audit all degrade; the baseline
turns those channels on (issue #1211 item 4). These tests pin its properties
at two levels: the composed script's guards as text, and — because the guards
live in the shell so they hold on the container's actual filesystem — the
script's real behavior against real directories with real git.

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
assert _spec is not None and _spec.loader is not None
_git_baseline = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_git_baseline)

GIT_BASELINE_SIZE_CAP_KB = _git_baseline.GIT_BASELINE_SIZE_CAP_KB
git_baseline_script = _git_baseline.git_baseline_script
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

        assert script.startswith("cd /workspace 2>/dev/null || exit 0; ")
        # No git in the image → skip: the adapter provisions no utilities.
        assert "command -v git >/dev/null" in script
        # An existing repo — including fix-git's broken one and a parent-dir
        # repo — is the task's own state and must not be touched.
        assert "[ -e .git ] || git rev-parse --git-dir" in script
        # Dataset-heavy images must not have their disk doubled by objects.
        assert "du -sk" in script
        assert str(GIT_BASELINE_SIZE_CAP_KB) in script
        # HEAD must exist even in an empty workspace, unsigned, and under an
        # identity that is obviously synthetic and written to no config file.
        assert "--allow-empty" in script
        assert "--no-gpg-sign" in script
        assert "-c user.email=baseline@stella.invalid" in script
        assert "git config" not in script
        # Stella's own state stays out of the baseline and every later diff.
        assert ".git/info/exclude" in script
        assert ".stella/" in script
        # Every skip is a printed reason plus exit 0 — never a failed trial.
        assert script.count("exit 0") == 4

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


class TestScriptBehavior:
    """The script against real directories — what actually runs in-container."""

    def test_bare_directory_gets_one_synthetic_baseline_commit(
        self, tmp_path: Path
    ) -> None:
        (tmp_path / "file.txt").write_text("hello\n")
        (tmp_path / "sub").mkdir()
        (tmp_path / "sub" / "data.txt").write_text("world\n")

        done = _run(git_baseline_script(str(tmp_path)))
        assert done.returncode == 0
        assert "git baseline committed" in done.stdout

        log = _git(tmp_path, "log", "--format=%an <%ae> %s")
        assert log.stdout.strip() == (
            "stella-baseline <baseline@stella.invalid> "
            "stella adapter: pre-agent baseline"
        )
        # The agent's later edits are what a diff shows — the tree is clean now.
        assert _git(tmp_path, "status", "--short").stdout == ""
        # …and Stella's own state stays invisible to that diff.
        stella_dir = tmp_path / ".stella" / "private"
        stella_dir.mkdir(parents=True)
        (stella_dir / "store.db").write_bytes(b"db")
        assert _git(tmp_path, "status", "--short").stdout == ""

    def test_an_existing_repository_is_left_untouched(self, tmp_path: Path) -> None:
        _git(tmp_path, "init", "-q")
        (tmp_path / "f").write_text("x\n")

        done = _run(git_baseline_script(str(tmp_path)))
        assert done.returncode == 0
        assert "a repository is already present" in done.stdout
        # No commit was added: the repo still has no HEAD.
        assert _git(tmp_path, "rev-parse", "HEAD").returncode != 0

    def test_a_broken_repository_is_left_untouched(self, tmp_path: Path) -> None:
        # fix-git's shape: a .git that exists but is corrupt IS the task.
        (tmp_path / ".git").mkdir()
        (tmp_path / ".git" / "HEAD").write_text("junk\n")

        done = _run(git_baseline_script(str(tmp_path)))
        assert done.returncode == 0
        assert "a repository is already present" in done.stdout
        assert (tmp_path / ".git" / "HEAD").read_text() == "junk\n"

    def test_an_empty_directory_still_gets_a_head(self, tmp_path: Path) -> None:
        done = _run(git_baseline_script(str(tmp_path)))
        assert done.returncode == 0
        assert "git baseline committed" in done.stdout
        assert _git(tmp_path, "rev-parse", "HEAD").returncode == 0

    def test_a_missing_workdir_is_a_silent_skip(self, tmp_path: Path) -> None:
        done = _run(git_baseline_script(str(tmp_path / "does-not-exist")))
        assert done.returncode == 0
        assert done.stdout == ""


class TestRunGitBaseline:
    def test_env_kill_switch_skips_the_exec_entirely(self) -> None:
        async def _exec(command: str) -> SimpleNamespace:
            raise AssertionError("must not exec when disabled")

        agent = _StubAgent({"STELLA_TB_GIT_BASELINE": "0"}, _exec)
        asyncio.run(run_git_baseline(agent, _environment()))
        assert agent.calls == []

    def test_default_on_sends_the_composed_script(self) -> None:
        async def _exec(command: str) -> SimpleNamespace:
            return SimpleNamespace(return_code=0, stdout="")

        agent = _StubAgent({}, _exec)
        asyncio.run(run_git_baseline(agent, _environment("/workspace")))
        assert agent.calls == [git_baseline_script("/workspace")]

    def test_a_failed_exec_is_not_fatal(self) -> None:
        async def _exec(command: str) -> SimpleNamespace:
            raise RuntimeError("compose exec unavailable in this test")

        agent = _StubAgent({}, _exec)
        # Must not raise — a baseline aid can never fail a trial.
        asyncio.run(run_git_baseline(agent, _environment()))
        assert len(agent.calls) == 1


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q"]))
