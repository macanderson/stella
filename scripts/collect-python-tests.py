#!/usr/bin/env python3
"""Collect Python test names at one git ref, for check-deleted-tests.sh.

`scripts/check-deleted-tests.sh` names any `#[test]` that a merge drops, but
it never looked at Python. This script gives it the other half: every
`def test_*` function or method, anywhere under a tracked `*.py` file, at a
given ref.

An AST walk, not a regex. A regex both over- and under-matches once
decorators, `@pytest.mark.parametrize`, and nested classes are in play;
`bench/harbor_adapter/tests/test_manifest_parity.py` already parses Python
source with `ast` in this repository, so this follows that precedent rather
than inventing a new one.

Usage:

    collect-python-tests.py <git-ref>

Prints one test name per line, sorted and de-duplicated, to stdout. Exits 0
on an ordinary result, including a ref with no Python files at all -- most
fixtures `scripts/test-deleted-tests.sh` builds are Rust-only, and that is a
legitimate tree, not a bug. Exits 3 when the ref carries at least one file
named the way pytest expects a test file to be named (`test_*.py` or
`*_test.py`) but the walk found zero test names anywhere in the tree: that
combination means this script's own glob or parser broke, not that the tree
has no tests, and the caller must not read it as an empty, passing scan --
the same anti-vacuity requirement this repository's own parity sweep already
enforces for its check. Exits 2 on a usage error, and 1 if git itself fails.

A name is counted wherever it sits, including inside a `Test*` class --
`bench/harbor_adapter/tests/test_manifest_parity.py`'s own
`TestSweepAntiVacuity` holds several `def test_*` methods, and each still
counts on its own, the same way a Rust `#[test]` inside an `impl` block
does. The one known miss, and it is the direct match of the Rust guard's own
`#[test]`-in-a-string-literal miss: a helper that merely defines a string
containing `def test_foo` is not distinguished from a real one by this walk,
because the walk only sees what `ast.parse` sees, and a string is opaque to
it. That miss is symmetric across both trees being compared, so it costs
nothing unless the fixture itself is edited.
"""
from __future__ import annotations

import ast
import subprocess
import sys

_TEST_FUNCTION_PREFIX = "test_"


def _run_git(args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        capture_output=True,
        text=True,
    )


def _python_files_at(ref: str) -> list[str]:
    # `git ls-tree`'s own glob pathspec (`-- '*.py'`) does not recurse into
    # subdirectories the way `git grep`'s does, so it silently returns zero
    # matches for a `.py` file anywhere but the repository root. Filtering in
    # Python after listing every tracked path sidesteps that quirk instead of
    # relying on it.
    result = _run_git(["ls-tree", "-r", "--name-only", ref])
    if result.returncode != 0:
        raise RuntimeError(
            f"git ls-tree failed for {ref!r}: {result.stderr.strip()}"
        )
    return [
        line
        for line in result.stdout.splitlines()
        if line.endswith(".py")
    ]


def _looks_like_a_test_file(path: str) -> bool:
    """True for the two file names pytest discovers tests in by default."""
    name = path.rsplit("/", 1)[-1]
    return (name.startswith("test_") and name.endswith(".py")) or name.endswith("_test.py")


def _source_at(ref: str, path: str) -> str | None:
    result = _run_git(["show", f"{ref}:{path}"])
    if result.returncode != 0:
        return None
    return result.stdout


def _test_names_in(source: str, path: str) -> list[str]:
    try:
        tree = ast.parse(source, filename=path)
    except SyntaxError:
        # A file that does not parse cannot be walked. It is reported as a
        # scan gap, not silently skipped, so a build artifact or a fixture
        # written in another language under a `.py` name cannot hide a real
        # test loss behind it.
        print(f"collect-python-tests: note: could not parse {path}, skipped", file=sys.stderr)
        return []
    names: list[str] = []
    for node in ast.walk(tree):
        is_function = isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        if is_function and node.name.startswith(_TEST_FUNCTION_PREFIX):
            names.append(node.name)
    return names


def collect(ref: str) -> tuple[list[str], bool]:
    """Return (sorted unique test names, whether the ref looked test-bearing)."""
    files = _python_files_at(ref)
    test_bearing = any(_looks_like_a_test_file(path) for path in files)
    names: set[str] = set()
    for path in files:
        source = _source_at(ref, path)
        if source is None:
            continue
        names.update(_test_names_in(source, path))
    return sorted(names), test_bearing


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("collect-python-tests: usage: collect-python-tests.py <git-ref>", file=sys.stderr)
        return 2
    ref = argv[1]
    try:
        names, test_bearing = collect(ref)
    except RuntimeError as exc:
        print(f"collect-python-tests: {exc}", file=sys.stderr)
        return 1
    for name in names:
        print(name)
    if test_bearing and not names:
        print(
            f"collect-python-tests: {ref} has a test-named Python file "
            "(test_*.py or *_test.py) but the walk found zero test "
            "functions. That is this script's own glob or parser breaking, "
            "not an empty tree.",
            file=sys.stderr,
        )
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
