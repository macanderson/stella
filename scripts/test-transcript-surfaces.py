#!/usr/bin/env python3
"""Directions `scripts/check-transcript-surfaces.py` must fail in.

A guard is only worth its place in the gate if its failures are demonstrated.
This one exists because a claim ("every surface renders the same") was believed
without being checked, so shipping it unexercised would repeat the mistake at
one remove: a green `make transcript-surfaces` would prove the ledger is
*self-consistent*, not that the guard can tell a true ledger from a false one.

Each case below builds a synthetic tree, drives `check()` against it, and
asserts on the message — hermetic, no network, no cargo. Not part of `make
gate`; run it with `make transcript-surfaces-test`.
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "check_transcript_surfaces", HERE / "check-transcript-surfaces.py"
)
assert spec and spec.loader
guard = importlib.util.module_from_spec(spec)
# Registered before execution: `@dataclass` resolves the defining module out of
# `sys.modules`, so a module loaded by path alone raises while processing its
# own class definitions.
sys.modules[spec.name] = guard
spec.loader.exec_module(guard)

Surface = guard.Surface
GRID = guard.GRID
HTML = guard.HTML

FAILURES: list[str] = []


def scenario(name: str):
    """Register a case; a raised AssertionError is recorded, not fatal."""

    def wrap(fn):
        try:
            fn()
            print(f"  ok   {name}")
        except AssertionError as exc:
            FAILURES.append(f"{name}: {exc}")
            print(f"  FAIL {name}: {exc}")
        return fn

    return wrap


def tree(files: dict[str, str]) -> Path:
    """A git-tracked synthetic workspace holding exactly `files`."""
    root = Path(tempfile.mkdtemp(prefix="transcript-surfaces-"))
    for rel, body in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")
    subprocess.run(["git", "init", "-q"], cwd=root, check=True)
    subprocess.run(["git", "add", "-A"], cwd=root, check=True)
    return root


CALLS_GRID = "fn draw() { let _ = grid::render(&run, &state, 100); }\n"
NO_CALL = "fn draw() { self.paint_my_own_way(); }\n"
MENTIONS_ONLY = "//! See [`stella_transcript::grid::render`].\nfn draw() { mine(); }\n"

# A file that paints its own way and calls the shared renderer ONLY from its
# inline test module — the shape #5083 was filed about. `production_sources`
# drops a sibling `tests.rs` by path and cannot see this one.
TEST_ONLY_CALL = (
    "fn draw() { self.paint_my_own_way(); }\n"
    "\n"
    "#[cfg(test)]\n"
    "mod tests {\n"
    "    use super::*;\n"
    "    #[test]\n"
    "    fn renders() {\n"
    "        let _ = grid::render(&run, &state, 100);\n"
    "    }\n"
    "}\n"
)

# The same, under the two other spellings this tree uses.
TEST_ONLY_CALL_ALL = TEST_ONLY_CALL.replace("#[cfg(test)]", "#[cfg(all(test, unix))]")
TEST_ONLY_CALL_ATTR = TEST_ONLY_CALL.replace(
    "#[cfg(test)]", '#[cfg_attr(test, allow(clippy::all))]'
)

# Ships a call AND has a test module that also calls: still adoption.
SHIPS_AND_TESTS = CALLS_GRID + TEST_ONLY_CALL.split("fn draw() { self.paint_my_own_way(); }\n")[1]


def only(problems: list[str], needle: str) -> None:
    assert any(needle in p for p in problems), f"expected {needle!r} in {problems}"


@scenario("a SHARED row whose file stopped calling the renderer fails")
def _():
    root = tree({"a/src/lib.rs": NO_CALL})
    rows = [Surface("a", "a/src/lib.rs", GRID, True, None, "")]
    problems = guard.check(root, rows, [])
    only(problems, "declared SHARED but")


@scenario("a SHARED row whose file does call the renderer passes")
def _():
    root = tree({"a/src/lib.rs": CALLS_GRID})
    rows = [Surface("a", "a/src/lib.rs", GRID, True, None, "")]
    assert guard.check(root, rows, []) == [], "a correct ledger must be silent"


@scenario("an OWN row that quietly adopted the renderer fails, naming its issue")
def _():
    root = tree({"a/src/lib.rs": CALLS_GRID})
    rows = [Surface("a", "a/src/lib.rs", GRID, False, 1234, "")]
    problems = guard.check(root, rows, [])
    only(problems, "declared OWN (#1234) but")
    only(problems, "close #1234")


@scenario("an OWN row with no issue fails — a gap must have an address")
def _():
    root = tree({"a/src/lib.rs": NO_CALL})
    rows = [Surface("a", "a/src/lib.rs", GRID, False, None, "")]
    only(guard.check(root, rows, []), "must cite the issue")


@scenario("a SHARED row carrying a stale issue fails")
def _():
    root = tree({"a/src/lib.rs": CALLS_GRID})
    rows = [Surface("a", "a/src/lib.rs", GRID, True, 99, "")]
    only(guard.check(root, rows, []), "SHARED rows carry no issue")


@scenario("an undeclared file that calls the renderer fails")
def _():
    root = tree({"a/src/lib.rs": CALLS_GRID, "b/src/lib.rs": CALLS_GRID})
    rows = [Surface("a", "a/src/lib.rs", GRID, True, None, "")]
    only(guard.check(root, rows, []), "b/src/lib.rs calls")


@scenario("a call made only from an inline #[cfg(test)] module is not adoption")
def _():
    root = tree({"a/src/lib.rs": TEST_ONLY_CALL})
    rows = [Surface("a", "a/src/lib.rs", GRID, False, 1234, "")]
    assert guard.check(root, rows, []) == [], (
        "an OWN row whose only call is in its test module must stay OWN (#5083)"
    )


@scenario("the false-negative direction: a SHARED row cannot be held up by a test-only call")
def _():
    # The dangerous half of #5083. A surface that regressed to its own painter
    # but kept one call in its test module used to satisfy rule 1, which is the
    # "the crate exists and two surfaces use it" versus "every surface renders
    # the same" confusion this guard exists to end.
    root = tree({"a/src/lib.rs": TEST_ONLY_CALL})
    rows = [Surface("a", "a/src/lib.rs", GRID, True, None, "")]
    only(guard.check(root, rows, []), "declared SHARED but")


@scenario("cfg(all(test, …)) and cfg_attr(test, …) are stripped too")
def _():
    for body in (TEST_ONLY_CALL_ALL, TEST_ONLY_CALL_ATTR):
        root = tree({"a/src/lib.rs": body})
        rows = [Surface("a", "a/src/lib.rs", GRID, False, 1234, "")]
        assert guard.check(root, rows, []) == [], f"not stripped: {body[:40]!r}"


@scenario("a file that ships a call keeps it, test module or not")
def _():
    root = tree({"a/src/lib.rs": SHIPS_AND_TESTS})
    rows = [Surface("a", "a/src/lib.rs", GRID, True, None, "")]
    assert guard.check(root, rows, []) == [], (
        "stripping the test module must not strip the shipping call above it"
    )


@scenario("a doc comment naming the renderer is prose, not adoption")
def _():
    root = tree({"a/src/lib.rs": MENTIONS_ONLY})
    rows = [Surface("a", "a/src/lib.rs", GRID, False, 1234, "")]
    assert guard.check(root, rows, []) == [], "a doc mention must not read as a call"


@scenario("a test file that calls the renderer is not a surface")
def _():
    root = tree({"a/src/lib.rs": NO_CALL, "a/src/tests.rs": CALLS_GRID})
    rows = [Surface("a", "a/src/lib.rs", GRID, False, 1234, "")]
    assert guard.check(root, rows, []) == [], "tests.rs must not count as a caller"


@scenario("a moved or deleted surface file fails")
def _():
    root = tree({"a/src/lib.rs": NO_CALL})
    rows = [Surface("a", "a/src/gone.rs", GRID, True, None, "")]
    only(guard.check(root, rows, []), "does not exist")


@scenario("a crate depending on stella-transcript with no row fails")
def _():
    root = tree(
        {
            "a/src/lib.rs": CALLS_GRID,
            "a/Cargo.toml": "[dependencies]\n",
            "c/Cargo.toml": "[dependencies]\nstella-transcript.workspace = true\n",
            "c/src/lib.rs": NO_CALL,
        }
    )
    rows = [Surface("a", "a/src/lib.rs", GRID, True, None, "")]
    only(guard.check(root, rows, []), "owns no row")


@scenario("a retired foreign port fails rather than passing silently")
def _():
    root = tree({"a/src/lib.rs": CALLS_GRID})
    rows = [Surface("a", "a/src/lib.rs", GRID, True, None, "")]
    problems = guard.check(root, rows, [("port", "ui/port.tsx", 777, "")])
    only(problems, "is gone")


@scenario("the html entry point is checked on the same terms as the grid one")
def _():
    root = tree({"a/src/lib.rs": "fn p() { html::render_page(&run, &state) }\n"})
    rows = [Surface("a", "a/src/lib.rs", HTML, False, 1234, "")]
    only(guard.check(root, rows, []), "declared OWN (#1234) but")


if FAILURES:
    print(f"\n{len(FAILURES)} scenario(s) failed", file=sys.stderr)
    sys.exit(1)
print("\ntranscript-surfaces guard: every failure direction demonstrated")
