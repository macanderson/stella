#!/usr/bin/env python3
"""Guard: `#[allow(dead_code)]` is justified where it survives, and never grows.

CLAUDE.md § "Code style": *"Do not `#[allow]` your way past a lint without a
comment saying why the lint is wrong here."* Nothing checked it, and nothing
in `make gate` can: clippy is the one tool that would notice, and silencing
clippy is the attribute's entire job. The single existing check is a
hand-rolled one-file assertion (`crates/stella-cli/tests/dead_code_witness.rs`)
that greps `memory/replay/cc_adapter.rs` because somebody was burned by that
one file.

The tree reached zero suppressions twice and drifted back both times (#3949).
It drifts because a `dead_code` allow is the cheapest way to make a red build
green, and because the code under it is by definition code nothing calls -- so
nothing else ever fails to notice it is gone.

Two questions, deliberately kept separate, because they fail for different
reasons and have different remedies:

  1. **Is it justified?** A floor, not a ratchet: the answer must be yes
     everywhere, today and forever. An allow is justified by an in-band
     `reason = "..."` (Rust's own mechanism) or by a comment line directly
     above it. This is CLAUDE.md's rule stated mechanically. It is not a high
     bar -- it is not meant to be. It is meant to make the author write the
     sentence, because writing "why is this lint wrong here?" is what surfaces
     the cases where the honest answer is "it isn't".

  2. **Is the total shrinking?** A per-crate down-only ratchet, the same shape
     as `scripts/typed-errors-baseline.txt` and legitimate for the same one
     reason: the rule predates the guard, so the baseline records a debt that
     already existed rather than granting new permission. `--update` refuses
     to raise a number or to add a crate, so the only way past it is to remove
     a suppression.

A justified allow still counts toward (2). That is the point. A comment makes
a suppression *reviewable*; it does not make it *good*, and the tree's history
is of well-argued suppressions accumulating one reasonable-sounding paragraph
at a time.

# `#[cfg(test)]` is the better answer, and this guard is built to say so

Most `dead_code` allows in this tree sit on items whose only callers really are
tests. For those, `#[cfg(test)]` beats `#[allow(dead_code)]` outright:

  * `#[allow(dead_code)]` asserts *the lint is wrong*. For an item only tests
    call, it is not wrong -- the item genuinely is dead in the shipped binary.
  * `#[cfg(test)]` asserts *this exists for the tests*, keeps it out of the
    release build, and is **compiler-enforced**: a production caller sneaking
    in later becomes a build error instead of silently re-justifying the allow.

So this guard does not count `#[cfg(test)]`, and converting to it is the first
remedy named in every failure message. Deletion is the second.

# What is excluded, and why by construction rather than by allowlist

  * **Test code.** `crates/*/tests/**` is not scanned at all, and neither is
    the in-crate `tests.rs` / `tests/` submodule convention, nor the body of a
    `#[cfg(test)]` module inside a production file. A helper shared across test
    binaries carrying a module-level `#![allow(dead_code)]` -- canonical Rust,
    since each binary uses a subset -- is therefore excluded by living where it
    lives, not by being named here. An allowlist would have to be maintained;
    a path rule maintains itself.

  * **`cfg_attr` forms.** `#[cfg_attr(not(target_os = "..."), allow(dead_code))]`
    is platform-conditional and genuinely correct -- the item is live on the
    other target (`crates/stella-cli/src/daemon/service.rs`). Only the
    unconditional attribute form is counted.

An inner `#![allow(dead_code)]` in production `src/` is NOT excluded: outside a
test helper it is the same suppression with a wider blast radius, and scoping
the exclusion to test paths is what keeps that hole closed.

This is a fact about the repository rather than about a crate, so it is never
scoped by CARGO_SCOPE (AGENTS.md § "The gate"), and a text-level walk keeps it
in the toolchain-free `guards-fast` rung.

Usage:
    ./scripts/check-dead-code-allows.py [--update | --seed] [ROOT]

`--seed` writes the baseline for the first time and refuses if one exists;
`--update` only ever tightens it. They are separate verbs because a ratchet
that can write its own first number is one that anybody can reset by deleting
the file.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

BASELINE = "scripts/dead-code-allows-baseline.txt"

# The lints this guard governs. `dead_code` is the regrowth hazard #3949 names;
# `unused_imports` is here because it is the same move against a different lint
# -- and because the legitimate use of it in this tree (keeping an intra-doc
# link resolving) is exactly the kind of thing that should have to say so.
LINTS = ("dead_code", "unused_imports")

# An unconditional `#[allow(...)]` or `#![allow(...)]` naming one of them. The
# `cfg_attr` form does not match: it opens `#[cfg_attr(`, so the literal
# `#[allow(`/`#![allow(` prefix required here is absent.
ALLOW = re.compile(
    r"^\s*#!?\[\s*allow\s*\(([^)]*)\)\s*\]",
)

CFG_TEST = re.compile(r"#\[cfg\((?:all\()?test\b")


def strip_cfg_test_lines(src: str) -> set[int]:
    """1-based line numbers inside a `#[cfg(test)]` block.

    Test code is out of scope: an allow there suppresses a lint about test
    scaffolding, which is neither shipped nor the drift this guard exists to
    catch. Brace-matched from the block's opening `{`, the same walk
    `scripts/check-typed-errors.py` uses for the same purpose.
    """
    excluded: set[int] = set()
    for m in CFG_TEST.finditer(src):
        brace = src.find("{", m.start())
        if brace < 0:
            continue
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
        first = src.count("\n", 0, m.start()) + 1
        last = src.count("\n", 0, min(j, len(src) - 1)) + 1
        excluded.update(range(first, last + 1))
    return excluded


def is_justified(lines: list[str], index: int, attr_text: str) -> bool:
    """Does the allow at `lines[index]` say why the lint is wrong here?

    Two accepted forms. The in-band `reason = "..."` is preferred -- it cannot
    drift away from the attribute the way a comment above it can, since there
    is nothing between them to edit. A comment line directly above is accepted
    because it is what this repository already writes, at length, and demanding
    a mechanical rewrite of prose that already answers the question would be
    the guard picking a fight with its own tree.

    "Directly above" tolerates further attributes in between (`#[must_use]`
    sits between the doc comment and the allow in `reward.rs`) but nothing
    else: a blank line ends the association, because a comment separated by one
    is documenting something other than this attribute.

    An inner doc comment (`//!`) never justifies. It documents the *module*,
    not the item below it, so accepting it would mean every suppression near
    the top of a file was justified by whatever that file happened to say about
    itself — a coincidence of layout, which is the same thing the blank-line
    rule exists to reject.
    """
    if re.search(r"\breason\s*=\s*\"[^\"]+\"", attr_text):
        return True
    i = index - 1
    while i >= 0:
        line = lines[i].strip()
        if not line:
            return False
        if line.startswith("//!"):
            return False
        if line.startswith("//"):
            # A comment with nothing in it explains nothing.
            return len(line.lstrip("/").strip()) > 0
        if line.startswith("#["):
            i -= 1
            continue
        return False
    return False


def suppressions(root: Path) -> list[tuple[str, Path, int, str, bool]]:
    """Every counted allow: `(crate, path, line, lints, justified)`."""
    found: list[tuple[str, Path, int, str, bool]] = []
    for crate_src in sorted(root.glob("crates/*/src")):
        crate = crate_src.parent.name
        for path in sorted(crate_src.rglob("*.rs")):
            rel = path.relative_to(root)
            # The in-crate test conventions, by path: `foo/tests.rs` and
            # anything under a `tests/` directory.
            if "tests" in rel.parts or path.name == "tests.rs":
                continue
            src = path.read_text(encoding="utf-8", errors="replace")
            if "allow" not in src:
                continue
            lines = src.splitlines()
            in_test = strip_cfg_test_lines(src)
            for index, line in enumerate(lines):
                m = ALLOW.match(line)
                if not m:
                    continue
                inner = m.group(1)
                named = sorted({lint for lint in LINTS if re.search(rf"\b{lint}\b", inner)})
                if not named:
                    continue
                lineno = index + 1
                if lineno in in_test:
                    continue
                found.append(
                    (
                        crate,
                        rel,
                        lineno,
                        ", ".join(named),
                        is_justified(lines, index, line),
                    )
                )
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
# Down-only ratchet for `#[allow(dead_code)]` / `#[allow(unused_imports)]` in
# production source (CLAUDE.md's "an expedient is a defect"; #3949).
#
# Each line is `<crate> <count>` -- suppressions of those two lints outside test
# code. A crate may shrink its count; it may never grow it, and a crate absent
# from this file must be at zero. Regenerate with `make dead-code-allows-update`,
# which refuses to raise a number or to add a crate.
#
# The counts here are the debt #3872 left behind, not permission to add more.
# The remedy is almost always `#[cfg(test)]` -- compiler-enforced, and honest
# about what the item is for -- or deletion. This file is meant to reach empty.
"""

REMEDY = (
    "Remedies, in order: `#[cfg(test)]` if the only callers are tests (it is "
    "compiler-enforced, keeps the item out of the shipped binary, and turns a "
    "future production caller into a build error); deletion if there are no "
    "callers at all; repointing at a live call site if the item outlived only "
    "its old one. `#[allow(dead_code)]` is correct only when the lint is "
    "genuinely wrong -- which for an uncalled item it usually is not."
)


def main() -> int:
    args = sys.argv[1:]
    update = "--update" in args
    # Seeding is its own verb because `--update` must never be able to do it.
    # A ratchet that writes its own first number can be reset by anyone who
    # deletes the file, which is not a ratchet -- and `--update` refusing to
    # add a crate (the #3750 lesson) means it cannot bootstrap either. So the
    # one-time seed is explicit, refuses to overwrite, and lands as a
    # reviewable diff a human agreed to, exactly like `check-file-size.sh
    # --update --grandfather`.
    seed = "--seed" in args
    positional = [a for a in args if not a.startswith("--")]
    root = Path(positional[0]) if positional else Path(__file__).resolve().parent.parent
    root = root.resolve()

    found = suppressions(root)

    # Question 1, the floor. Checked before the ratchet and reported first: an
    # unjustified suppression is a defect at any count, and telling an author
    # their number is fine while their attribute explains nothing would bury
    # the actionable half under the accounting half.
    unjustified = [(c, rel, line, lints) for c, rel, line, lints, ok in found if not ok]

    counts: dict[str, int] = {}
    for crate, _, _, _, _ in found:
        counts[crate] = counts.get(crate, 0) + 1

    baseline_path = root / BASELINE
    baseline = read_baseline(baseline_path)

    if seed:
        if baseline_path.is_file():
            print(
                f"check-dead-code-allows: {BASELINE} already exists -- "
                "use `--update` to tighten it. Seeding is one-time.",
                file=sys.stderr,
            )
            return 1
        if unjustified:
            print(
                "check-dead-code-allows: refusing to seed while "
                f"{len(unjustified)} suppression(s) explain nothing -- the "
                "floor is not a debt to be recorded, it is a rule. Run without "
                "--seed to see them.",
                file=sys.stderr,
            )
            return 1
        body = "".join(f"{c} {n}\n" for c, n in sorted(counts.items()) if n)
        baseline_path.write_text(HEADER + body, encoding="utf-8")
        print(
            f"check-dead-code-allows: seeded {BASELINE} "
            f"({sum(counts.values())} suppression(s) recorded as debt)."
        )
        return 0

    if update:
        # An absent crate's allowance is ZERO, never "whatever it happens to
        # have now" -- the bug #3750 found in the typed-errors writer, which
        # spelled this `baseline.get(c, n)` and so silently grandfathered the
        # one case a ratchet exists to block.
        merged = {c: min(n, baseline.get(c, 0)) for c, n in counts.items()}
        raised = {
            c: (baseline.get(c, 0), n) for c, n in counts.items() if n > baseline.get(c, 0)
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
                print(f"check-dead-code-allows: {note}", file=sys.stderr)
            print(f"check-dead-code-allows: the ratchet only tightens. {REMEDY}", file=sys.stderr)
            return 1
        body = "".join(f"{c} {n}\n" for c, n in sorted(merged.items()) if n)
        baseline_path.write_text(HEADER + body, encoding="utf-8")
        print(
            f"check-dead-code-allows: wrote {BASELINE} "
            f"({sum(merged.values())} suppression(s) remaining)."
        )
        return 0

    # The verdict is decided before anything is written: a guard that prints as
    # it scans dies mid-report when its reader exits early, and whatever partial
    # state it had reached becomes the exit status (#1815).
    report: list[str] = []
    status = 0

    if unjustified:
        status = 1
        report.append(
            f"{len(unjustified)} suppression(s) do not say why the lint is wrong here:"
        )
        for crate, rel, line, lints in unjustified:
            report.append(f"    {rel}:{line}  #[allow({lints})]  ({crate})")
        report.append(
            "    Add `reason = \"...\"` to the attribute, or a comment line "
            "directly above it. If the honest reason is that the code is dead, "
            "that is the answer -- act on it rather than writing it down."
        )
        report.append("")

    by_crate: dict[str, list[tuple[Path, int, str]]] = {}
    for crate, rel, line, lints, _ in found:
        by_crate.setdefault(crate, []).append((rel, line, lints))

    for crate in sorted(set(counts) | set(baseline)):
        now = counts.get(crate, 0)
        allowed = baseline.get(crate, 0)
        if now > allowed:
            status = 1
            report.append(
                f"{crate}: {now} dead-code/unused-import suppression(s), "
                f"ratchet allows {allowed}."
            )
            for rel, line, lints in by_crate.get(crate, [])[:20]:
                report.append(f"    {rel}:{line}  #[allow({lints})]")
        elif now < allowed:
            report.append(
                f"note: {crate} is down to {now} (ratchet says {allowed}) -- "
                f"run `make dead-code-allows-update` to lock the win in."
            )

    if status:
        report.append("")
        report.append(REMEDY)

    if report:
        sys.stderr.write("\n".join(report) + "\n")
    if not status and not found:
        print("check-dead-code-allows: OK -- no dead-code suppressions in production source.")
    elif not status:
        print(
            f"check-dead-code-allows: OK -- {len(found)} suppression(s), "
            "each justified and within the ratchet."
        )
    return status


if __name__ == "__main__":
    sys.exit(main())
