"""The grade ratchet: how hard each file's prose is to read.

`scripts/prose-grade-baseline.txt` records a Flesch-Kincaid reading grade per
file, and a file may lower its grade and never raise it. The rule it holds is
docs/prose-guidelines.md's hard rule 5, a 5th grade reading level. It scores
the prose `scan.prose_only` and `scan.prose_lines` isolate, so code and
identifiers are never read as words.
"""

from __future__ import annotations

import re
from pathlib import Path

from .git_pairing import git
from .scan import prose_lines, prose_only


GRADE_BASELINE = "scripts/prose-grade-baseline.txt"

# The ceiling a file with no baseline entry is held to, in hundredths of a
# grade. The rule (docs/prose-guidelines.md, hard rule 5) asks for a 5th
# grade reading level; 6.00 is the gate so a sentence at the target has room
# to breathe. A file already above it keeps its recorded grade as the
# ceiling and may only come down.
NEW_FILE_GRADE = 600

# Files with less prose than this many words are not scored. A grade needs
# enough sentences to mean something; a two-line comment does not.
GRADE_WORD_FLOOR = 100

GRADE_HEADER = """\
# Down-only ratchet on reading grade, per file (docs/prose-guidelines.md,
# hard rule 5: write at a 5th grade level).
#
# Each line is `<path> <grade>` -- the file's Flesch-Kincaid reading grade,
# in hundredths. A file may lower its grade; it may never raise it, and a
# file absent from this list is held to 6.00. Files with under 100 words of
# prose are not scored. Regenerate with `make prose-update`, which refuses
# to raise a number.
#
# This file is meant to reach empty. Do not add a line here to turn the
# gate green -- rewrite the sentences instead. `make prose-report` names
# the worst ones.
"""


# A comment marker is not a word: the leading `//!`, `///`, `#`, `*` and
# list markers are stripped before sentences are counted.
GRADE_MARKER = re.compile(r"^\s*(?://[!/]{0,2}|#{1,6}|\*+|<!--|-->|-\s|\d+\.\s)\s*")
GRADE_WORD = re.compile(r"\b[A-Za-z][a-zA-Z']*\b")


def _syllables(word: str) -> int:
    """A close-enough syllable count: vowel groups, minus a silent final e."""
    w = word.lower()
    groups = re.findall(r"[aeiouy]+", w)
    n = len(groups)
    if n > 1 and w.endswith("e") and not w.endswith(("le", "ee", "ye")):
        n -= 1
    return max(n, 1)


def _sentence_grade(words: list[str]) -> float:
    syllables = sum(_syllables(w) for w in words)
    return 0.39 * len(words) + 11.8 * (syllables / len(words)) - 15.59


def reading_grade(lines: list[str]) -> tuple[int, list[tuple[float, str]]] | None:
    """A file's Flesch-Kincaid grade in hundredths, with its worst sentences.

    Scores the prose the pattern scan already isolates: fenced blocks and
    backticked spans are blanked before this runs, so code and identifiers
    do not count as words. Words that look like CamelCase names are skipped
    too. Returns None when the file holds too little prose to score.
    """
    text = " ".join(GRADE_MARKER.sub("", line) for line in lines)
    sentences: list[tuple[str, list[str]]] = []
    for raw in re.split(r"(?<=[.!?])\s+", text):
        words = [
            w for w in GRADE_WORD.findall(raw)
            if not any(c.isupper() for c in w[1:])
        ]
        if len(words) >= 3:
            sentences.append((" ".join(raw.split()), words))
    total_words = sum(len(words) for _, words in sentences)
    if not sentences or total_words < GRADE_WORD_FLOOR:
        return None
    total_syllables = sum(
        _syllables(w) for _, words in sentences for w in words
    )
    grade = (
        0.39 * (total_words / len(sentences))
        + 11.8 * (total_syllables / total_words)
        - 15.59
    )
    worst = sorted(
        ((_sentence_grade(words), text) for text, words in sentences),
        key=lambda pair: -pair[0],
    )[:3]
    return max(round(grade * 100), 0), worst


def grades(root: Path, paths: list[str]) -> dict[str, tuple[int, list]]:
    """Per-file reading grade for every file with enough prose to score."""
    out: dict[str, tuple[int, list]] = {}
    for path in paths:
        try:
            text = (root / path).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        scored = reading_grade(prose_lines(path, prose_only(text)))
        if scored is not None:
            out[path] = scored
    return out


def read_grade_baseline(path: Path) -> dict[str, int]:
    baseline: dict[str, int] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        fields = line.split()
        if len(fields) != 2:
            raise SystemExit(
                f"check-prose: {path} line {line!r} is not `<path> <grade>`. "
                "Regenerate it with `make prose-update`."
            )
        baseline[fields[0]] = int(round(float(fields[1]) * 100))
    return baseline


def write_grade_baseline(path: Path, data: dict[str, int]) -> None:
    body = "".join(
        f"{file_path} {n / 100:.2f}\n"
        for file_path, n in sorted(data.items())
        if n > NEW_FILE_GRADE
    )
    path.write_text(GRADE_HEADER + body, encoding="utf-8")


def grade_at_commit(root: Path, commit: str, path: str) -> int | None:
    """One file's reading grade at `commit`, or None when it cannot be scored
    there -- the file is absent, or holds too little prose."""
    text = git(root, ["show", f"{commit}:{path}"])
    if not text:
        return None
    scored = reading_grade(prose_lines(path, prose_only(text)))
    return None if scored is None else scored[0]
