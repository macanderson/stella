#!/usr/bin/env python3
"""Guard: shipping Rust must not spell a retired role model key.

Five `agent_engine_config` keys are gone. So are the `agents.<persona>`
blocks. The core loop has one role. A model for any other role is a seat.
A plugin declares the seat. A user assigns it in `[seats]`.

The keys are still known. `crates/stella-cli/src/settings/unknown.rs` names
each one back to the user. It gives the seat that takes its place. The
trusted launcher still takes a posture that carries one.

What must not come back is code that uses one. A live string named
`pipeline_verifier_model` is dead config that reads like a feature. That is
the flaw the retirement ended.

`scripts/check-role-names.sh` asks a different thing. It holds four languages
to one spelling of the role words. It never asks if Rust still uses them.

A hit is a quoted string on a line of code. A doc comment may cite a retired
key. Half these files explain the retirement that way. Test code is skipped
for the same reason.

`HOMES` names the files where the spelling is the subject. `DEBT` holds what
came before this guard. A count there may fall. It may never rise.

Usage:
    ./scripts/check-retired-model-keys.py [ROOT]
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# The five retired personas. A settings file spelled each one twice: as
# the flat `pipeline_<role>_model` key, and as the `agents.<role>` block.
PERSONAS = ("worker", "verifier", "triage", "research", "plan")

KEY = re.compile(
    r'"[^"\n]*(?:pipeline_(?:%s)_model|agents\.(?:%s))[^"\n]*"'
    % ("|".join(PERSONAS), "|".join(PERSONAS))
)

CFG_TEST = re.compile(r"#\[cfg\((?:all\()?test\b")

# Files where a retired spelling is the subject. None of them is a use.
HOMES = {
    # The retirement table. It holds every key, the reason, and the seat
    # that takes its place.
    "crates/stella-cli/src/settings/unknown.rs",
    # The self-tuning ledger reads the name it wrote before the change.
    # An older build's promotion can then still be rolled back.
    "crates/stella-cli/src/memory/self_tuning.rs",
}

# Down-only. Each line is what a file held before this guard. Never raise a
# number here. Never add a file. Delete the spelling instead.
DEBT = {
    # The demo session behind `stella deck` shots and the `/models` golden
    # frame. It still draws six rows from retired keys. A rebuild on seats
    # redraws the golden frame, so it lands with the settings pane.
    "crates/stella-tui/src/scenario.rs": 2,
}


def strip_cfg_test(src: str) -> str:
    """Drop `#[cfg(test)]` / `#[cfg(all(test, ...))]` blocks."""
    out: list[str] = []
    i = 0
    while True:
        m = CFG_TEST.search(src, i)
        if not m:
            out.append(src[i:])
            return "".join(out)
        out.append(src[i : m.start()])
        brace = src.find("{", m.start())
        if brace < 0:
            return "".join(out)
        depth = 0
        j = brace
        while j < len(src):
            if src[j] == "{":
                depth += 1
            elif src[j] == "}":
                depth -= 1
                if depth == 0:
                    break
            j += 1
        i = j + 1


def hits(root: Path) -> dict[str, list[tuple[int, str]]]:
    """Every retired spelling in a string literal, by repo-relative path."""
    found: dict[str, list[tuple[int, str]]] = {}
    for path in sorted(root.glob("crates/*/src/**/*.rs")):
        rel = path.relative_to(root).as_posix()
        parts = path.relative_to(root).parts
        if rel in HOMES or "tests" in parts or path.name == "tests.rs":
            continue
        src = strip_cfg_test(path.read_text(encoding="utf-8", errors="replace"))
        for number, line in enumerate(src.splitlines(), 1):
            if line.lstrip().startswith("//"):
                continue
            if KEY.search(line):
                found.setdefault(rel, []).append((number, line.strip()[:100]))
    return found


def main() -> int:
    argv = sys.argv[1:]
    root = Path(argv[0]) if argv else Path(__file__).resolve().parent.parent
    root = root.resolve()

    found = hits(root)

    # The verdict is settled before a word is written. A guard that prints
    # as it scans dies mid-report when its reader exits early. The half-done
    # state then becomes the exit status.
    report: list[str] = []
    status = 0
    for rel in sorted(set(found) | set(DEBT)):
        now = len(found.get(rel, []))
        allowed = DEBT.get(rel, 0)
        if now > allowed:
            status = 1
            report.append(f"{rel}: {now} retired key(s) in code, {allowed} allowed.")
            for number, line in found.get(rel, [])[:20]:
                report.append(f"    {rel}:{number}  {line}")
        elif now < allowed and (root / rel).is_file():
            report.append(
                f"note: {rel} is down to {now} (DEBT says {allowed}) -- "
                "lower the number in this script to lock the win in."
            )

    if status:
        report.append("")
        report.append(
            "These keys are retired: a model for anything but the session's "
            "own role is a plugin-declared seat, assigned in `[seats]` "
            "(`agent_engine_config.seat_models`). Cite a retired key in a doc "
            "comment if you must name it; do not put one in code."
        )

    if report:
        sys.stderr.write("\n".join(report) + "\n")
    if not status:
        total = sum(len(v) for v in found.values())
        print(f"check-retired-model-keys: OK -- {total} recorded, none new.")
    return status


if __name__ == "__main__":
    sys.exit(main())
