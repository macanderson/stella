"""Regression tests for the SUT binary freshness guard.

These would have caught the 2026-08-07 defect. ``~/.arenabench/sut/stella`` —
the path every runbook exports as ``STELLA_BINARY`` — was 291 commits and three
days behind ``origin/main``, and it carried its own ``sut_commit.txt`` naming
that ancient commit. So it *looked* pinned and passed every check that existed:
it was pinned, just not to anything near the code the numbers were reported as
measuring. Pinning and freshness are different properties and only the first
was ever checked.

Every assertion here is on the *artifact and the graph*: a file is written with
chosen compile-time stamp bytes, a throwaway repository is given a chosen
history, and the guard's verdict is checked. Nothing asserts that a particular
command was typed, because the failure was that nobody re-typed the build
command for three days and everything downstream still agreed.

``stella_harbor/freshness.py`` is loaded by path, the same way
``test_portability.py`` loads its subject: the module is stdlib-only on purpose
— ``env.sh`` invokes it by file path on a host that is only building the SUT —
and importing it through the package would pull in the adapter root, and with
it ``harbor``.
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path

import pytest

_REPO_ROOT = Path(__file__).resolve().parents[3]
_CHECKER = _REPO_ROOT / "bench" / "harbor_adapter" / "stella_harbor" / "freshness.py"
_ENV_SH = _REPO_ROOT / "bench" / "evidence" / "run" / "env.sh"
_BUILD_SH = _REPO_ROOT / "bench" / "evidence" / "run" / "build_sut.sh"

_spec = importlib.util.spec_from_file_location("stella_harbor_freshness", _CHECKER)
# Module-scope assert, deliberately: an import precondition, not a constant
# check — without it there is nothing to collect.
assert _spec is not None and _spec.loader is not None
_freshness = importlib.util.module_from_spec(_spec)
# Registered before execution: `@dataclass` resolves a class's own module out of
# `sys.modules` while the decorator runs, and a module that is not there yet
# fails the whole load.
sys.modules[_spec.name] = _freshness
_spec.loader.exec_module(_freshness)

DEFAULT_MAX_BEHIND = _freshness.DEFAULT_MAX_BEHIND
SIDECAR_FILENAME = _freshness.SIDECAR_FILENAME
SutFreshnessError = _freshness.SutFreshnessError
check_freshness = _freshness.check_freshness
embedded_source_commits = _freshness.embedded_source_commits
measure_distance = _freshness.measure_distance
read_identity = _freshness.read_identity

# The commit the stale binary was actually stamped with, and the distance it
# actually sat at. Used verbatim so the fixtures stay recognisable as the defect.
STALE_COMMIT = "47d587a37da0e1013010f4479295b33878760b7d"
STALE_DISTANCE = 291


def _git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=True
    ).stdout.strip()


def _stamp(commit: str) -> bytes:
    """The exact byte shape `build.rs` emits: NUL-delimited `<version>-dev.<sha>`."""
    return b"\x000.6.132-dev." + commit.encode("ascii") + b"\x00"


def _binary(directory: Path, commit: str | None, *, name: str = "stella") -> Path:
    """A stand-in SUT binary carrying (or lacking) a compile-time stamp."""
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / name
    payload = b"\x7fELF" + b"\x00" * 128
    if commit is not None:
        payload += _stamp(commit)
    payload += b"trailing bytes"
    path.write_bytes(payload)
    return path


def _seed(root: Path, commits: int) -> Path:
    """A clone whose `origin/main` is ``commits`` ahead of the root commit."""
    origin = root / "origin"
    origin.mkdir(parents=True)
    _git(origin, "init", "-q", "--initial-branch=main")
    _git(origin, "config", "user.email", "t@example.com")
    _git(origin, "config", "user.name", "t")
    _git(origin, "commit", "-q", "--allow-empty", "-m", "base")

    work = root / "work"
    _git(root, "clone", "-q", str(origin), str(work))
    _git(work, "config", "user.email", "t@example.com")
    _git(work, "config", "user.name", "t")
    for index in range(1, commits + 1):
        _git(origin, "commit", "-q", "--allow-empty", "-m", f"c{index}")
    _git(work, "fetch", "-q", "origin")
    return work


@pytest.fixture(scope="module")
def repo(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """A checkout whose `origin/main` is far ahead of an older commit.

    Built to the shape of the defect rather than to a convenient minimum: the
    guard's job is to notice a large distance, so the fixture produces the real
    one and the assertions can name it. Module-scoped because 291 commits are
    not free and nothing here mutates the history — tests stage their binaries
    under their own ``tmp_path``.
    """
    return _seed(tmp_path_factory.mktemp("stale"), STALE_DISTANCE)


@pytest.fixture
def base_commit(repo: Path) -> str:
    """The commit `origin/main` is exactly ``STALE_DISTANCE`` ahead of."""
    return _git(repo, "rev-list", "--max-parents=0", "origin/main")


class TestEmbeddedIdentity:
    """Reading the commit out of the artifact, not out of a file beside it."""

    def test_reads_the_compile_time_stamp(self, tmp_path: Path) -> None:
        assert embedded_source_commits(_binary(tmp_path, STALE_COMMIT)) == (
            STALE_COMMIT,
        )

    def test_uppercase_stamp_is_normalised(self, tmp_path: Path) -> None:
        assert embedded_source_commits(_binary(tmp_path, STALE_COMMIT.upper())) == (
            STALE_COMMIT,
        )

    def test_repeated_stamp_is_one_commit(self, tmp_path: Path) -> None:
        path = tmp_path / "stella"
        path.write_bytes(_stamp(STALE_COMMIT) * 3)
        assert embedded_source_commits(path) == (STALE_COMMIT,)

    def test_a_longer_hex_run_is_not_a_stamp(self, tmp_path: Path) -> None:
        """The literal is NUL-delimited so a whole token is identifiable."""
        path = tmp_path / "stella"
        path.write_bytes(b"-dev." + (STALE_COMMIT + "ab").encode("ascii") + b"\x00")
        assert embedded_source_commits(path) == ()

    def test_a_stamp_split_across_reads_is_still_found(self, tmp_path: Path) -> None:
        """A missed marker would silently downgrade the answer to the sidecar.

        The reader streams in 1 MiB chunks, so a stamp straddling that boundary
        is the one input a naive implementation loses — and losing it fails
        *open*, back onto exactly the extrinsic claim this module distrusts.
        """
        path = tmp_path / "stella"
        stamp = _stamp(STALE_COMMIT)
        # Land the stamp so it begins a few bytes before the first boundary.
        path.write_bytes(b"\x00" * ((1 << 20) - 5) + stamp + b"\x00" * 32)
        assert embedded_source_commits(path) == (STALE_COMMIT,)

    def test_a_stamp_at_end_of_file_is_found(self, tmp_path: Path) -> None:
        path = tmp_path / "stella"
        path.write_bytes(b"\x00" * 64 + b"-dev." + STALE_COMMIT.encode("ascii"))
        assert embedded_source_commits(path) == (STALE_COMMIT,)


class TestIdentityResolution:
    """Which of the two identities decides, and what a disagreement means."""

    def test_the_binary_outranks_the_sidecar(self, tmp_path: Path) -> None:
        fresh = "a" * 40
        path = _binary(tmp_path, fresh)
        (tmp_path / SIDECAR_FILENAME).write_text(STALE_COMMIT)
        identity = read_identity(path)
        assert identity.commit == fresh
        assert identity.source == "embedded build stamp"
        assert identity.sidecar_disagrees

    def test_the_sidecar_answers_only_for_an_unstamped_binary(
        self, tmp_path: Path
    ) -> None:
        path = _binary(tmp_path, None)
        (tmp_path / SIDECAR_FILENAME).write_text(STALE_COMMIT)
        identity = read_identity(path)
        assert identity.commit == STALE_COMMIT
        assert identity.source == f"{SIDECAR_FILENAME} sidecar"
        assert not identity.sidecar_disagrees

    def test_two_stamps_name_no_commit(self, tmp_path: Path) -> None:
        path = tmp_path / "stella"
        path.write_bytes(_stamp(STALE_COMMIT) + _stamp("b" * 40))
        identity = read_identity(path)
        assert not identity.known
        assert len(identity.embedded_commits) == 2

    def test_an_unidentifiable_binary_is_reported_as_such(self, tmp_path: Path) -> None:
        assert not read_identity(_binary(tmp_path, None)).known

    def test_a_symlink_reads_the_sidecar_beside_its_target(
        self, tmp_path: Path
    ) -> None:
        """The rig's mitigation for this defect is a symlink at the legacy path.

        Reading ``sut_commit.txt`` next to the *link* rather than next to the
        *target* would report whatever used to be staged there — the very
        confusion the symlink was introduced to end.
        """
        target_dir = tmp_path / "sut" / ("c" * 40)
        target = _binary(target_dir, None)
        (target_dir / SIDECAR_FILENAME).write_text("c" * 40)
        # A stale sidecar at the legacy location, exactly as found on the rig.
        (tmp_path / "sut" / SIDECAR_FILENAME).write_text(STALE_COMMIT)
        link = tmp_path / "sut" / "stella"
        link.symlink_to(target)

        identity = read_identity(link)
        assert identity.commit == "c" * 40
        assert identity.resolved_path == target.resolve()
        assert identity.path == link


class TestDistance:
    """Measuring how far the binary is from what it will be reported as."""

    def test_counts_commits_behind_the_reference(
        self, repo: Path, base_commit: str
    ) -> None:
        distance = measure_distance(base_commit, repo)
        assert distance.behind == STALE_DISTANCE
        assert distance.ahead == 0
        assert distance.comparable

    def test_the_reference_itself_is_identical(self, repo: Path) -> None:
        head = _git(repo, "rev-parse", "origin/main")
        assert measure_distance(head, repo).identical

    def test_an_unknown_commit_is_incomparable_not_zero(self, repo: Path) -> None:
        """Unknown must never render as "current" — that is how a wrong number ships."""
        distance = measure_distance("f" * 40, repo)
        assert not distance.comparable
        assert distance.behind == 0
        assert "cannot be measured" in distance.summary()

    def test_a_divergent_commit_reports_both_directions(self, tmp_path: Path) -> None:
        """Its own checkout: this is the one test here that writes history."""
        work = _seed(tmp_path, 10)
        _git(work, "checkout", "-q", "-b", "side", "origin/main~10")
        _git(work, "commit", "-q", "--allow-empty", "-m", "side")
        distance = measure_distance(_git(work, "rev-parse", "HEAD"), work)
        assert distance.ahead == 1
        assert distance.behind == 10

    def test_a_non_checkout_is_refused(self, tmp_path: Path) -> None:
        with pytest.raises(SutFreshnessError):
            measure_distance("a" * 40, tmp_path)

    def test_an_option_shaped_ref_never_reaches_git(self, repo: Path) -> None:
        with pytest.raises(SutFreshnessError):
            measure_distance("a" * 40, repo, "--upload-pack=touch /tmp/pwned")

    def test_an_option_shaped_commit_never_reaches_git(self, repo: Path) -> None:
        """Both halves of the `A...B` range are validated, not just the ref.

        In practice `commit` only ever arrives from `read_identity`, which can
        produce nothing but a full hex SHA — but this is public API, and a
        value starting with `-` lands in an argument git reads as an option.
        """
        with pytest.raises(SutFreshnessError):
            measure_distance("--upload-pack=touch /tmp/pwned", repo)

    def test_a_short_commit_is_refused(self, repo: Path) -> None:
        with pytest.raises(SutFreshnessError):
            measure_distance(_git(repo, "rev-parse", "--short", "origin/main"), repo)


class TestVerdict:
    """What is refused, and what the refusal says."""

    def test_the_stale_binary_is_refused(self, repo: Path, base_commit: str, tmp_path: Path) -> None:
        identity = read_identity(_binary(tmp_path, base_commit))
        violations = check_freshness(identity, measure_distance(base_commit, repo))
        assert violations
        assert f"{STALE_DISTANCE} commit(s) behind" in violations[0]

    def test_a_binary_within_the_limit_passes(self, repo: Path, tmp_path: Path) -> None:
        near = _git(repo, "rev-parse", f"origin/main~{DEFAULT_MAX_BEHIND}")
        identity = read_identity(_binary(tmp_path, near))
        assert check_freshness(identity, measure_distance(near, repo)) == []

    def test_one_commit_past_the_limit_is_refused(self, repo: Path, tmp_path: Path) -> None:
        past = _git(repo, "rev-parse", f"origin/main~{DEFAULT_MAX_BEHIND + 1}")
        identity = read_identity(_binary(tmp_path, past))
        assert check_freshness(identity, measure_distance(past, repo))

    def test_a_disagreeing_sidecar_is_refused_even_when_fresh(self, repo: Path, tmp_path: Path) -> None:
        """Bookkeeping describing an absent artifact is a defect either way."""
        head = _git(repo, "rev-parse", "origin/main")
        stage = tmp_path
        identity_path = _binary(stage, head)
        (stage / SIDECAR_FILENAME).write_text(STALE_COMMIT)
        violations = check_freshness(
            read_identity(identity_path), measure_distance(head, repo)
        )
        assert any(SIDECAR_FILENAME in item for item in violations)

    def test_an_incomparable_commit_is_refused(self, repo: Path, tmp_path: Path) -> None:
        identity = read_identity(_binary(tmp_path, "f" * 40))
        assert check_freshness(identity, measure_distance("f" * 40, repo))


class TestCli:
    """The surface `env.sh` actually invokes."""

    def _run(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(_CHECKER), *args],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_the_documented_witness(self, repo: Path, base_commit: str, tmp_path: Path) -> None:
        """Point the checker at the stale binary and it refuses, naming both facts.

        This is the witness the issue asks for: before this module the same
        invocation produced scoreable trials, because nothing in the pipeline
        had an opinion about distance.
        """
        binary = _binary(tmp_path, base_commit)
        result = self._run(str(binary), "--repo", str(repo))
        assert result.returncode == 1
        assert base_commit[:8] in result.stderr
        assert f"{STALE_DISTANCE} commit(s) behind" in result.stderr

    def test_a_fresh_binary_is_accepted(self, repo: Path, tmp_path: Path) -> None:
        head = _git(repo, "rev-parse", "origin/main")
        result = self._run(str(_binary(tmp_path, head)), "--repo", str(repo))
        assert result.returncode == 0, result.stderr
        assert "fresh" in result.stdout

    def test_an_unidentifiable_binary_fails_closed(self, repo: Path, tmp_path: Path) -> None:
        """Status 2, not 0: not shown to be near the reference is not near it."""
        result = self._run(str(_binary(tmp_path, None)), "--repo", str(repo))
        assert result.returncode == 2
        assert "does not say which commit it is" in result.stderr

    def test_an_explicit_limit_is_honoured(self, repo: Path, base_commit: str, tmp_path: Path) -> None:
        binary = _binary(tmp_path, base_commit)
        result = self._run(
            str(binary), "--repo", str(repo), "--max-behind", str(STALE_DISTANCE)
        )
        assert result.returncode == 0, result.stderr

    def test_an_explicit_reference_pins_equality(self, repo: Path, tmp_path: Path) -> None:
        """`--reference <sha> --max-behind 0` is how build_sut.sh checks its own output."""
        head = _git(repo, "rev-parse", "origin/main")
        binary = _binary(tmp_path, head)
        assert (
            self._run(
                str(binary),
                "--repo",
                str(repo),
                "--reference",
                head,
                "--max-behind",
                "0",
            ).returncode
            == 0
        )
        stale = _binary(tmp_path / "other", _git(repo, "rev-parse", "origin/main~1"))
        assert (
            self._run(
                str(stale),
                "--repo",
                str(repo),
                "--reference",
                head,
                "--max-behind",
                "0",
            ).returncode
            == 1
        )

    def test_json_carries_the_numbers(self, repo: Path, base_commit: str, tmp_path: Path) -> None:
        import json

        binary = _binary(tmp_path, base_commit)
        result = self._run(str(binary), "--repo", str(repo), "--json")
        payload = json.loads(result.stdout)
        assert payload["commit"] == base_commit
        assert payload["distance"]["behind"] == STALE_DISTANCE
        assert payload["fresh"] is False


class TestWiring:
    """The guard is only a guard if the runbook runs it."""

    def test_preflight_calls_it(self) -> None:
        text = _ENV_SH.read_text()
        assert "assert_fresh_sut" in text
        preflight = text.split("preflight() {", 1)[1]
        assert "assert_fresh_sut || return 1" in preflight

    def test_the_limit_is_not_restated_in_shell(self) -> None:
        """One copy of the number, so the two cannot drift apart.

        Asserted as "preflight passes no limit" rather than "the digits do not
        appear": `env.sh` is full of incidental digits (the dataset digest alone
        contains most two-digit numbers), and a substring search over it would
        be a test of the fixture's luck.
        """
        preflight = _ENV_SH.read_text().split("preflight() {", 1)[1]
        assert "--max-behind" not in preflight

    def test_build_sut_verifies_its_own_output(self) -> None:
        assert "assert_fresh_sut" in _BUILD_SH.read_text()

    def test_build_sut_accepts_an_explicit_commit(self) -> None:
        """The wrapper that fetches and checks out must be able to name the commit.

        Without it the script re-fetches and can resolve a *different*
        `origin/main` than the caller prepared against, whose symptom is a drift
        list of files nobody touched.
        """
        text = _BUILD_SH.read_text()
        assert 'if [ "$#" -gt 0 ]; then' in text
        assert "moved while this script ran" in text

    def test_runs_without_importing_the_adapter_package(self) -> None:
        """`env.sh` invokes this by path on a host that has no `harbor` installed."""
        result = subprocess.run(
            [sys.executable, "-I", str(_CHECKER), "--help"],
            capture_output=True,
            text=True,
            check=False,
        )
        assert result.returncode == 0, result.stderr
