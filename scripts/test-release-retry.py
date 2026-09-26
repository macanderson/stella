#!/usr/bin/env python3
"""Self-test for `check-release-retry.py`.

A guard nobody can watch fail is a guard nobody knows works. Each case below
builds a small workflow in a temporary directory, breaks one property of the
release-publish retry, and asserts the guard says so. The last case runs it
over this repository, where it has to pass.

Hermetic: no network, no `gh`, nothing outside the temporary directory but
the guard itself.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
GUARD = REPO / "scripts" / "check-release-retry.py"

# A stand-in for the real `release` job, with a second job after it — the
# same shape as `release.yml`'s `release:` followed by `homebrew:` — so a
# guard that bounds a step's block by the wrong rule and reads into the next
# job gets caught by fixtures, not just by the live file.
WORKFLOW = """\
name: release

jobs:
  release:
    name: publish GitHub Release
    runs-on: ubuntu-latest
    steps:
      - name: Create / update GitHub Release
        id: publish
        continue-on-error: true
        uses: softprops/action-gh-release@efb35369e0ad2afab669f228072c1b0d510eae64 # v3
        with:
          files: |
            artifacts/stella-*.tar.gz
            artifacts/SHA256SUMS
          fail_on_unmatched_files: true
          body_path: notes.md

      - name: Create / update GitHub Release (retry)
        if: steps.publish.outcome == 'failure'
        uses: softprops/action-gh-release@efb35369e0ad2afab669f228072c1b0d510eae64 # v3
        with:
          files: |
            artifacts/stella-*.tar.gz
            artifacts/SHA256SUMS
          fail_on_unmatched_files: true
          body_path: notes.md

  homebrew:
    name: publish Homebrew formula
    needs: release
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@0000000000000000000000000000000000000000 # v7
"""

failures: list[str] = []


def build(workflow: str) -> Path:
    root = Path(tempfile.mkdtemp(prefix="release-retry-"))
    target = root / ".github" / "workflows"
    target.mkdir(parents=True)
    (target / "release.yml").write_text(workflow, encoding="utf-8")
    return root


def run(cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(GUARD)],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=False,
    )


def case(name: str, workflow: str, expect_pass: bool) -> None:
    result = run(build(workflow))
    passed = result.returncode == 0
    if passed != expect_pass:
        want = "pass" if expect_pass else "fail"
        failures.append(
            f"{name}: expected the guard to {want}, got exit {result.returncode}\n{result.stdout}"
        )


case("the retry as it stands", WORKFLOW, expect_pass=True)

case(
    "no id on the primary step",
    WORKFLOW.replace("        id: publish\n", ""),
    expect_pass=False,
)

case(
    "continue-on-error dropped",
    WORKFLOW.replace("        continue-on-error: true\n", ""),
    expect_pass=False,
)

case(
    "the retry step dropped entirely",
    WORKFLOW[: WORKFLOW.index("      - name: Create / update GitHub Release (retry)")]
    + WORKFLOW[WORKFLOW.index("  homebrew:") :],
    expect_pass=False,
)

case(
    "the retry step's name loses '(retry)'",
    WORKFLOW.replace(
        "- name: Create / update GitHub Release (retry)", "- name: Create / update GitHub Release"
    ),
    expect_pass=False,
)

case(
    "the retry gates on the wrong step id",
    WORKFLOW.replace(
        "if: steps.publish.outcome == 'failure'", "if: steps.upload.outcome == 'failure'"
    ),
    expect_pass=False,
)

case(
    "the retry pins a different action",
    WORKFLOW.replace(
        "      - name: Create / update GitHub Release (retry)\n"
        "        if: steps.publish.outcome == 'failure'\n"
        "        uses: softprops/action-gh-release@efb35369e0ad2afab669f228072c1b0d510eae64 # v3\n",
        "      - name: Create / update GitHub Release (retry)\n"
        "        if: steps.publish.outcome == 'failure'\n"
        "        uses: softprops/action-gh-release@0000000000000000000000000000000000000000 # v2\n",
    ),
    expect_pass=False,
)

case(
    "the retry's files: list is narrower than the primary's",
    WORKFLOW.replace(
        "            artifacts/stella-*.tar.gz\n            artifacts/SHA256SUMS\n"
        "          fail_on_unmatched_files: true\n          body_path: notes.md\n\n"
        "      - name: Create / update GitHub Release (retry)",
        "            artifacts/stella-*.tar.gz\n"
        "          fail_on_unmatched_files: true\n          body_path: notes.md\n\n"
        "      - name: Create / update GitHub Release (retry)",
    ),
    expect_pass=False,
)

WORKFLOW_RETRY_FIRST = """\
name: release

jobs:
  release:
    name: publish GitHub Release
    runs-on: ubuntu-latest
    steps:
      - name: Create / update GitHub Release (retry)
        if: steps.publish.outcome == 'failure'
        uses: softprops/action-gh-release@efb35369e0ad2afab669f228072c1b0d510eae64 # v3
        with:
          files: |
            artifacts/stella-*.tar.gz
            artifacts/SHA256SUMS
          fail_on_unmatched_files: true
          body_path: notes.md

      - name: Create / update GitHub Release
        id: publish
        continue-on-error: true
        uses: softprops/action-gh-release@efb35369e0ad2afab669f228072c1b0d510eae64 # v3
        with:
          files: |
            artifacts/stella-*.tar.gz
            artifacts/SHA256SUMS
          fail_on_unmatched_files: true
          body_path: notes.md

  homebrew:
    name: publish Homebrew formula
    needs: release
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@0000000000000000000000000000000000000000 # v7
"""

case("the retry runs before the primary attempt", WORKFLOW_RETRY_FIRST, expect_pass=False)

missing = Path(tempfile.mkdtemp(prefix="release-retry-"))
if run(missing).returncode == 0:
    failures.append("a tree with no workflow: expected the guard to fail")

live = run(REPO)
if live.returncode != 0:
    failures.append(f"this repository: expected the guard to pass\n{live.stdout}")

if failures:
    print("test-release-retry: FAIL")
    for failure in failures:
        print(f"  {failure}")
    sys.exit(1)

print("test-release-retry: OK — 11 cases")
