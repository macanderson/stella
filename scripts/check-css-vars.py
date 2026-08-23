#!/usr/bin/env python3
"""Guard: every custom property a token sheet reads is declared in it. #4122.

An unresolvable ``var()`` is not a parse error. The browser drops the
declaration and takes the fallback, so a token sheet can name a stop that was
deleted two releases ago and every page still renders -- and every guard stays
green, because ``check-tokens.py`` reasons about hex literals rather than about
whether a property resolves.

That is how ``--stella-mark-shape: var(--stella-brand-700)`` survived in two
files after v5.0 deleted the eleven-step ``--stella-brand-*`` ramp (#4066).
Nothing looked broken -- every consumer passed a literal fallback -- but the
declaration and the paragraph above it stated an accessibility rule the tree
did not apply. #4296 repaired both declarations; this is the check that was
asked for alongside it, and without which the next deleted stop does the same
thing just as quietly.

**Scope: self-contained token sheets only.** Each file listed below is the
declaration home for the properties it reads, so an undeclared reference in one
is a dangling token by construction. A component stylesheet is a different
question -- its ``var()`` calls resolve against whatever cascade the page
assembles -- and answering it needs a resolution set spanning files plus the
page's own import order. That is deliberately not attempted here: a guard that
guesses at the cascade would produce false failures on the files it is least
able to reason about, and a guard that cries wolf gets deleted rather than
fixed.

Compiles nothing, so it is a ``guards-fast`` step.

    ./scripts/check-css-vars.py [file ...]

Exit 0 when every reference resolves, 1 otherwise, naming each dangling
reference with its file and line.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# The self-contained token sheets. Adding a file here is a claim that it
# declares everything it reads; adding one that does not is how this guard
# starts crying wolf.
SHEETS = (
    "website/src/app/tokens.css",
    "docs/brand/css/tokens.css",
)

COMMENT_RE = re.compile(r"/\*.*?\*/", re.DOTALL)
DECL_RE = re.compile(r"(--[A-Za-z0-9_-]+)\s*:")
REF_RE = re.compile(r"var\(\s*(--[A-Za-z0-9_-]+)")


def blank_comments(text: str) -> str:
    """Blank out comments, keeping every newline so line numbers survive.

    A commented-out declaration must not count as one, and prose inside a
    comment that spells a token name must not count as a reference -- both
    files carry long explanatory comments naming retired stops, which is
    exactly the vocabulary this guard matches on.
    """
    return COMMENT_RE.sub(lambda m: re.sub(r"[^\n]", " ", m.group(0)), text)


def check(path: Path) -> list[str]:
    """Return one line per dangling reference in *path*."""
    try:
        rel: Path | str = path.relative_to(REPO_ROOT)
    except ValueError:
        rel = path

    text = blank_comments(path.read_text(encoding="utf-8"))
    declared = set(DECL_RE.findall(text))

    failures: list[str] = []
    seen: set[str] = set()
    for lineno, line in enumerate(text.splitlines(), start=1):
        for name in REF_RE.findall(line):
            if name in declared or name in seen:
                continue
            seen.add(name)
            failures.append(f"  {rel}:{lineno}  var({name}) -- nothing declares it")
    return failures


def main(argv: list[str]) -> int:
    if len(argv) > 1:
        paths = [Path(a).resolve() for a in argv[1:]]
    else:
        paths = [REPO_ROOT / s for s in SHEETS]

    failures: list[str] = []
    for path in paths:
        if not path.is_file():
            print(f"check-css-vars: no such file: {path}", file=sys.stderr)
            return 1
        failures.extend(check(path))

    if failures:
        print(
            f"check-css-vars: FAIL -- {len(failures)} unresolvable custom "
            "property reference(s):",
            file=sys.stderr,
        )
        for line in failures:
            print(line, file=sys.stderr)
        print(
            "\nA var() with no declaration is not a parse error: the browser\n"
            "drops the declaration and takes the fallback, so the page renders\n"
            "and the sheet still states a rule it no longer applies. Point the\n"
            "reference at a stop that exists, or delete it.",
            file=sys.stderr,
        )
        return 1

    print(f"check-css-vars: OK -- {len(paths)} token sheet(s), every var() resolves.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
