# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Which Stella binary a match runs, and proving it is the one you meant.

The system under test is a **cross-compiled binary staged on disk**, not
something the arena builds on the fly: it must be linux/amd64 against a glibc
2.17 floor so it can exec inside every task image, which is a `cargo zigbuild`
away from an ordinary build (`bench/evidence/run/build_sut.sh`). That script
already proves its own provenance — it refuses to build unless every Rust input
is byte-identical to `origin/main`, then records the commit and the binary's
SHA-256 beside the artifact.

**The gap this module closes is that the proof was only ever made at build
time.** Nothing re-checked it at *match* time, so the guarantee decayed
silently the moment nobody re-ran the script: measured 2026-08-06, the staged
binary was 291 commits behind `main` and three days old, and every match in
between reported numbers for that stale code without a word. A benchmark that
cannot say which commit it measured is not a benchmark, and one that says the
wrong commit is worse.

Two design points, both borrowed from how this codebase already treats
datasets:

**A branch is not a freeze.** :mod:`arenabench.registry` pins datasets by
digest because "an unversioned name is not a freeze", and the identical logic
governs the SUT: `main` on 2026-08-04 and `main` today are 291 commits apart.
So a *ref* is how an operator chooses, and a resolved *commit* is what the
system records and keys the binary by. Recording "we ran main" tells a reader
nothing they can check.

**Building is never implicit.** A release cross-compile with LTO takes minutes.
Doing it inside a match launch would turn the fast path into a slow one that
fails in new ways, so builds are an explicit action and their artifacts are
cached per commit under ``<sut root>/<commit>/``. A match then only ever has to
*find* a binary, which either exists or does not.

Git is consulted read-only and every ref that reaches a subprocess is either a
full hex SHA or a name drawn from :func:`list_branches` — never a string the
caller supplied, so a loopback HTTP client cannot smuggle an option into
``git``.
"""

from __future__ import annotations

import hashlib
import os
import re
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

__all__ = [
    "MAX_BEHIND_UNPINNED",
    "STELLA_BINARY_ENV",
    "STELLA_REPO_ENV",
    "Branch",
    "Drift",
    "StagedSut",
    "SutUnavailableError",
    "ambient_sut",
    "binary_for",
    "drift_between",
    "embedded_commit",
    "list_branches",
    "resolve_ref",
    "staged_for",
    "stella_repo",
    "sut_problem_for",
    "sut_root",
    "sut_status",
]

#: Points the arena at the Stella checkout it resolves refs and builds from.
STELLA_REPO_ENV = "ARENABENCH_STELLA_REPO"

#: The binary a match runs when it is not pinned to a commit.
STELLA_BINARY_ENV = "STELLA_BINARY"

#: How far behind the default branch an *unpinned* binary may be.
#:
#: An unpinned match is asking for "whatever is current", so a binary that is
#: measurably not current contradicts the request rather than opting out of it.
#: Measuring older code on purpose is still available and is spelled the
#: supported way — pin ``sut_ref`` to that commit, which records in the result
#: what was measured instead of leaving it to the state of one path on one
#: machine. The number matches the checker the Terminal-Bench runbook uses
#: (``bench/harbor_adapter/stella_harbor/freshness.py``): about six hours of
#: this repository's ``main``, which moved 654 commits in the seven days to
#: 2026-08-07.
MAX_BEHIND_UNPINNED = 25

#: The compile-time ``-dev.<sha>`` marker Stella's ``build.rs`` stamps into the
#: binary. ``crates/stella-cli/src/build_info.rs`` deliberately delimits it with
#: NUL bytes so LLVM's string pooling cannot adjoin identifier characters to it,
#: which is what makes the identity readable without executing the artifact —
#: necessary here, because the SUT is a linux/amd64 cross-build and the arena
#: routinely runs on a macOS host that cannot exec it.
_VERSION_COMMIT_BYTES = re.compile(rb"-dev\.([0-9A-Fa-f]{40})(?=[^0-9A-Fa-f])")

#: A full 40-hex commit id — the only ref shape ever passed to git unvalidated.
_FULL_SHA = re.compile(r"\A[0-9a-f]{40}\Z")

#: Branch names the arena will accept from a client. Deliberately stricter than
#: git's own rules: no leading dash (which git would read as an option), no
#: whitespace, no `..`, and nothing outside a conservative character set.
_SAFE_REF = re.compile(r"\A[A-Za-z0-9][A-Za-z0-9._/-]{0,200}\Z")


class SutUnavailableError(RuntimeError):
    """No Stella checkout, or git could not answer."""


def sut_root() -> Path:
    """Where staged SUT binaries live, honouring ``ARENABENCH_HOME``."""
    home = os.environ.get("ARENABENCH_HOME")
    base = Path(home).expanduser() if home else Path.home() / ".arenabench"
    return base / "sut"


def stella_repo() -> Path | None:
    """The Stella checkout to resolve refs against, or ``None``.

    ``ARENABENCH_STELLA_REPO`` wins. Otherwise the adapter path already needed
    to run a Stella seat (``ARENABENCH_STELLA_ADAPTER`` points at
    ``<repo>/bench/harbor_adapter``) names the repository two levels up, so a
    rig that can run Stella at all can usually answer this without new
    configuration.
    """
    explicit = os.environ.get(STELLA_REPO_ENV)
    if explicit:
        candidate = Path(explicit).expanduser()
        return candidate if (candidate / ".git").exists() else None
    adapter = os.environ.get("ARENABENCH_STELLA_ADAPTER")
    if adapter:
        candidate = Path(adapter).expanduser().resolve().parent.parent
        if (candidate / ".git").exists():
            return candidate
    return None


def _git(repo: Path, *args: str, timeout: float = 30.0) -> str:
    try:
        out = subprocess.run(
            ["git", *args],
            cwd=str(repo),
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise SutUnavailableError(f"git {' '.join(args)} failed: {exc}") from exc
    if out.returncode != 0:
        raise SutUnavailableError(
            f"git {' '.join(args)} exited {out.returncode}: {out.stderr.strip()}"
        )
    return out.stdout


def _safe_ref(ref: str) -> str:
    """Validate a ref before it reaches a subprocess.

    A full SHA passes unchanged. Anything else must look like an ordinary
    branch name; in particular a leading ``-`` is rejected, because git would
    read it as an option rather than a ref.
    """
    ref = ref.strip()
    if _FULL_SHA.match(ref) or _SAFE_REF.match(ref):
        return ref
    raise SutUnavailableError(f"refusing to resolve unsafe ref {ref!r}")


@dataclass(frozen=True)
class Branch:
    """One branch an operator may build the SUT from."""

    #: Display name, e.g. ``main`` or ``worktree-foo`` — remote prefix stripped.
    name: str
    #: What is passed to git, e.g. ``origin/main``.
    ref: str
    commit: str
    subject: str = ""
    committed_at: int = 0
    #: ``True`` for the repository's default branch, which the UI preselects
    #: and which every published number should normally come from.
    is_default: bool = False

    def to_json(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "ref": self.ref,
            "commit": self.commit,
            "short": self.commit[:8],
            "subject": self.subject,
            "committed_at": self.committed_at,
            "is_default": self.is_default,
        }


@dataclass(frozen=True)
class StagedSut:
    """A built Stella binary on disk, with whatever provenance was recorded."""

    path: Path
    #: Full commit the binary was built from. Empty when unrecorded — which is
    #: reported as unknown, never guessed. A binary whose commit nobody wrote
    #: down cannot be shown to be the one you meant.
    commit: str = ""
    sha256: str = ""
    built_at: float = 0.0

    @property
    def known(self) -> bool:
        return bool(self.commit)

    def to_json(self) -> dict[str, Any]:
        return {
            "path": str(self.path),
            "commit": self.commit,
            "short": self.commit[:8] if self.commit else "",
            "sha256": self.sha256,
            "built_at": self.built_at,
            "known": self.known,
        }


@dataclass(frozen=True)
class Drift:
    """How far a staged binary is from the commit a match asked for."""

    staged: str
    target: str
    behind: int = 0
    ahead: int = 0
    #: ``False`` when the two commits cannot be compared at all — the staged
    #: commit is not in this checkout (built elsewhere, or the branch was
    #: force-pushed away). Unknown is not zero, and must not render as "match".
    comparable: bool = True

    @property
    def identical(self) -> bool:
        return bool(self.staged) and self.staged == self.target

    def summary(self) -> str:
        if self.identical:
            return "exactly the requested commit"
        if not self.staged:
            return "the staged binary records no commit, so it cannot be identified"
        if not self.comparable:
            return (
                f"staged {self.staged[:8]} is not an ancestor or descendant of "
                f"{self.target[:8]} in this checkout — it cannot be compared"
            )
        parts = []
        if self.behind:
            parts.append(f"{self.behind} commit(s) behind")
        if self.ahead:
            parts.append(f"{self.ahead} commit(s) ahead of")
        return f"staged {self.staged[:8]} is " + " and ".join(parts) + (
            f" {self.target[:8]}"
        )

    def to_json(self) -> dict[str, Any]:
        return {
            "staged": self.staged,
            "target": self.target,
            "behind": self.behind,
            "ahead": self.ahead,
            "comparable": self.comparable,
            "identical": self.identical,
            "summary": self.summary(),
        }


def resolve_ref(ref: str, repo: Path | None = None) -> str:
    """Resolve a branch name or SHA to a full commit id, **remote first**.

    ``main`` means ``origin/main``, not the local branch of the same name.
    That ordering is the entire point of this module: a local branch is
    whatever the operator's checkout last happened to fetch or commit, and on
    the machine this was written for the two were 291 commits apart. A
    benchmark that says "main" and measures a stale local copy is reporting a
    number nobody else can reproduce.

    A full SHA and an explicitly-qualified ref (``origin/main``) are used as
    given; only a bare name gets the ``origin/`` preference.
    """
    repo = repo or stella_repo()
    if repo is None:
        raise SutUnavailableError(
            "no Stella checkout found — set "
            f"{STELLA_REPO_ENV} to the repository root"
        )
    safe = _safe_ref(ref)
    candidates = [safe] if (_FULL_SHA.match(safe) or "/" in safe) else [
        f"origin/{safe}",
        safe,
    ]
    last: SutUnavailableError | None = None
    for candidate in candidates:
        try:
            return _git(
                repo, "rev-parse", "--verify", f"{candidate}^{{commit}}"
            ).strip()
        except SutUnavailableError as exc:
            last = exc
    raise last or SutUnavailableError(f"cannot resolve ref {ref!r}")


def list_branches(repo: Path | None = None, *, fetch: bool = False) -> list[Branch]:
    """Every remote branch, newest commit first, default branch flagged.

    Remote branches rather than local ones on purpose: the question a benchmark
    asks is "what does this code look like *for everyone*", and a local branch
    is by definition not that. A local-only experiment is still reachable by
    pasting its SHA.
    """
    repo = repo or stella_repo()
    if repo is None:
        raise SutUnavailableError(
            f"no Stella checkout found — set {STELLA_REPO_ENV} to the repository root"
        )
    if fetch:
        # Best-effort: a rig offline mid-session should still list what it has
        # rather than fail the whole picker.
        try:
            _git(repo, "fetch", "--quiet", "--prune", "origin", timeout=120.0)
        except SutUnavailableError:
            pass

    default = ""
    try:
        head = _git(repo, "symbolic-ref", "--quiet", "refs/remotes/origin/HEAD").strip()
        default = head.rsplit("/", 1)[-1]
    except SutUnavailableError:
        default = "main"

    raw = _git(
        repo,
        "for-each-ref",
        "--sort=-committerdate",
        "--format=%(refname:short)%09%(objectname)%09%(committerdate:unix)%09%(subject)",
        "refs/remotes/origin",
    )
    branches: list[Branch] = []
    for line in raw.splitlines():
        parts = line.split("\t")
        if len(parts) < 3:
            continue
        ref, commit, when = parts[0], parts[1], parts[2]
        subject = parts[3] if len(parts) > 3 else ""
        name = ref.split("/", 1)[-1] if ref.startswith("origin/") else ref
        if name == "HEAD":
            continue
        branches.append(
            Branch(
                name=name,
                ref=ref,
                commit=commit,
                subject=subject,
                committed_at=int(when or 0),
                is_default=(name == default),
            )
        )
    # The default branch first regardless of commit date: it is the answer for
    # almost every run, and a picker that buries it invites the wrong choice.
    branches.sort(key=lambda b: (not b.is_default, -b.committed_at))
    return branches


def _read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8").strip()
    except OSError:
        return ""


def _staged_at(directory: Path) -> StagedSut | None:
    binary = directory / "stella"
    if not binary.is_file():
        return None
    try:
        built_at = binary.stat().st_mtime
    except OSError:
        built_at = 0.0
    return StagedSut(
        path=binary,
        commit=_read(directory / "sut_commit.txt"),
        sha256=_read(directory / "binary_sha256.txt"),
        built_at=built_at,
    )


def staged_for(commit: str) -> StagedSut | None:
    """The cached binary built from ``commit``, if one has been staged."""
    if not _FULL_SHA.match(commit or ""):
        return None
    return _staged_at(sut_root() / commit)


def legacy_staged() -> StagedSut | None:
    """The single unversioned binary older tooling stages at ``<root>/stella``.

    Kept because `build_sut.sh` writes there and rigs already depend on it. It
    is only ever *reported*, never assumed to be any particular commit: the
    whole failure this module exists for was treating that path as though it
    were current.
    """
    return _staged_at(sut_root())


def embedded_commit(path: Path) -> str:
    """The commit stamped into a Stella binary at compile time, or ``""``.

    This is the artifact's *intrinsic* identity: it travels inside the file, so
    no rename, copy, or symlink can separate the two. ``sut_commit.txt`` beside
    a binary is a *claim about* the file, and a claim is what went wrong — the
    stale binary had one, correctly naming a commit from three days earlier, and
    so passed for pinned.

    Read in chunks with an overlap, because a marker split across two reads
    would otherwise be missed and silently downgrade the answer to the sidecar.
    Any read error yields ``""``: this reports identity, and "cannot tell" is a
    state its callers already handle as unknown rather than as current.
    """
    found: list[str] = []
    tail = b""
    try:
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1 << 20), b""):
                window = tail + chunk
                found.extend(
                    match.decode("ascii").lower()
                    for match in _VERSION_COMMIT_BYTES.findall(window)
                )
                tail = window[-64:]
    except OSError:
        return ""
    at_eof = re.search(rb"-dev\.([0-9A-Fa-f]{40})\Z", tail)
    if at_eof is not None:
        found.append(at_eof.group(1).decode("ascii").lower())
    unique = set(found)
    # More than one stamp is not one build, and naming either would be a guess.
    return unique.pop() if len(unique) == 1 else ""


def ambient_sut() -> StagedSut | None:
    """The binary ``STELLA_BINARY`` names, with whatever identity it carries.

    This is what an unpinned match runs, and it is the path the operator runbook
    hands to `arenabench serve` — so it is the one binary nothing was checking.
    Symlinks are followed before the sidecar is read: the mitigation applied on
    the rig for this defect is a symlink at the legacy location, and reading the
    commit file next to the *link* rather than next to the *target* would report
    the identity of whatever used to be there.
    """
    raw = os.environ.get(STELLA_BINARY_ENV)
    if not raw:
        return None
    try:
        resolved = Path(raw).expanduser().resolve(strict=True)
    except OSError:
        return None
    if not resolved.is_file():
        return None
    # Read the sidecars directly rather than through `_staged_at`, which finds
    # them only beside a file literally named `stella`. STELLA_BINARY may name
    # anything, and this must report on the binary it was actually handed.
    intrinsic = embedded_commit(resolved)
    try:
        built_at = resolved.stat().st_mtime
    except OSError:
        built_at = 0.0
    return StagedSut(
        path=resolved,
        commit=intrinsic or _read(resolved.parent / "sut_commit.txt"),
        sha256=_read(resolved.parent / "binary_sha256.txt"),
        built_at=built_at,
    )


def binary_for(commit: str) -> StagedSut | None:
    """The binary a match pinned to ``commit`` should run, or ``None``.

    Prefers the per-commit cache. Falls back to the legacy path **only when it
    records that exact commit** — an unlabelled binary is never silently
    accepted as the one requested, which is precisely how a three-day-old
    artifact kept being measured.
    """
    exact = staged_for(commit)
    if exact is not None:
        return exact
    legacy = legacy_staged()
    if legacy is not None and legacy.commit == commit:
        return legacy
    return None


def drift_between(staged: str, target: str, repo: Path | None = None) -> Drift:
    """How far ``staged`` is from ``target`` in this checkout."""
    if not staged:
        return Drift(staged="", target=target, comparable=False)
    if staged == target:
        return Drift(staged=staged, target=target)
    repo = repo or stella_repo()
    if repo is None:
        return Drift(staged=staged, target=target, comparable=False)
    try:
        raw = _git(
            repo, "rev-list", "--left-right", "--count", f"{staged}...{target}"
        ).split()
        ahead, behind = int(raw[0]), int(raw[1])
    except (SutUnavailableError, ValueError, IndexError):
        return Drift(staged=staged, target=target, comparable=False)
    return Drift(staged=staged, target=target, behind=behind, ahead=ahead)


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def unpinned_problem() -> str | None:
    """Why an *unpinned* Stella match must not launch, or ``None``.

    Clearing the pin says "run whatever is current", so the one thing that can
    still be wrong is the binary not being current. That is not hypothetical:
    the path the operator runbook hands to ``arenabench serve`` was 291 commits
    behind ``main`` for three days, carrying a ``sut_commit.txt`` that named its
    own ancient commit, and every unpinned match against it produced perfectly
    scoreable trials attributed to code that had been rewritten underneath them.

    **Refuses only on positive evidence of staleness.** An ordinary
    ``cargo build --release`` carries no compile-time stamp and cannot be dated,
    and refusing it would block the local development loop this tool exists to
    serve; those runs keep the existing "unverified" label and warning. The
    Terminal-Bench evidence runbook takes the opposite posture and fails closed
    on an unidentifiable binary, because it publishes numbers — the difference
    is deliberate and follows from who reads the result, not from a disagreement
    about what is safe.
    """
    ambient = ambient_sut()
    if ambient is None or not ambient.commit:
        return None
    try:
        target = resolve_ref("main")
    except SutUnavailableError:
        # No checkout to compare against. Nothing is proven either way, and an
        # arena that cannot see a repository still has to run matches.
        return None
    drift = drift_between(ambient.commit, target)
    if not drift.comparable or drift.behind <= MAX_BEHIND_UNPINNED:
        return None
    return (
        f"the Stella seat is unpinned, so it runs {STELLA_BINARY_ENV}="
        f"{ambient.path} — and {drift.summary()}, past the "
        f"{MAX_BEHIND_UNPINNED}-commit limit. An unpinned match asks for "
        "whatever is current, and this binary is not. Rebuild it "
        "(`bench/evidence/run/build_sut.sh`), or pin the SUT to the commit you "
        "actually mean to measure — pinning is how the result records which "
        "Stella it was."
    )


def sut_problem_for(spec: Any) -> str | None:
    """Why this match must not launch, as far as the SUT is concerned.

    ``None`` means "go". A string is a refusal, phrased for an operator who
    now has to do something about it.

    Only matches carrying a Stella seat are checked — a Claude-Code-vs-Codex
    contest has no system under test of ours and must not be blocked by the
    state of a binary it never runs. An empty ``sut_ref`` opts out of *pinning*,
    not out of every check: see :func:`unpinned_problem`.
    """
    if not any(c.agent == "stella" for c in spec.contestants):
        return None
    if not getattr(spec, "sut_ref", ""):
        return unpinned_problem()
    status = sut_status(spec.sut_ref)
    if status["ready"]:
        return None
    detail = status.get("problem") or "the requested Stella build is unavailable"
    return (
        f"Stella seat pinned to {spec.sut_ref!r}, but {detail} "
        "Build it from the arena's SUT panel, or run "
        "`bench/evidence/run/build_sut.sh`. To measure an unattributable "
        "binary on purpose, clear the SUT pin — the match is then recorded "
        "as unverified."
    )


def sut_status(ref: str = "main") -> dict[str, Any]:
    """Everything the UI needs to say which Stella is about to run.

    Never raises for an unusable rig: a missing checkout, a git that will not
    answer, or an absent binary are all *states to report*, because the whole
    point is to tell an operator what is wrong rather than to hide behind an
    error page.
    """
    out: dict[str, Any] = {
        "ref": ref,
        "repo": None,
        "target": None,
        "staged": None,
        "legacy": None,
        "drift": None,
        "ready": False,
        "problem": None,
    }
    repo = stella_repo()
    if repo is None:
        out["problem"] = (
            "no Stella checkout is configured, so the arena cannot say which "
            f"commit a Stella seat would run. Set {STELLA_REPO_ENV} to the "
            "repository root."
        )
        legacy = legacy_staged()
        out["legacy"] = legacy.to_json() if legacy else None
        return out
    out["repo"] = str(repo)
    try:
        target = resolve_ref(ref, repo)
    except SutUnavailableError as exc:
        out["problem"] = str(exc)
        return out
    out["target"] = target

    staged = binary_for(target)
    legacy = legacy_staged()
    out["legacy"] = legacy.to_json() if legacy else None
    if staged is not None:
        out["staged"] = staged.to_json()
        out["drift"] = Drift(staged=target, target=target).to_json()
        out["ready"] = True
        return out

    # Nothing built for the requested commit. Say what *is* there and how far
    # off it is, because "no binary" and "a binary from three days ago" call
    # for very different reactions.
    fallback = legacy
    if fallback is None:
        out["problem"] = (
            f"no Stella binary has been built for {target[:8]}. Build one "
            "before running a Stella seat."
        )
        return out
    drift = drift_between(fallback.commit, target, repo)
    out["drift"] = drift.to_json()
    out["problem"] = (
        f"the only staged binary is not {target[:8]}: {drift.summary()}. "
        "Build the requested commit before running a Stella seat."
    )
    return out
