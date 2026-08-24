#!/usr/bin/env python3
"""Guard: every transcript surface declares whether it renders through the
shared renderer, and the declaration is checked against the tree.

`crates/stella-transcript` was extracted so that one session transcript would
render the same way everywhere (#3764). Extracting it proved nothing on its
own, and that gap is not hypothetical — #3578 ("render the Command Deck's
session view through stella-transcript") was closed as *completed* twice while
`grid::render` had zero callers inside `stella-tui`, and the second close was a
manual one with no commit attached. Nothing in the gate could tell "the shared
crate exists and two surfaces use it" from "every surface renders the same",
because both look identical from the outside: the crate builds, its own tests
pass, and the surfaces that never adopted it are simply absent from the
evidence.

So the fact this guard checks is *adoption*, and it checks it from both sides,
the same shape `stella-model`'s provider-parity matrix and
`stella-protocol`'s event-consumer ledger use:

  - a `SHARED` row names a file that must really reference the shared entry
    point, so a surface silently regressing to its own painter goes red;
  - an `OWN` row names a file that must NOT reference it and must cite the
    issue where the gap is being decided, so a divergence is a declared,
    tracked gap rather than a silence nobody noticed;
  - the caller sets must match **exactly**, so a new surface cannot wire
    itself in without declaring itself, and a row cannot go stale in either
    direction;
  - every crate that depends on `stella-transcript` must own at least one row,
    so a new consumer crate cannot appear undeclared.

An `OWN` row is not permission. It is a debt with an address on it. The
`Done when` of each cited issue is written as "…and this row reads SHARED".

── Deliberately not checked ─────────────────────────────────────────────────

Whether the *output* of two SHARED surfaces actually looks the same. That is a
rendering property and belongs in a test that renders one fixture through both
paths (each cited issue names the one it needs). This guard answers the
narrower question that kept being answered wrongly: does this surface call the
shared renderer at all.

Aliased imports (`use stella_transcript::grid::render;` then a bare `render()`)
are not recognised. Both shipping callers qualify the path, which is the
neighbourhood idiom; a surface that stops qualifying will fail this guard and
should re-qualify rather than teach the guard to parse Rust.
"""

from __future__ import annotations

import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The crate that *defines* the renderers. Its own tests and examples are not
# surfaces, so nothing under it counts as a caller.
RENDERER_CRATE = "crates/stella-transcript/"

# The shared entry points a surface is expected to draw through.
GRID = "stella_transcript::grid::render{,_turn_lines}"
HTML = "stella_transcript::html::render_{page,run,turn_tail}"

# What a reference to each entry point looks like in source, once comments are
# stripped. The `use stella_transcript::grid;` form means a call site reads
# `grid::render(...)`, so the crate-qualified prefix cannot be required.
#
# `render` and `render_turn_lines` both count, because both are the shared
# painter: the first draws a finished run, the second one turn of a run that is
# still going. A live surface has to use the second — an append-only one cannot
# redraw a whole run per event — and refusing to count it would have read a
# surface's move *onto* the streaming API as a regression away from sharing.
# `render_page`, `render_run` and `render_turn_tail` all count, mirroring the
# grid pair: the first two draw a standalone document and a fragment a host
# page embeds; `render_turn_tail` (#4566) draws only the tail a live poll has
# not seen yet, HTML's counterpart to grid's `render_turn_lines`. The
# Observatory moved from the page to the fragment when its standalone
# `/transcript` route was consolidated into the turn page, and later grew the
# tail call for its live poll — refusing to count either would have read a
# surface's own move further onto the shared renderer as a regression away
# from sharing it.
PATTERNS = {
    GRID: re.compile(r"\bgrid::render(_turn_lines)?\s*\("),
    HTML: re.compile(r"\bhtml::render_(page|run|turn_tail)\s*\("),
}


@dataclass(frozen=True)
class Surface:
    """One place in this workspace that draws a session transcript."""

    ident: str
    path: str
    entry: str
    shared: bool
    #: The issue deciding the gap. Required for an OWN row, forbidden on SHARED.
    issue: int | None
    note: str


# ── The ledger ───────────────────────────────────────────────────────────────
#
# One row per surface that renders a session transcript to a human. Adding a
# surface means adding a row; there is no default.
SURFACES: list[Surface] = [
    Surface(
        ident="plain",
        path="crates/stella-cli/src/plain/transcript.rs",
        entry=GRID,
        shared=True,
        issue=None,
        note="`stella run` on a terminal and the bytes a daemon child writes "
        "to its console file are the same code path.",
    ),
    Surface(
        ident="observatory",
        path="crates/stella-observatory/src/lib.rs",
        entry=HTML,
        shared=True,
        issue=None,
        note="`stella observe` -> the turn page embeds /api/transcript-html.",
    ),
    Surface(
        ident="deck-v1",
        path="crates/stella-tui/src/render/entry.rs",
        entry=GRID,
        shared=False,
        issue=3578,
        note="The Command Deck's session view. Paints from its own "
        "`TranscriptEntry` (28 variants) through `render/entry.rs`, "
        "`render/row.rs` and `diff.rs`; takes only `syntax::body_paint` and "
        "`tabs::expand` from the shared crate.",
    ),
    Surface(
        ident="deck-v2",
        path="crates/stella-tui/src/v2/transcript.rs",
        entry=GRID,
        shared=False,
        issue=4289,
        note="The v2 deck's SPEC 6 frame. The design question is settled — "
        "SPEC 6 is the one frame and `grid.rs` is changed to draw it, not the "
        "other way round (#4271, recorded in stella-transcript's charter). "
        "What is left is the migration, and its first step is a correctness "
        "precondition rather than wiring: `model` has no notion of an "
        "*unmeasured* row, so moving the deck across before that lands would "
        "reintroduce the fabricated `+0 -0` its `Option` fields exist to "
        "prevent.",
    ),
    Surface(
        ident="export-html",
        path="crates/stella-cli/src/export/transcript.rs",
        entry=HTML,
        shared=False,
        issue=4270,
        note="The `/export` archive's readable half. Folds "
        "`Store::session_events` itself and emits its own HTML; carries the "
        "#817 redaction pass the shared renderer cannot host.",
    ),
]

# Surfaces outside the Rust workspace, tracked here so the ledger is the whole
# answer to "how many ways does this repository draw a transcript". Not
# mechanically checked against an entry point — they have no Rust to reference
# one — but the file must exist, so retiring the port makes this row fail.
# Empty since the ejection (#2380): ArenaBench's hand-maintained TypeScript
# port left with its repository (https://github.com/macanderson/arenabench),
# which now guards its own parity against vendored golden matrices.
FOREIGN: list[tuple[str, str, int, str]] = []


def production_sources(repo: Path = REPO) -> list[Path]:
    """Every tracked Rust file that ships, excluding tests and examples."""
    out = subprocess.run(
        ["git", "ls-files", "*.rs"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.splitlines()
    keep = []
    for rel in out:
        if rel.startswith(RENDERER_CRATE):
            continue
        parts = Path(rel).parts
        if "tests" in parts or "examples" in parts or "benches" in parts:
            continue
        if Path(rel).name == "tests.rs":
            continue
        keep.append(Path(rel))
    return keep


LINE_COMMENT = re.compile(r"//.*$", re.MULTILINE)


def references(path: Path, entry: str, repo: Path = REPO) -> bool:
    """Whether `path` calls `entry`, ignoring anything inside a line comment.

    Comments are stripped because a doc comment naming the renderer is prose,
    not adoption — `plain/transcript.rs` explains the renderer in its header
    and calls it in its body, and only the second one is the fact this guard
    is about.
    """
    try:
        text = (repo / path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return False
    return bool(PATTERNS[entry].search(LINE_COMMENT.sub("", text)))


def dependent_crates(repo: Path = REPO) -> set[str]:
    """Crate directories whose manifest depends on `stella-transcript`."""
    out = subprocess.run(
        ["git", "ls-files", "*/Cargo.toml"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.splitlines()
    deps = set()
    for rel in out:
        if rel.startswith(RENDERER_CRATE):
            continue
        text = (repo / rel).read_text(encoding="utf-8", errors="replace")
        if re.search(r"^stella-transcript\b", text, re.MULTILINE):
            deps.add(str(Path(rel).parent) + "/")
    return deps


def check(
    repo: Path = REPO,
    surfaces: list[Surface] | None = None,
    foreign: list[tuple[str, str, int, str]] | None = None,
) -> list[str]:
    """Every disagreement between the ledger and the tree, as prose.

    Parameterised on the tree and the ledger so `scripts/test-transcript-surfaces.py`
    can drive it against synthetic ones. A guard whose failure directions are
    never exercised is a guard nobody has evidence about — which is the same
    mistake, one level up, that this whole file is here to stop repeating.
    """
    surfaces = SURFACES if surfaces is None else surfaces
    foreign = FOREIGN if foreign is None else foreign
    problems: list[str] = []

    # 1. Each row's own claim.
    for s in surfaces:
        exists = (repo / s.path).exists()
        if not exists:
            problems.append(
                f"{s.ident}: {s.path} does not exist. A surface was moved or "
                f"retired without updating this ledger."
            )
            continue
        calls = references(Path(s.path), s.entry, repo)
        if s.shared and not calls:
            problems.append(
                f"{s.ident}: declared SHARED but {s.path} does not call "
                f"{s.entry}. Either the surface regressed to its own painter "
                f"— fix that, it is the whole point — or the call moved to "
                f"another file and this row must name it."
            )
        if not s.shared and calls:
            problems.append(
                f"{s.ident}: declared OWN (#{s.issue}) but {s.path} now calls "
                f"{s.entry}. If the surface was migrated, flip this row to "
                f"SHARED, drop the issue, and close #{s.issue}."
            )
        if s.shared and s.issue is not None:
            problems.append(
                f"{s.ident}: SHARED rows carry no issue; #{s.issue} is either "
                f"stale or this row should be OWN."
            )
        if not s.shared and s.issue is None:
            problems.append(
                f"{s.ident}: OWN rows must cite the issue deciding the gap. "
                f"A divergence with no address is the silence this guard exists "
                f"to prevent."
            )

    # 2. Totality: no undeclared caller. The other direction — a SHARED row
    # whose file does not call the entry point — is rule 1's job, so it is not
    # restated here; a doubled message reads as two defects.
    sources = production_sources(repo)
    for entry in (GRID, HTML):
        found = {str(p) for p in sources if references(p, entry, repo)}
        declared = {s.path for s in surfaces if s.entry == entry and s.shared}
        for extra in sorted(found - declared):
            problems.append(
                f"{extra} calls {entry} but declares no row. Add it to "
                f"SURFACES as SHARED — a transcript surface that nothing "
                f"declares is one nothing can hold to the charter."
            )

    # 3. No undeclared consumer crate.
    owners = {s.path for s in surfaces}
    for crate in sorted(dependent_crates(repo)):
        if not any(path.startswith(crate) for path in owners):
            problems.append(
                f"{crate} depends on stella-transcript but owns no row in "
                f"SURFACES. Declare what it draws and how."
            )

    # 4. Foreign ports still exist where the ledger says.
    for ident, path, issue, _note in foreign:
        if not (repo / path).exists():
            problems.append(
                f"{ident}: {path} is gone. If the port was retired, drop the "
                f"row and close #{issue}."
            )

    return problems


def main() -> int:
    problems = check()
    if problems:
        print("transcript-surfaces: the ledger and the tree disagree.\n", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        print(
            "\nThe ledger is scripts/check-transcript-surfaces.py; the charter "
            "it enforces is crates/stella-transcript/src/lib.rs.",
            file=sys.stderr,
        )
        return 1

    shared = sum(1 for s in SURFACES if s.shared)
    own = len(SURFACES) - shared
    print(
        f"transcript-surfaces: {shared} shared, {own} declared gaps, "
        f"{len(FOREIGN)} foreign port(s) — ledger matches the tree."
    )
    for s in SURFACES:
        if not s.shared:
            print(f"    gap: {s.ident} -> #{s.issue} ({s.path})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
