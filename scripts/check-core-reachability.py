#!/usr/bin/env python3
"""Guard: a `stella-core` module is reachable from the engine's step path.

See #5115 (child of #5113).

`check-module-reachability.py` proves every `.rs` file is reachable from its
**crate root**, and passes on all of today's residents — `search.rs`,
`records/`, `skills/` and the rest *are* reachable from `lib.rs`. Reachability
from the crate root is not reachability from the engine, and nothing stopped
the next 15k-LOC subsystem from moving into `stella-core` behind a `pub mod`
line. That is exactly how the record plane got there.

So this walks a different graph: from the step path — `driver`, `step`,
`ports` — outward through the crate-internal paths those modules actually
*name*, and reports every module the engine never reaches. A module outside
that closure is in `stella-core` because somebody put it there, not because
the engine needs it.

## The baseline is a down-only ratchet

`scripts/core-reachability-baseline.txt` records the modules that are outside
the closure **today**, so this lands green and each eviction PR shrinks a list
the guard already enforces. It may never gain an entry: `--update` refuses to
add one, exactly as the typed-errors ratchet does, and the only way past a red
run is to move the module out or to make the engine genuinely use it. It is
meant to reach empty.

## A test-only reference is not reachability

`#[cfg(test)]` blocks are stripped before any path is read. The doc's v0.9.40
sweep found this false positive first: `driver/restore.rs`'s test module names
`crate::skills`, which would otherwise have made the whole skill plane look
like engine code. A subsystem the engine only touches from its tests is a
subsystem the engine does not need.

This is a fact about the repository rather than about a crate, so it is never
scoped by CARGO_SCOPE (AGENTS.md § "The gate"), and a text-level walk keeps it
in the toolchain-free `guards-fast` rung.

Usage:
    ./scripts/check-core-reachability.py [ROOT]
    ./scripts/check-core-reachability.py --update [ROOT]   # shrink the baseline
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

CRATE = "crates/stella-core"
BASELINE = "scripts/core-reachability-baseline.txt"

# The engine. Everything else in the crate has to earn its place by being named
# from here, transitively.
STEP_PATH_ROOTS = ("driver.rs", "driver", "step.rs", "step", "ports.rs", "ports")

# `crate::foo`, `use crate::{foo, bar}`, `super::foo` — the crate-internal
# references that make a module engine code. Only the FIRST segment matters:
# reaching `crate::records::promotion::x` reaches the `records` module, and the
# walk continues from that module's own file.
CRATE_PATH = re.compile(r"\bcrate\s*::\s*([A-Za-z_][A-Za-z0-9_]*)")
USE_GROUP = re.compile(r"\buse\s+crate\s*::\s*\{([^}]*)\}", re.S)
GROUP_HEAD = re.compile(r"([A-Za-z_][A-Za-z0-9_]*)")

# `mod foo;`, matching the sibling guard's rule so the two agree about what a
# module declaration is.
MOD_DECL = re.compile(
    r"(?:^|[;{}\]\s])(?:pub\s*(?:\([^)]*\)\s*)?)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)

CFG_TEST = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")


def strip_comments(src: str) -> str:
    """Blank out comments and string literals, keeping newlines.

    Same shape as the sibling guard's, and needed for the same reason: a
    `crate::records` written inside a doc comment describing this very rule
    must not count as a reference.
    """
    out: list[str] = []
    i, n = 0, len(src)
    while i < n:
        two = src[i : i + 2]
        if two == "//":
            while i < n and src[i] != "\n":
                out.append(" ")
                i += 1
        elif two == "/*":
            depth = 1
            out.append("  ")
            i += 2
            while i < n and depth:
                two = src[i : i + 2]
                if two == "/*":
                    depth += 1
                    out.append("  ")
                    i += 2
                elif two == "*/":
                    depth -= 1
                    out.append("  ")
                    i += 2
                else:
                    out.append("\n" if src[i] == "\n" else " ")
                    i += 1
        elif src[i] == '"':
            out.append(" ")
            i += 1
            while i < n and src[i] != '"':
                if src[i] == "\\":
                    out.append(" ")
                    i += 1
                    if i < n:
                        out.append("\n" if src[i] == "\n" else " ")
                        i += 1
                    continue
                out.append("\n" if src[i] == "\n" else " ")
                i += 1
            if i < n:
                out.append(" ")
                i += 1
        else:
            out.append(src[i])
            i += 1
    return "".join(out)


def strip_cfg_test(text: str) -> str:
    """Blank out every `#[cfg(test)]` item, braces balanced.

    A subsystem the engine reaches only from its own tests is not engine code —
    the false positive the issue names, where `driver/restore.rs`'s test module
    references `crate::skills`.

    A `#[cfg(test)] mod tests;` declaration (no block) is blanked to its
    semicolon, so the sibling file is not walked either.
    """
    out = list(text)
    for match in CFG_TEST.finditer(text):
        i = match.end()
        while i < len(text) and text[i] not in "{;":
            i += 1
        if i >= len(text):
            break
        start = match.start()
        if text[i] == ";":
            end = i + 1
        else:
            depth, j = 0, i
            while j < len(text):
                if text[j] == "{":
                    depth += 1
                elif text[j] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                j += 1
            end = min(j + 1, len(text))
        for k in range(start, end):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def child_dir(file: Path) -> Path:
    """Where `mod foo;` inside `file` looks for `foo` — the sibling's rule."""
    if file.stem in {"lib", "main", "mod"}:
        return file.parent
    return file.parent / file.stem


def module_file(src: Path, name: str) -> Path | None:
    """`crate::<name>` as a file under `src/`, either spelling."""
    for candidate in (src / f"{name}.rs", src / name / "mod.rs"):
        if candidate.is_file():
            return candidate
    return None


def retired_because(src: Path, name: str) -> str:
    """Why a baseline entry stopped being unreached.

    A name drops out of the unreached set for two reasons, and they are
    opposites: the engine started naming the module, or the module is gone.
    `module_file` answers which, so a run reports the one that happened
    instead of asserting the first in both cases.
    """
    if module_file(src, name) is None:
        return "evicted, gone from stella-core"
    return "the engine reaches it now"


def referenced_modules(text: str) -> set[str]:
    """Every top-level crate module this text names."""
    names = set(CRATE_PATH.findall(text))
    for group in USE_GROUP.findall(text):
        # `use crate::{a, b::c, d as e}` — each entry's head is a module.
        for entry in group.split(","):
            head = GROUP_HEAD.search(entry)
            if head:
                names.add(head.group(1))
    return names


def walk(src: Path) -> set[Path]:
    """Every file the engine reaches, from the step path outward."""
    queue: list[Path] = []
    for entry in STEP_PATH_ROOTS:
        target = src / entry
        if target.is_file():
            queue.append(target)
        elif target.is_dir():
            queue.extend(sorted(target.rglob("*.rs")))

    seen: set[Path] = set()
    while queue:
        file = queue.pop()
        if file in seen or not file.is_file():
            continue
        seen.add(file)
        text = strip_cfg_test(strip_comments(file.read_text(encoding="utf-8", errors="replace")))

        # Submodules of this file are part of it.
        base = child_dir(file)
        for name in MOD_DECL.findall(text):
            for candidate in (base / f"{name}.rs", base / name / "mod.rs"):
                if candidate.is_file():
                    queue.append(candidate)
                    break

        # And every crate-level module it names, with that module's whole
        # subtree: reaching `crate::records` reaches the record plane, which is
        # the claim this guard is about.
        for name in referenced_modules(text):
            target = module_file(src, name)
            if target is None:
                continue
            queue.append(target)
            subtree = src / name
            if subtree.is_dir():
                queue.extend(sorted(subtree.rglob("*.rs")))
    return seen


def crate_sources(src: Path) -> list[Path]:
    return sorted(p for p in src.rglob("*.rs") if p.is_file())


def top_level_name(src: Path, file: Path) -> str:
    """The crate-level module a file belongs to — what the baseline records.

    A directory rather than a file, so evicting `records/` shrinks the baseline
    by one line rather than by thirty.
    """
    rel = file.relative_to(src)
    return rel.parts[0][:-3] if len(rel.parts) == 1 else rel.parts[0]


def read_baseline(path: Path) -> set[str]:
    if not path.is_file():
        return set()
    return {
        line.strip()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.startswith("#")
    }


BASELINE_HEADER = """\
# Modules in `stella-core` that the engine's step path does not reach.
#
# A DOWN-ONLY ratchet (#5115). This records the debt that predates the guard;
# it may never gain an entry, and `--update` refuses to add one. Each eviction
# under #5113 deletes a line here. It is meant to reach empty.
#
# Regenerate after an eviction:  ./scripts/check-core-reachability.py --update
"""


def main() -> int:
    args = [a for a in sys.argv[1:] if a != "--update"]
    update = "--update" in sys.argv[1:]
    root = Path(args[0] if args else ".").resolve()
    src = root / CRATE / "src"
    baseline_path = root / BASELINE

    if not src.is_dir():
        print(f"check-core-reachability: no {CRATE}/src at {root} — nothing to check")
        return 0

    reached = walk(src)
    unreached = sorted(
        {
            top_level_name(src, f)
            for f in crate_sources(src)
            if f not in reached and f.name != "lib.rs"
        }
    )
    baseline = read_baseline(baseline_path)

    if update:
        added = sorted(set(unreached) - baseline)
        if added:
            print("check-core-reachability: REFUSING to grow the baseline.\n")
            print("These modules are new debt, not recorded debt:")
            for name in added:
                print(f"  {name}")
            print(
                "\nThe ratchet only goes down. Move the module out of stella-core,"
                "\nor make the engine actually use it — do not record it here."
            )
            return 1
        kept = sorted(baseline & set(unreached))
        baseline_path.write_text(
            BASELINE_HEADER + "\n".join(kept) + ("\n" if kept else ""), encoding="utf-8"
        )
        for name in sorted(baseline - set(unreached)):
            print(
                f"check-core-reachability: retired {name} — "
                f"{retired_because(src, name)}"
            )
        print(f"check-core-reachability: baseline holds {len(kept)} module(s)")
        return 0

    new_debt = sorted(set(unreached) - baseline)
    dead_entries = sorted(baseline - set(unreached))

    if new_debt:
        print("check-core-reachability: FAILED\n")
        print(
            "These stella-core modules are not reachable from the engine's step\n"
            "path (driver / step / ports) and are not in the baseline:\n"
        )
        for name in new_debt:
            print(f"  {name}")
        print(
            "\nstella-core holds the step path (AGENTS.md rule 12). A subsystem\n"
            "that lands here behind a `pub mod` line is how the record plane got in.\n"
            "Move it to its own crate, or make the engine genuinely use it.\n"
            "Do NOT add a baseline entry — the ratchet only goes down."
        )
        return 1

    if dead_entries:
        print("check-core-reachability: baseline is STALE\n")
        print("These baseline entries are dead:\n")
        for name in dead_entries:
            print(f"  {name} — {retired_because(src, name)}")
        print("\nRun ./scripts/check-core-reachability.py --update and commit the diff.")
        return 1

    print(
        f"check-core-reachability: OK — {len(baseline)} module(s) outside the step "
        f"path, all recorded; none added."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
