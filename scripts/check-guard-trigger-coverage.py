#!/usr/bin/env python3
"""Guard: the three steps `ci.yml`'s required job cannot reach for a
scripts-only diff still run on one, somewhere.

`ci.yml`'s required job skips `prose`, `hue-separation`, and
`transcript-surfaces` for a diff confined to Rust, scripts, or any path
outside its own prose carve-out (AGENTS.md names the skip). Today
`guard-self-tests.yml` runs all three with no `paths:` filter at all, so
they fire on every pull request no matter which files changed. Nothing kept
that true except a comment: a future edit could add a `paths:` filter to
that trigger, or narrow it to `docs/**`, and the three steps would go back to
running nowhere for a scripts-only pull request, quietly, because
`gate-parity` only asks whether a step runs in some workflow, never whether
that workflow's trigger reaches the files the step reads.

So this checks the trigger, not just the step. For each guard script named
below, it scans every workflow's `run:` lines for an invocation, then reads
that workflow's `pull_request:` trigger for a `paths:` or `paths-ignore:`
key. A guard passes when **at least one** workflow runs it with neither key
present under `pull_request:` — `hue-separation` runs a second, path-filtered
copy in `docs-guards.yml` too, and that copy does not have to be
unconditional for the guard to be satisfied, because the unconditional copy
in `guard-self-tests.yml` already is.

Only `pull_request:` is read. `merge_group` triggers admit no `paths:` key at
all on GitHub Actions, and a `push:` trigger fires after the merge decision
is already made, so it cannot be what blocks a red result from landing.

The YAML reading here is line-based and only correct for the shapes this
repository's own workflow files use — a mapping `on:` block with each event
as its own key, or the rare bare scalar/list form that cannot carry a
`paths:` key in the first place. It is not a general YAML parser, and does
not try to be one: `scripts/test-hermetic-guard-triggers.py` builds fixture
workflows to prove it still fails on both.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# The three steps AGENTS.md names as the ones `ci.yml`'s required job
# structurally cannot run for a prose-only diff. A guard added to that set
# later joins this list in the same change that documents it there.
WATCHED_GUARDS = (
    "check-prose.py",
    "check-hue-separation.py",
    "check-transcript-surfaces.py",
)

WORKFLOWS_DIR = "workflows"


def _indent(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def _is_blank_or_comment(line: str) -> bool:
    s = line.strip()
    return s == "" or s.startswith("#")


def extract_run_commands(lines: list[str]) -> list[str]:
    """Every command a `run:` step executes, inline or as a block scalar.

    Mirrors `scripts/check-gate-parity.sh`'s `workflow_commands()` so the two
    guards agree on what counts as "runs" — a `paths:` entry naming a script
    is a trigger watching that file, not an execution of it, and a shell
    comment inside a `run: |` block is prose about a command, not the
    command.
    """
    commands: list[str] = []
    block_indent: int | None = None
    for raw in lines:
        line = raw.rstrip("\n").rstrip("\r")
        if block_indent is not None:
            if line.strip() and _indent(line) > block_indent:
                body = line.strip()
                if not body.startswith("#"):
                    commands.append(body)
                continue
            block_indent = None
        m = re.match(r"^[ \t]*(?:-[ \t]+)?run:[ \t]*([|>].*)?$", line)
        if m and m.group(1) is not None:
            block_indent = _indent(line)
            continue
        m = re.match(r"^[ \t]*(?:-[ \t]+)?run:[ \t]*(\S.*)$", line)
        if m:
            commands.append(m.group(1).strip())
    return commands


def top_level_block(lines: list[str], key: str) -> tuple[bool, list[str], str]:
    """The lines nested under a `<key>:` line at column 0.

    Returns `(found, nested_lines, own_line)` — `own_line` is the `key:`
    line itself, stripped, so a caller can fall back to reading it as a bare
    scalar or flow list when the block is empty.
    """
    start = None
    for i, raw in enumerate(lines):
        line = raw.rstrip("\n")
        if _indent(line) != 0:
            continue
        if re.match(rf"^{re.escape(key)}:(\s|$)", line):
            start = i
            break
    if start is None:
        return False, [], ""
    block: list[str] = []
    for raw in lines[start + 1 :]:
        line = raw.rstrip("\n")
        if _is_blank_or_comment(line):
            block.append(raw)
            continue
        if _indent(line) == 0:
            break
        block.append(raw)
    return True, block, lines[start].rstrip("\n").strip()


def nested_block(lines: list[str], key: str) -> tuple[bool, list[str]]:
    """The lines nested under `<key>:` inside an already-nested block,
    using that key's own indentation as the reference column."""
    idx = None
    key_indent = None
    for i, raw in enumerate(lines):
        line = raw.rstrip("\n")
        if _is_blank_or_comment(line):
            continue
        if re.match(rf"^{re.escape(key)}:(\s|$)", line.strip()):
            idx = i
            key_indent = _indent(line)
            break
    if idx is None:
        return False, []
    sub: list[str] = []
    for raw in lines[idx + 1 :]:
        line = raw.rstrip("\n")
        if line.strip() == "":
            sub.append(raw)
            continue
        if _indent(line) <= key_indent:
            break
        sub.append(raw)
    return True, sub


def has_path_filter(lines: list[str]) -> bool:
    for raw in lines:
        line = raw.rstrip("\n").strip()
        if not line or line.startswith("#"):
            continue
        if re.match(r"^(paths|paths-ignore):", line):
            return True
    return False


def pull_request_is_unconditional(lines: list[str]) -> bool | None:
    """Whether this workflow's `pull_request` trigger carries no `paths:` or
    `paths-ignore:` key. `None` when the workflow never triggers on
    `pull_request` at all, which is a different gap from the one this guard
    watches — such a workflow provides no pull-request-time coverage,
    filtered or not."""
    found_on, on_block, on_own_line = top_level_block(lines, "on")
    if not found_on:
        return None
    found_pr, pr_block = nested_block(on_block, "pull_request")
    if found_pr:
        return not has_path_filter(pr_block)
    # The bare/flow form -- `on: pull_request` or `on: [pull_request, push]`
    # -- carries no per-event block to filter, so if it names the event at
    # all it is unconditional by construction.
    if not on_block and re.search(r"\bpull_request\b", on_own_line):
        return True
    return None


def main(argv: list[str]) -> int:
    # `--manifest-dir DIR` points the whole check at a fixture tree instead of
    # the real repository, the same convention `check-file-size.sh` and
    # `check-lockfile-sync.sh` use for their own hermetic self-tests.
    repo_root = Path(__file__).resolve().parent.parent
    if len(argv) >= 2 and argv[0] == "--manifest-dir":
        repo_root = Path(argv[1])
    workflows_dir = repo_root / ".github" / WORKFLOWS_DIR
    if not workflows_dir.is_dir():
        print(
            "check-guard-trigger-coverage: FAIL -- "
            f"{workflows_dir} does not exist, so no workflow trigger can be read."
        )
        return 1

    # guard -> list of (workflow name, unconditional | None)
    coverage: dict[str, list[tuple[str, bool | None]]] = {g: [] for g in WATCHED_GUARDS}

    for wf_path in sorted(workflows_dir.glob("*.yml")):
        lines = wf_path.read_text(encoding="utf-8").splitlines(keepends=True)
        commands = extract_run_commands(lines)
        guards_here = [g for g in WATCHED_GUARDS if any(g in cmd for cmd in commands)]
        if not guards_here:
            continue
        unconditional = pull_request_is_unconditional(lines)
        for g in guards_here:
            coverage[g].append((wf_path.name, unconditional))

    fail = False
    lines_out: list[str] = []
    for guard in WATCHED_GUARDS:
        runners = coverage[guard]
        if not runners:
            fail = True
            lines_out.append(f"  {guard}: no workflow runs it at all.")
            continue
        if any(ok is True for _name, ok in runners):
            continue
        fail = True
        detail = ", ".join(
            f"{name} ({'no pull_request trigger' if u is None else 'paths-restricted'})"
            for name, u in runners
        )
        lines_out.append(
            f"  {guard}: every workflow that runs it restricts pull_request by path: {detail}."
        )

    if fail:
        print("check-guard-trigger-coverage: FAIL -- a scripts-only pull request would not run:")
        for line in lines_out:
            print(line)
        print(
            "  Add or restore an unfiltered `pull_request:` trigger in the workflow that "
            "runs the guard, so it fires on every pull request regardless of which paths changed."
        )
        return 1

    covered = sorted(WATCHED_GUARDS)
    print(
        "check-guard-trigger-coverage: OK -- "
        f"{len(covered)} guard(s) each run unconditionally on pull_request in some workflow: "
        f"{', '.join(covered)}."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
