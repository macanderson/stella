#!/usr/bin/env python3
"""Self-test for `check-release-wiring.py`.

A guard nobody can watch fail is a guard nobody knows works. Each case
below builds a small tree in a temporary directory, breaks one line of the
release job's verification path, and asserts the guard says so. The last
case runs it over this repository, where it has to pass.

Hermetic: no network, no `gh`, nothing outside the temporary directory but
the guard itself.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
GUARD = REPO / "scripts" / "check-release-wiring.py"

WORKFLOW = """\
name: auto-tag

jobs:
  tag:
    runs-on: ubuntu-latest
    env:
      SYNC_BRANCH: bot/version-sync
    steps:
      - name: Wait for the required checks, then merge the version-sync PR
        id: merge
        env:
          BRANCH: ${{ env.SYNC_BRANCH }}
        run: |
          if gh pr merge "${BRANCH}" --squash --auto; then
            echo "armed=true" >>"$GITHUB_OUTPUT"
          fi

      - name: Wait for an armed auto-merge to land, then check the merged tip
        env:
          ARMED: ${{ steps.merge.outputs.armed }}
        run: |
          if [ "${ARMED:-}" = "true" ]; then
            ./scripts/wait-for-armed-merge.sh "${SYNC_BRANCH}"
          fi
          ./scripts/dispatch-main-verification.sh
"""

failures: list[str] = []


def build(workflow: str, executable: bool = True) -> Path:
    """A tree with one workflow and the two scripts it calls."""
    root = Path(tempfile.mkdtemp(prefix="release-wiring-"))
    target = root / ".github" / "workflows"
    target.mkdir(parents=True)
    (target / "auto-tag.yml").write_text(workflow, encoding="utf-8")

    scripts = root / "scripts"
    scripts.mkdir()
    for name in ("wait-for-armed-merge.sh", "dispatch-main-verification.sh"):
        path = scripts / name
        path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        path.chmod(0o755 if executable else 0o644)
    return root


def run(cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(GUARD)],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=False,
    )


def case(name: str, workflow: str, expect_pass: bool, executable: bool = True) -> None:
    result = run(build(workflow, executable))
    passed = result.returncode == 0
    if passed != expect_pass:
        want = "pass" if expect_pass else "fail"
        failures.append(f"{name}: expected the guard to {want}, got exit {result.returncode}\n{result.stdout}")


case("the wiring as it stands", WORKFLOW, expect_pass=True)

case(
    "a second copy of the branch name",
    WORKFLOW.replace('"${SYNC_BRANCH}"\n          fi', '"bot/version-sync"\n          fi'),
    expect_pass=False,
)

case(
    "no branch declared",
    WORKFLOW.replace("      SYNC_BRANCH: bot/version-sync\n", ""),
    expect_pass=False,
)

case(
    "the armed output dropped",
    WORKFLOW.replace('            echo "armed=true" >>"$GITHUB_OUTPUT"\n', ""),
    expect_pass=False,
)

case(
    "nothing reads the armed output",
    WORKFLOW.replace("${{ steps.merge.outputs.armed }}", "false"),
    expect_pass=False,
)

case(
    "the wait dropped",
    WORKFLOW.replace('            ./scripts/wait-for-armed-merge.sh "${SYNC_BRANCH}"\n', ""),
    expect_pass=False,
)

case(
    "the dispatcher dropped",
    WORKFLOW.replace("          ./scripts/dispatch-main-verification.sh\n", ""),
    expect_pass=False,
)

case(
    "the dispatcher inside the armed branch",
    WORKFLOW.replace(
        "          fi\n          ./scripts/dispatch-main-verification.sh\n",
        "            ./scripts/dispatch-main-verification.sh\n          fi\n",
    ),
    expect_pass=False,
)

case("the wait script not executable", WORKFLOW, expect_pass=False, executable=False)

missing = Path(tempfile.mkdtemp(prefix="release-wiring-"))
if run(missing).returncode == 0:
    failures.append("a tree with no workflow: expected the guard to fail")

live = run(REPO)
if live.returncode != 0:
    failures.append(f"this repository: expected the guard to pass\n{live.stdout}")

if failures:
    print("test-release-wiring: FAIL")
    for failure in failures:
        print(f"  {failure}")
    sys.exit(1)

print("test-release-wiring: OK — 11 cases")
