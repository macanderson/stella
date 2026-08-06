#!/usr/bin/env python3
"""Guard: every .rs file under a crate's src/ is reachable from its crate root.

See #1750.

Deleting a `mod` declaration silently disables the file it pointed at, and
nothing else in `make gate` notices. rustc, rustfmt and clippy all walk `mod`
declarations from the crate root, so an orphaned file is invisible to the
compiler, the formatter and the linter *simultaneously*, and `cargo test`
still reports green.

Measured, in PR #1739: commit f6284d0d dropped `#[cfg(test)] mod tests;` from
the bottom of `crates/stella-graph/src/store.rs`.

    $ cargo test -p stella-graph --lib -- --list | rg -c '^store::tests::'
    0          # without the declaration — and the suite reports "ok"
    19         # with it restored

`crates/stella-graph/src/store/tests.rs` was also unformatted the whole time
and `cargo fmt --check` passed, because rustfmt never reached it either. One
deleted line hid a file from three gates at once, and only a reviewer reading
the diff caught it.

This is a fact about the repository rather than about a crate, so it is never
scoped by CARGO_SCOPE (AGENTS.md § "The gate"), and a text-level walk keeps it
in the toolchain-free `guards-fast` rung.

Usage:
    ./scripts/check-module-reachability.py [ROOT]
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

# `mod foo;` — a declaration, not an inline `mod foo { … }`, which needs no
# file. Leading attributes on the same line are allowed so `#[cfg(test)] mod
# tests;` counts: a cfg-gated declaration still makes the file reachable, and
# treating it otherwise would report every test module in the tree.
MOD_DECL = re.compile(
    r"(?:^|[;{}\]\s])(?:pub\s*(?:\([^)]*\)\s*)?)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)

# `#[path = "…"]` relocates a module's file, which this walker does not model.
# There are none in this tree today (#1393 removed the last of them), so rather
# than guess, the guard says it cannot verify and fails loudly. A silent wrong
# answer from a reachability check is worse than no check.
PATH_ATTR = re.compile(r"#\s*\[\s*path\s*=")


def strip_comments(src: str) -> str:
    """Blank out comments, keeping newlines so line structure survives.

    Needed because a `mod x;` inside a doc comment — describing the very rule
    this guard enforces — would otherwise count as a declaration.

    String literals are tracked, and that is not a nicety. A first version
    skipped them on the theory that a `mod x;` inside a string is vanishingly
    rare and the failure would be benign. Both halves were wrong: the risk is
    not a `mod` inside a string, it is a `/*` inside one. This tree has

        "guard-deny-path: protected/**"

    in `crates/stella-cli/src/agent/tests.rs`, whose `/*` opened a block
    comment that never closed — blanking the remaining 450 lines, hiding three
    real `mod` declarations, and reporting three perfectly reachable files as
    orphans. A reachability guard that answers wrongly is worse than none.

    Raw strings get their hash count matched (`r#"…"#`) so a quoted `"` inside
    one does not end it early.
    """
    out: list[str] = []
    i, n = 0, len(src)
    depth = 0  # block-comment nesting; Rust's nest.
    while i < n:
        two = src[i : i + 2]
        if depth:
            if two == "/*":
                depth += 1
                out.append("  ")
                i += 2
                continue
            if two == "*/":
                depth -= 1
                out.append("  ")
                i += 2
                continue
            out.append("\n" if src[i] == "\n" else " ")
            i += 1
            continue
        if two == "//":
            while i < n and src[i] != "\n":
                out.append(" ")
                i += 1
            continue
        if two == "/*":
            depth = 1
            out.append("  ")
            i += 2
            continue
        if src[i] == '"':
            out.append('"')
            i += 1
            while i < n:
                if src[i] == "\\":
                    out.append(src[i : i + 2])
                    i += 2
                    continue
                out.append(src[i])
                if src[i] == '"':
                    i += 1
                    break
                i += 1
            continue
        if src[i] == "r":
            hashes = 0
            while i + 1 + hashes < n and src[i + 1 + hashes] == "#":
                hashes += 1
            if i + 1 + hashes < n and src[i + 1 + hashes] == '"':
                closer = '"' + "#" * hashes
                end = src.find(closer, i + 2 + hashes)
                end = n if end == -1 else end + len(closer)
                out.append(src[i:end])
                i = end
                continue
        out.append(src[i])
        i += 1
    return "".join(out)


def child_dir(file: Path) -> Path:
    """Where `mod foo;` inside `file` looks for `foo`.

    A crate/module root (`lib.rs`, `main.rs`, `mod.rs`) owns its own directory;
    any other module file owns a directory named after itself.
    """
    if file.stem in {"lib", "main", "mod"}:
        return file.parent
    return file.parent / file.stem


def declared_mods(text: str) -> list[str]:
    return MOD_DECL.findall(text)


def crate_roots(crate: Path) -> list[Path]:
    """Every compilation root of `crate`: the lib, the bin, and extra bins.

    `src/bin/*.rs` is Cargo's auto-discovery and each file there is its own
    root, not a module of anything — a walker that missed that would report
    every one of them as an orphan.
    """
    roots = [p for p in (crate / "src/lib.rs", crate / "src/main.rs") if p.is_file()]
    manifest = crate / "Cargo.toml"
    if manifest.is_file():
        for m in re.finditer(r'^\s*path\s*=\s*"([^"]+)"', manifest.read_text(), re.M):
            candidate = crate / m.group(1)
            if candidate.is_file() and candidate.suffix == ".rs":
                roots.append(candidate)
    roots.extend(sorted((crate / "src/bin").glob("*.rs")))
    return roots


def reachable_from(roots: list[Path]) -> tuple[set[Path], list[str]]:
    seen: set[Path] = set()
    problems: list[str] = []
    queue = list(roots)
    while queue:
        file = queue.pop()
        if file in seen or not file.is_file():
            continue
        seen.add(file)
        raw = file.read_text(encoding="utf-8", errors="replace")
        text = strip_comments(raw)
        if PATH_ATTR.search(text):
            problems.append(
                f"{file}: uses #[path], which this guard does not model — "
                "teach it the attribute rather than leaving the crate unchecked"
            )
        base = child_dir(file)
        for name in declared_mods(text):
            for candidate in (base / f"{name}.rs", base / name / "mod.rs"):
                if candidate.is_file():
                    queue.append(candidate)
                    break
            # A `mod` that resolves to nothing is a compile error rustc will
            # report; it is not this guard's finding to make.
    return seen, problems


def tracked_sources(root: Path, crate: Path) -> list[Path]:
    rel = crate.relative_to(root)
    out = subprocess.run(
        ["git", "ls-files", "-z", f"{rel}/src/**/*.rs", f"{rel}/src/*.rs"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [root / p for p in out.split("\0") if p]


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    crates = sorted(
        p.parent
        for p in root.glob("*/*/Cargo.toml")
        if (p.parent / "src").is_dir()
    ) or sorted(
        p.parent for p in root.glob("*/Cargo.toml") if (p.parent / "src").is_dir()
    )

    orphans: list[Path] = []
    problems: list[str] = []
    checked = 0
    for crate in crates:
        roots = crate_roots(crate)
        if not roots:
            continue
        reachable, crate_problems = reachable_from(roots)
        problems.extend(crate_problems)
        for src in tracked_sources(root, crate):
            checked += 1
            if src.resolve() not in {r.resolve() for r in reachable}:
                orphans.append(src)

    if problems:
        print("check-module-reachability: FAILED", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        return 1

    if orphans:
        print("check-module-reachability: FAILED", file=sys.stderr)
        print(
            "\nThese files are tracked under a crate's src/ but no `mod` declaration\n"
            "reaches them from the crate root. rustc, rustfmt and clippy all walk the\n"
            "same declarations, so each of these is invisible to the compiler, the\n"
            "formatter AND the linter at once — and its tests do not run while the\n"
            "suite still reports ok (#1750).\n"
            "\nRestore the `mod` declaration that used to point at it, or delete the\n"
            "file if it is genuinely dead.\n",
            file=sys.stderr,
        )
        for o in orphans:
            print(f"  {o.relative_to(root)}", file=sys.stderr)
        return 1

    print(
        f"check-module-reachability: OK — {checked} source file(s) across "
        f"{len(crates)} crate(s), all reachable from a crate root."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
