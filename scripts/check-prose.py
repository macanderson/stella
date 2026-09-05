#!/usr/bin/env python3
"""Guard: no content-free prose or insider vocabulary in this repository's text.

Bans two kinds of prose. The first is constructions that announce writing
instead of doing it. The canonical specimen, and the one that named this
guard:

    Two things stated rather than hidden: ...

Strip that clause and nothing is lost. The sentence after it still says
whatever it said. That is the test every pattern here has to pass -- a
construction earns a ban only when deleting it costs the reader nothing.

The second is words a reader outside this repository cannot parse, or filler
that adds nothing: declarations of honesty ("the honest claim" -- honesty is
assumed; state the fact), interface jargon ("TUI", "REPL", "Command Deck" --
say interactive mode or non-interactive mode), and Rust-internal terms in
prose ("variant" -- say option; "invariant" -- say rule). The bar for every
sentence a human reads here: an 8th grader can follow it.

Scope is every tracked text file: prose, Rust doc comments, Python
docstrings, shell headers. A comment is text a human reads, so it is held to
the same bar as a `.md` file. Fenced blocks and backticked spans are skipped
-- a document that names a banned construction in order to ban it is citing
it, and has to be able to spell it.

The baseline (`scripts/prose-baseline.txt`) is a **down-only ratchet**, the
same shape as `scripts/typed-errors-baseline.txt`, kept per (file, pattern):
a pair may shrink its count, never grow it, and a pair absent from the
baseline must be at zero. `--update` refuses to raise a count and refuses to
add a pair, so the only way past a failure is to delete the prose. Per
pattern rather than per file so a file cannot pay for new prose of one kind
by deleting prose of another.

Keyed by path, which means a file that MOVES takes its debt with it or the
same sentences are read as newly written the moment they land somewhere else.
`--update` therefore asks git what was renamed and carries each entry to the
file's new path first (`renamed_paths`). The ratchet is unchanged by this: a
carried entry still goes through the same `min`, so a move can lower a count
and never raise one, and a genuinely new file still starts at zero. Without
it, splitting a module means rewording prose nobody was editing — #5420 hit
this and had to hand-edit the baseline, which is the one thing this file is
supposed to make unnecessary.

Adding a pattern is the one case a count legitimately goes up, and
`--adopt=<name>` is the only door: it records that pattern's pre-existing
hits and refuses to touch any other pattern's numbers, or to run twice for
one pattern.

The ratchet is legitimate here for the one reason a ratchet ever is: the rule
predates the guard. The baseline records debt that already existed; it grants
no new permission.

A second ratchet, `scripts/prose-density-baseline.txt`, asks the other
question: not whether a sentence is content-free, but whether there are too
many of them. It records the mean length of every crate's leading `//!`
blocks, and a unit may lower that mean and never raise it. A file can be
entirely within the count ratchet and still be three times longer than it
needs to be.

The baseline is a shared cell in the sense AGENTS.md describes for
`Cargo.lock`: two branches each landing one ordinary header are green alone
and compose into a red `main`. Two doors close that, mirroring
`check-file-size.sh`'s own split:

- The plain check judges a unit against `max(its baseline, its mean in the
  base tree)`. Inherited drift is reported as drift and does not fail a
  branch that did not cause it, so landing one ordinary header cannot
  redden `main` on its own. `--absolute` opts out of the base comparison for
  the one caller that must not get this mercy: a post-merge canary is
  exactly asking whether drift already reached `main`, and the base-relative
  reading would forgive the thing it exists to catch.
- `--update` alone leaves every unit's ceiling where it stands, except for a
  unit a file move took a header out of or into: that entry is re-based
  against the same files' lengths in the base tree, because a move changes a
  mean with nobody having written a word, and without that a crate can never
  be split out of another. Only `--update --retighten` lowers every ceiling
  to its current mean, as a deliberate, separately-landed pass. Retightening
  on every `--update` run is what put every unit at exactly its ceiling with
  zero headroom in the first place.

Usage:

    ./scripts/check-prose.py [--update] [--adopt=NAME] [--report] [ROOT]

    --update      lower the count baseline; leave every unit's header-length
                  ceiling where it stands, apart from a unit a file move
                  re-based (`make prose-update`)
    --retighten   with --update, also lower every unit's header-length
                  ceiling to its current mean -- a deliberate, separate pass
                  (`make prose-retighten`)
    --adopt=NAME  record the pre-existing debt of a pattern added to PATTERNS
                  after the baseline was written, and nothing else. Once per
                  pattern: a pattern already in the baseline is refused
                  (`make prose-adopt PATTERN=NAME`)
    --bootstrap   create both baselines from the current tree; one-time, and
                  refuses to run when the count baseline already exists
    --bootstrap-density
                  create the density baseline alone; the same one-time door,
                  for the tree that already had a count baseline when the
                  density ratchet arrived
    --bootstrap-grade
                  create the reading-grade baseline alone; the same one-time
                  door, for the tree that already had the other baselines
                  when the grade ratchet arrived
    --report      print every offending line, grouped by file, then each
                  unit's mean header length; changes nothing
    --absolute    judge every unit's density against its baseline alone,
                  ignoring the base tree. For the post-merge canary, which
                  must catch drift a base-relative check would forgive.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

BASELINE = "scripts/prose-baseline.txt"

# Extensions whose contents a human reads as prose or comments.
SCANNED = (".md", ".mdx", ".rs", ".py", ".sh", ".ts", ".tsx", ".toml")

# Paths that are generated, vendored, or are this guard's own subject matter.
# A pattern list cannot scan the file that defines it without matching itself,
# and the test suite has to spell every banned construction to feed one to the
# guard -- its heredocs are fixtures, not prose anyone reads for meaning.
EXCLUDED_PREFIXES = (
    "scripts/check-prose.py",
    "scripts/test-prose-guard.sh",
    "scripts/prose-baseline.txt",
    "docs/wire/",
)
EXCLUDED_SUBSTRINGS = ("/snapshots/", "/fixtures/")

# Each entry is (name, compiled regex, what to write instead).
#
# Every pattern is deletion-safe by construction: the offending clause can be
# cut without rewriting the sentence around it. That is why the remedy column
# says "delete" more often than it says "rephrase".
PATTERNS: list[tuple[str, re.Pattern[str], str]] = [
    (
        "enumerative-announcement",
        # "Two things follow", "Both halves matter", "Three reasons to know".
        # Announcing a list instead of writing it. Spared when the phrase is a
        # real subject with a real object -- "Both halves of the rev-range are
        # validated" says something; "Both halves matter" does not.
        re.compile(r"\b(?:Two|Three|Four|Five|Six|Both)\s+(?:things?|halves|reasons?|consequences?|problems?|causes?|claims?|facts?|parts?)\b(?!\s+of\b)"),
        "delete the announcement; write the list",
    ),
    (
        "ranking-flourish",
        # Telling the reader which item to care about, instead of ordering the
        # list so the important one comes first.
        re.compile(r"\bthe (?:one|part|half) that (?:matters|counts)\b|\band the (?:second|first) is the\b|\bwhich is the (?:part|half) that\b"),
        "delete it; put the important item first",
    ),
    (
        "meta-writing",
        # Prose about the prose: narrating that a thing is being stated
        # rather than stating it.
        re.compile(r"\b(?:stated|named|said|spelled out|written|declared)\s+(?:out loud\s+)?rather than\b|\bworth (?:naming|saying|stating)\b"),
        "delete it; just state the thing",
    ),
    (
        "empty-epigram",
        # The "X, not Y" tail where Y is a rhetorical foil, not an alternative
        # anyone proposed.
        re.compile(r",\s+not (?:decoration|prose|a slogan|an afterthought|theater|theatre|an accident|a coincidence)\b|\bis a feature, not a bug\b"),
        "delete the tail; the claim stands alone",
    ),
    (
        "tired-metaphor",
        re.compile(r"\b(?:load-bearing|belt and braces|in the same breath)\b"),
        "say what it does: required, checked twice, in the same PR",
    ),
    (
        "honesty-declaration",
        # "the honest number", "measured honestly", "N honest claims about".
        # Honesty is assumed; declaring it is filler. State the fact.
        re.compile(r"\b[Hh]onest(?:ly|y)?\b"),
        "delete it; honesty is assumed -- state the fact",
    ),
    (
        "interface-jargon",
        # The interface has one public name per mode. "TUI", "REPL" and
        # "Command Deck" are insider names; a reader should see "interactive
        # mode" or "non-interactive mode". File and crate names like
        # `stella-tui` stay in backticks, which this guard already exempts.
        re.compile(r"\bTUI\b|\bREPL\b|\bCommand Deck\b|\b[Dd]eck consumers?\b|\b[Hh]eadless\b"),
        "say interactive mode / non-interactive mode",
    ),
    (
        "rust-jargon",
        # Rust terms in prose a non-Rust reader has to decode. An enum
        # "variant" is an option in a list; an "invariant" is a rule.
        re.compile(r"\b[Ii]nvariants?\b|\b[Vv]ariants?\b"),
        "say option (for variant) or rule (for invariant)",
    ),
    (
        "filler-adverb",
        # "deliberately" sounds like it is carrying a reason and never is.
        # Either the reason follows, in which case the word adds nothing to
        # it, or no reason follows, in which case the word stands in for one.
        # #4392's audit found it 2,500 times across 1,100 files, which is what
        # a word means when it means nothing.
        re.compile(r"\b[Dd]eliberate(?:ly)?\b"),
        "delete it; state the reason, or state nothing",
    ),
    (
        "bare-issue-citation",
        # An issue number is not an explanation. A reader outside the tracker
        # cannot follow it, and a reader inside it should not have to in order
        # to understand the line they are looking at. Narrow on purpose: this
        # matches only a sentence whose entire content is the citation, never
        # a number appended to a sentence that says something.
        re.compile(r"(?:^|[.;:!?]\s+)[Ss]ee #\d+\.?\s*$"),
        "say what the issue decided; keep the number beside it",
    ),
    (
        "issue-reference",
        # An issue number in prose sends the reader to a tracker to find out
        # what the sentence means. The sentence must say it instead. Tracking
        # markers (TODO and friends) keep their numbers: a gate requires them
        # there, and they are bookkeeping, not explanation.
        re.compile(r"^(?!.*(?:TODO|FIXME|XXX|HACK|Closes #|Refs #)).*?(#\d{2,})"),
        "say the fact; drop the issue number from the prose",
    ),
    (
        "historical-reference",
        # Prose about what the code did before. The reader has today's code;
        # yesterday's belongs in git history and the tracker, not in comments
        # they must read past.
        re.compile(
            r"\b[Nn]o longer\b|\b[Pp]reviously\b|\b[Hh]istorically\b"
            r"|\b[Ww]as once\b|\b[Bb]ack when\b"
            r"|\b[Tt]he old (?:behaviou?r|way|code|shape|design)\b"
            r"|(?<!is )(?<!be )(?<!are )(?<!was )(?<!were )(?<!een )\b[Uu]sed to\b"
        ),
        "delete the history; describe what the code does now",
    ),
    (
        "complex-word",
        # Words with a plain replacement. Say dependency, rule, unrelated.
        re.compile(r"\b[Cc]oncretions?\b|\b[Rr]eif(?:y|ies|ied|ication)\b|\b[Oo]rthogonal(?:ly|ity)?\b"),
        "use the plain word: dependency, rule, unrelated",
    ),
]

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


def renamed_paths(root: Path, commit: str = "HEAD") -> dict[str, str]:
    """Old path -> new path, for every file git sees as renamed against `commit`.

    The count and grade ratchets consult this from `--update` alone, against
    HEAD: the plain check judges the tree as it stands, so a move fails until
    someone runs `--update` — the same workflow the file-size ratchet has.
    The density ratchet also consults it from the plain check, against the
    base commit, because a unit's mean is arithmetic over a file *set* and a
    move changes that set without anyone writing a word.

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
        if len(fields) == 3 and fields[0].startswith("R"):
            moved[fields[1]] = fields[2]
    return moved


def tracked_files(root: Path) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split("\n")
    keep = []
    for path in out:
        if not path or not path.endswith(SCANNED):
            continue
        if path.startswith(EXCLUDED_PREFIXES):
            continue
        if any(s in path for s in EXCLUDED_SUBSTRINGS):
            continue
        keep.append(path)
    return keep


# A backticked span is a citation, not a use: a document that names a banned
# construction in order to ban it must be able to spell it. A span may wrap
# across lines, so both passes run over the whole file and blank their matches
# to spaces -- newlines survive, which keeps every reported line number the
# one a reader will find in their editor.
CODE_SPAN = re.compile(r"`[^`]*`", re.DOTALL)
FENCE = re.compile(r"^\s*(?:```|~~~)")


def _blank(text: str) -> str:
    return "".join(c if c == "\n" else " " for c in text)


def prose_only(text: str) -> list[str]:
    """The file's lines with fenced blocks and code spans blanked out."""
    lines = text.splitlines()
    in_fence = False
    for i, line in enumerate(lines):
        if FENCE.match(line):
            in_fence = not in_fence
            lines[i] = ""
            continue
        if in_fence:
            lines[i] = ""
    return CODE_SPAN.sub(lambda m: _blank(m.group(0)), "\n".join(lines)).split("\n")


def scan(root: Path, path: str) -> list[tuple[int, str, str, str]]:
    """Return (lineno, pattern name, matched text, remedy) for one file."""
    try:
        text = (root / path).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return []
    hits: list[tuple[int, str, str, str]] = []
    for lineno, line in enumerate(prose_only(text), start=1):
        for name, rx, remedy in PATTERNS:
            for m in rx.finditer(line):
                hits.append((lineno, name, m.group(0), remedy))
    return hits


def counts(root: Path) -> tuple[dict[tuple[str, str], int], dict[str, list]]:
    """Per-(file, pattern) counts, and every hit grouped by file."""
    per_pair: dict[tuple[str, str], int] = {}
    detail: dict[str, list] = {}
    for path in tracked_files(root):
        hits = scan(root, path)
        if hits:
            detail[path] = hits
            for _, name, _, _ in hits:
                per_pair[(path, name)] = per_pair.get((path, name), 0) + 1
    return per_pair, detail


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


def prose_lines(path: str, lines: list[str]) -> list[str]:
    """The lines a reader reads as prose.

    In a code file that means comment lines only -- a shell pipeline or a
    match arm is not a sentence, and counting it as one turns the grade
    into noise. In a markdown file every line is prose.
    """
    if path.endswith((".md", ".mdx")):
        return lines
    marker = "#" if path.endswith((".py", ".sh", ".toml")) else "//"
    kept = []
    for line in lines:
        stripped = line.lstrip()
        if stripped.startswith(marker) and not stripped.startswith("#!"):
            kept.append(stripped)
    return kept


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


def _git(root: Path, args: list[str]) -> str:
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
    """The commit a unit's density is judged against, or "" for none.

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
        return _git(root, ["rev-parse", "--verify", "--quiet", f"{override}^{{commit}}"])
    if _git(root, ["rev-parse", "--verify", "--quiet", "HEAD^2"]):
        return _git(root, ["rev-parse", "--verify", "--quiet", "HEAD^1^{commit}"])
    mb = _git(root, ["merge-base", "HEAD", "origin/main"])
    head = _git(root, ["rev-parse", "HEAD"])
    if mb and mb != head:
        return mb
    return _git(root, ["rev-parse", "--verify", "--quiet", "HEAD^1^{commit}"])


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
        text = _git(root, ["show", f"{commit}:{path}"])
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


def main() -> int:
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = [a for a in sys.argv[1:] if a.startswith("--")]
    flagset = {a.split("=", 1)[0] for a in flags}
    root = Path(argv[0]) if argv else Path(__file__).resolve().parent.parent
    root = root.resolve()

    tracked = tracked_files(root)
    per_pair, detail = counts(root)
    files = {path for path, _ in per_pair}
    baseline_path = root / BASELINE
    density_path = root / DENSITY_BASELINE
    per_unit = density(root, tracked)
    grade_path = root / GRADE_BASELINE
    per_grade = grades(root, tracked)

    if "--report" in flagset:
        total = sum(per_pair.values())
        by_pattern: Counter[str] = Counter()
        for hits in detail.values():
            for _, name, _, _ in hits:
                by_pattern[name] += 1
        for path in sorted(detail):
            print(f"\n{path}")
            for lineno, name, text, remedy in detail[path]:
                print(f"  {lineno:>5}  [{name}] {text!r} -- {remedy}")
        print(f"\n{total} construction(s) in {len(files)} file(s)")
        for name, n in by_pattern.most_common():
            print(f"  {n:>5}  {name}")
        allowed = read_density_baseline(density_path) if density_path.exists() else {}
        print("\nmean module-header length, worst first")
        for unit, mean in sorted(per_unit.items(), key=lambda kv: -kv[1]):
            ceiling = allowed.get(unit, NEW_UNIT_MEAN)
            mark = "  OVER" if mean > ceiling else ""
            print(f"  {mean / 100:>7.2f}  (ceiling {ceiling / 100:.2f})  {unit}{mark}")
        graded = read_grade_baseline(grade_path) if grade_path.exists() else {}
        print("\nreading grade, worst first (top 20)")
        by_grade = sorted(per_grade.items(), key=lambda kv: -kv[1][0])[:20]
        for path, (grade, worst) in by_grade:
            ceiling = graded.get(path, NEW_FILE_GRADE)
            mark = "  OVER" if grade > ceiling else ""
            print(f"  {grade / 100:>7.2f}  (ceiling {ceiling / 100:.2f})  {path}{mark}")
            for sentence_grade, sentence in worst[:1]:
                print(f"           worst: [{sentence_grade:.1f}] {sentence[:110]}")
        return 0

    if "--bootstrap" in flagset:
        if baseline_path.exists():
            print(
                "check-prose: refusing to bootstrap -- "
                f"{BASELINE} already exists. Use --adopt=<pattern> to record "
                "the debt of a newly added pattern, or --update, which only "
                "ever lowers a count.",
                file=sys.stderr,
            )
            return 1
        write_baseline(baseline_path, per_pair)
        write_density_baseline(density_path, per_unit)
        write_grade_baseline(
            grade_path, {path: grade for path, (grade, _) in per_grade.items()}
        )
        print(
            f"check-prose: wrote {BASELINE} with "
            f"{sum(per_pair.values())} construction(s) in {len(files)} file(s), "
            f"{DENSITY_BASELINE} with {len(per_unit)} unit(s), "
            f"and {GRADE_BASELINE}."
        )
        return 0

    # The density ratchet arrived after the count ratchet, so `--bootstrap`
    # (which refuses once scripts/prose-baseline.txt exists) could not
    # introduce it. This is that one-time door, and it closes behind itself
    # for the reason `--bootstrap` does: a regenerated baseline records
    # today's tree as the ceiling.
    if "--bootstrap-density" in flagset:
        if density_path.exists():
            print(
                "check-prose: refusing to bootstrap -- "
                f"{DENSITY_BASELINE} already exists. Use --update, which only "
                "ever lowers a mean.",
                file=sys.stderr,
            )
            return 1
        write_density_baseline(density_path, per_unit)
        print(
            f"check-prose: wrote {DENSITY_BASELINE} with "
            f"{len(per_unit)} unit(s)."
        )
        return 0

    if "--bootstrap-grade" in flagset:
        if grade_path.exists():
            print(
                "check-prose: refusing to bootstrap -- "
                f"{GRADE_BASELINE} already exists. Use --update, which only "
                "ever lowers a grade.",
                file=sys.stderr,
            )
            return 1
        write_grade_baseline(
            grade_path, {path: grade for path, (grade, _) in per_grade.items()}
        )
        over_count = sum(
            1 for grade, _ in per_grade.values() if grade > NEW_FILE_GRADE
        )
        print(
            f"check-prose: wrote {GRADE_BASELINE} with {over_count} file(s) "
            f"above grade {NEW_FILE_GRADE / 100:.0f}. Every one is debt; "
            "take it down."
        )
        return 0

    if not density_path.exists():
        print(
            f"check-prose: {DENSITY_BASELINE} is missing. It records what each "
            "crate's module headers already average, and without it every unit "
            f"is held to {NEW_UNIT_MEAN / 100:.2f}. Restore it from git rather "
            "than regenerating it: a fresh one records today's tree as the "
            "ceiling, which is the grandfathering this ratchet exists to "
            "refuse.",
            file=sys.stderr,
        )
        return 2

    if not grade_path.exists():
        print(
            f"check-prose: {GRADE_BASELINE} is missing. It records each "
            "file's reading grade, and without it every file is held to "
            f"{NEW_FILE_GRADE / 100:.2f}. Restore it from git rather than "
            "regenerating it: a fresh one records today's tree as the "
            "ceiling.",
            file=sys.stderr,
        )
        return 2

    baseline = read_baseline(baseline_path)
    density_baseline = read_density_baseline(density_path)
    grade_baseline = read_grade_baseline(grade_path)

    # `--adopt=<pattern>[,<pattern>]` records the debt of a pattern added to
    # PATTERNS after the baseline was written, and touches no other pattern's
    # numbers. This is the one legitimate way a count in this file goes up, and
    # it is legitimate for the reason --bootstrap was: the prose predates the
    # check, so the entries record debt that already existed rather than
    # granting permission to write more. A pattern that already appears in the
    # baseline has been adopted, and is refused.
    adopt = next((a for a in flags if a.startswith("--adopt")), None)
    if adopt is not None:
        _, _, raw = adopt.partition("=")
        wanted = [name.strip() for name in raw.split(",") if name.strip()]
        known = {name for name, _, _ in PATTERNS}
        if not wanted:
            print(
                "check-prose: --adopt needs a pattern name: "
                f"--adopt={sorted(known)[0]}",
                file=sys.stderr,
            )
            return 2
        unknown = [name for name in wanted if name not in known]
        if unknown:
            print(
                f"check-prose: no such pattern(s): {', '.join(unknown)}. "
                f"Known: {', '.join(sorted(known))}.",
                file=sys.stderr,
            )
            return 2
        already = [
            name for name in wanted if any(p == name for _, p in baseline)
        ]
        if already:
            print(
                "check-prose: refusing to adopt -- these pattern(s) already "
                f"have baseline entries: {', '.join(already)}. Adoption is "
                "once per pattern; use --update, which only lowers a count.",
                file=sys.stderr,
            )
            return 1
        merged = dict(baseline)
        added = 0
        for (path, pattern), n in per_pair.items():
            if pattern in wanted:
                merged[(path, pattern)] = n
                added += n
        write_baseline(baseline_path, merged)
        print(
            f"check-prose: adopted {', '.join(wanted)} -- "
            f"{added} pre-existing construction(s) recorded. "
            "Every one of them is debt; take it down."
        )
        return 0

    if "--update" in flagset:
        # A moved file's debt follows it, so the same sentences in a new home
        # are not read as newly written. Applied before anything else looks at
        # `baseline`, so the carried entry goes through the identical `min`
        # below and a move can still only lower a count.
        moved = renamed_paths(root)
        if moved:
            baseline = {
                (moved.get(path, path), pattern): n
                for (path, pattern), n in baseline.items()
            }
            carried = sorted(
                (old, new)
                for old, new in moved.items()
                if any(path == new for path, _ in baseline)
            )
            for old, new in carried:
                print(f"check-prose: carried {old} -> {new}")
        # A pair absent from the baseline is held to zero, so `.get(pair, 0)`
        # is what makes --update refuse to grandfather a first-time offender.
        merged = {p: min(n, baseline.get(p, 0)) for p, n in per_pair.items()}
        raised = {
            p: (baseline.get(p, 0), n)
            for p, n in per_pair.items()
            if n > baseline.get(p, 0)
        }
        if raised:
            print(
                "check-prose: refusing to update -- these files gained prose:",
                file=sys.stderr,
            )
            for (path, pattern), (was, now) in sorted(raised.items()):
                print(f"  {path} [{pattern}]: {was} -> {now}", file=sys.stderr)
            print(
                "\nDelete the constructions instead. "
                "`./scripts/check-prose.py --report` names every line.\n"
                "If one of these files was MOVED rather than written, stage "
                "the move (`git mv`, or `git add` after `mv`) and re-run: an "
                "unstaged move reads as a delete plus a new file, so its "
                "entry cannot be carried.",
                file=sys.stderr,
            )
            return 1
        # A unit absent from the density baseline is held to NEW_UNIT_MEAN, so
        # `.get(unit, NEW_UNIT_MEAN)` is what stops a new crate grandfathering
        # its own headers the first time anyone runs --update. A unit a move
        # touched is judged against the same files' lengths at HEAD instead --
        # see `base_tracked_paths` for why a set change is not a prose change.
        rebased = {
            unit: density_at_commit(
                root,
                "HEAD",
                unit,
                base_tracked_paths(root, "HEAD", unit, tracked, moved),
            )
            for unit in units_a_move_touched(moved)
            if unit in per_unit
        }

        def unit_ceiling(unit: str) -> int:
            return max(density_baseline.get(unit, NEW_UNIT_MEAN), rebased.get(unit, 0))

        loosened = {
            unit: (unit_ceiling(unit), mean)
            for unit, mean in per_unit.items()
            if mean > unit_ceiling(unit)
        }
        if loosened:
            print(
                "check-prose: refusing to update -- these units' module "
                "headers grew:",
                file=sys.stderr,
            )
            for unit, (was, now) in sorted(loosened.items()):
                print(
                    f"  {unit}: {was / 100:.2f} -> {now / 100:.2f} mean lines",
                    file=sys.stderr,
                )
            print(
                "\nCut the headers instead. "
                "`./scripts/check-prose.py --report` lists every unit's mean.",
                file=sys.stderr,
            )
            return 1
        if moved:
            grade_baseline = {
                moved.get(path, path): n for path, n in grade_baseline.items()
            }
        grade_raised = {
            path: (grade_baseline.get(path, NEW_FILE_GRADE), grade)
            for path, (grade, _) in per_grade.items()
            if grade > grade_baseline.get(path, NEW_FILE_GRADE)
        }
        if grade_raised:
            print(
                "check-prose: refusing to update -- these files' reading "
                "grade rose:",
                file=sys.stderr,
            )
            for path, (was, now) in sorted(grade_raised.items()):
                print(
                    f"  {path}: {was / 100:.2f} -> {now / 100:.2f}",
                    file=sys.stderr,
                )
            print(
                "\nRewrite the sentences instead. "
                "`./scripts/check-prose.py --report` names the worst ones.",
                file=sys.stderr,
            )
            return 1
        merged_grades = {
            path: min(grade, grade_baseline.get(path, NEW_FILE_GRADE))
            for path, (grade, _) in per_grade.items()
        }
        write_grade_baseline(grade_path, merged_grades)
        # Pairs that reached zero drop out entirely; the ratchet retightens.
        for pair in baseline:
            if pair not in per_pair:
                merged.pop(pair, None)
        write_baseline(baseline_path, merged)
        # Retightening every unit to its current mean on every `--update` run
        # is what left each crate sitting at exactly its ceiling with zero
        # headroom: the reclaim is unconditional and global, so clearing one
        # crate's drift silently removes every other crate's slack too. Split
        # the same way `check-file-size.sh --update`/`--retighten` are:
        # `--update` alone touches only the units a move re-based, and
        # `--retighten` is the deliberate, separately-landed pass that reclaims
        # slack across every unit at once.
        if "--retighten" in flagset:
            tightened = {
                unit: min(mean, max(density_baseline.get(unit, NEW_UNIT_MEAN), rebased.get(unit, 0)))
                for unit, mean in per_unit.items()
            }
            write_density_baseline(density_path, tightened)
            density_msg = f"{DENSITY_BASELINE} retightened to {len(tightened)} unit(s)."
        elif rebased:
            # A crate split moves headers between units, and both means change
            # with nobody having written a word. The entry follows the files,
            # exactly as the count and grade entries above do -- capped at the
            # unit's own mean, so a move can never buy a unit more room than
            # the headers it now holds.
            carried = dict(density_baseline)
            for unit in sorted(rebased):
                carried[unit] = min(per_unit[unit], unit_ceiling(unit))
                print(f"check-prose: re-based {unit} to {carried[unit] / 100:.2f} mean lines")
            write_density_baseline(density_path, carried)
            density_msg = f"{DENSITY_BASELINE} re-based {len(rebased)} moved unit(s)."
        else:
            density_msg = f"{DENSITY_BASELINE} left alone -- pass --retighten to reclaim slack."
        print(f"check-prose: {BASELINE} retightened to {sum(merged.values())}, {density_msg}")
        return 0

    # Judged against max(the recorded ceiling, this unit's mean in the base
    # tree) -- the same rule check-file-size.sh uses, and for the same
    # reason: a unit already over its ceiling before this change landed is
    # inherited drift, and failing the branch that merely did not fix it is
    # what turned this ratchet into a main-red generator. `--absolute` (the
    # post-merge canary) skips the base entirely, because that is exactly
    # the drift a canary exists to catch.
    absolute = "--absolute" in flagset
    base_commit = resolve_base_commit(root, absolute)
    base_moves = renamed_paths(root, base_commit) if base_commit else {}
    over = []
    for unit, mean in sorted(per_unit.items()):
        ceiling = density_baseline.get(unit, NEW_UNIT_MEAN)
        if mean <= ceiling:
            continue
        if base_commit:
            base_paths = base_tracked_paths(root, base_commit, unit, tracked, base_moves)
            ceiling = max(ceiling, density_at_commit(root, base_commit, unit, base_paths))
        if mean > ceiling:
            over.append((unit, ceiling, mean))

    failures = []
    for pair in sorted(set(per_pair) | set(baseline)):
        now = per_pair.get(pair, 0)
        allowed = baseline.get(pair, 0)
        if now > allowed:
            failures.append((pair, allowed, now))

    if failures:
        print("check-prose: FAIL -- content-free prose added.\n", file=sys.stderr)
        for (path, pattern), allowed, now in failures:
            print(
                f"  {path} [{pattern}]: {allowed} allowed, {now} found",
                file=sys.stderr,
            )
            for lineno, name, text, remedy in detail.get(path, []):
                if name != pattern:
                    continue
                print(f"      {lineno}: [{name}] {text!r} -- {remedy}", file=sys.stderr)
        print(
            "\nThese constructions announce writing instead of doing it; "
            "deleting one costs the reader nothing.\n"
            "Fix the prose. Do not add a baseline entry. "
            "docs/prose-guidelines.md is the full rule.",
            file=sys.stderr,
        )
        return 1

    grade_failures = []
    for path, (grade, worst) in sorted(per_grade.items()):
        allowed_grade = grade_baseline.get(path, NEW_FILE_GRADE)
        if grade > allowed_grade:
            grade_failures.append((path, allowed_grade, grade, worst))

    if grade_failures:
        print(
            "check-prose: FAIL -- prose got harder to read.\n",
            file=sys.stderr,
        )
        for path, allowed_grade, grade, worst in grade_failures:
            print(
                f"  {path}: grade {allowed_grade / 100:.2f} allowed, "
                f"{grade / 100:.2f} found",
                file=sys.stderr,
            )
            for sentence_grade, sentence in worst:
                print(
                    f"      [{sentence_grade:.1f}] {sentence[:110]}",
                    file=sys.stderr,
                )
        print(
            "\nShorten the sentences and use plainer words. The target is a "
            "5th grade reading level (docs/prose-guidelines.md, hard rule 5). "
            f"Do not add a line to {GRADE_BASELINE}.",
            file=sys.stderr,
        )
        return 1

    if over:
        print(
            "check-prose: FAIL -- module headers got longer.\n",
            file=sys.stderr,
        )
        for unit, ceiling, mean in over:
            print(
                f"  {unit}: {ceiling / 100:.2f} allowed, "
                f"{mean / 100:.2f} mean header lines",
                file=sys.stderr,
            )
        print(
            "\nA header is what this file does and what a reader must not do "
            "to it -- two to four sentences. History belongs in the pull "
            "request, a design document, or the tracker.\n"
            "Cut a header. Do not raise the number in "
            f"{DENSITY_BASELINE}.",
            file=sys.stderr,
        )
        return 1

    total = sum(per_pair.values())
    print(
        f"check-prose: OK -- {total} grandfathered construction(s) "
        f"in {len(files)} file(s), none added; "
        f"{len(per_unit)} unit(s) within their header-length ceiling; "
        f"{len(per_grade)} file(s) within their reading-grade ceiling."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
