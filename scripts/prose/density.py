"""The density ratchet: how long each crate's module headers are on average.

`scripts/prose-density-baseline.txt` asks a different question from the count
ratchet: not whether a sentence is content-free, but whether there are too
many of them. It records the mean length of every crate's leading `//!`
blocks, and a unit may lower that mean and never raise it. A file can be
entirely within the count ratchet and still be three times longer than it
needs to be.
"""

from __future__ import annotations

from pathlib import Path

from .git_pairing import git


DENSITY_BASELINE = "scripts/prose-density-baseline.txt"

# Which units the density ratchet measures. A unit is one crate directory.
DENSITY_UNIT_ROOT = "crates/"

# What a unit with no baseline entry -- a crate added after this file was
# written -- is held to, in hundredths of a line. A two-to-four sentence header
# wraps to four to eight `//!` lines, so 12.00 leaves room for a short table or
# list without leaving room for an essay. The tightest unit in the tree when
# this ratchet was written averaged 18.00, so nothing existing is held to this
# number; it is the target a new crate starts at rather than one it inherits.
NEW_UNIT_MEAN = 1200

DENSITY_HEADER = """\
# Down-only ratchet on module-header density, per crate (#4760).
#
# Each line is `<unit> <mean>` -- the mean length, in lines, of every leading
# `//!` block in that crate's Rust files, recorded in hundredths so the
# comparison is integer arithmetic. A unit may lower its mean; it may never
# raise it, and a unit absent from this list is held to 12.00. Lower one with
# `make prose-retighten`.
#
# `make prose-update` writes here only for a unit a file move touched, and
# only up to that unit's own mean. Extracting a crate moves headers from one
# unit to another with nobody having written a word, and both means change --
# the source unit's because below-average files left it, the new unit's
# because it did not exist. Judging those against the same files' old lengths
# is what keeps this a ratchet on prose rather than a bar on splitting a
# crate.
#
# Mean header length rather than comment share, because share is a bad proxy on
# its own: a well-documented pure-function crate should be comment-heavy, and
# stella-diff scored 74/100 at 26% share. What #4392 measured and #4758 is
# fixing is the forty-line header, which is what this counts.
#
# It answers a different question from scripts/prose-baseline.txt: that one
# asks whether a sentence is content-free, this one asks whether there are too
# many sentences. A file can be entirely within the first and three times
# longer than it needs to be.
"""


def header_length(text: str) -> int:
    """Lines in a Rust file's leading `//!` block, 0 when it has none.

    The licence banner and any blank line above the block are skipped; the
    first line that is neither `//!` nor part of that preamble ends it.
    """
    length = 0
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("//!"):
            length += 1
            continue
        if length:
            break
        if stripped.startswith("//") or not stripped:
            continue
        break
    return length


def density(root: Path, paths: list[str]) -> dict[str, int]:
    """Per-unit mean module-header length, in hundredths of a line."""
    total: dict[str, int] = {}
    files: dict[str, int] = {}
    for path in paths:
        if not path.endswith(".rs") or not path.startswith(DENSITY_UNIT_ROOT):
            continue
        unit = unit_of(path)
        try:
            text = (root / path).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        length = header_length(text)
        if not length:
            continue
        total[unit] = total.get(unit, 0) + length
        files[unit] = files.get(unit, 0) + 1
    return {unit: round(total[unit] * 100 / files[unit]) for unit in total}


def unit_of(path: str) -> str:
    """The density unit a path belongs to -- `crates/<crate>`."""
    return "/".join(path.split("/")[:2])


def units_a_move_touched(moved: dict[str, str]) -> set[str]:
    """Every density unit a rename took a `.rs` file out of or into."""
    units: set[str] = set()
    for old, new in moved.items():
        if old.endswith(".rs"):
            units.add(unit_of(old))
        if new.endswith(".rs"):
            units.add(unit_of(new))
    return units


def base_tracked_paths(
    root: Path, commit: str, unit: str, paths: list[str], moved: dict[str, str]
) -> list[str]:
    """Where each of this unit's current `.rs` files lived at `commit`.

    A file the unit still holds maps to itself, or to its old path when
    `moved` (old -> new) says it was renamed in. A file that did not exist at
    `commit` maps to nothing and drops out, because `git show` cannot read it.

    Restricting the base measurement to the files the unit holds *now* is what
    lets a crate split pass. Extracting a crate moves a set of headers from one
    unit to another; nobody wrote a word, yet both means change -- the source
    unit's because below-average files left it, the new unit's because it did
    not exist and is held to `NEW_UNIT_MEAN`. Reading the same files' old
    lengths answers "did this change grow a header?" instead of "did this
    change move a file?".
    """
    if not commit:
        return []
    came_from = {new: old for old, new in moved.items()}
    return [
        came_from.get(path, path)
        for path in paths
        if path.endswith(".rs") and unit_of(path) == unit
    ]


def density_at_commit(root: Path, commit: str, unit: str, paths: list[str]) -> int:
    """[`density`] for one unit, reading each file's content from `commit`
    via `git show` rather than the working tree. 0 when the unit had no
    headers there (including when it did not exist at all)."""
    total = 0
    files = 0
    for path in paths:
        if not path.endswith(".rs"):
            continue
        text = git(root, ["show", f"{commit}:{path}"])
        length = header_length(text)
        if not length:
            continue
        total += length
        files += 1
    return round(total * 100 / files) if files else 0


def read_density_baseline(path: Path) -> dict[str, int]:
    baseline: dict[str, int] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        fields = line.split()
        if len(fields) != 2:
            raise SystemExit(
                f"check-prose: {path} line {line!r} is not `<unit> <mean>`. "
                "Regenerate it with `make prose-update`."
            )
        baseline[fields[0]] = int(round(float(fields[1]) * 100))
    return baseline


def write_density_baseline(path: Path, data: dict[str, int]) -> None:
    body = "".join(f"{unit} {n / 100:.2f}\n" for unit, n in sorted(data.items()))
    path.write_text(DENSITY_HEADER + body, encoding="utf-8")
