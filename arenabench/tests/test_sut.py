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

import subprocess
from pathlib import Path

import pytest

from arenabench import sut
from arenabench.model import MatchSpec
from arenabench.provenance import Provenance


def _git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=True
    ).stdout.strip()


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

    monkeypatch.setenv(sut.STELLA_REPO_ENV, str(work))
    monkeypatch.setenv("ARENABENCH_HOME", str(tmp_path / "home"))
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

    def test_the_empty_ref_is_an_explicit_opt_out(self, repo: Path):
        assert sut.sut_problem_for(self._spec(ref="")) is None

    def test_a_spec_written_before_this_field_defaults_to_main_not_unpinned(self):
        """Absent must not read as the opt-out.

        Otherwise every template predating the field silently accepts any
        binary — turning a new safety property off exactly where it is least
        visible.
        """
        assert MatchSpec.from_json({"name": "m", "dataset": "d"}).sut_ref == "main"


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
