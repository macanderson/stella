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

Usage:

    ./scripts/check-prose.py [--update] [--adopt=NAME] [--report] [ROOT]

    --update      rewrite both baselines downward only (`make prose-update`)
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
    --report      print every offending line, grouped by file, then each
                  unit's mean header length; changes nothing
"""

from __future__ import annotations

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
# raise it, and a unit absent from this list is held to 12.00. Regenerate with
# `make prose-update`, which refuses to raise a number.
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
        unit = "/".join(path.split("/")[:2])
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
        print(
            f"check-prose: wrote {BASELINE} with "
            f"{sum(per_pair.values())} construction(s) in {len(files)} file(s), "
            f"and {DENSITY_BASELINE} with {len(per_unit)} unit(s)."
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

    baseline = read_baseline(baseline_path)
    density_baseline = read_density_baseline(density_path)

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
                "`./scripts/check-prose.py --report` names every line.",
                file=sys.stderr,
            )
            return 1
        # A unit absent from the density baseline is held to NEW_UNIT_MEAN, so
        # `.get(unit, NEW_UNIT_MEAN)` is what stops a new crate grandfathering
        # its own headers the first time anyone runs --update.
        loosened = {
            unit: (density_baseline.get(unit, NEW_UNIT_MEAN), mean)
            for unit, mean in per_unit.items()
            if mean > density_baseline.get(unit, NEW_UNIT_MEAN)
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
        # Pairs that reached zero drop out entirely; the ratchet retightens.
        for pair in baseline:
            if pair not in per_pair:
                merged.pop(pair, None)
        write_baseline(baseline_path, merged)
        tightened = {
            unit: min(mean, density_baseline.get(unit, NEW_UNIT_MEAN))
            for unit, mean in per_unit.items()
        }
        write_density_baseline(density_path, tightened)
        print(
            f"check-prose: {BASELINE} retightened to {sum(merged.values())}, "
            f"{DENSITY_BASELINE} to {len(tightened)} unit(s)."
        )
        return 0

    over = [
        (unit, density_baseline.get(unit, NEW_UNIT_MEAN), mean)
        for unit, mean in sorted(per_unit.items())
        if mean > density_baseline.get(unit, NEW_UNIT_MEAN)
    ]

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
        f"{len(per_unit)} unit(s) within their header-length ceiling."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
