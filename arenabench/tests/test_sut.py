# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Proving which Stella a match ran.

The defect these pin is the worst kind this tool can have, because it is
invisible from the outside: on 2026-08-06 the staged SUT binary was **291
commits behind main and three days old**, and every match in between reported
numbers for that stale code without a word. Nothing compared the binary to
anything, so a benchmark whose entire purpose is measuring Stella could not say
which Stella it measured.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

from arenabench import runner as runner_module
from arenabench import sut
from arenabench.adapter import stage_adapter
from arenabench.model import MatchSpec
from arenabench.provenance import Provenance
from arenabench.registry import DEFAULT_REGISTRY
from arenabench.runner import MatchRunner
from arenabench.telemetry import seat_manifest_path


def _git(repo: Path, *args: str) -> str:
    """Run git in `repo`, and on failure say what git actually said.

    Deliberately not `check=True`: `CalledProcessError` renders as the argv and
    the exit status alone, so git's own message — the half that says *why* — is
    captured into the exception and never printed. A run on 2026-08-17 failed
    here leaving nothing in the CI log but

        CalledProcessError: Command '['git', 'fetch', '-q', 'origin']'
        returned non-zero exit status 128.

    for a fixture whose `origin` is a local directory it had created moments
    earlier. That is a failure nobody can diagnose from the log it leaves, and
    re-running it was the only move available (#3402).
    """
    done = subprocess.run(["git", *args], cwd=repo, capture_output=True, text=True)
    if done.returncode != 0:
        raise AssertionError(
            f"git {' '.join(args)} in {repo} exited {done.returncode}\n"
            f"stdout: {done.stdout.strip() or '(empty)'}\n"
            f"stderr: {done.stderr.strip() or '(empty)'}"
        )
    return done.stdout.strip()


@pytest.fixture
def repo(tmp_path: Path, monkeypatch) -> Path:
    """A tiny repo with a local `main` deliberately behind `origin/main`.

    That skew is the fixture's whole point: it is the exact shape of the bug —
    an operator's checkout lagging the remote — and a resolver that reads the
    local branch looks perfectly correct until you compare the two.
    """
    origin = tmp_path / "origin"
    origin.mkdir()
    _git(origin, "init", "-q", "--initial-branch=main")
    _git(origin, "config", "user.email", "t@example.com")
    _git(origin, "config", "user.name", "t")
    (origin / "a.txt").write_text("one")
    _git(origin, "add", "-A")
    _git(origin, "commit", "-qm", "first")

    work = tmp_path / "work"
    _git(tmp_path, "clone", "-q", str(origin), str(work))
    _git(work, "config", "user.email", "t@example.com")
    _git(work, "config", "user.name", "t")

    # Advance origin twice; the clone's local `main` stays on the first commit.
    (origin / "a.txt").write_text("two")
    _git(origin, "commit", "-qam", "second")
    (origin / "a.txt").write_text("three")
    _git(origin, "commit", "-qam", "third")
    _git(work, "fetch", "-q", "origin")

    # A Stella checkout carries the harbor adapter, and a launch now refuses a
    # Stella seat without one rather than producing a seat that imports nothing
    # and reports done (#2127, #2192). The fixture models that, minimally.
    adapter = work / "bench" / "harbor_adapter" / "stella_harbor"
    adapter.mkdir(parents=True)
    (adapter / "__init__.py").write_text("VERSION = 'fixture'\n")

    monkeypatch.setenv(sut.STELLA_REPO_ENV, str(work))
    monkeypatch.setenv("ARENABENCH_HOME", str(tmp_path / "home"))
    # An unpinned seat runs whatever STELLA_BINARY names, so a developer whose
    # shell exports it would otherwise be running a different test than CI.
    monkeypatch.delenv(sut.STELLA_BINARY_ENV, raising=False)
    return work


class TestRemoteFirstResolution:
    """`main` must mean `origin/main`, never the operator's local branch.

    Caught while writing this module: the first implementation resolved `main`
    to the *local* branch, which on the development machine was 291 commits
    behind the remote. A benchmark that says "we ran main" and measures a stale
    checkout reports a number nobody else can reproduce — and it looks entirely
    correct from the inside.
    """

    def test_a_bare_name_resolves_to_the_remote_branch(self, repo: Path):
        assert sut.resolve_ref("main") == _git(repo, "rev-parse", "origin/main")

    def test_the_local_branch_of_the_same_name_is_not_what_runs(self, repo: Path):
        local = _git(repo, "rev-parse", "refs/heads/main")
        assert sut.resolve_ref("main") != local, (
            "resolving to the local branch is how a stale checkout gets "
            "published as the project's own result"
        )

    def test_an_explicitly_qualified_ref_is_used_as_given(self, repo: Path):
        assert sut.resolve_ref("origin/main") == _git(repo, "rev-parse", "origin/main")

    def test_a_full_sha_resolves_to_itself(self, repo: Path):
        head = _git(repo, "rev-parse", "origin/main")
        assert sut.resolve_ref(head) == head


class TestRefsReachingGitAreValidated:
    """A ref from an HTTP client must never become a git option."""

    @pytest.mark.parametrize(
        "hostile",
        [
            "--upload-pack=touch /tmp/pwned",
            "-o",
            "main; rm -rf /",
            "main space",
            "../../etc/passwd",
            "a" * 300,
        ],
    )
    def test_a_hostile_ref_is_refused_before_it_reaches_a_subprocess(
        self, repo: Path, hostile: str
    ):
        with pytest.raises(sut.SutUnavailableError):
            sut.resolve_ref(hostile)


class TestDrift:
    """Unknown is not zero, and must never render as agreement."""

    def test_a_behind_binary_reports_how_far(self, repo: Path):
        old = _git(repo, "rev-parse", "refs/heads/main")
        new = _git(repo, "rev-parse", "origin/main")
        drift = sut.drift_between(old, new)
        assert drift.behind == 2 and drift.ahead == 0
        assert not drift.identical
        assert "behind" in drift.summary()

    def test_an_unrecorded_commit_is_incomparable_not_equal(self, repo: Path):
        drift = sut.drift_between("", _git(repo, "rev-parse", "origin/main"))
        assert not drift.identical
        assert not drift.comparable, (
            "a binary whose commit nobody wrote down must not read as a match"
        )

    def test_the_same_commit_is_identical(self, repo: Path):
        head = _git(repo, "rev-parse", "origin/main")
        assert sut.drift_between(head, head).identical


class TestBinaryLookupNeverGuesses:
    """An unlabelled binary is never accepted as the commit you asked for.

    This is the precise mechanism of the original defect: a single unversioned
    path was treated as though it were whatever the caller currently wanted.
    """

    def _stage(self, directory: Path, commit: str = "", sha: str = "abc") -> None:
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "stella").write_text("#!/bin/sh\n")
        (directory / "stella").chmod(0o755)
        if commit:
            (directory / "sut_commit.txt").write_text(commit)
        (directory / "binary_sha256.txt").write_text(sha)

    def test_a_binary_cached_under_its_commit_is_found(self, repo: Path):
        head = _git(repo, "rev-parse", "origin/main")
        self._stage(sut.sut_root() / head, commit=head)
        found = sut.binary_for(head)
        assert found is not None and found.commit == head

    def test_the_legacy_path_is_accepted_only_when_it_names_that_commit(
        self, repo: Path
    ):
        head = _git(repo, "rev-parse", "origin/main")
        other = _git(repo, "rev-parse", "refs/heads/main")
        self._stage(sut.sut_root(), commit=other)
        assert sut.binary_for(head) is None, (
            "accepting a differently-labelled binary is the 291-commit bug"
        )
        assert sut.binary_for(other) is not None

    def test_an_unlabelled_legacy_binary_is_never_accepted(self, repo: Path):
        head = _git(repo, "rev-parse", "origin/main")
        self._stage(sut.sut_root(), commit="")
        assert sut.binary_for(head) is None


class TestLaunchRefusal:
    """A Stella seat cannot run a binary that is not the commit it pinned."""

    def _spec(self, *, agent: str = "stella", ref: str | None = None) -> MatchSpec:
        raw = {
            "name": "m",
            "dataset": "terminal-bench-2.1",
            "tasks": ["fix-git"],
            "contestants": [
                {
                    "name": "seat",
                    "agent": agent,
                    "engine": {"api": "openrouter", "model": "m"},
                }
            ],
        }
        if ref is not None:
            raw["sut_ref"] = ref
        return MatchSpec.from_json(raw)

    def test_a_stale_binary_blocks_the_launch(self, repo: Path):
        stale = _git(repo, "rev-parse", "refs/heads/main")
        directory = sut.sut_root()
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "stella").write_text("#!/bin/sh\n")
        (directory / "sut_commit.txt").write_text(stale)

        problem = sut.sut_problem_for(self._spec())
        assert problem is not None
        assert "behind" in problem, "the refusal must say how stale, not just that"

    def test_a_matching_binary_permits_the_launch(self, repo: Path):
        head = _git(repo, "rev-parse", "origin/main")
        directory = sut.sut_root() / head
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "stella").write_text("#!/bin/sh\n")
        (directory / "sut_commit.txt").write_text(head)
        assert sut.sut_problem_for(self._spec()) is None

    def test_a_match_with_no_stella_seat_is_never_blocked(self, repo: Path):
        """A Claude-Code-vs-Codex contest runs no SUT of ours."""
        assert sut.sut_problem_for(self._spec(agent="claude-code")) is None

    def test_the_empty_ref_opts_out_of_pinning_with_nothing_staged(self, repo: Path):
        """With no STELLA_BINARY there is nothing to be stale, so nothing to say."""
        assert sut.sut_problem_for(self._spec(ref="")) is None

    def test_a_spec_written_before_this_field_defaults_to_main_not_unpinned(self):
        """Absent must not read as the opt-out.

        Otherwise every template predating the field silently accepts any
        binary — turning a new safety property off exactly where it is least
        visible.
        """
        assert MatchSpec.from_json({"name": "m", "dataset": "d"}).sut_ref == "main"


class TestCheckoutDiscovery:
    """An arena running out of a checkout must not report that it has none.

    The arena ships *inside* the repository it measures (``<repo>/arenabench``),
    so ``python -m arenabench serve`` from a clone is the ordinary development
    launch — and it was the one launch that could not resolve a ref. Neither
    ``ARENABENCH_STELLA_REPO`` nor ``ARENABENCH_STELLA_ADAPTER`` is set on that
    path, so every Stella seat was refused with "no Stella checkout is
    configured" by a process whose own source file sat two directories below
    the repository root, and the model select fell back to free text for the
    same reason.
    """

    @pytest.fixture(autouse=True)
    def _unconfigured(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """Exactly the environment of a bare `python -m arenabench serve`."""
        monkeypatch.delenv(sut.STELLA_REPO_ENV, raising=False)
        monkeypatch.delenv("ARENABENCH_STELLA_ADAPTER", raising=False)

    def test_the_checkout_this_arena_runs_from_needs_no_configuration(self) -> None:
        found = sut.stella_repo()
        assert found is not None, (
            "the arena refused to name the checkout it is executing out of"
        )
        for marker in sut._STELLA_MARKERS:
            assert (found / marker).exists(), marker
        assert sut.repo_problem() is None

    def test_an_unrelated_git_checkout_is_declined(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """Discovery must not claim whatever repository the arena happens to be
        installed inside: resolving `main` against someone else's history
        answers with a commit that means nothing here, which is worse than
        answering with nothing at all."""
        package = tmp_path / "elsewhere" / "arenabench"
        package.mkdir(parents=True)
        (tmp_path / "elsewhere" / ".git").mkdir()
        monkeypatch.setattr(sut, "_PACKAGE_DIR", package)

        assert sut.stella_repo() is None
        assert sut.STELLA_REPO_ENV in (sut.repo_problem() or "")

    def test_a_misconfigured_repo_env_is_not_reported_as_unconfigured(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """An explicit setting that is wrong is reported, never quietly
        replaced by discovery — and telling an operator who set the variable to
        set the variable is how a five-second fix becomes an afternoon."""
        wrong = tmp_path / "not-a-checkout"
        monkeypatch.setenv(sut.STELLA_REPO_ENV, str(wrong))

        assert sut.stella_repo() is None
        problem = sut.repo_problem() or ""
        assert "not a git checkout" in problem
        assert str(wrong) in problem

    def _stage_head(self, monkeypatch: pytest.MonkeyPatch, home: Path) -> tuple[Path, str]:
        """A staged binary for a commit this checkout really has.

        Pins the resolved commit rather than `main`: a branch name is a moving
        target and these tests assert about discovery, not about the tip.
        """
        monkeypatch.setenv("ARENABENCH_HOME", str(home))
        monkeypatch.delenv(sut.STELLA_BINARY_ENV, raising=False)
        repo = sut.stella_repo()
        assert repo is not None
        head = _git(repo, "rev-parse", "HEAD")
        staged = sut.sut_root() / head
        staged.mkdir(parents=True)
        binary = staged / "stella"
        binary.write_text("#!/bin/sh\n")
        binary.chmod(0o755)
        (staged / "sut_commit.txt").write_text(head)
        return repo, head

    def _spec(self, ref: str) -> MatchSpec:
        return MatchSpec.from_json(
            {
                "name": "m",
                "dataset": "terminal-bench-2.1",
                "tasks": ["fix-git"],
                "sut_ref": ref,
                "contestants": [
                    {
                        "name": "seat",
                        "agent": "stella",
                        "engine": {"api": "openrouter", "model": "m"},
                    }
                ],
            }
        )

    def test_a_pinned_seat_launches_with_nothing_configured(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The whole point, end to end: a staged binary for the pinned commit
        is enough to launch, with no arena-specific environment at all."""
        _, head = self._stage_head(monkeypatch, tmp_path / "home")
        assert sut.sut_problem_for(self._spec(head)) is None

    def test_the_launch_wires_the_adapter_from_that_same_checkout(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """Launching is not enough if the seat then runs nothing.

        Without ``stella_harbor`` on the subprocess's ``PYTHONPATH``, the seat
        makes no model call at all — historically in silence, since
        ``harbor_agent.py`` fell back to ``_Base = object`` and Harbor exited 0
        (#2192, now a named refusal). The adapter ships in the checkout
        discovery just found, so the arena can wire it rather than depend on an
        operator remembering a variable whose omission is invisible.

        What reaches ``PYTHONPATH`` is a **staged copy** of that checkout's
        adapter rather than the checkout path itself (#2127): the launch
        worktree was deleted mid-match once and took four trials with it. The
        assertion below therefore pins the property that matters — the seat can
        import ``stella_harbor``, and what it imports came from this checkout —
        rather than the literal source path, which is no longer what is wired.
        """
        repo, head = self._stage_head(monkeypatch, tmp_path / "home")
        _stub_harbor(monkeypatch)
        captured = _capture_popen(monkeypatch)

        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        match = runner.create(self._spec(head))
        runner._launch(match, match.spec.contestants[0], runner.resolve_harbor(match))

        roots = captured[0]["env"]["PYTHONPATH"].split(os.pathsep)
        staged = stage_adapter(repo / "bench" / "harbor_adapter")
        assert str(staged) in roots
        assert (Path(staged) / "stella_harbor").is_dir()


def _stamped(path: Path, commit: str | None) -> Path:
    """A stand-in Stella binary carrying the compile-time stamp `build.rs` emits.

    NUL-delimited on both sides, exactly as ``build_info.rs`` lays it out — that
    delimiting is what makes the commit readable out of the file, and a fixture
    that omitted it would be testing a shape the real artifact does not have.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    body = b"\x7fELF" + b"\x00" * 64
    if commit is not None:
        body += b"\x000.6.132-dev." + commit.encode("ascii") + b"\x00"
    path.write_bytes(body)
    return path


class TestAnUnpinnedMatchStillRefusesStaleCode:
    """Clearing the pin says "run whatever is current", not "run anything".

    The witness for #2032. The operator runbook launches with
    ``STELLA_BINARY=~/.arenabench/sut/stella``, and that path sat 291 commits
    behind ``main`` for three days while producing perfectly scoreable trials.
    The commit pinning added in #2016/#2020 did not catch it, because the stale
    binary *was* pinned — to a commit from three days earlier. Pinning answers
    which code; only a distance answers whether it is the code anyone meant.
    """

    #: Deeper than `MAX_BEHIND_UNPINNED`, so "behind" and "too far behind" are
    #: distinguishable rather than coincidentally the same assertion.
    DEPTH = sut.MAX_BEHIND_UNPINNED * 2

    @pytest.fixture
    def deep_repo(self, tmp_path: Path, monkeypatch) -> Path:
        origin = tmp_path / "deep-origin"
        origin.mkdir()
        _git(origin, "init", "-q", "--initial-branch=main")
        _git(origin, "config", "user.email", "t@example.com")
        _git(origin, "config", "user.name", "t")
        _git(origin, "commit", "-q", "--allow-empty", "-m", "base")
        work = tmp_path / "deep-work"
        _git(tmp_path, "clone", "-q", str(origin), str(work))
        for index in range(self.DEPTH):
            _git(origin, "commit", "-q", "--allow-empty", "-m", f"c{index}")
        _git(work, "fetch", "-q", "origin")
        monkeypatch.setenv(sut.STELLA_REPO_ENV, str(work))
        monkeypatch.delenv(sut.STELLA_BINARY_ENV, raising=False)
        return work

    def _unpinned_stella_spec(self) -> MatchSpec:
        return MatchSpec.from_json(
            {
                "name": "m",
                "dataset": "terminal-bench-2.1",
                "tasks": ["fix-git"],
                "sut_ref": "",
                "contestants": [
                    {
                        "name": "seat",
                        "agent": "stella",
                        "engine": {"api": "openrouter", "model": "m"},
                    }
                ],
            }
        )

    def test_the_stale_runbook_binary_now_blocks_the_launch(
        self, deep_repo: Path, tmp_path: Path, monkeypatch
    ):
        """The exact scenario in the issue: unpinned seat, ancient STELLA_BINARY."""
        old = _git(deep_repo, "rev-list", "--max-parents=0", "origin/main")
        binary = _stamped(tmp_path / "sut" / "stella", old)
        monkeypatch.setenv(sut.STELLA_BINARY_ENV, str(binary))

        problem = sut.sut_problem_for(self._unpinned_stella_spec())
        assert problem is not None, (
            "an unpinned match asks for current code; this binary is not it"
        )
        assert old[:8] in problem, "the refusal must name the commit"
        assert f"{self.DEPTH} commit(s) behind" in problem, (
            "the refusal must name the distance, not merely that there is one"
        )

    def test_a_symlinked_runbook_path_is_followed_to_its_target(
        self, deep_repo: Path, tmp_path: Path, monkeypatch
    ):
        """The rig's mitigation is a symlink; the guard must see through it.

        A guard that read the link's own directory would report whatever
        `sut_commit.txt` was last left beside the legacy path — which is the
        stale claim, not the binary that would actually run.
        """
        head = _git(deep_repo, "rev-parse", "origin/main")
        target = _stamped(tmp_path / "sut" / head / "stella", head)
        old = _git(deep_repo, "rev-list", "--max-parents=0", "origin/main")
        (tmp_path / "sut" / "sut_commit.txt").write_text(old)
        link = tmp_path / "sut" / "stella"
        link.symlink_to(target)
        monkeypatch.setenv(sut.STELLA_BINARY_ENV, str(link))

        assert sut.ambient_sut().commit == head
        assert sut.sut_problem_for(self._unpinned_stella_spec()) is None

    def test_a_binary_within_the_limit_still_launches(
        self, deep_repo: Path, tmp_path: Path, monkeypatch
    ):
        """`main` moves under every run; the limit exists so that is not fatal."""
        near = _git(
            deep_repo, "rev-parse", f"origin/main~{sut.MAX_BEHIND_UNPINNED}"
        )
        monkeypatch.setenv(
            sut.STELLA_BINARY_ENV, str(_stamped(tmp_path / "stella", near))
        )
        assert sut.sut_problem_for(self._unpinned_stella_spec()) is None

    def test_an_unstamped_development_build_is_not_blocked(
        self, deep_repo: Path, tmp_path: Path, monkeypatch
    ):
        """`cargo build --release` carries no stamp and cannot be dated.

        Refusing it would block the local loop this tool exists to serve. The
        arena refuses only on positive evidence of staleness; the Terminal-Bench
        evidence runbook fails closed instead, because it publishes numbers.
        """
        monkeypatch.setenv(
            sut.STELLA_BINARY_ENV, str(_stamped(tmp_path / "stella", None))
        )
        assert sut.sut_problem_for(self._unpinned_stella_spec()) is None

    def test_a_non_stella_contest_is_never_blocked(
        self, deep_repo: Path, tmp_path: Path, monkeypatch
    ):
        old = _git(deep_repo, "rev-list", "--max-parents=0", "origin/main")
        monkeypatch.setenv(
            sut.STELLA_BINARY_ENV, str(_stamped(tmp_path / "stella", old))
        )
        spec = MatchSpec.from_json(
            {
                "name": "m",
                "dataset": "terminal-bench-2.1",
                "tasks": ["fix-git"],
                "sut_ref": "",
                "contestants": [
                    {
                        "name": "seat",
                        "agent": "claude-code",
                        "engine": {"api": "anthropic", "model": "m"},
                    }
                ],
            }
        )
        assert sut.sut_problem_for(spec) is None

    def test_two_stamps_name_no_commit_and_do_not_block(
        self, deep_repo: Path, tmp_path: Path, monkeypatch
    ):
        """Two stamps is not one build; guessing which is the point of failure."""
        path = tmp_path / "stella"
        path.write_bytes(
            b"\x000.6.1-dev." + b"a" * 40 + b"\x00\x000.6.1-dev." + b"b" * 40 + b"\x00"
        )
        monkeypatch.setenv(sut.STELLA_BINARY_ENV, str(path))
        assert sut.embedded_commit(path) == ""
        assert sut.sut_problem_for(self._unpinned_stella_spec()) is None


class TestACommitPinSurvivesAMovingBranch:
    """Why a finished build pins its own commit rather than the branch.

    Observed while building this: a release cross-compile of Stella took 40
    minutes, and ``origin/main`` moved 10 commits while it ran. On a repo that
    active, "the binary must equal origin/main" is unsatisfiable by
    construction — the build is slower than the interval between merges.

    The fix is emphatically *not* to loosen the check; a tolerance is how the
    291-commit drift survived in the first place. It is that a branch is how
    you reach for something fresh, and the commit it resolved to is what you
    actually ran and must record.
    """

    def _stage(self, commit: str) -> None:
        directory = sut.sut_root() / commit
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "stella").write_text("#!/bin/sh\n")
        (directory / "sut_commit.txt").write_text(commit)
        (directory / "binary_sha256.txt").write_text("deadbeef")

    def test_a_commit_pin_is_ready_with_no_checkout_at_all(
        self, tmp_path: Path, monkeypatch
    ) -> None:
        """The cloud runner's exact condition: a staged binary, no source.

        A checkout answers one question — what commit does this symbolic ref
        mean — and a full SHA does not ask it. The binary is content-addressed
        by that commit, so the runner can say precisely which Stella it runs.
        Refusing here rejected every cloud trial of ctl-0808-0028 after
        credentials, staging and dockerd had all succeeded.
        """
        monkeypatch.delenv(sut.STELLA_REPO_ENV, raising=False)
        monkeypatch.delenv("ARENABENCH_STELLA_ADAPTER", raising=False)
        package = tmp_path / "elsewhere" / "arenabench"
        package.mkdir(parents=True)
        monkeypatch.setattr(sut, "_PACKAGE_DIR", package)
        monkeypatch.setenv("ARENABENCH_HOME", str(tmp_path / "home"))
        monkeypatch.chdir(tmp_path)
        assert sut.stella_repo() is None, "the fixture must leave no checkout"

        commit = "7edec3b2333e0a8649bb1fcc7190ad97242d9e3e"
        self._stage(commit)
        status = sut.sut_status(commit)
        assert status["ready"], status["problem"]
        assert status["target"] == commit

    def test_a_branch_pin_still_needs_a_checkout(
        self, tmp_path: Path, monkeypatch
    ) -> None:
        """The relaxation is scoped to what needs no resolving. `main` is
        still unanswerable without a repository, and must say so rather than
        guess at whatever binary happens to be staged."""
        monkeypatch.delenv(sut.STELLA_REPO_ENV, raising=False)
        monkeypatch.delenv("ARENABENCH_STELLA_ADAPTER", raising=False)
        package = tmp_path / "elsewhere" / "arenabench"
        package.mkdir(parents=True)
        monkeypatch.setattr(sut, "_PACKAGE_DIR", package)
        monkeypatch.setenv("ARENABENCH_HOME", str(tmp_path / "home"))
        monkeypatch.chdir(tmp_path)
        self._stage("7edec3b2333e0a8649bb1fcc7190ad97242d9e3e")

        status = sut.sut_status("main")
        assert not status["ready"]
        assert "no Stella checkout is configured" in status["problem"]

    def test_a_commit_pin_stays_ready_when_the_branch_moves_on(self, repo: Path):
        built = _git(repo, "rev-parse", "origin/main")
        self._stage(built)
        assert sut.sut_status(built)["ready"]

        # The branch advances, exactly as it does mid-build.
        origin = repo.parent / "origin"
        (origin / "a.txt").write_text("four")
        _git(origin, "commit", "-qam", "fourth")
        _git(repo, "fetch", "-q", "origin")

        assert sut.sut_status(built)["ready"], (
            "a commit pin names a fixed artifact; a branch moving cannot "
            "invalidate what was already built and measured"
        )
        assert not sut.sut_status("main")["ready"], (
            "the branch pin must go unready, or the guard would be tolerating "
            "exactly the drift it exists to catch"
        )

    def test_a_commit_pin_is_recorded_verbatim(self, repo: Path):
        built = _git(repo, "rev-parse", "origin/main")
        spec = MatchSpec.from_json(
            {
                "name": "m",
                "dataset": "d",
                "sut_ref": built,
                "contestants": [
                    {"name": "s", "agent": "stella", "engine": {"model": "m"}}
                ],
            }
        )
        assert spec.sut_ref == built


class TestProvenanceRecordsTheSut:
    """A result set that cannot name its SUT is not comparable to another."""

    def test_the_commit_joins_the_comparability_key(self):
        record = Provenance(
            dataset_key="terminal-bench-2.1",
            dataset_digest="sha256:7d7bdc1c",
            harbor_version="0.6.1",
            sut_commit="6c345532aaaa",
        )
        assert "stella6c345532" in record.comparability_key

    def test_two_stella_revisions_do_not_share_a_key(self):
        base = {
            "dataset_key": "terminal-bench-2.1",
            "dataset_digest": "sha256:7d7bdc1c",
            "harbor_version": "0.6.1",
        }
        old = Provenance(**base, sut_commit="47d587a3ffff")
        new = Provenance(**base, sut_commit="6c345532aaaa")
        assert old.comparability_key != new.comparability_key, (
            "two Stella revisions answer different questions; one key would "
            "invite them into a single average"
        )

    def test_an_unpinned_run_is_marked_unknown_rather_than_blank(self):
        assert "stella?" in Provenance(dataset_key="d").comparability_key

    def test_the_record_round_trips(self):
        record = Provenance(sut_ref="main", sut_commit="abc123", sut_sha256="def")
        again = Provenance.from_json(record.to_json())
        assert (again.sut_ref, again.sut_commit, again.sut_sha256) == (
            "main",
            "abc123",
            "def",
        )


class TestProvenanceRecordsEverySeat:
    """One match may race two Stella builds; the record must carry both (#2082)."""

    SEATS = {
        "champ": {"name": "champion", "ref": "a" * 40, "commit": "a" * 40, "sha256": "x"},
        "chall": {"name": "challenger", "ref": "b" * 40, "commit": "b" * 40, "sha256": "y"},
    }

    def _record(self) -> Provenance:
        return Provenance(
            dataset_key="terminal-bench-2.1",
            dataset_digest="sha256:7d7bdc1c",
            harbor_version="0.6.1",
            sut_seats=dict(self.SEATS),
        )

    def test_per_seat_records_round_trip(self):
        again = Provenance.from_json(self._record().to_json())
        assert again.sut_seats == self.SEATS

    def test_a_two_twin_match_names_both_commits_in_its_key(self):
        key = self._record().comparability_key
        assert "a" * 8 in key and "b" * 8 in key, (
            "a key naming one commit invites a two-twin match into a "
            "single-SUT average"
        )

    def test_arm_level_keys_differ_only_in_the_sut_half(self):
        record = self._record()
        champ = record.comparability_key_for("champ")
        chall = record.comparability_key_for("chall")
        assert champ != chall
        assert champ.rsplit("/", 1)[0] == chall.rsplit("/", 1)[0], (
            "two arms of one match share the whole apparatus; only the "
            "stella<commit> half may differ"
        )
        assert champ.endswith(f"stella{'a' * 8}")
        assert chall.endswith(f"stella{'b' * 8}")

    def test_a_legacy_record_keeps_its_single_field_key(self):
        legacy = Provenance(
            dataset_key="terminal-bench-2.1",
            dataset_digest="sha256:7d7bdc1c",
            harbor_version="0.6.1",
            sut_commit="6c345532aaaa",
        )
        assert "stella6c345532" in legacy.comparability_key
        assert legacy.comparability_key_for("anything").endswith("stella6c345532")


def _stub_harbor(monkeypatch: pytest.MonkeyPatch) -> None:
    """The same three answers `test_arena._fake_harbor` stubs, for `_launch`."""
    monkeypatch.setattr(
        "arenabench.harbor.harbor_bin", lambda dataset_key=None: "/usr/bin/harbor"
    )
    monkeypatch.setattr(
        "arenabench.harbor.harbor_version", lambda binary=None: "0.20.0"
    )
    monkeypatch.setattr(
        "arenabench.harbor.supports_agent_import_path", lambda binary=None: False
    )


def _capture_popen(monkeypatch: pytest.MonkeyPatch) -> list[dict]:
    """Capture every (command, env) the *launch* hands Popen.

    Only the stubbed Harbor binary is intercepted: `arenabench.runner` imports
    the shared ``subprocess`` module, so a blanket patch would also break the
    real ``git`` subprocesses the pin resolution runs mid-launch.
    """
    captured: list[dict] = []
    real_popen = subprocess.Popen

    class _Recorder:
        def __init__(self, command, **kwargs):
            captured.append({"command": command, "env": kwargs.get("env") or {}})
            self.returncode = None

        def poll(self):
            return None

    def dispatch(command, **kwargs):
        if command and command[0] == "/usr/bin/harbor":
            return _Recorder(command, **kwargs)
        return real_popen(command, **kwargs)

    monkeypatch.setattr("arenabench.runner.subprocess.Popen", dispatch)
    return captured


class TestTwoSeatsRaceTwoBuilds:
    """#2082: a champion-vs-challenger match is two Stella builds on one grid."""

    def _stage(self, commit: str) -> Path:
        directory = sut.sut_root() / commit
        directory.mkdir(parents=True, exist_ok=True)
        binary = directory / "stella"
        binary.write_text("#!/bin/sh\n")
        binary.chmod(0o755)
        (directory / "sut_commit.txt").write_text(commit)
        (directory / "binary_sha256.txt").write_text(f"sha-of-{commit[:8]}")
        return binary

    def _two_seat_spec(self, ref_a: str, ref_b: str) -> MatchSpec:
        return MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "tasks": ["fix-git"],
                "sut_ref": ref_a,
                "contestants": [
                    {
                        "id": "champ",
                        "name": "champion",
                        "agent": "stella",
                        "engine": {"api": "openrouter", "model": "m"},
                    },
                    {
                        "id": "chall",
                        "name": "challenger",
                        "agent": "stella",
                        "sut_ref": ref_b,
                        "engine": {"api": "openrouter", "model": "m"},
                    },
                ],
            }
        )

    def test_each_seats_pin_is_checked_independently(self, repo: Path):
        head = _git(repo, "rev-parse", "origin/main")
        unbuilt = _git(repo, "rev-parse", "refs/heads/main")
        self._stage(head)
        problem = sut.sut_problem_for(self._two_seat_spec(head, unbuilt))
        assert problem is not None
        assert "challenger" in problem, "the refusal must name the seat"
        assert "champion" not in problem, (
            "the seat whose build exists must not be blamed for the one whose "
            "build does not"
        )

    def test_both_builds_present_permit_the_launch(self, repo: Path):
        head = _git(repo, "rev-parse", "origin/main")
        other = _git(repo, "rev-parse", "refs/heads/main")
        self._stage(head)
        self._stage(other)
        assert sut.sut_problem_for(self._two_seat_spec(head, other)) is None

    def test_two_seats_launch_two_binaries_and_record_them(
        self, repo: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        """Verify (#2082): two pinned refs launch two STELLA_BINARY values,
        provable from the `.seat.json` launch records."""
        head = _git(repo, "rev-parse", "origin/main")
        other = _git(repo, "rev-parse", "refs/heads/main")
        binary_a = self._stage(head)
        binary_b = self._stage(other)
        _stub_harbor(monkeypatch)
        captured = _capture_popen(monkeypatch)

        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        match = runner.create(self._two_seat_spec(head, other))
        resolution = runner.resolve_harbor(match)
        runs = {
            contestant.id: runner._launch(match, contestant, resolution)
            for contestant in match.spec.contestants
        }

        assert captured[0]["env"]["STELLA_BINARY"] == str(binary_a)
        assert captured[1]["env"]["STELLA_BINARY"] == str(binary_b)

        records = {
            seat_id: json.loads(
                seat_manifest_path(run.job_dir).read_text(encoding="utf-8")
            )
            for seat_id, run in runs.items()
        }
        assert records["champ"]["sut_commit"] == head
        assert records["chall"]["sut_commit"] == other
        assert records["champ"]["sut_sha256"] != records["chall"]["sut_sha256"]

    def test_provenance_records_both_commits(
        self, repo: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        head = _git(repo, "rev-parse", "origin/main")
        other = _git(repo, "rev-parse", "refs/heads/main")
        self._stage(head)
        self._stage(other)
        _stub_harbor(monkeypatch)

        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        match = runner.create(self._two_seat_spec(head, other))
        record = runner_module._provenance_for(match, runner.resolve_harbor(match))

        assert record.sut_seats["champ"]["commit"] == head
        assert record.sut_seats["chall"]["commit"] == other
        assert head[:8] in record.comparability_key
        assert other[:8] in record.comparability_key
        # No single match-level commit is true of a diverged match.
        assert record.sut_commit == ""


class TestAPinIsObservedOnce:
    """#2098: the two halves of a pin must see one value of a floating ref.

    `SutBuilder.start` and `_agent_environment` each called `resolve_ref`
    independently, minutes apart; when `main` moved between the calls the
    launch found no staged binary and silently degraded to the ambient/PATH
    tier — which uploaded the operator's native macOS build into Linux task
    containers. The reproduction is pinned here with a resolver that moves
    the ref between calls, exactly as the real repository did.
    """

    def _stage(self, commit: str) -> Path:
        directory = sut.sut_root() / commit
        directory.mkdir(parents=True, exist_ok=True)
        binary = directory / "stella"
        binary.write_text("#!/bin/sh\n")
        binary.chmod(0o755)
        (directory / "sut_commit.txt").write_text(commit)
        (directory / "binary_sha256.txt").write_text("deadbeef")
        return binary

    def _stella_spec(self, ref: str) -> MatchSpec:
        return MatchSpec.from_json(
            {
                "dataset": "terminal-bench-2.1",
                "tasks": ["fix-git"],
                "sut_ref": ref,
                "contestants": [
                    {
                        "id": "st",
                        "name": "seat",
                        "agent": "stella",
                        "engine": {"api": "openrouter", "model": "m"},
                    }
                ],
            }
        )

    def _advance_origin(self, repo: Path) -> None:
        origin = repo.parent / "origin"
        (origin / "a.txt").write_text("moved")
        _git(origin, "commit", "-qam", "moved while building")
        _git(repo, "fetch", "-q", "origin")

    def test_a_commit_pin_survives_the_branch_moving(
        self, repo: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        """Verify (c): built, origin advanced, pinned to the built commit —
        the match runs the original binary, unaffected by the drift."""
        built = _git(repo, "rev-parse", "origin/main")
        binary = self._stage(built)
        self._advance_origin(repo)
        _stub_harbor(monkeypatch)
        captured = _capture_popen(monkeypatch)

        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        match = runner.create(self._stella_spec(built))
        run = runner._launch(match, match.spec.contestants[0], runner.resolve_harbor(match))

        assert run.error == ""
        assert captured[0]["env"]["STELLA_BINARY"] == str(binary)
        assert captured[0]["env"]["ARENABENCH_SUT_COMMIT"] == built

    def test_a_drifted_branch_pin_refuses_rather_than_falling_through(
        self, repo: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        """Verify (d): built, origin advanced, pinned to the bare branch —
        refuse naming the drift; never launch on the ambient/PATH tier."""
        built = _git(repo, "rev-parse", "origin/main")
        self._stage(built)
        self._advance_origin(repo)
        _stub_harbor(monkeypatch)
        captured = _capture_popen(monkeypatch)
        # An ambient binary is available; falling through to it is the bug.
        decoy = tmp_path / "ambient-stella"
        decoy.write_text("#!/bin/sh\n")
        monkeypatch.setenv(sut.STELLA_BINARY_ENV, str(decoy))

        preflight = sut.sut_problem_for(self._stella_spec("main"))
        assert preflight is not None
        assert "behind" in preflight, "the refusal must name the drift"

        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        match = runner.create(self._stella_spec("main"))
        with pytest.raises(sut.SutUnavailableError) as refusal:
            runner._launch(match, match.spec.contestants[0], runner.resolve_harbor(match))
        assert "moved since the build" in str(refusal.value)
        assert built[:8] in str(refusal.value)
        assert captured == [], "a refused trial must launch nothing at all"

    def test_provenance_and_launch_read_the_same_observation(
        self, repo: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ):
        """The mechanism itself: a resolver that moves the ref between calls
        must not be able to hand provenance one commit and the launch another.
        """
        built = _git(repo, "rev-parse", "origin/main")
        moved = _git(repo, "rev-parse", "refs/heads/main")
        binary = self._stage(built)
        _stub_harbor(monkeypatch)
        captured = _capture_popen(monkeypatch)

        calls: list[str] = []

        def moving(ref: str, repo_path=None):
            calls.append(ref)
            # First observation: the built tip. Every later one: the ref has
            # moved — exactly the 2026-08-07 reproduction.
            return built if len(calls) == 1 else moved

        monkeypatch.setattr(sut, "resolve_ref", moving)
        runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        match = runner.create(self._stella_spec("main"))
        match.provenance = runner_module._provenance_for(
            match, runner.resolve_harbor(match)
        )
        run = runner._launch(match, match.spec.contestants[0], runner.resolve_harbor(match))

        assert run.error == ""
        recorded = match.provenance.sut_seats["st"]["commit"]
        launched = captured[0]["env"]["ARENABENCH_SUT_COMMIT"]
        assert recorded == launched == built, (
            "provenance and the launch observed different values of one "
            "floating ref — the exact #2098 race"
        )
        assert captured[0]["env"]["STELLA_BINARY"] == str(binary)


def test_a_failing_git_reports_what_git_said(tmp_path: Path) -> None:
    """The witness for #3402: the message survives to the log.

    `_git` used to raise `CalledProcessError`, whose string is the argv and the
    exit status — git's own explanation was captured and discarded. A fixture
    failure was then undiagnosable from CI output, which is how a `git fetch`
    exiting 128 against a purely local origin cost a full investigation and
    ended in a re-run rather than an answer.
    """
    with pytest.raises(AssertionError) as excinfo:
        _git(tmp_path, "rev-parse", "HEAD")

    message = str(excinfo.value)
    assert "exited" in message, "the exit status must survive"
    assert "stderr:" in message, "git's own channel must be reported, not swallowed"
    # git's wording differs across versions; what must hold is that SOMETHING
    # git wrote is present, rather than an empty stderr line standing in for it.
    assert "stderr: (empty)" not in message, (
        "a git failure with no stderr reported is exactly the undiagnosable "
        "shape this guards against"
    )
