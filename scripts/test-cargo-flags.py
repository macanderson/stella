#!/usr/bin/env python3
"""Self-test: `scripts/check-cargo-flags.py` can still fail.

A ratchet that has quietly stopped being able to fail reports green forever,
which is worse than not having one. Every case below builds a fixture
Makefile in a temporary directory and runs the guard against it with
`--makefile`, so nothing here reads or writes this repository -- except the
last case, which runs the guard on the live tree and expects a pass.

The case that matters most is the one the guard was written for: the refused
flag arriving through a make variable rather than spelled out. That is how
`cargo clean --doc $(SCHEMA_CRATES)` shipped, and a guard that read the
Makefile text instead of asking make to expand it would miss it.

    ./scripts/test-cargo-flags.py
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

GUARD = Path(__file__).resolve().parent / "check-cargo-flags.py"
REPO_MAKEFILE = Path(__file__).resolve().parent.parent / "Makefile"

failures: list[str] = []
passes = 0


def run(makefile: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(GUARD), "--makefile", str(makefile)],
        capture_output=True,
        text=True,
        check=False,
    )


def case(name: str, body: str, want_code: int, want_text: str = "") -> None:
    """Write a fixture Makefile, run the guard, and hold it to one verdict."""
    global passes
    with tempfile.TemporaryDirectory(prefix="stella-cargo-flags-") as directory:
        makefile = Path(directory) / "Makefile"
        makefile.write_text(body, encoding="utf-8")
        result = run(makefile)
    output = result.stdout + result.stderr
    if result.returncode != want_code:
        failures.append(
            f"{name}: exit {result.returncode}, wanted {want_code}\n{output}"
        )
        return
    if want_text and want_text not in output:
        failures.append(f"{name}: output never said {want_text!r}\n{output}")
        return
    passes += 1


# C1 — the defect itself, reaching cargo through a variable.
case(
    "C1 refused pair via a variable",
    "CRATES := -p one -p two\n"
    ".PHONY: docs\n"
    "docs:\n"
    "\tcargo clean --doc $(CRATES)\n",
    1,
    "--doc cannot be used with -p",
)

# C2 — the same pair spelled out, and in its long form.
case(
    "C2 refused pair spelled out",
    ".PHONY: docs\ndocs:\n\tcargo clean --doc --package one\n",
    1,
    "--doc cannot be used with -p",
)

# C3 — the repaired recipe. The scope moves to the target directory, which is
# what the live Makefile does.
case(
    "C3 the repaired recipe passes",
    ".PHONY: docs\ndocs:\n\tCARGO_TARGET_DIR=target/doc-schema cargo clean --doc\n",
    0,
    "none carrying a flag pair cargo refuses",
)

# C4 — either flag alone is fine, and so is the pair on another subcommand.
case(
    "C4 neither flag alone is an offence",
    ".PHONY: a b c\n"
    "a:\n\tcargo clean --doc\n"
    "b:\n\tcargo clean -p one\n"
    "c:\n\tcargo doc -p one --no-deps\n",
    0,
)

# C5 — cargo named in an argument is not a cargo invocation. A false positive
# here would fire on the live tree, where a help message names `cargo install
# cargo-deny` and a guard script is called `check-cargo-install-pins.sh`.
case(
    "C5 cargo inside an argument is not an invocation",
    ".PHONY: talk\n"
    "talk:\n"
    "\tprintf 'run: cargo clean --doc -p one\\n'\n"
    "\t./scripts/check-cargo-install-pins.sh\n",
    0,
)

# C6 — flags after a bare `--` belong to the tool cargo runs, not to cargo.
case(
    "C6 flags after -- are not cargo's",
    ".PHONY: lint\nlint:\n\tcargo clean --doc -- -p one\n",
    0,
)

# C7 — a Makefile make itself refuses stops the run. Reporting a read it could
# not finish as a pass is the one outcome a guard must never have.
case(
    "C7 a Makefile make refuses is reported, not passed",
    ".PHONY: broken\nbroken: no-rule-makes-this\n\tcargo clean --doc\n",
    2,
    "make --dry-run` failed",
)

# C8 — a Makefile that is not there.
with tempfile.TemporaryDirectory(prefix="stella-cargo-flags-") as empty:
    absent = run(Path(empty) / "Makefile")
    if absent.returncode == 2:
        passes += 1
    else:
        failures.append(
            f"C8 missing Makefile: exit {absent.returncode}, wanted 2\n"
            f"{absent.stdout}{absent.stderr}"
        )

# C9 — the live tree passes. Without this the suite could go green on a guard
# that fails on everything.
live = run(REPO_MAKEFILE)
if live.returncode == 0:
    passes += 1
else:
    failures.append(
        f"C9 live Makefile: exit {live.returncode}, wanted 0\n{live.stdout}{live.stderr}"
    )

for failure in failures:
    print(f"test-cargo-flags: FAIL — {failure}", file=sys.stderr)

if failures:
    print(f"test-cargo-flags: {len(failures)} of {passes + len(failures)} cases failed.")
    sys.exit(1)

print(f"test-cargo-flags: OK — {passes} cases.")
