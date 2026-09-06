#!/usr/bin/env python3
"""Guard: the steps `ci.yml`'s required job cannot reach for some diffs
still run on one, somewhere.

`ci.yml`'s required job skips `prose`, `hue-separation`, and
`transcript-surfaces` for a diff confined to Rust, scripts, or any path
outside its own prose carve-out (AGENTS.md names the skip). Today
`guard-self-tests.yml` runs all three with no `paths:` filter at all, so
they fire on every pull request no matter which files changed. Nothing kept
that true except a comment: a future edit could add a `paths:` filter to
that trigger, or narrow it to `docs/**`, and those steps would go back to
running nowhere for a scripts-only pull request, quietly, because
`gate-parity` only asks whether a step runs in some workflow, never whether
that workflow's trigger reaches the files the step reads.

So this checks the trigger, not just the step — and not the trigger alone
either. A job can be skipped for reasons its workflow's trigger never shows:
`ci.yml`'s `check` job carries `if: needs.changes.outputs.rust != 'false'`,
reading an output a `changes` job set from the diff, so a step inside it can
still run nowhere for a prose-only pull request even though `ci.yml`'s own
`pull_request:` trigger has no `paths:` key at all — that filter moved out of
the trigger and into the job condition so the required contexts keep
reporting for a job GitHub skips rather than fails. A guard whose only
invocation sits in a job gated that way is exactly as uncovered as one
sitting behind a `paths:` filter — the check has to read both.

For each guard script named below, this scans every workflow's jobs for an
invocation, then asks, for the job it sits in: does the workflow's
`pull_request:` trigger carry a `paths:`/`paths-ignore:` key, and does the
job's own `if:` read a `needs.*.outputs.*` value? A guard passes when **at
least one** workflow runs it from a job that neither gate touches —
`hue-separation` runs a second, path-filtered copy in `docs-guards.yml`, and
a third inside `ci.yml`'s gated `check` job, and neither has to be
unconditional for the guard to be satisfied, because the unconditional copy
in `guard-self-tests.yml`'s `gate-steps` job already is.

Only `pull_request:` is read for the trigger half. `merge_group` triggers
admit no `paths:` key at all on GitHub Actions, and a `push:` trigger fires
after the merge decision is already made, so it cannot be what blocks a red
result from landing.

The YAML reading here is line-based and only correct for the shapes this
repository's own workflow files use — a mapping `on:` block with each event
as its own key, or the rare bare scalar/list form that cannot carry a
`paths:` key in the first place; a job-level `if:` read as the one line
sitting at the job body's own indentation, never a step's. It is not a
general YAML parser, and does not try to be one:
`scripts/test-guard-trigger-coverage.py` builds a fixture workflow in each
of those shapes and runs this guard against it, so no reading is left to a
claim in this docstring.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# Steps `ci.yml`'s required job cannot run for a prose-only diff, plus any
# other guard this check now watches. Add a new guard here and in AGENTS.md
# in the same change.
WATCHED_GUARDS = (
    "check-prose.py",
    "check-hue-separation.py",
    "check-transcript-surfaces.py",
    "check-closing-keywords.py",
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


def parse_jobs(lines: list[str]) -> dict[str, list[str]]:
    """Every job in this workflow's `jobs:` block, mapped to its own lines.

    Splits the block the same way `top_level_block` splits the whole file:
    the first non-blank, non-comment line under `jobs:` fixes the column
    every job name sits at, and each job's lines run until the next name at
    that same column.
    """
    found, jobs_block, _ = top_level_block(lines, "jobs")
    if not found:
        return {}
    job_indent: int | None = None
    for raw in jobs_block:
        line = raw.rstrip("\n")
        if _is_blank_or_comment(line):
            continue
        job_indent = _indent(line)
        break
    if job_indent is None:
        return {}
    jobs: dict[str, list[str]] = {}
    name: str | None = None
    body: list[str] = []
    for raw in jobs_block:
        line = raw.rstrip("\n")
        if not _is_blank_or_comment(line) and _indent(line) == job_indent:
            if name is not None:
                jobs[name] = body
            m = re.match(r"^[ \t]*([A-Za-z0-9_.-]+):", line)
            name = m.group(1) if m else None
            body = []
            continue
        if name is not None:
            body.append(raw)
    if name is not None:
        jobs[name] = body
    return jobs


def job_if_expression(job_lines: list[str]) -> str | None:
    """This job's own `if:` value, or `None`.

    Only a line at the job body's own first indentation counts. A `steps:`
    list sits at that same column, but each step lives one or more levels
    deeper as a list item, so a step's `if:` is never read as the job's.
    """
    body_indent: int | None = None
    for raw in job_lines:
        line = raw.rstrip("\n")
        if _is_blank_or_comment(line):
            continue
        body_indent = _indent(line)
        break
    if body_indent is None:
        return None
    for raw in job_lines:
        line = raw.rstrip("\n")
        if _is_blank_or_comment(line) or _indent(line) != body_indent:
            continue
        m = re.match(r"^if:[ \t]*(.*)$", line.strip())
        if m:
            return m.group(1).strip()
    return None


# This is the shape `ci.yml`'s `check` job uses. That job runs only when an
# earlier `changes` job says the diff touches its area, e.g.
# `if: needs.changes.outputs.rust != 'false'`. A step in that job is not
# unconditional just because the trigger has no `paths:` key. The job can
# still skip itself for a diff outside its area.
_DIFF_GATE_RE = re.compile(r"needs\.[A-Za-z0-9_-]+\.outputs\.[A-Za-z0-9_-]+")


def job_diff_gate(job_lines: list[str]) -> str | None:
    """This job's own `if:` text, when it reads a `needs.*.outputs.*` value.

    `None` when the job has no `if:`, or one that cannot vary with which
    files a diff touches (`github.event_name == 'pull_request'`, say).
    """
    expr = job_if_expression(job_lines)
    if expr and _DIFF_GATE_RE.search(expr):
        return expr
    return None


# guard -> list of (workflow name, job name, status, gate expression).
# status is one of:
#   "ok"     -- unfiltered trigger, job not diff-gated: this covers it.
#   "paths"  -- the trigger itself carries paths:/paths-ignore:.
#   "gated"  -- the trigger is unfiltered, but the job's own `if:` reads a
#               needs.*.outputs.* value. See the shape above.
#   "no_pr"  -- the workflow never triggers on pull_request at all.
Runner = tuple[str, str, str, str | None]


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

    coverage: dict[str, list[Runner]] = {g: [] for g in WATCHED_GUARDS}

    for wf_path in sorted(workflows_dir.glob("*.yml")):
        lines = wf_path.read_text(encoding="utf-8").splitlines(keepends=True)
        trigger_unconditional = pull_request_is_unconditional(lines)
        for job_name, job_lines in parse_jobs(lines).items():
            commands = extract_run_commands(job_lines)
            guards_here = [g for g in WATCHED_GUARDS if any(g in cmd for cmd in commands)]
            if not guards_here:
                continue
            if trigger_unconditional is None:
                status, gate = "no_pr", None
            elif trigger_unconditional is False:
                status, gate = "paths", None
            else:
                gate = job_diff_gate(job_lines)
                status = "gated" if gate else "ok"
            for g in guards_here:
                coverage[g].append((wf_path.name, job_name, status, gate))

    fail = False
    lines_out: list[str] = []
    for guard in WATCHED_GUARDS:
        runners = coverage[guard]
        if not runners:
            fail = True
            lines_out.append(f"  {guard}: no workflow runs it at all.")
            continue
        if any(status == "ok" for _wf, _job, status, _gate in runners):
            continue
        fail = True
        parts = []
        for wf, job, status, gate in runners:
            if status == "no_pr":
                parts.append(f"{wf} (no pull_request trigger)")
            elif status == "paths":
                parts.append(f"{wf} (paths-restricted)")
            else:  # "gated"
                parts.append(f"{wf}:{job} (job `{job}` skipped when `if: {gate}` is false)")
        detail = ", ".join(parts)
        lines_out.append(
            f"  {guard}: every invocation is filtered or conditionally skipped: {detail}."
        )

    if fail:
        print("check-guard-trigger-coverage: FAIL -- a scripts-only pull request would not run:")
        for line in lines_out:
            print(line)
        print(
            "  Add or restore an invocation that runs from an unfiltered `pull_request:` "
            "trigger, in a job whose own `if:` cannot read a `needs.*.outputs.*` value, so "
            "the guard fires on every pull request regardless of which paths changed."
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
