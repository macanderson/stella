#!/usr/bin/env python3
"""Static checks on workflow.template.js. Run before every launch.

The harness's guarantees are structural, so they can be checked structurally. Each check
here corresponds to a rule that, if broken, produces a report that is confidently wrong
rather than obviously broken:

  1. the template is valid JS once spliced            (a syntax error wastes a whole launch)
  2. every agent() sets an explicit model             (model-panel.md rule 5; one omission
                                                       silently collapses the panel)
  3. every agent() sets an explicit phase             (progress reporting is how the run
                                                       survives — operations.md §0)
  4. every prompt opens with the provenance header    (or --provenance cannot verify the mix)
  5. meta.phases covers every phase used              (an uncovered phase gets its own
                                                       unlabelled progress box)
  6. no Date.now / Math.random                        (they throw in the sandbox and would
                                                       break resume)

    python3 tests/check_template.py [path-to-template]
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT = os.path.join(HERE, "..", "scripts", "workflow.template.js")

FIXTURE = {
    "__ROOT__": "/tmp/x",
    "/*__RUN__*/ {}": json.dumps({"date": "2026-01-01", "commit": "abc123", "depth": "deep"}),
    "/*__GATE__*/ {}": json.dumps({
        "toolchains": [{"name": "cargo", "type_gate": True, "lint": "x", "cache_note": "y"}],
        "fix_authority": {"level": "full"}, "llm_surface": {"present": True},
        "languages": [], "total_files": 10, "total_loc": 100, "project_gate": None}),
    "/*__UNITS__*/ []": json.dumps([{"unit": "a", "loc": 10, "n_files": 2,
                                     "owns_globs": ["a/**"], "group": "a", "note": ""}]),
    "/*__PRIOR__*/ null": "null",
    "/*__PRIOR_ITEMS__*/ []": "[]",
    "`__BASELINE__`": "`clean`",
    "/*__SALVAGE__*/ null": "null",
}


def splice(src: str) -> str:
    for k, v in FIXTURE.items():
        src = src.replace(k, v)
    return src


def find_agent_calls(src: str) -> list[tuple[int, str]]:
    """Every agent(...) call body, by paren matching that respects strings and templates."""
    calls = []
    for m in re.finditer(r"\bagent\(", src):
        i = m.end()
        depth, quote, out = 1, None, []
        while i < len(src) and depth:
            c = src[i]
            if quote:
                if c == "\\":
                    out.append(src[i:i + 2]); i += 2; continue
                if c == quote:
                    quote = None
            elif c in "'\"`":
                quote = c
            elif c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if not depth:
                    break
            out.append(c)
            i += 1
        calls.append((m.start(), "".join(out)))
    return calls


def main() -> int:
    path = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else DEFAULT)
    src = open(path).read()
    fails: list[str] = []
    ok: list[str] = []

    # 1 — valid JS. Top-level return/await are legal inside the harness's async wrapper but
    # not in a bare module, so reproduce the wrapper before checking.
    spliced = splice(src)
    left = re.findall(r"__[A-Z_]+__", spliced)
    if left:
        fails.append(f"unspliced placeholders remain after fixture splice: {sorted(set(left))}")
    cut = spliced.index("\n// ──")
    wrapped = spliced[:cut] + "\nasync function __run(){\n" + spliced[cut:] + "\n}\nvoid __run;\n"
    with tempfile.NamedTemporaryFile("w", suffix=".mjs", delete=False) as fh:
        fh.write(wrapped)
        tmp = fh.name
    p = subprocess.run(["node", "--check", tmp], capture_output=True, text=True)
    os.unlink(tmp)
    if p.returncode:
        fails.append(f"node --check failed:\n{p.stderr.strip()}")
    else:
        ok.append("valid JS once spliced")

    # 2-4 — per-agent invariants.
    calls = find_agent_calls(src)
    if len(calls) < 8:
        fails.append(f"only {len(calls)} agent() calls found — the paren scanner is probably "
                     f"broken, not the template")
    no_model = [i for i, b in calls if not re.search(r"\bmodel:\s*", b)]
    no_phase = [i for i, b in calls if not re.search(r"\bphase:\s*", b)]
    no_hdr = [i for i, b in calls if "hdr(" not in b]
    for label, bad in (("model", no_model), ("phase", no_phase), ("hdr() header", no_hdr)):
        if bad:
            fails.append(f"{len(bad)} agent() call(s) missing explicit {label} "
                         f"(at char offsets {bad})")
        else:
            ok.append(f"all {len(calls)} agent() calls set an explicit {label}")

    # 5 — meta.phases covers everything actually used.
    meta_titles = set(re.findall(r"\{\s*title:\s*'([^']+)'", src))
    used = set(re.findall(r"phase\('([^']+)'\)", src)) | \
        set(re.findall(r"phase:\s*'([^']+)'", src))
    missing = used - meta_titles
    if missing:
        fails.append(f"phases used but absent from meta.phases: {sorted(missing)}")
    else:
        ok.append(f"meta.phases covers all {len(used)} phases in use")

    # 6 — banned non-determinism.
    banned = [b for b in ("Date.now", "Math.random", "new Date()") if b in src]
    if banned:
        fails.append(f"banned in the workflow sandbox (throws, and breaks resume): {banned}")
    else:
        ok.append("no Date.now / Math.random / new Date()")

    # Informational: the model mix the template will actually produce.
    models = sorted(set(re.findall(r"model:\s*'([a-z]+)'", src)))
    dyn = sorted(set(re.findall(r"model:\s*(\w+)", src)) - set(models))
    print(f"models referenced literally: {models or '(none)'}")
    print(f"models referenced by variable: {dyn or '(none)'}")
    if "haiku" in models:
        fails.append("haiku appears as a judging/scoring model — model-panel.md forbids it")

    for line in ok:
        print(f"  PASS  {line}")
    for line in fails:
        print(f"  FAIL  {line}")
    print(f"\n{len(ok)} passed, {len(fails)} failed  ({os.path.relpath(path)})")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
