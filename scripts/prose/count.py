"""The count ratchet: how many of each banned construction a file may hold.

The baseline (`scripts/prose-baseline.txt`) is a **down-only ratchet**, the
same shape as `scripts/typed-errors-baseline.txt`, kept per (file, pattern):
a pair may shrink its count, never grow it, and a pair absent from the
baseline must be at zero. `--update` refuses to raise a count and refuses to
add a pair, so the only way past a failure is to delete the prose. Per
pattern rather than per file so a file cannot pay for new prose of one kind
by deleting prose of another.
"""

from __future__ import annotations

from pathlib import Path

from .git_pairing import git
from .scan import scan, scan_text


BASELINE = "scripts/prose-baseline.txt"


HEADER = """\
# Down-only ratchet for the no-content-free-prose rule (CLAUDE.md, "Prose is
# code you cannot compile"; the full rules are docs/prose-guidelines.md).
#
# Each line is `<path> <pattern> <count>` -- how many of that one banned
# construction are still in that file. A (file, pattern) pair may shrink its
# count; it may never grow it, and a pair absent from this list must be at
# zero. Regenerate with `make prose-update`, which refuses to raise a number or
# add a pair.
#
# Per pattern rather than per file so a file cannot pay for new prose of one
# kind by deleting prose of another -- the two are unrelated edits and a single
# total lets them net out.
#
# This file is meant to reach empty. Do not add a line here to turn the gate
# green -- delete the prose instead. See ./scripts/check-prose.py --report for
# the exact lines.
"""


def counts(
    root: Path, paths: list[str]
) -> tuple[dict[tuple[str, str], int], dict[str, list]]:
    """Per-(file, pattern) counts over `paths`, and every hit grouped by file."""
    per_pair: dict[tuple[str, str], int] = {}
    detail: dict[str, list] = {}
    for path in paths:
        hits = scan(root, path)
        if hits:
            detail[path] = hits
            for _, name, _, _ in hits:
                per_pair[(path, name)] = per_pair.get((path, name), 0) + 1
    return per_pair, detail


def counts_at_commit(root: Path, commit: str, path: str) -> dict[str, int]:
    """Per-pattern hit counts for one file, read from `commit` by `git show`.

    An empty map when the file did not exist there, which is what holds a
    first-time offender to zero: a file the base tree never had cannot
    inherit anything.
    """
    text = git(root, ["show", f"{commit}:{path}"])
    if not text:
        return {}
    per_pattern: dict[str, int] = {}
    for _, name, _, _ in scan_text(text):
        per_pattern[name] = per_pattern.get(name, 0) + 1
    return per_pattern


def read_baseline(path: Path) -> dict[tuple[str, str], int]:
    if not path.exists():
        return {}
    baseline: dict[tuple[str, str], int] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        fields = line.split()
        if len(fields) != 3:
            raise SystemExit(
                f"check-prose: {path} line {line!r} is not "
                "`<path> <pattern> <count>`. Regenerate it with "
                "`make prose-update`."
            )
        file_path, pattern, num = fields
        baseline[(file_path, pattern)] = int(num)
    return baseline


def write_baseline(path: Path, data: dict[tuple[str, str], int]) -> None:
    body = "".join(
        f"{file_path} {pattern} {n}\n"
        for (file_path, pattern), n in sorted(data.items())
        if n > 0
    )
    path.write_text(HEADER + body, encoding="utf-8")
