#!/usr/bin/env python3
"""Directions `scripts/check-guard-trigger-coverage.py` must fail in.

Each case builds a throwaway `.github/workflows/` tree under `--manifest-dir`
and runs the real guard against it as a subprocess, the same posture
`scripts/test-gate-parity.sh` uses: nothing here reads or writes this
repository. Not part of `make gate`; run it with
`make guard-trigger-coverage-test`.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
GUARD = HERE / "check-guard-trigger-coverage.py"

pass_count = 0
fail_count = 0


def workflow(pull_request_body: str, run_line: str) -> str:
    return (
        "name: fixture\n"
        "on:\n"
        f"  pull_request:{pull_request_body}\n"
        "jobs:\n"
        "  guards:\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        "      - name: a step\n"
        f"        run: {run_line}\n"
    )


def fixture(name: str, workflows: dict[str, str]) -> Path:
    root = Path(tempfile.mkdtemp(prefix=f"guard-trigger-coverage-{name}-"))
    wf_dir = root / ".github" / "workflows"
    wf_dir.mkdir(parents=True)
    for filename, body in workflows.items():
        (wf_dir / filename).write_text(body, encoding="utf-8")
    return root


def run_guard(root: Path) -> tuple[int, str]:
    proc = subprocess.run(
        [sys.executable, str(GUARD), "--manifest-dir", str(root)],
        capture_output=True,
        text=True,
    )
    return proc.returncode, proc.stdout + proc.stderr


def expect(name: str, root: Path, want_rc: int, needle: str = "") -> None:
    global pass_count, fail_count
    rc, out = run_guard(root)
    if rc != want_rc:
        print(f"FAIL  {name} -- exit {rc}, wanted {want_rc}\n      {out.strip()}")
        fail_count += 1
        return
    if needle and needle not in out:
        print(f"FAIL  {name} -- output never says {needle!r}\n      {out.strip()}")
        fail_count += 1
        return
    print(f"ok    {name}")
    pass_count += 1


# ── T1  an unfiltered trigger for all three watched guards passes ───────────
root = fixture(
    "unfiltered",
    {
        "guards.yml": (
            workflow("\n  merge_group:", "python3 ./scripts/check-prose.py")
            + "      - name: hue\n        run: python3 ./scripts/check-hue-separation.py\n"
            + "      - name: transcript\n        run: python3 ./scripts/check-transcript-surfaces.py\n"
        )
    },
)
expect("T1 an unrestricted pull_request trigger passes", root, 0)

# ── T2  a paths: filter under pull_request fails ─────────────────────────────
root = fixture(
    "paths_filter",
    {
        "guards.yml": workflow(
            '\n    paths:\n      - "docs/**"', "python3 ./scripts/check-prose.py"
        )
        + "      - name: hue\n        run: python3 ./scripts/check-hue-separation.py\n"
        + "      - name: transcript\n        run: python3 ./scripts/check-transcript-surfaces.py\n"
    },
)
expect(
    "T2 a paths: filter on the only runner fails",
    root,
    1,
    "check-prose.py: every workflow that runs it restricts",
)

# ── T3  paths-ignore: is caught the same way ─────────────────────────────────
root = fixture(
    "paths_ignore",
    {
        "guards.yml": workflow(
            '\n    paths-ignore:\n      - "**/*.png"', "python3 ./scripts/check-prose.py"
        )
        + "      - name: hue\n        run: python3 ./scripts/check-hue-separation.py\n"
        + "      - name: transcript\n        run: python3 ./scripts/check-transcript-surfaces.py\n"
    },
)
expect(
    "T3 a paths-ignore: filter on the only runner fails",
    root,
    1,
    "check-prose.py: every workflow that runs it restricts",
)

# ── T4  one filtered copy plus one unfiltered copy still passes ─────────────
root = fixture(
    "second_copy_saves_it",
    {
        "filtered.yml": workflow(
            '\n    paths:\n      - "docs/**"', "python3 ./scripts/check-hue-separation.py"
        ),
        "unfiltered.yml": (
            workflow("\n  merge_group:", "python3 ./scripts/check-hue-separation.py")
            + "      - name: prose\n        run: python3 ./scripts/check-prose.py\n"
            + "      - name: transcript\n        run: python3 ./scripts/check-transcript-surfaces.py\n"
        ),
    },
)
expect("T4 a second unfiltered copy of the same guard still passes", root, 0)

# ── T5  a workflow that never triggers on pull_request at all fails ─────────
root = fixture(
    "push_only",
    {
        "guards.yml": (
            "name: fixture\n"
            "on:\n"
            "  push:\n"
            "    branches: [main]\n"
            "jobs:\n"
            "  guards:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - name: prose\n        run: python3 ./scripts/check-prose.py\n"
            "      - name: hue\n        run: python3 ./scripts/check-hue-separation.py\n"
            "      - name: transcript\n        run: python3 ./scripts/check-transcript-surfaces.py\n"
        )
    },
)
expect(
    "T5 a push-only workflow provides no pull_request coverage",
    root,
    1,
    "no pull_request trigger",
)

# ── T6  a guard named only in a paths: filter is not counted as running ─────
root = fixture(
    "watched_not_run",
    {
        "guards.yml": (
            workflow("\n  merge_group:", "python3 ./scripts/check-hue-separation.py")
            + "      - name: transcript\n        run: python3 ./scripts/check-transcript-surfaces.py\n"
        )
    },
)
# check-prose.py never appears in a `run:` line here at all.
expect(
    "T6 a guard no run: line invokes fails as unrun",
    root,
    1,
    "check-prose.py: no workflow runs it at all.",
)

# ── T7  a guard named only in a shell comment inside a run: block is not a run
root = fixture(
    "shell_comment",
    {
        "guards.yml": (
            "name: fixture\n"
            "on:\n"
            "  pull_request:\n"
            "  merge_group:\n"
            "jobs:\n"
            "  guards:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - name: prose\n"
            "        run: |\n"
            "          # python3 ./scripts/check-prose.py is retired from this step\n"
            "          true\n"
            "      - name: hue\n        run: python3 ./scripts/check-hue-separation.py\n"
            "      - name: transcript\n        run: python3 ./scripts/check-transcript-surfaces.py\n"
        )
    },
)
expect(
    "T7 a guard named only in a shell comment is not counted as running",
    root,
    1,
    "check-prose.py: no workflow runs it at all.",
)

print()
print(f"test-guard-trigger-coverage: {pass_count} passed, {fail_count} failed")
sys.exit(1 if fail_count else 0)
