#!/usr/bin/env python3
"""Guard: a down-only ratchet on unclassified `ToolOutput::error(` sites.

#3145 gave `ToolOutput::Error` an optional `class: Option<ErrorClass>`
(`crates/stella-protocol/src/tool.rs`) so a per-tool error rate can attribute
failures to *whose problem they are* instead of counting every refusal the
same way. `ToolOutput::error(msg)` is the pre-#3145 constructor, kept as the
migration-friendly default: it leaves `class: None`, meaning "not audited
yet" -- deliberately not a class of its own. #3167 is the sweep that audits
those sites into `ToolOutput::classified_error(class, msg)`, and this is its
ratchet: nothing stops the unclassified count drifting back up once nobody is
looking, so a crate may shrink its count, never grow it, exactly the shape
`scripts/check-typed-errors.py` (AGENTS.md invariant #5) uses for the same
reason -- the rule predates the guard, so the baseline records a debt that
already existed rather than granting new permission.

Two scoping decisions, both different from `check-typed-errors.py`'s:

  * **Every crate, not just libraries.** A tool can report an unclassified
    error from a binary crate (`stella-cli`) as easily as a library one --
    `ToolExecutor` is not library-scoped -- so this walks `crates/*/src`
    without requiring a `lib.rs`.

  * **Every call site, not just `pub fn`s.** The hazard invariant #5 guards is
    a caller across a crate boundary that cannot branch; the hazard here is a
    site that never got read for its real failure cause, public or not. A
    private helper's unclassified error is exactly as unaudited as a public
    one.

Test code is excluded the same way `check-typed-errors.py` excludes it: a
path with a `tests` component or named `tests.rs` is skipped outright, and a
`#[cfg(test)]` (or `#[cfg(all(test, ...))]`) block inside a kept file is
stripped before scanning -- classifying a test's fixture data audits nothing
real, and would make the ratchet a function of test coverage rather than of
production error handling.

The one thing this guard cannot check, by construction: whether the class
assigned to an already-classified site is the *honest* one. That is a review
question (does this failure really belong to the model, the policy plane, the
world, or us?), not a mechanical one -- the guard only counts what is still
`class: None`.

The baseline is a down-only ratchet, the same shape as
`scripts/typed-errors-baseline.txt`: a crate may shrink its count, never grow
it, and a crate absent from the baseline must be at zero. Regenerate with
`make tool-error-class-update` after classifying sites -- which only ever
tightens, because the writer refuses to raise a count.

This is a fact about the repository rather than about a crate, so it is never
scoped by CARGO_SCOPE (AGENTS.md § "The gate"), and a text-level walk keeps it
in the toolchain-free `guards-fast` rung.

Usage:
    ./scripts/check-tool-error-class.py [--update] [ROOT]
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

BASELINE = "scripts/tool-error-class-baseline.txt"

# The pre-#3145 constructor: `ToolOutput::error(`. Deliberately does NOT match
# `ToolOutput::classified_error(` -- the literal text immediately after
# `ToolOutput::` differs (`error` vs `classified_error`), so an already-audited
# site never counts twice.
UNCLASSIFIED = re.compile(r"ToolOutput::error\(")

CFG_TEST = re.compile(r"#\[cfg\((?:all\()?test\b")


def strip_cfg_test(src: str) -> str:
    """Drop `#[cfg(test)]` / `#[cfg(all(test, ...))]` blocks.

    A classified/unclassified split inside a test fixture audits nothing --
    counting it would make the ratchet a measure of test-fixture style
    instead of production error handling.
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


#  A file whose OWN `mod` declaration is `#[cfg(test)]`-gated in a sibling
# file -- so every line in it is test code even though nothing inside the
# file itself says so. A per-file text scan cannot see across that boundary
# (neither can check-typed-errors.py's, which has the identical blind spot).
# `crates/stella-cli/src/tool_docs.rs` is declared `#[cfg(test)] mod
# tool_docs;` in `crates/stella-cli/src/main.rs` -- its own module doc names
# the reason ("Why this lives in a #[cfg(test)] module of a binary crate").
# Its one `ToolOutput::error(` site deliberately exercises the pre-#3145
# (unclassified) wire shape to pin the generated docs' schema, so converting
# it would misrepresent the site AND change what the test proves. Declared
# here, by exact path, rather than silently miscounted.
TEST_ONLY_FILES = frozenset({"crates/stella-cli/src/tool_docs.rs"})


def violations(root: Path) -> list[tuple[str, Path, int]]:
    """Every still-unclassified `ToolOutput::error(` site, by crate."""
    found: list[tuple[str, Path, int]] = []
    for crate_dir in sorted((root / "crates").glob("*")):
        src_dir = crate_dir / "src"
        if not src_dir.is_dir():
            continue
        crate = crate_dir.name
        for path in sorted(src_dir.rglob("*.rs")):
            rel = path.relative_to(root)
            # Integration tests and the `tests.rs` submodule convention are
            # test code by another name -- same skip `check-typed-errors.py`
            # uses.
            if "tests" in rel.parts or path.name == "tests.rs":
                continue
            if rel.as_posix() in TEST_ONLY_FILES:
                continue
            src = strip_cfg_test(path.read_text(encoding="utf-8", errors="replace"))
            for m in UNCLASSIFIED.finditer(src):
                line = src.count("\n", 0, m.start()) + 1
                found.append((crate, rel, line))
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
# Down-only ratchet for #3167: every `ToolOutput::error(` construction is
# either classified (`ToolOutput::classified_error(class, msg)`, #3145) or
# accounted for here.
#
# Each line is `<crate> <count>` -- the number of still-unclassified sites in
# that crate. A crate may shrink its count; it may never grow it, and a crate
# absent from this file must be at zero. Regenerate with
# `make tool-error-class-update`, which refuses to raise a number.
#
# This file is meant to reach empty -- see #3167 for the remaining sweep.
# Do not add a crate here to turn the gate green; classify the sites instead.
"""


def main() -> int:
    argv = [a for a in sys.argv[1:] if a != "--update"]
    update = "--update" in sys.argv[1:]
    root = Path(argv[0]) if argv else Path(__file__).resolve().parent.parent
    root = root.resolve()

    found = violations(root)
    counts: dict[str, int] = {}
    for crate, _, _ in found:
        counts[crate] = counts.get(crate, 0) + 1

    baseline_path = root / BASELINE
    baseline = read_baseline(baseline_path)

    if update:
        # An absent crate's allowance is ZERO, never "whatever it happens to
        # have now" -- see check-typed-errors.py's #3750 postmortem, the same
        # defect shape this mirrors the fix for.
        merged = {c: min(n, baseline.get(c, 0)) for c, n in counts.items()}
        raised = {
            c: (baseline.get(c, 0), n)
            for c, n in counts.items()
            if n > baseline.get(c, 0)
        }
        if raised:
            for crate, (was, now) in sorted(raised.items()):
                note = (
                    f"refusing to raise {crate} from {was} to {now}."
                    if crate in baseline
                    else (
                        f"refusing to add {crate} at {now} -- a crate absent "
                        "from the ratchet must be at zero."
                    )
                )
                print(f"check-tool-error-class: {note}", file=sys.stderr)
            print(
                "check-tool-error-class: the ratchet only tightens -- "
                "classify the new sites instead (#3167).",
                file=sys.stderr,
            )
            return 1
        body = "".join(f"{c} {n}\n" for c, n in sorted(merged.items()) if n)
        baseline_path.write_text(HEADER + body, encoding="utf-8")
        print(
            f"check-tool-error-class: wrote {BASELINE} ({sum(merged.values())} remaining)."
        )
        return 0

    # The verdict is decided before anything is written: a guard that prints as
    # it scans dies mid-report when its reader exits early, and whatever partial
    # state it had reached becomes the exit status (#1815).
    report: list[str] = []
    status = 0
    by_crate: dict[str, list[tuple[Path, int]]] = {}
    for crate, rel, line in found:
        by_crate.setdefault(crate, []).append((rel, line))

    for crate in sorted(set(counts) | set(baseline)):
        now = counts.get(crate, 0)
        allowed = baseline.get(crate, 0)
        if now > allowed:
            status = 1
            report.append(
                f"{crate}: {now} unclassified ToolOutput::error( site(s), "
                f"ratchet allows {allowed}."
            )
            for rel, line in by_crate.get(crate, [])[:20]:
                report.append(f"    {rel}:{line}")
        elif now < allowed:
            report.append(
                f"note: {crate} is down to {now} (ratchet says {allowed}) -- "
                f"run `make tool-error-class-update` to lock the win in."
            )

    if status:
        report.append("")
        report.append(
            "#3167: give the site an honest ErrorClass -- "
            "ToolOutput::classified_error(class, message) -- without changing "
            "the message bytes (the loop detector and prompt cache compare "
            "them). See crates/stella-protocol/src/tool.rs's ErrorClass doc "
            "comment for the vocabulary."
        )

    if report:
        sys.stderr.write("\n".join(report) + "\n")
    return status


if __name__ == "__main__":
    sys.exit(main())
