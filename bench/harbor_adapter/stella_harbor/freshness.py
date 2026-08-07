"""Refuse a SUT binary too far behind the code it will be reported as measuring.

Why this exists
---------------
Pinning and freshness are different properties, and until this module only the
first was checked. A run records the commit its binary was built from, so every
number is attributable to *some* revision — but nothing asked whether that
revision is anywhere near the one a reader will assume was measured.

Measured 2026-08-07, the path every runbook and launch script exports as
``STELLA_BINARY``::

    ~/.arenabench/sut/stella                 built Aug 4
    ~/.arenabench/sut/sut_commit.txt         47d587a37da0e1013010f4479295b33878760b7d

    $ git rev-list --count 47d587a3..origin/main
    291

That binary was accompanied by its own commit file, so it *looked* pinned and
passed every check that existed: it was pinned, to a commit from three days and
291 changes earlier. Every match run against it reported numbers for code that
had since been rewritten underneath them, and nothing said a word.

What this module checks
-----------------------
Two questions the caller cannot answer from a path:

**Which commit is this binary, really?** ``crates/stella-cli/build.rs`` stamps
``STELLA_BUILD_GIT_SHA`` into a single ``<version>-dev.<40-hex>`` literal, and
``build_info.rs`` deliberately surrounds it with NUL bytes so LLVM's string
pooling cannot adjoin identifier characters to it — precisely so the identity
can be read out of the file without executing it. That is *intrinsic*: it
travels inside the artifact and no rename, copy, or symlink can separate the
two. ``sut_commit.txt`` beside the binary is *extrinsic* — a claim about the
file, which is the thing that went stale above. Both are read; the intrinsic
one decides, and a disagreement between them is itself a refusal, because it
means somebody's bookkeeping describes a binary that is not there.

**How far is that commit from the reference?** ``git rev-list --left-right
--count <commit>...<reference>`` in a checkout of this repository. A commit that
is not an ancestor of the reference at all — built from a topic branch, or from
a branch since force-pushed away — is reported as incomparable rather than as
zero, because unknown is not the same as current and must never render as one.

Reading the ELF rather than running ``stella --version`` is what lets this run
on the host *before any container exists*, on a macOS development machine that
cannot exec a linux/amd64 binary at all. A guard that no-ops on the machine most
likely to be staging the wrong build is not a guard — the same reasoning that
made :mod:`stella_harbor.portability` parse ``.gnu.version_r`` in pure stdlib
instead of shelling out to ``readelf``.

The reference and the distance
------------------------------
``origin/main`` is the reference because it is what a published number is read
as measuring, and because ``bench/evidence/run/build_sut.sh`` already refuses to
build anything else.

:data:`DEFAULT_MAX_BEHIND` is 25 commits. It is a stated distance, not a claim
that 25 commits change nothing: at the rate this repository's ``main`` actually
moves — 654 commits in the seven days to 2026-08-07, about 93 a day — 25 is
roughly six hours of drift. That tolerates a binary staged at the start of a
long Terminal-Bench run whose ``main`` moved underneath it, and refuses the
291-commit artifact above by a factor of twelve. An operator who genuinely means
to measure older code passes ``--max-behind`` explicitly, which makes the
distance appear in the launch command instead of in nobody's head.

Run standalone::

    python3 stella_harbor/freshness.py /path/to/stella --repo /path/to/checkout

Exit status is ``0`` fresh, ``1`` stale, ``2`` could not be determined.
Preflight treats every nonzero status as fatal: a binary whose distance from the
reference cannot be established has not been shown to be near it.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

__all__ = [
    "DEFAULT_MAX_BEHIND",
    "DEFAULT_REFERENCE",
    "SIDECAR_FILENAME",
    "Distance",
    "ReferenceUnavailableError",
    "SutFreshnessError",
    "SutIdentity",
    "SutIdentityUnknownError",
    "check_freshness",
    "embedded_source_commits",
    "measure_distance",
    "read_identity",
    "read_sidecar_commit",
]

#: How far behind the reference a SUT binary may be. See the module docstring
#: for where the number comes from; it is deliberately a stated distance rather
#: than a tolerance anybody has argued is harmless.
DEFAULT_MAX_BEHIND = 25

#: What a published number is read as measuring. `build_sut.sh` refuses to build
#: from anything else, so the reference and the build agree by construction.
DEFAULT_REFERENCE = "origin/main"

#: The commit file `build_sut.sh` writes beside a staged binary.
SIDECAR_FILENAME = "sut_commit.txt"

#: The compile-time `-dev.<sha>` marker `build.rs` stamps into the binary. The
#: trailing lookahead is what makes a match a whole token: the literal is
#: NUL-delimited on purpose (`crates/stella-cli/src/build_info.rs`), so a
#: 40-hex run followed by another hex digit is not this marker.
_VERSION_COMMIT_BYTES = re.compile(rb"-dev\.([0-9A-Fa-f]{40})(?=[^0-9A-Fa-f])")

_FULL_SHA = re.compile(r"\A[0-9a-f]{40}\Z")

#: Only a full hex SHA or a conservative ref name ever reaches `git`. In
#: particular a leading `-` is rejected, which git would otherwise read as an
#: option rather than as a ref.
_SAFE_REF = re.compile(r"\A[A-Za-z0-9][A-Za-z0-9._/-]{0,200}\Z")

_CHUNK_BYTES = 1024 * 1024

#: Longest boundary a marker can straddle between two reads: the literal is
#: `-dev.` plus 40 hex plus the delimiter that ends it.
_OVERLAP_BYTES = 64


class SutFreshnessError(RuntimeError):
    """A binary's distance from the reference could not be established."""


class SutIdentityUnknownError(SutFreshnessError):
    """The binary does not say which commit it was built from, and nothing else does."""


class ReferenceUnavailableError(SutFreshnessError):
    """The reference commit could not be resolved in the given checkout."""


@dataclass(frozen=True)
class SutIdentity:
    """Which commit a staged binary is, and how that was established."""

    #: The path as given, before symlinks are followed.
    path: Path
    #: The same path with every symlink resolved. The rig's mitigation for the
    #: incident above is a symlink at the legacy location, so this is where the
    #: sidecar actually lives — and reporting both is what makes a run's log
    #: show that the runbook path and the artifact are the same file.
    resolved_path: Path
    #: The commit this binary is, or ``""`` when nothing recorded it.
    commit: str = ""
    #: ``"embedded"`` (read out of the binary) or ``"sidecar"``.
    source: str = ""
    #: Every distinct commit marker found in the binary's bytes. More than one
    #: means the artifact is not a single stamped build and cannot be named.
    embedded_commits: tuple[str, ...] = ()
    #: What `sut_commit.txt` beside the resolved binary claims, if anything.
    sidecar_commit: str = ""

    @property
    def known(self) -> bool:
        return bool(self.commit)

    @property
    def sidecar_disagrees(self) -> bool:
        """The sidecar names a different commit than the binary's own bytes."""
        return bool(
            self.sidecar_commit
            and self.embedded_commits
            and self.sidecar_commit not in self.embedded_commits
        )

    def describe(self) -> str:
        """One line naming the properties that decide the verdict."""
        if not self.known:
            return "records no commit"
        parts = [f"{self.commit[:8]} (from the {self.source})"]
        if self.sidecar_disagrees:
            parts.append(f"sidecar claims {self.sidecar_commit[:8]}")
        if len(self.embedded_commits) > 1:
            parts.append(f"{len(self.embedded_commits)} embedded markers")
        return ", ".join(parts)


@dataclass(frozen=True)
class Distance:
    """How far a binary's commit is from the reference, in commits."""

    commit: str
    reference: str
    reference_commit: str
    behind: int = 0
    ahead: int = 0
    #: ``False`` when the two commits cannot be related in this checkout at all.
    #: Unknown is not zero and must never be rendered as "current".
    comparable: bool = True

    @property
    def identical(self) -> bool:
        return bool(self.commit) and self.commit == self.reference_commit

    def summary(self) -> str:
        if self.identical:
            return f"exactly {self.reference} ({self.reference_commit[:8]})"
        if not self.comparable:
            return (
                f"{self.commit[:8]} is neither an ancestor nor a descendant of "
                f"{self.reference} ({self.reference_commit[:8]}) in this "
                "checkout, so its distance cannot be measured"
            )
        parts = []
        if self.behind:
            parts.append(f"{self.behind} commit(s) behind")
        if self.ahead:
            parts.append(f"{self.ahead} commit(s) ahead of")
        joined = " and ".join(parts) or "level with"
        return (
            f"{self.commit[:8]} is {joined} {self.reference} "
            f"({self.reference_commit[:8]})"
        )

    def to_json(self) -> dict[str, object]:
        return {
            "commit": self.commit,
            "reference": self.reference,
            "reference_commit": self.reference_commit,
            "behind": self.behind,
            "ahead": self.ahead,
            "comparable": self.comparable,
            "identical": self.identical,
            "summary": self.summary(),
        }


def embedded_source_commits(path: Path | str) -> tuple[str, ...]:
    """Every compile-time commit marker in a binary's bytes, lowercased.

    Streamed with a fixed overlap rather than read whole: the SUT is ~32 MiB and
    preflight has no reason to hold it in memory. The overlap is what keeps a
    marker split across two reads from being missed — silently dropping the only
    intrinsic evidence would fall back to the sidecar, which is the artifact this
    module exists not to trust.
    """
    binary = Path(path)
    found: list[str] = []
    tail = b""
    with binary.open("rb") as handle:
        for chunk in iter(lambda: handle.read(_CHUNK_BYTES), b""):
            window = tail + chunk
            found.extend(
                match.decode("ascii").lower()
                for match in _VERSION_COMMIT_BYTES.findall(window)
            )
            tail = window[-_OVERLAP_BYTES:]
    # A marker ending exactly at EOF has no delimiter after it for the lookahead
    # to match, so it is accepted here instead of being lost to truncation.
    at_eof = re.search(rb"-dev\.([0-9A-Fa-f]{40})\Z", tail)
    if at_eof is not None:
        found.append(at_eof.group(1).decode("ascii").lower())
    return tuple(dict.fromkeys(found))


def read_sidecar_commit(directory: Path) -> str:
    """The commit ``build_sut.sh`` recorded beside a staged binary, or ``""``."""
    try:
        text = (directory / SIDECAR_FILENAME).read_text(encoding="utf-8").strip()
    except OSError:
        return ""
    return text.lower() if _FULL_SHA.match(text.lower()) else ""


def read_identity(path: Path | str) -> SutIdentity:
    """Establish which commit a staged binary is.

    The binary's own bytes decide. The sidecar is read regardless — not as a
    fallback ranking but so a disagreement between the two can be *reported*,
    since a commit file describing a binary that is not there is a bookkeeping
    failure whichever of them is right.
    """
    given = Path(path)
    try:
        resolved = given.resolve(strict=True)
    except OSError as error:
        raise SutFreshnessError(f"{given} cannot be resolved: {error}") from error
    if not resolved.is_file():
        raise SutFreshnessError(f"{given} does not resolve to a regular file")

    try:
        embedded = embedded_source_commits(resolved)
    except OSError as error:
        raise SutFreshnessError(f"cannot read {resolved}: {error}") from error
    sidecar = read_sidecar_commit(resolved.parent)

    commit, source = "", ""
    if len(embedded) == 1:
        commit, source = embedded[0], "embedded build stamp"
    elif not embedded and sidecar:
        commit, source = sidecar, f"{SIDECAR_FILENAME} sidecar"

    return SutIdentity(
        path=given,
        resolved_path=resolved,
        commit=commit,
        source=source,
        embedded_commits=embedded,
        sidecar_commit=sidecar,
    )


def _git(repo: Path, *args: str, timeout: float = 30.0) -> str:
    try:
        result = subprocess.run(
            ["git", *args],
            cwd=str(repo),
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ReferenceUnavailableError(
            f"git {' '.join(args)} failed: {error}"
        ) from error
    if result.returncode != 0:
        raise ReferenceUnavailableError(
            f"git {' '.join(args)} exited {result.returncode}: {result.stderr.strip()}"
        )
    return result.stdout


def _safe_ref(ref: str) -> str:
    stripped = ref.strip()
    if _FULL_SHA.match(stripped.lower()) or _SAFE_REF.match(stripped):
        return stripped
    raise ReferenceUnavailableError(f"refusing to resolve unsafe ref {ref!r}")


def measure_distance(
    commit: str,
    repo: Path | str,
    reference: str = DEFAULT_REFERENCE,
) -> Distance:
    """How far ``commit`` is behind ``reference`` in the checkout at ``repo``.

    Never fetches. Preflight runs on a rig that may be offline mid-session, and
    a guard that reaches the network is a guard that fails for reasons unrelated
    to what it measures — whoever wants a fresher reference fetches first, in
    the wrapper that also builds. What this reports is therefore the distance
    from the reference *as this checkout knows it*, which is stated in the
    verdict so a stale checkout cannot masquerade as a fresh binary.
    """
    # Both halves of the rev-range are validated before either reaches git.
    # `commit` always arrives from `read_identity` in practice, which can only
    # produce a full hex SHA — but this is public API interpolated into a
    # `A...B` argument, and a value beginning with `-` would be read by git as
    # an option rather than as a revision.
    if not _FULL_SHA.match((commit or "").lower()):
        raise ReferenceUnavailableError(
            f"refusing to measure from {commit!r}: not a full 40-hex commit"
        )
    checkout = Path(repo)
    if not (checkout / ".git").exists():
        raise ReferenceUnavailableError(f"{checkout} is not a git checkout")
    reference_commit = _git(
        checkout, "rev-parse", "--verify", f"{_safe_ref(reference)}^{{commit}}"
    ).strip()

    if commit == reference_commit:
        return Distance(
            commit=commit, reference=reference, reference_commit=reference_commit
        )
    try:
        counts = _git(
            checkout,
            "rev-list",
            "--left-right",
            "--count",
            f"{commit}...{reference_commit}",
        ).split()
        ahead, behind = int(counts[0]), int(counts[1])
    except (ReferenceUnavailableError, IndexError, ValueError):
        # The binary's commit is not in this checkout — built elsewhere, or on a
        # branch since force-pushed away. Reported as incomparable, not as zero.
        return Distance(
            commit=commit,
            reference=reference,
            reference_commit=reference_commit,
            comparable=False,
        )
    return Distance(
        commit=commit,
        reference=reference,
        reference_commit=reference_commit,
        behind=behind,
        ahead=ahead,
    )


def check_freshness(
    identity: SutIdentity,
    distance: Distance,
    *,
    max_behind: int = DEFAULT_MAX_BEHIND,
) -> list[str]:
    """Every reason this binary must not be measured, most damning first.

    An empty list means the binary is close enough to the reference that a
    number produced with it can be attributed to that reference.
    """
    violations: list[str] = []

    if len(identity.embedded_commits) > 1:
        named = ", ".join(sha[:8] for sha in identity.embedded_commits)
        violations.append(
            f"carries {len(identity.embedded_commits)} different compile-time "
            f"commit stamps ({named}), so it is not one stamped build and no "
            "single commit can be attributed to it"
        )

    if identity.sidecar_disagrees:
        violations.append(
            f"{SIDECAR_FILENAME} beside it claims "
            f"{identity.sidecar_commit[:8]}, but the binary's own compile-time "
            f"stamp says {identity.commit[:8]} — the bookkeeping describes a "
            "different artifact than the one that would run"
        )

    if not distance.comparable:
        violations.append(distance.summary())
    elif distance.behind > max_behind:
        violations.append(
            f"{distance.summary()}, past the {max_behind}-commit limit — a "
            "number measured with it would be reported as this branch's and is "
            "not"
        )
    elif distance.ahead:
        violations.append(
            f"{distance.summary()} — it carries {distance.ahead} commit(s) that "
            f"{distance.reference} does not, so it is not the code a reader of "
            "the result will assume was measured"
        )

    return violations


def _render_report(
    identity: SutIdentity, distance: Distance, violations: list[str]
) -> str:
    lines = [f"binary: {identity.path}"]
    if identity.resolved_path != identity.path:
        lines.append(f"resolves to: {identity.resolved_path}")
    lines.append(f"identity: {identity.describe()}")
    lines.append(f"distance: {distance.summary()}")
    if violations:
        lines.append("STALE:")
        lines.extend(f"  - {item}" for item in violations)
        lines.append(
            "Rebuild with `bench/evidence/run/build_sut.sh`, which resolves the "
            "commit once and stamps it into the binary. Pass --max-behind "
            "explicitly to measure older code on purpose — the distance then "
            "appears in the launch command rather than in nobody's head."
        )
    else:
        lines.append("fresh: close enough to the reference to be reported as it")
    return "\n".join(lines)


def _default_repo() -> str:
    """The checkout to measure against, without asking the caller twice.

    ``TB_REPO`` is what ``bench/evidence/run/env.sh`` already exports, and this
    file lives three directories below the repository root, so a standalone
    invocation from anywhere still finds the right checkout.
    """
    from_env = os.environ.get("TB_REPO")
    if from_env:
        return from_env
    return str(Path(__file__).resolve().parents[3])


def main(argv: list[str] | None = None) -> int:
    """CLI entry point. 0 fresh, 1 stale, 2 undeterminable."""
    parser = argparse.ArgumentParser(
        prog="freshness",
        description=(
            "Assert a SUT binary is close enough to the reference branch that a "
            "number measured with it can be attributed to that branch."
        ),
    )
    parser.add_argument("binary", help="path to the staged SUT binary")
    parser.add_argument(
        "--repo",
        default=None,
        help="checkout to measure against (default: $TB_REPO, else this repository)",
    )
    parser.add_argument(
        "--reference",
        default=DEFAULT_REFERENCE,
        help="ref the binary is reported as measuring (default: %(default)s)",
    )
    parser.add_argument(
        "--max-behind",
        type=int,
        default=DEFAULT_MAX_BEHIND,
        help="commits behind the reference the binary may be (default: %(default)s)",
    )
    parser.add_argument(
        "--json", action="store_true", help="emit the identity and verdict as JSON"
    )
    args = parser.parse_args(argv)

    if args.max_behind < 0:
        print("FATAL: --max-behind cannot be negative", file=sys.stderr)
        return 2

    try:
        identity = read_identity(args.binary)
    except (SutFreshnessError, OSError) as error:
        print(f"FATAL: cannot establish SUT identity: {error}", file=sys.stderr)
        return 2

    if not identity.known:
        # Fail closed. A binary nobody stamped is exactly as unattributable as
        # one stamped with the wrong commit, and this check exists because
        # "it looked fine" is how the stale artifact kept being measured.
        detail = (
            f"it carries {len(identity.embedded_commits)} compile-time stamps"
            if identity.embedded_commits
            else "it carries no compile-time stamp and has no "
            f"{SIDECAR_FILENAME} beside it"
        )
        print(
            f"FATAL: {identity.resolved_path} does not say which commit it is: "
            f"{detail}. Build it with bench/evidence/run/build_sut.sh, which "
            "stamps STELLA_BUILD_GIT_SHA into the artifact.",
            file=sys.stderr,
        )
        return 2

    try:
        distance = measure_distance(
            identity.commit, args.repo or _default_repo(), args.reference
        )
    except SutFreshnessError as error:
        print(f"FATAL: cannot measure distance: {error}", file=sys.stderr)
        return 2

    violations = check_freshness(identity, distance, max_behind=args.max_behind)

    if args.json:
        print(
            json.dumps(
                {
                    "binary": str(identity.path),
                    "resolved_path": str(identity.resolved_path),
                    "commit": identity.commit,
                    "commit_source": identity.source,
                    "embedded_commits": list(identity.embedded_commits),
                    "sidecar_commit": identity.sidecar_commit,
                    "distance": distance.to_json(),
                    "max_behind": args.max_behind,
                    "violations": violations,
                    "fresh": not violations,
                },
                indent=2,
            )
        )
    else:
        stream = sys.stderr if violations else sys.stdout
        print(_render_report(identity, distance, violations), file=stream)

    return 1 if violations else 0


if __name__ == "__main__":  # pragma: no cover - exercised via subprocess in tests
    raise SystemExit(main())
