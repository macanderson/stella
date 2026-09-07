#!/usr/bin/env python3
"""Guard: the release job still asks for a run on the commit it merges.

`auto-tag.yml` merges the version write-back with the token the run holds.
A push made with that token starts no workflow. So the
`chore(release): sync versions` commit lands on `main` with nothing to
check it. `scripts/dispatch-main-verification.sh` asks for the missing run.

The last resort merges later. When the plain merge and the admin merge both
fail, the job arms auto-merge and GitHub lands the pull request on its own,
after the step ends. Asking about the tip of `main` right then asks about
the wrong commit. `scripts/wait-for-armed-merge.sh` is the wait in between.

Four lines hold that path open, and nothing read them back:

1. The merge step records `armed=true` when it arms auto-merge.
2. A later step reads that output.
3. That step runs the wait, under the flag.
4. It runs the dispatcher after the wait, outside the flag.

Drop any one of them and the release still passes. The gap comes back, and
only a release that takes the last-resort path would show it — which is
rare, and which is why the first one took two weeks to find.

The branch name is the fifth line. Four steps name it: the push, the run
approval, the merge, and the wait. They agree by convention until somebody
edits one. A wait aimed at a branch with no pull request reads UNKNOWN and
returns at once, so the dispatcher behind it asks about the tip from before
the merge, finds a run, and stops. This guard reads the name out of the
job's `env:` block and fails on a second copy anywhere the shell can see.

Text, not YAML. The rules above are about the shape of a shell block inside
a `run:` key, which a parsed document flattens into one string anyway, and
this has to run on a bare runner with no PyYAML.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

WORKFLOW = Path(".github/workflows/auto-tag.yml")

WAIT_SCRIPT = "scripts/wait-for-armed-merge.sh"
DISPATCH_SCRIPT = "scripts/dispatch-main-verification.sh"

BRANCH_KEY = "SYNC_BRANCH"


def is_comment(line: str) -> bool:
    return line.lstrip().startswith("#")


def indent_of(line: str) -> int:
    return len(line) - len(line.lstrip())


def main() -> int:
    if not WORKFLOW.exists():
        print(f"check-release-wiring: FAIL — {WORKFLOW} does not exist.")
        return 1

    lines = WORKFLOW.read_text(encoding="utf-8").splitlines()
    code = [(n, text) for n, text in enumerate(lines, 1) if not is_comment(text)]

    problems: list[str] = []

    # ── The branch, named once ────────────────────────────────────────────
    declarations = [
        (n, m.group(1))
        for n, text in code
        if (m := re.fullmatch(rf"\s+{BRANCH_KEY}:\s*(\S+)", text))
    ]
    if len(declarations) != 1:
        problems.append(
            f"the job declares {BRANCH_KEY} {len(declarations)} times; it takes "
            "exactly one, in the job's `env:` block."
        )
    else:
        _, branch = declarations[0]
        strays = [
            n for n, text in code if branch in text and not text.strip().startswith(BRANCH_KEY)
        ]
        for n in strays:
            problems.append(
                f"line {n} writes `{branch}` again. Every step reads "
                f"${BRANCH_KEY} instead, so the wait cannot end up aimed at a "
                "branch the merge step never touched."
            )

    # ── The four lines of the last-resort path ────────────────────────────
    armed = [n for n, text in code if "armed=true" in text and "GITHUB_OUTPUT" in text]
    if not armed:
        problems.append(
            "no step records `armed=true` into $GITHUB_OUTPUT. That output is "
            "how the step after the merge learns a merge is still coming."
        )

    reads_armed = [n for n, text in code if re.search(r"steps\.\w+\.outputs\.armed", text)]
    if not reads_armed:
        problems.append(
            "nothing reads `steps.<merge step>.outputs.armed`, so the wait "
            "below can never fire."
        )

    waits = [(n, text) for n, text in code if WAIT_SCRIPT in text]
    dispatches = [(n, text) for n, text in code if DISPATCH_SCRIPT in text]

    if not waits:
        problems.append(
            f"nothing runs {WAIT_SCRIPT}. An armed auto-merge lands after the "
            "job ends, and the dispatcher would read the tip from before it."
        )
    if not dispatches:
        problems.append(
            f"nothing runs {DISPATCH_SCRIPT}. The merged commit gets no run at "
            "all until the daily canary asks."
        )

    if waits and dispatches:
        wait_line, wait_text = waits[-1]
        dispatch_line, dispatch_text = dispatches[-1]
        if dispatch_line < wait_line:
            problems.append(
                f"line {dispatch_line} runs the dispatcher before line "
                f"{wait_line} waits. It would ask about the tip from before "
                "the merge."
            )
        elif indent_of(dispatch_text) >= indent_of(wait_text):
            problems.append(
                f"line {dispatch_line} runs the dispatcher at or below the "
                f"wait's own indent. The wait is conditional and the "
                "dispatcher must not be: every other merge path in this job "
                "needs it too."
            )

    for script in (WAIT_SCRIPT, DISPATCH_SCRIPT):
        path = Path(script)
        if not path.exists():
            problems.append(f"{script} does not exist.")
        elif not path.stat().st_mode & 0o111:
            problems.append(f"{script} is not executable, so the step would fail.")

    if problems:
        print("check-release-wiring: FAIL — the release job's verification path is broken.")
        for problem in problems:
            print(f"       {problem}")
        print("       Each script's own header says what its line holds open.")
        return 1

    print(
        "check-release-wiring: OK — the sync branch is named once, and the "
        "armed-merge wait still runs before the dispatcher."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
