#!/usr/bin/env python3
"""Directions `scripts/check-priority-scheme.py` must fail in.

Each case builds a throwaway tree under `--manifest-dir` and runs the real
guard against it as a subprocess, the posture
`scripts/test-guard-trigger-coverage.py` uses: nothing here reads or writes
this repository. Not part of `make gate`; run it with
`make priority-scheme-test`.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
GUARD = HERE / "check-priority-scheme.py"

SCR = "docs/scr/SCR-005-triage-separation-of-duties.md"
WORKFLOW = ".github/workflows/triage-guard.yml"

pass_count = 0
fail_count = 0


def scr_body(scheme: str) -> str:
    return (
        "---\nid: scr/005-triage-separation-of-duties\n---\n\n"
        "## Directive\n\n"
        "Issue creators may apply exactly one label.\n\n"
        f"{scheme}\n"
    )


FOUR_LEVELS = (
    "Priority scheme: `P0` drop everything - `P1` this cycle - `P2` next\n"
    "cycle - `P3` backlog. Rule: every open issue carries one of them or\n"
    "`triage`."
)

FIVE_LEVELS = (
    "Priority scheme: `P0` drop everything - `P1` this cycle - `P2` next\n"
    "cycle - `P3` backlog - `P4` someday. Rule: every open issue carries one\n"
    "of them or `triage`."
)


def workflow_body(ceiling: int | None) -> str:
    pattern = "/^P$/" if ceiling is None else f"/^P[0-{ceiling}]$/"
    return (
        "name: triage-guard\n"
        "on:\n  issues:\n    types: [opened, labeled]\n"
        "jobs:\n  guard:\n    runs-on: ubuntu-latest\n    steps:\n"
        "      - uses: actions/github-script@v9\n"
        "        with:\n"
        "          script: |\n"
        f"            const P = {pattern};\n"
    )


# The two shapes the guard rejects, built rather than typed. Typed out, each
# would be a second copy of the scheme, and the guard would report this file.
# It would be right to.
def span_line(top: int) -> str:
    return f"Priority runs `P0`-`P{top}`.\n"


def class_line(top: int) -> str:
    return f"const p = /^P[0-{top}]$/;\n"


def fixture(name: str, files: dict[str, str]) -> Path:
    root = Path(tempfile.mkdtemp(prefix=f"priority-scheme-{name}-"))
    for rel, body in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")
    return root


def tree(scheme: str, ceiling: int | None, extra: dict[str, str] | None = None) -> dict:
    files = {SCR: scr_body(scheme), WORKFLOW: workflow_body(ceiling)}
    files.update(extra or {})
    return files


def expect(name: str, files: dict[str, str], want_rc: int, needle: str = "") -> None:
    global pass_count, fail_count
    root = fixture(name, files)
    proc = subprocess.run(
        [sys.executable, str(GUARD), "--manifest-dir", str(root)],
        capture_output=True,
        text=True,
    )
    out = proc.stdout + proc.stderr
    if proc.returncode != want_rc:
        print(f"FAIL  {name} -- exit {proc.returncode}, wanted {want_rc}")
        print(f"      {out.strip()}")
        fail_count += 1
        return
    if needle and needle not in out:
        print(f"FAIL  {name} -- output never says {needle!r}")
        print(f"      {out.strip()}")
        fail_count += 1
        return
    print(f"ok    {name}")
    pass_count += 1


# C1 — the shape that must pass, or every case below proves nothing.
expect("C1 a tree whose regex matches the scheme", tree(FIVE_LEVELS, 4), 0)

# C2 — the defect this guard was written for: the label set and the SCR grow a
# level and the regex does not.
expect(
    "C2 a regex one level short fails",
    tree(FIVE_LEVELS, 3),
    1,
    "triage-guard.yml matches up to `P3`",
)

# C3 — the other way round. A regex may not match a level nobody declared: it
# would strip a label that is not a priority.
expect("C3 a regex one level long fails", tree(FOUR_LEVELS, 4), 1)

# C4 — a second statement of the scheme is what drifts, whoever writes it.
expect(
    "C4 a second file stating the scheme fails",
    tree(FIVE_LEVELS, 4, {"HANDBOOK.md": span_line(4)}),
    1,
    "HANDBOOK.md:1 states the priority scheme",
)

# C5 — the same, spelled as a regex rather than as prose.
expect(
    "C5 a second character class fails",
    tree(FIVE_LEVELS, 4, {"tools/rank.js": class_line(4)}),
    1,
    "tools/rank.js:1 states the priority scheme",
)

# C6 — naming two levels as examples is a use of the scheme, not a copy of it.
# A guard that cannot tell them apart gets switched off.
expect(
    "C6 a sentence naming two levels passes",
    tree(FIVE_LEVELS, 4, {"docs/queue.md": "Choosing `P0` and `P2` takes\n"}),
    0,
)

# C7 — with no declaration there is nothing to hold the regex to, and silence
# must not read as agreement.
expect(
    "C7 an SCR with no scheme line fails",
    tree("The triage agent sizes the work.", 4),
    1,
    "no `Priority scheme:` line",
)

# C8 — a gap in the levels means the guard read something that is not a
# scheme, and its ceiling would be a guess.
expect(
    "C8 levels with a gap fail",
    tree("Priority scheme: `P0` now - `P1` soon - `P3` later.", 3),
    1,
    "do not run from 0 up",
)

# C9 — the two files the guard reads are the two it cannot do without.
expect("C9 a missing SCR fails", {WORKFLOW: workflow_body(4)}, 1, "is missing")
expect("C10 a missing workflow fails", {SCR: scr_body(FIVE_LEVELS)}, 1, "is missing")
expect(
    "C11 a workflow with no pattern fails",
    tree(FIVE_LEVELS, None),
    1,
    "has no `P[0-N]` pattern",
)

print(f"\n{pass_count} passed, {fail_count} failed")
sys.exit(1 if fail_count else 0)
