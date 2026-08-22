#!/usr/bin/env python3
"""Guard: no content-free prose in this repository's text.

Bans a short list of constructions that announce writing instead of doing it.
The canonical specimen, and the one that named this guard:

    Two things stated rather than hidden: ...

Strip that clause and nothing is lost. The sentence after it still says
whatever it said. That is the test every pattern here has to pass -- a
construction earns a ban only when deleting it costs the reader nothing.

Scope is every tracked text file: prose, Rust doc comments, Python
docstrings, shell headers. A comment is text a human reads, so it is held to
the same bar as a `.md` file. Fenced blocks and backticked spans are skipped
-- a document that names a banned construction in order to ban it is citing
it, and has to be able to spell it.

The baseline (`scripts/prose-baseline.txt`) is a **down-only ratchet**, the
same shape as `scripts/typed-errors-baseline.txt`: a file may shrink its
count, never grow it, and a file absent from the baseline must be at zero.
`--update` refuses to raise a count and refuses to add a file, so the only
way past a failure is to delete the prose.

The ratchet is legitimate here for the one reason a ratchet ever is: the rule
predates the guard. The baseline records debt that already existed; it grants
no new permission.

Usage:

    ./scripts/check-prose.py [--update] [--bootstrap] [--report] [ROOT]

    --update     rewrite the baseline downward only (`make prose-update`)
    --bootstrap  create the baseline from the current tree; one-time, and
                 refuses to run when the baseline already exists
    --report     print every offending line, grouped by file; changes nothing
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
# A pattern list cannot scan the file that defines it without matching itself.
EXCLUDED_PREFIXES = (
    "scripts/check-prose.py",
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
        re.compile(r"\b(?:Two|Three|Four|Five|Six|Both)\s+(?:things?|halves|reasons?|consequences?)\b(?!\s+of\b)"),
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
]

HEADER = """\
# Down-only ratchet for the no-content-free-prose rule (CLAUDE.md, "Prose is
# code you cannot compile").
#
# Each line is `<path> <count>` -- the number of banned constructions still in
# that file. A file may shrink its count; it may never grow it, and a file
# absent from this list must be at zero. Regenerate with `make prose-update`,
# which refuses to raise a number or add a file.
#
# This file is meant to reach empty. Do not add a line here to turn the gate
# green -- delete the prose instead. See ./scripts/check-prose.py --report for
# the exact lines.
"""


def tracked_files(root: Path) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files"], cwd=root, capture_output=True, text=True, check=True
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


def counts(root: Path) -> tuple[dict[str, int], dict[str, list]]:
    per_file: dict[str, int] = {}
    detail: dict[str, list] = {}
    for path in tracked_files(root):
        hits = scan(root, path)
        if hits:
            per_file[path] = len(hits)
            detail[path] = hits
    return per_file, detail


def read_baseline(path: Path) -> dict[str, int]:
    if not path.exists():
        return {}
    baseline: dict[str, int] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        name, _, num = line.rpartition(" ")
        baseline[name] = int(num)
    return baseline


def write_baseline(path: Path, data: dict[str, int]) -> None:
    body = "".join(f"{p} {n}\n" for p, n in sorted(data.items()) if n > 0)
    path.write_text(HEADER + body, encoding="utf-8")


def main() -> int:
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = {a for a in sys.argv[1:] if a.startswith("--")}
    root = Path(argv[0]) if argv else Path(__file__).resolve().parent.parent
    root = root.resolve()

    per_file, detail = counts(root)
    baseline_path = root / BASELINE

    if "--report" in flags:
        total = sum(per_file.values())
        by_pattern: Counter[str] = Counter()
        for hits in detail.values():
            for _, name, _, _ in hits:
                by_pattern[name] += 1
        for path in sorted(detail):
            print(f"\n{path}")
            for lineno, name, text, remedy in detail[path]:
                print(f"  {lineno:>5}  [{name}] {text!r} -- {remedy}")
        print(f"\n{total} construction(s) in {len(per_file)} file(s)")
        for name, n in by_pattern.most_common():
            print(f"  {n:>5}  {name}")
        return 0

    if "--bootstrap" in flags:
        if baseline_path.exists():
            print(
                "check-prose: refusing to bootstrap -- "
                f"{BASELINE} already exists. Use --update, which only ever "
                "lowers a count.",
                file=sys.stderr,
            )
            return 1
        write_baseline(baseline_path, per_file)
        print(
            f"check-prose: wrote {BASELINE} with "
            f"{sum(per_file.values())} construction(s) in {len(per_file)} file(s)."
        )
        return 0

    baseline = read_baseline(baseline_path)

    if "--update" in flags:
        # A file absent from the baseline is held to zero, so `.get(path, 0)`
        # is what makes --update refuse to grandfather a first-time offender.
        merged = {p: min(n, baseline.get(p, 0)) for p, n in per_file.items()}
        raised = {
            p: (baseline.get(p, 0), n) for p, n in per_file.items() if n > baseline.get(p, 0)
        }
        if raised:
            print(
                "check-prose: refusing to update -- these files gained prose:",
                file=sys.stderr,
            )
            for p, (was, now) in sorted(raised.items()):
                print(f"  {p}: {was} -> {now}", file=sys.stderr)
            print(
                "\nDelete the constructions instead. "
                "`./scripts/check-prose.py --report` names every line.",
                file=sys.stderr,
            )
            return 1
        # Files that reached zero drop out entirely; the ratchet retightens.
        for p in baseline:
            if p not in per_file:
                merged.pop(p, None)
        write_baseline(baseline_path, merged)
        print(f"check-prose: {BASELINE} retightened to {sum(merged.values())}.")
        return 0

    failures = []
    for path in sorted(set(per_file) | set(baseline)):
        now = per_file.get(path, 0)
        allowed = baseline.get(path, 0)
        if now > allowed:
            failures.append((path, allowed, now))

    if failures:
        print("check-prose: FAIL -- content-free prose added.\n", file=sys.stderr)
        for path, allowed, now in failures:
            print(f"  {path}: {allowed} allowed, {now} found", file=sys.stderr)
            for lineno, name, text, remedy in detail.get(path, []):
                print(f"      {lineno}: [{name}] {text!r} -- {remedy}", file=sys.stderr)
        print(
            "\nThese constructions announce writing instead of doing it; "
            "deleting one costs the reader nothing.\n"
            "Fix the prose. Do not add a baseline entry.",
            file=sys.stderr,
        )
        return 1

    total = sum(per_file.values())
    print(
        f"check-prose: OK -- {total} grandfathered construction(s) "
        f"in {len(per_file)} file(s), none added."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
