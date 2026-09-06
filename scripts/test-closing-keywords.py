#!/usr/bin/env python3
"""Directions `scripts/check-closing-keywords.py` must fail in.

Each case runs the real guard as a subprocess with `--fixture-pr-body` and/or
`--fixture-commit-messages` standing in for a live `gh pr view` fetch — the
same fixture-flag idiom `check-deleted-tests.sh`'s own
`--fixture-pr-body`/`--fixture-pr-body-error` use, and the reason neither
`gh` nor a network is needed here. No git repository is built: this guard
reads text, not a tree, so there is nothing to check out.

The three cases named in `#6190`'s own "How to verify" section anchor the
suite: a body that negates a closing keyword fails; a `Refs #N` body passes;
a genuine `Closes #N` body passes. The rest cover the shape the guard is
built to get right without going brittle — backtick escaping, the negation
list, the sentence boundary, and the commit-message channel.

Not part of `make gate`: `check-closing-keywords.py` has no `make gate` step
either, for the reason `check-deleted-tests.sh` gives — it reads a PR's
description and commit messages, which are not information a single local
tree carries. Run it with `make closing-keywords-test`.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
GUARD = HERE / "check-closing-keywords.py"

# A fake issue number for the fixture bodies below. It is built here, not
# typed out by hand each time. Most fixtures must stay un-backticked, or the
# check would pass instead of failing.
ISSUE_NUM = 99999
REF = f"#{ISSUE_NUM}"

pass_count = 0
fail_count = 0


def run(body: str = "", commits: str = "") -> subprocess.CompletedProcess[str]:
    args = [sys.executable, str(GUARD)]
    if body:
        args += ["--fixture-pr-body", body]
    if commits:
        args += ["--fixture-commit-messages", commits]
    return subprocess.run(args, capture_output=True, text=True)


def check(name: str, condition: bool, detail: str = "") -> None:
    global pass_count, fail_count
    if condition:
        pass_count += 1
        print(f"ok   {name}")
    else:
        fail_count += 1
        print(f"FAIL {name}")
        if detail:
            print(f"     {detail}")


# ── The three cases `#6190`'s "How to verify" names ──────────────────────────

result = run(body=f"This does not close {REF}. Only the first item is done.")
check(
    "a body negating a closing keyword fails",
    result.returncode == 1,
    f"exit={result.returncode} stderr={result.stderr!r}",
)
check(
    "the failure names the offending phrase",
    f"does not close {REF}" in result.stderr,
    result.stderr,
)
check(
    "the failure prints a safe spelling",
    "backticks" in result.stderr and REF in result.stderr,
    result.stderr,
)

result = run(body=f"This advances the fix. Refs {REF}.")
check(
    "a Refs body passes",
    result.returncode == 0,
    f"exit={result.returncode} stderr={result.stderr!r}",
)

result = run(body=f"Closes {REF}")
check(
    "a genuine Closes body passes",
    result.returncode == 0,
    f"exit={result.returncode} stderr={result.stderr!r}",
)

# ── The backtick escape hatch this guard's own remedy recommends ────────────

result = run(body=f"See `{REF}` for background; unrelated to this change.")
check(
    "a bare backticked reference with no keyword passes",
    result.returncode == 0,
    f"exit={result.returncode} stderr={result.stderr!r}",
)

result = run(body=f"This does not close `{REF}` — only advances it.")
check(
    "a negated keyword with a backticked reference passes",
    result.returncode == 0,
    f"exit={result.returncode} stderr={result.stderr!r}",
)

# ── The negation vocabulary the tracking issue names ─────────────────────────

for negation, body in [
    ("doesn't", f"This doesn't close {REF}."),
    ("never", f"This change never closes {REF} on its own."),
    ("without", f"This lands without needing to close {REF} outright."),
]:
    result = run(body=body)
    check(
        f"negation word '{negation}' is caught",
        result.returncode == 1,
        f"body={body!r} exit={result.returncode} stderr={result.stderr!r}",
    )

# ── Sentence boundary: a trailer after an unrelated negated sentence ────────

result = run(body=f"This does not fix the flaky test.\n\nCloses {REF}")
check(
    "a negation in an earlier sentence does not poison a later trailer",
    result.returncode == 0,
    f"exit={result.returncode} stderr={result.stderr!r}",
)

# ── The keyword family: fix/close/resolve and their inflections ────────────

for keyword in ["closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved"]:
    result = run(body=f"This change does not {keyword} {REF} by itself.")
    check(
        f"keyword '{keyword}' is caught under negation",
        result.returncode == 1,
        f"exit={result.returncode} stderr={result.stderr!r}",
    )

# ── The commit-message channel ───────────────────────────────────────────────

result = run(
    body=f"Refs {REF}",
    commits=f"chore: prep work\n\nThis does not close {REF}.",
)
check(
    "a negated keyword in a commit message fails, even with a clean body",
    result.returncode == 1,
    f"exit={result.returncode} stderr={result.stderr!r}",
)
check(
    "a commit-message violation is labelled as such",
    "a commit message" in result.stderr,
    result.stderr,
)

# ── Nothing to check ──────────────────────────────────────────────────────────

result = run()
check(
    "no body and no commit messages is a clean no-op, not a failure",
    result.returncode == 0,
    f"exit={result.returncode} stderr={result.stderr!r}",
)

# ── Summary ───────────────────────────────────────────────────────────────────

print(f"\n{pass_count} passed, {fail_count} failed")
sys.exit(1 if fail_count else 0)
