#!/usr/bin/env python3
"""Guard: prose cites code by name, never by line number (#4392).

A citation like `src/fleet.rs:463` is wrong the moment anyone inserts a line
above 463, and nothing reports it: the path still resolves, the number still
looks plausible, and a reader lands somewhere unrelated with no reason to
doubt it. `crates/stella-fleet/README.md` carried six at once -- `dispatch`
cited at line 463 when it started at 607, `start_attempt` at 178 against 241,
`finish_attempt` at 197 against 260, `watch_ci` at 606 against 658, `decide`
at 428 against 471, `migrate` at 376 against 514. Every one of them read as
precise.

Verification is not available here. A drifted number usually still points
inside the file, so no check can tell a correct citation from a stale one --
which is why the rule is to write the name instead. `fleet.rs`'s `dispatch`
survives every edit above it and is what a reader searches for anyway.

Scope is prose: markdown, and comment lines in source. A line number inside
running code is data (a parser fixture, a test's expected output), not a
citation, so the scan never looks there. Test files, fixtures and snapshots
are skipped whole.

The baseline (`scripts/line-citations-baseline.txt`) is a **down-only
ratchet**, the same shape as `scripts/prose-baseline.txt`: a file may shrink
its count, never grow it, and a file absent from the baseline must be at zero.
`--update` refuses to raise a count and refuses to add a file, so the only way
past a failure is to write the name. It is meant to reach empty.

Usage:

    ./scripts/check-line-citations.py [--update] [--report] [ROOT]

    --update   rewrite the baseline downward only (`make line-citations-update`)
    --report   print every citation, grouped by file; changes nothing
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

BASELINE = "scripts/line-citations-baseline.txt"

SCANNED = (".md", ".mdx", ".rs", ".py", ".sh", ".ts", ".tsx")

# Generated, vendored, or this guard's own subject matter: a guard that bans a
# form has to be able to spell it, and its self-test has to feed it one.
EXCLUDED_PREFIXES = (
    "scripts/check-line-citations.py",
    "scripts/test-line-citations.sh",
    "scripts/line-citations-baseline.txt",
    "docs/wire/",
    "CHANGELOG.md",
)
EXCLUDED_SUBSTRINGS = ("/tests/", "/fixtures/", "/snapshots/", "/testdata/")
EXCLUDED_SUFFIXES = ("tests.rs", "_test.py", "_test.ts", ".test.ts")

# `path.ext:123`. The extension list is what keeps a bare `foo:12` and a URL
# port out of it.
CITATION = re.compile(r"\b[A-Za-z0-9_./-]+\.(?:rs|py|sh|ts|tsx|mjs)\b:\d+\b")

# What counts as prose in a source file. Markdown is prose throughout.
COMMENT = re.compile(r"^\s*(?:///|//!|//|#|\*|--)")

HEADER = """\
# Down-only ratchet for the cite-by-name rule (docs/prose-guidelines.md, "A
# number that will be wrong next month").
#
# Each line is `<path> <count>` -- how many line-pinned citations that file
# still has. A file may shrink its count; it may never grow it, and a file
# absent from this list must be at zero. Regenerate with
# `make line-citations-update`, which refuses to raise a number or add a file.
#
# This file is meant to reach empty. Do not add a line here to turn the gate
# green -- cite the symbol instead of the line.
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
        if path.endswith(EXCLUDED_SUFFIXES):
            continue
        if any(s in path for s in EXCLUDED_SUBSTRINGS):
            continue
        keep.append(path)
    return keep


def scan(root: Path, path: str) -> list[tuple[int, str]]:
    """Return (lineno, citation) for one file."""
    try:
        text = (root / path).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return []
    prose_throughout = path.endswith((".md", ".mdx"))
    hits: list[tuple[int, str]] = []
    for lineno, line in enumerate(text.split("\n"), start=1):
        if not prose_throughout and not COMMENT.match(line):
            continue
        for m in CITATION.finditer(line):
            hits.append((lineno, m.group(0)))
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
    root = (Path(argv[0]) if argv else Path(__file__).resolve().parent.parent).resolve()

    per_file, detail = counts(root)
    baseline_path = root / BASELINE

    if "--report" in flags:
        for path in sorted(detail):
            print(f"\n{path}")
            for lineno, text in detail[path]:
                print(f"  {lineno:>5}  {text}")
        print(
            f"\n{sum(per_file.values())} line-pinned citation(s) "
            f"in {len(per_file)} file(s). Cite the symbol instead."
        )
        return 0

    baseline = read_baseline(baseline_path)

    if "--update" in flags:
        # A file absent from the baseline is held to zero, so `.get(path, 0)`
        # is what makes --update refuse to grandfather a first-time offender.
        merged = {p: min(n, baseline.get(p, 0)) for p, n in per_file.items()}
        raised = {
            p: (baseline.get(p, 0), n)
            for p, n in per_file.items()
            if n > baseline.get(p, 0)
        }
        if raised:
            print(
                "check-line-citations: refusing to update -- these files "
                "gained line-pinned citations:",
                file=sys.stderr,
            )
            for p, (was, now) in sorted(raised.items()):
                print(f"  {p}: {was} -> {now}", file=sys.stderr)
            print(
                "\nCite the symbol instead. "
                "`./scripts/check-line-citations.py --report` names every one.",
                file=sys.stderr,
            )
            return 1
        for p in baseline:
            if p not in per_file:
                merged.pop(p, None)
        write_baseline(baseline_path, merged)
        print(
            f"check-line-citations: {BASELINE} retightened to {sum(merged.values())}."
        )
        return 0

    failures = []
    for path in sorted(set(per_file) | set(baseline)):
        now = per_file.get(path, 0)
        allowed = baseline.get(path, 0)
        if now > allowed:
            failures.append((path, allowed, now))

    if failures:
        print(
            "check-line-citations: FAIL -- line-pinned citation(s) added.\n",
            file=sys.stderr,
        )
        for path, allowed, now in failures:
            print(f"  {path}: {allowed} allowed, {now} found", file=sys.stderr)
            for lineno, text in detail.get(path, []):
                print(f"      {lineno}: {text}", file=sys.stderr)
        print(
            "\nA line number is wrong after the next edit above it, and nothing "
            "reports it.\nCite the symbol: `fleet.rs`'s `dispatch`, not "
            "`fleet.rs:463`. Do not add a baseline entry.",
            file=sys.stderr,
        )
        return 1

    total = sum(per_file.values())
    print(
        f"check-line-citations: OK -- {total} grandfathered citation(s) "
        f"in {len(per_file)} file(s), none added."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
