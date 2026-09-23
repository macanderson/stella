"""Git pairing: which paths the guard reads, and where each one came from.

Every ratchet is keyed by path, which means a file that MOVES takes its debt
with it or the same sentences are read as newly written the moment they land
somewhere else. `--update` therefore asks git what was renamed and carries
each entry to the file's new path first (`renamed_paths`). The ratchet is
unchanged by this: a carried entry still goes through the same `min`, so a
move can lower a count and never raise one, and a genuinely new file still
starts at zero. Without it, splitting a module means rewording prose nobody
was editing — #5420 hit this and had to hand-edit the baseline, which is the
one thing this guard is supposed to make unnecessary.

A split is the move git cannot name, and `--update` carries it too, through
the `split_sources` pairing the plain check uses. Carrying it at check time
alone was not enough: the entry never reached the baseline, so the post-merge
`--absolute` run — which skips the base tree by design — charged the carried
sentences to the new-file ceiling and reddened `main` (#6408). A source with
no entry hands on nothing, having no allowance of its own to give.

`resolve_base_commit` names the tree a change is judged against, and `git`
is the wrapper each read of that tree goes through.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

from .scan import is_scanned, prose_lines, prose_only


def renamed_paths(root: Path, commit: str = "HEAD") -> dict[str, str]:
    """Old path -> new path, for every file git sees as renamed against `commit`.

    `--update` asks against HEAD, so a moved file's baseline entry follows it
    to the new path. The plain check asks against the base commit, so every
    ratchet reads the moved file's old path when it looks up what the base
    tree held: a rename is not new prose, and a unit's mean is arithmetic
    over a file *set* that a move changes with nobody writing a word.

    Fails open at every unknown (no git, no HEAD, an unreadable tree): an
    empty map means no entry is carried, which is the behaviour this had
    before, so a broken git can never turn a red gate green.

    Detection is git's, not ours, which is why the move must be staged —
    `git mv`, or `git add` after a plain `mv`. An unstaged move looks like a
    delete plus an untracked file, and nothing can honestly pair those.

    It is also why a move that rewrites most of the file carries nothing: git
    reports a delete beside an add once similarity drops far enough, and there
    is no rename to follow. That is the right answer rather than a gap — a
    file rewritten in transit is new prose, and it should be read as new.

    Only a pair of files the guard reads counts. A file it skips was never held
    to the rule, so it has no allowance to hand on, and its sentences are new
    prose the moment they land in a file the guard reads.
    """
    try:
        proc = subprocess.run(
            ["git", "diff", "-M", "--name-status", commit],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return {}
    if proc.returncode != 0:
        return {}
    moved: dict[str, str] = {}
    for line in proc.stdout.splitlines():
        fields = line.split("\t")
        if len(fields) != 3 or not fields[0].startswith("R"):
            continue
        if is_scanned(fields[1]) and is_scanned(fields[2]):
            moved[fields[1]] = fields[2]
    return moved


def tracked_files(root: Path) -> list[str]:
    """Every scanned path, each named once.

    `git ls-files --cached` prints a conflicted path once per stage, so a
    merge resolved on disk but not yet `git add`ed lists it three times and
    every count over it triples. `docs/adr/README.md [issue-reference]: 11
    allowed, 33 found` is what that reads like, on a resolution holding one
    copy of everything -- a fictional number, against a ratchet whose whole
    job is to be believed.

    Deduplicating rather than refusing to run: the file this reads is the
    working-tree copy, which during a resolution is the resolved text, so one
    entry per path gives the true count. Refusing would take the guard away
    at the moment its answer is wanted -- a conflict in prose is resolved by
    reading prose -- and the count ratchet is whole-tree, so there is no
    base-relative allowance an unmerged index could distort.
    """
    out = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split("\n")
    seen: dict[str, None] = {}
    for path in out:
        if path and is_scanned(path):
            seen[path] = None
    return list(seen)


def git(root: Path, args: list[str]) -> str:
    """`git <args>` from `root`, or "" on any failure. Fails closed: a
    broken or absent git must never grant base-relative leniency."""
    try:
        proc = subprocess.run(
            ["git", *args],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return ""
    return proc.stdout.strip() if proc.returncode == 0 else ""


def resolve_base_commit(root: Path, absolute: bool) -> str:
    """The commit this change is judged against, or "" for none.

    Mirrors `check-file-size.sh`'s `resolve_base_commit`: an explicit
    override (`PROSE_BASE_REF`, for hermetic tests with no `origin/main`),
    a merge commit (a `refs/pull/N/merge` checkout, where `HEAD^1` is the
    base branch tip), a local feature branch's merge-base with
    `origin/main`, or the immediate parent on a linear push. Empty under
    `--absolute` or on any git failure -- both collapse `max(ceiling,
    base)` to the ceiling, the strict whole-tree check.
    """
    if absolute:
        return ""
    override = os.environ.get("PROSE_BASE_REF")
    if override:
        return git(root, ["rev-parse", "--verify", "--quiet", f"{override}^{{commit}}"])
    if git(root, ["rev-parse", "--verify", "--quiet", "HEAD^2"]):
        return git(root, ["rev-parse", "--verify", "--quiet", "HEAD^1^{commit}"])
    mb = git(root, ["merge-base", "HEAD", "origin/main"])
    head = git(root, ["rev-parse", "HEAD"])
    if mb and mb != head:
        return mb
    return git(root, ["rev-parse", "--verify", "--quiet", "HEAD^1^{commit}"])


# Git calls a pair a rename at 50% similarity. A split is held higher than
# that: the text moves verbatim, and what a split rewrites in transit is the
# parent's own header, the imports, and the links it repoints. A file that is
# a fifth new prose is a new file wearing a move's clothes.
SPLIT_SHARE = 0.8
# Below this many prose lines a share is noise -- a short new file can match a
# long one by accident on a handful of ordinary sentences.
SPLIT_FLOOR = 20


def split_sources(root: Path, commit: str) -> dict[str, str]:
    """New path -> the file it was split out of, for every file this change adds.

    A rename is a move git can name: the old path is gone, so `git diff -M`
    pairs the delete with the add and [`renamed_paths`] reads the pair off.
    A **split** leaves the old path standing -- `envelope.rs` beside a new
    `envelope/inbound.rs` -- so git reports a modify beside an add and pairs
    nothing. The sentences in the new file are the sentences that were in the
    old one, and every ratchet keyed by path reads them as newly written. That
    charged a split the reading grade of a file nobody had opened, which is
    the one move `mod` hierarchy is for (AGENTS.md, "God files").

    A new file is judged split out of `old` when at least `SPLIT_SHARE` of the
    prose lines it holds sat in `old` at `commit` and have since left `old`.
    Requiring the lines to be **gone** from the source is what stops a copy
    claiming one allowance twice. Both files must be ones the guard reads, for
    the reason [`renamed_paths`] gives.

    Its honest limit: two new files can each claim the same source, so a split
    that duplicates a line between them is charged for it once rather than
    twice. Prose *written* during the split is still new -- it is in neither
    line set, so it lowers the share and, past the threshold, carries nothing.

    Fails open at every unknown, like [`renamed_paths`]: an empty map allows
    nothing that was not allowed before it could ask.
    """
    status = git(root, ["diff", "-M", "--name-status", commit])
    if not status:
        return {}
    added: list[str] = []
    modified: list[str] = []
    for line in status.splitlines():
        fields = line.split("\t")
        if len(fields) != 2 or not is_scanned(fields[1]):
            continue
        if fields[0] == "A":
            added.append(fields[1])
        elif fields[0] == "M":
            modified.append(fields[1])
    if not added or not modified:
        return {}

    def prose_set(path: str, text: str) -> set[str]:
        return {
            line.strip()
            for line in prose_lines(path, prose_only(text))
            if line.strip()
        }

    def working_set(path: str) -> set[str]:
        try:
            return prose_set(path, (root / path).read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError):
            return set()

    # The added files are read first so a change that adds no prose-bearing
    # file pays for no `git show` at all -- which is most of them.
    candidates = {path: working_set(path) for path in added}
    candidates = {
        path: lines for path, lines in candidates.items() if len(lines) >= SPLIT_FLOOR
    }
    if not candidates:
        return {}

    shed: dict[str, set[str]] = {}
    for path in modified:
        was = prose_set(path, git(root, ["show", f"{commit}:{path}"]))
        if not was:
            continue
        gone = was - working_set(path)
        if gone:
            shed[path] = gone

    out: dict[str, str] = {}
    for path, lines in candidates.items():
        best, share = "", 0.0
        for source, gone in shed.items():
            hit = len(lines & gone) / len(lines)
            if hit > share:
                best, share = source, hit
        if share >= SPLIT_SHARE:
            out[path] = best
    return out
