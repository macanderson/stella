#!/usr/bin/env python3
"""Guard: a library crate's public API does not return `Result<_, String>`.

Invariant #5 (AGENTS.md § "Architecture: ports, not concretions") says library
code returns typed, named errors and never a bare `String`. Nothing checked it,
and it had drifted far enough that the rule and the tree disagreed outright:

    109  library-crate signatures returning Result<_, String>
     52  of them fully `pub` -- i.e. crossing a crate boundary

`crates/stella-runtime/src/error.rs` had even written the contradiction into a
comment ("The CLI originals returned `Result<_, String>` -- fine for a binary
that prints the string and exits, useless to a server that has to decide ...").
A normative rule that the code openly disagrees with is worse than no rule: the
next author reads the tree, not the document.

Two scoping decisions, both deliberate:

  * **Library crates only.** `stella-cli` is a binary -- it prints a message and
    exits, so a `String` there is the finished product, not an unnamed error.
    Its 421 signatures were never violations, and an audit that counted them as
    such reached a number five times too large. "Library crate" is the literal
    thing: a crate exposing `src/lib.rs`.

  * **`pub` only.** The hazard invariant #5 names is a *caller* that cannot
    branch without parsing prose, and only the public surface has callers
    outside the crate. Internal helpers whose strings get wrapped at the
    boundary are tracked separately (#2392).

Parsing is depth-aware on purpose. The obvious regex --

    Result<[^>]*,\\s*String\\s*>

-- reports `Result<HashMap<String, String>, WitnessArtifactError>` as a
violation (the `, String>` it matches is *inside* the generic) and reports
`Result<String>` as one too (a crate `Result<T>` alias whose error is
defaulted). Both false positives were live during the audit that prompted this
guard, in both directions: they inflated `stella-store` from 1 to 17 and
invented a violation in `stella-pipeline`. A guard that cries wolf gets
`#[allow]`-ed, so this one counts only a `Result<>` with exactly two top-level
generic arguments whose second is literally `String`.

The baseline is a **down-only ratchet**, the same shape as
`scripts/file-size-baseline.txt`: a crate may shrink its count, never grow it,
and a crate absent from the baseline must be at zero. Regenerate with
`make typed-errors-update` after converting signatures -- which only ever
tightens, because the writer refuses to raise a count.

This is a fact about the repository rather than about a crate, so it is never
scoped by CARGO_SCOPE (AGENTS.md § "The gate"), and a text-level walk keeps it
in the toolchain-free `guards-fast` rung.

Usage:
    ./scripts/check-typed-errors.py [--update] [ROOT]
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

BASELINE = "scripts/typed-errors-baseline.txt"

# A `fn` item's opening. The optional group is the visibility; `pub(crate)` and
# friends are matched here so they can be told apart from a bare `pub` rather
# than falling through to the private case.
FN = re.compile(r"(?m)^[ \t]*(pub(?:\s*\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)")

CFG_TEST = re.compile(r"#\[cfg\((?:all\()?test\b")


def strip_cfg_test(src: str) -> str:
    """Drop `#[cfg(test)]` / `#[cfg(all(test, ...))]` blocks.

    `unwrap` is fine in tests and so is a `String` error; counting test code
    would make the ratchet a measure of how well-tested a crate is.
    """
    out: list[str] = []
    i = 0
    while True:
        m = CFG_TEST.search(src, i)
        if not m:
            out.append(src[i:])
            return "".join(out)
        out.append(src[i : m.start()])
        brace = src.find("{", m.start())
        if brace < 0:
            return "".join(out)
        depth = 0
        j = brace
        while j < len(src):
            if src[j] == "{":
                depth += 1
            elif src[j] == "}":
                depth -= 1
                if depth == 0:
                    break
            j += 1
        i = j + 1


def generic_args(src: str, lt: int) -> list[str] | None:
    """Split the `<...>` opening at `lt` into its top-level arguments."""
    depth = 0
    start = lt + 1
    args: list[str] = []
    i = lt
    while i < len(src):
        c = src[i]
        if c in "<([":
            depth += 1
        elif c in ">)]":
            depth -= 1
            if depth == 0:
                args.append(src[start:i])
                return [a.strip() for a in args]
        elif c == "," and depth == 1:
            args.append(src[start:i])
            start = i + 1
        i += 1
    return None


def return_type_start(src: str, after: int) -> int:
    """Index just past the `->` of the signature starting at `after`, or -1.

    Walks at bracket depth zero so a `->` inside a closure argument type does
    not get mistaken for the signature's own arrow, and stops at `{` or `;` so
    a function with no return type reports -1 instead of running into the body.
    """
    depth = 0
    i = after
    while i < len(src):
        c = src[i]
        if c in "(<[":
            depth += 1
        elif c in ")>]":
            depth -= 1
        elif depth == 0:
            if src.startswith("->", i):
                return i + 2
            if c in "{;":
                return -1
        i += 1
    return -1


def violations(root: Path) -> list[tuple[str, Path, int, str]]:
    """Every `pub fn` in a library crate returning `Result<_, String>`."""
    found: list[tuple[str, Path, int, str]] = []
    for lib in sorted(root.glob("crates/*/src/lib.rs")):
        crate_dir = lib.parent.parent
        crate = crate_dir.name
        for path in sorted(crate_dir.rglob("*.rs")):
            rel = path.relative_to(root)
            # Integration tests and the `tests.rs` submodule convention are
            # test code by another name.
            if "tests" in rel.parts or path.name == "tests.rs":
                continue
            src = strip_cfg_test(path.read_text(encoding="utf-8", errors="replace"))
            for m in FN.finditer(src):
                if not m.group(1) or m.group(1).strip() != "pub":
                    continue
                arrow = return_type_start(src, m.end())
                if arrow < 0:
                    continue
                rest = src[arrow:]
                lead = len(rest) - len(rest.lstrip())
                head = re.match(r"(?:std::result::)?Result\s*<", rest.lstrip())
                if not head:
                    continue
                args = generic_args(src, arrow + lead + head.end() - 1)
                if args and len(args) == 2 and args[1] == "String":
                    line = src.count("\n", 0, m.start()) + 1
                    found.append((crate, rel, line, m.group(2)))
    return found


def read_baseline(path: Path) -> dict[str, int]:
    if not path.is_file():
        return {}
    counts: dict[str, int] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        crate, _, count = line.rpartition(" ")
        counts[crate.strip()] = int(count)
    return counts


HEADER = """\
# Down-only ratchet for AGENTS.md invariant #5: a library crate's public API
# returns typed, named errors, never a bare `String`.
#
# Each line is `<crate> <count>` -- the number of `pub fn`s in that crate still
# returning `Result<_, String>`. A crate may shrink its count; it may never
# grow it, and a crate absent from this file must be at zero. Regenerate with
# `make typed-errors-update`, which refuses to raise a number.
#
# This file is meant to reach empty. The remaining conversions are tracked in
# #2392 -- do not add a crate here to turn the gate green.
"""


def main() -> int:
    argv = [a for a in sys.argv[1:] if a != "--update"]
    update = "--update" in sys.argv[1:]
    root = Path(argv[0]) if argv else Path(__file__).resolve().parent.parent
    root = root.resolve()

    found = violations(root)
    counts: dict[str, int] = {}
    for crate, _, _, _ in found:
        counts[crate] = counts.get(crate, 0) + 1

    baseline_path = root / BASELINE
    baseline = read_baseline(baseline_path)

    if update:
        # An absent crate's allowance is ZERO, never "whatever it happens to
        # have now" -- the check path below has always spelled it
        # `baseline.get(crate, 0)`, and the writer spelling it
        # `baseline.get(c, n)` made the comparison `n > n`, permanently False.
        # So the one case the ratchet exists to block -- a crate introducing
        # its FIRST `Result<_, String>` -- was the one case `--update` silently
        # grandfathered, writing the new entry and reporting success (#3750).
        merged = {c: min(n, baseline.get(c, 0)) for c, n in counts.items()}
        raised = {
            c: (baseline.get(c, 0), n)
            for c, n in counts.items()
            if n > baseline.get(c, 0)
        }
        if raised:
            for crate, (was, now) in sorted(raised.items()):
                # A crate with no entry is not "raised from 0" -- it is being
                # added, and saying so names the remedy the reader needs.
                note = (
                    f"refusing to raise {crate} from {was} to {now}."
                    if crate in baseline
                    else (
                        f"refusing to add {crate} at {now} -- a crate absent "
                        "from the ratchet must be at zero."
                    )
                )
                print(f"check-typed-errors: {note}", file=sys.stderr)
            print(
                "check-typed-errors: the ratchet only tightens -- type the new "
                "signatures instead (AGENTS.md invariant #5).",
                file=sys.stderr,
            )
            return 1
        body = "".join(f"{c} {n}\n" for c, n in sorted(merged.items()) if n)
        baseline_path.write_text(HEADER + body, encoding="utf-8")
        print(f"check-typed-errors: wrote {BASELINE} ({sum(merged.values())} remaining).")
        return 0

    # The verdict is decided before anything is written: a guard that prints as
    # it scans dies mid-report when its reader exits early, and whatever partial
    # state it had reached becomes the exit status (#1815).
    report: list[str] = []
    status = 0
    by_crate: dict[str, list[tuple[Path, int, str]]] = {}
    for crate, rel, line, name in found:
        by_crate.setdefault(crate, []).append((rel, line, name))

    for crate in sorted(set(counts) | set(baseline)):
        now = counts.get(crate, 0)
        allowed = baseline.get(crate, 0)
        if now > allowed:
            status = 1
            report.append(
                f"{crate}: {now} public fn(s) return `Result<_, String>`, "
                f"ratchet allows {allowed}."
            )
            for rel, line, name in by_crate.get(crate, [])[:20]:
                report.append(f"    {rel}:{line}  {name}")
        elif now < allowed:
            report.append(
                f"note: {crate} is down to {now} (ratchet says {allowed}) -- "
                f"run `make typed-errors-update` to lock the win in."
            )

    if status:
        report.append("")
        report.append(
            "Invariant #5: a library crate's public API returns typed, named "
            "errors. Give the failure an enum (thiserror) whose variants a "
            "caller can match on -- see crates/stella-tools/src/input.rs or "
            "crates/stella-runtime/src/error.rs for the shape."
        )

    if report:
        sys.stderr.write("\n".join(report) + "\n")
    return status


if __name__ == "__main__":
    sys.exit(main())
