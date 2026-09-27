#!/usr/bin/env python3
"""Guard: the release-publish step still survives a mid-step 5xx.

`v0.9.254` and `v0.9.264` each finished every build, uploaded every asset to
a release object `softprops/action-gh-release` had just created, and then
got a GitHub 5xx on the same step. Both tags sat as an unpublished draft for
a week before anyone noticed and re-ran them by hand. The `Upload
build artifact` step earlier in this workflow survives the same shape of
failure — `id: upload`, `continue-on-error: true`, and a second step gated
on `steps.upload.outcome == 'failure'` — and this checks that the
`Create / update GitHub Release` step now does too.

Four properties, because any one of them silently reopens the gap:

1. The first attempt is `id`'d and carries `continue-on-error: true`, or
   its own failure ends the job before a retry can run.
2. A second step named `Create / update GitHub Release (retry)` exists
   after it, gated on `steps.<that id>.outcome == 'failure'`. A stray
   `if:` naming a different step's id never fires.
3. Both steps pin the same `uses:`. A dependency bump that touches one
   occurrence and not the other would retry with a different version of
   the action than the one that just failed.
4. Both steps carry the same `with:` block. A retry that drops
   `fail_on_unmatched_files` or narrows `files:` would publish an
   incomplete release quietly instead of failing loudly the way a first
   attempt with the same input would.

Text, not YAML, for the reason `check-release-wiring.py` gives: this has to
run on a bare runner with no PyYAML, and the shape in question is a run of
plain-mapping step entries a parser would flatten anyway.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

WORKFLOW = Path(".github/workflows/release.yml")

PRIMARY_NAME = "Create / update GitHub Release"
RETRY_NAME = "Create / update GitHub Release (retry)"

STEP_RE = re.compile(r"^(\s*)- name:\s*(.+?)\s*$")


def is_comment(line: str) -> bool:
    return line.lstrip().startswith("#")


def find_steps(lines: list[str]) -> list[tuple[int, int, str]]:
    """Every `- name:` step as (start line index, indent, name)."""
    steps = []
    for i, line in enumerate(lines):
        m = STEP_RE.match(line)
        if m:
            steps.append((i, len(m.group(1)), m.group(2)))
    return steps


def indent_of(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def block_for(steps: list[tuple[int, int, str]], idx: int, lines: list[str]) -> list[str]:
    """The lines belonging to steps[idx]: from its `- name:` line up to (not
    including) the next non-blank line at or above its own indent — the next
    step, a trailing comment about the next step, or the next job entirely.
    Bounding by the next matched step alone overruns into whatever sits
    between the last step of one job and the first step of the next."""
    start, indent, _ = steps[idx]
    end = len(lines)
    for i in range(start + 1, len(lines)):
        line = lines[i]
        if line.strip() == "":
            continue
        if indent_of(line) <= indent:
            end = i
            break
    return lines[start:end]


def field(block: list[str], key: str) -> str | None:
    for line in block:
        if is_comment(line):
            continue
        m = re.match(rf"\s*{key}:\s*(.+?)\s*$", line)
        if m:
            return m.group(1)
    return None


def with_block(block: list[str]) -> list[str]:
    """Everything from the `with:` line to the end of the step, code lines
    only. This is what a retry must reproduce exactly to publish the same
    release the first attempt would have."""
    out = []
    seen_with = False
    for line in block:
        if is_comment(line):
            continue
        if not seen_with:
            if re.match(r"\s*with:\s*$", line):
                seen_with = True
                out.append(line.strip())
            continue
        out.append(line.strip())
    return out


def main() -> int:
    if not WORKFLOW.exists():
        print(f"check-release-retry: FAIL — {WORKFLOW} does not exist.")
        return 1

    lines = WORKFLOW.read_text(encoding="utf-8").splitlines()
    steps = find_steps(lines)

    primaries = [i for i, (_, _, name) in enumerate(steps) if name == PRIMARY_NAME]
    retries = [i for i, (_, _, name) in enumerate(steps) if name == RETRY_NAME]

    problems: list[str] = []

    if len(primaries) != 1:
        problems.append(
            f"found {len(primaries)} steps named '{PRIMARY_NAME}'; the guard "
            "expects exactly one."
        )
    if len(retries) != 1:
        problems.append(
            f"found {len(retries)} steps named '{RETRY_NAME}'; the retry "
            "step must exist, named exactly that, exactly once."
        )

    if problems:
        print("check-release-retry: FAIL — the release-publish retry is broken.")
        for problem in problems:
            print(f"       {problem}")
        return 1

    primary_idx, retry_idx = primaries[0], retries[0]
    if retry_idx < primary_idx:
        problems.append(f"'{RETRY_NAME}' appears before '{PRIMARY_NAME}'.")

    primary_block = block_for(steps, primary_idx, lines)
    retry_block = block_for(steps, retry_idx, lines)

    step_id = field(primary_block, "id")
    if not step_id:
        problems.append(
            f"'{PRIMARY_NAME}' has no `id:`. Without one, nothing can gate a "
            "retry on its outcome."
        )

    continue_on_error = field(primary_block, "continue-on-error")
    if continue_on_error != "true":
        problems.append(
            f"'{PRIMARY_NAME}' does not carry `continue-on-error: true`. "
            "Without it, a 5xx here ends the job before the retry step runs."
        )

    if step_id:
        retry_if = field(retry_block, "if")
        expected = f"steps.{step_id}.outcome == 'failure'"
        if not retry_if or expected not in retry_if:
            problems.append(
                f"'{RETRY_NAME}' does not gate on `{expected}`; found "
                f"{retry_if!r}. A wrong or missing id means the retry either "
                "never fires or always does."
            )

    primary_uses = field(primary_block, "uses")
    retry_uses = field(retry_block, "uses")
    if primary_uses != retry_uses:
        problems.append(
            f"the two steps pin different actions ('{primary_uses}' vs "
            f"'{retry_uses}'). A retry must run the exact action version "
            "that just failed, not a different one a partial dependency "
            "bump left behind."
        )

    primary_with = with_block(primary_block)
    retry_with = with_block(retry_block)
    if primary_with != retry_with:
        problems.append(
            f"'{PRIMARY_NAME}' and '{RETRY_NAME}' pass different `with:` "
            "blocks. A retry that drops `fail_on_unmatched_files` or "
            "narrows `files:` would publish an incomplete release without "
            "failing the job."
        )

    if problems:
        print("check-release-retry: FAIL — the release-publish retry is broken.")
        for problem in problems:
            print(f"       {problem}")
        return 1

    print(
        "check-release-retry: OK — a mid-publish 5xx retries once, against "
        "the same action and the same inputs."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
