#!/usr/bin/env python3
"""Stella Stop-hook oracle: deterministic probes first, judge fallback, bounded.

The verify_done successor for no-test-command turns, as an out-of-process
Stop hook (the deny-loop #2684 shipped and PR #3302 unbounded to three
rounds). The engine consults this script when a turn is about to complete;
an ``{"action": "deny", "reason": ...}`` answer holds the turn open with
the reason as the worker's next observation, and an ``{"action": "allow"}``
lets the completion stand.

Three rungs, weakest evidence last:

1. **Deterministic flip.** If ``STELLA_ORACLE_TEST_COMMAND`` is set (or a
   probe list survives from an earlier round), run it. Exit 0 allows; a
   failure denies with the output tail. The hook itself runs the command,
   so the observation cannot be spoofed by prose — the same posture as the
   retired ``verify_done``, without the shadow-worktree machinery.
2. **Judge-derived probes.** With no configured command, one judge call
   (OpenRouter chat completions, ``STELLA_ORACLE_JUDGE_MODEL``) reads the
   task statement + the diff + the final answer and must return *runnable
   probe commands*, not an opinion. The probes are then run exactly like
   rung 1 — the judge creates the oracle; running it decides. Only when
   the judge can derive no probe at all does its prose verdict decide,
   and an unparseable or unreachable judge ALLOWS (fail-open: a broken
   oracle must not hold a correct turn hostage).
3. **Approach-diversity steering.** State persists across rounds
   (``.stella/private/oracle-state.json``). On the second consecutive
   deny, the reason additionally tells the worker to step back, list two
   fundamentally different approaches, and reimplement with the strongest
   — sequential best-of-N, riding the observation that a model that fails
   twice on one approach usually lands it within three tries on a fresh
   one. Deliberately advice inside the deny reason, not machinery: the
   engine's ``MAX_STOP_CONSULTS`` already bounds the loop.

Deliberately stdlib-only, and every failure path is fail-open (exit 0
allow, or non-zero exit, which the engine treats as a diagnostic and
never a block).
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import urllib.request
from pathlib import Path

#: Where cross-round state lives, relative to the hook's cwd (the
#: workspace). Under ``.stella/private/`` so the generated workspace
#: ``.gitignore`` keeps it out of the diff the judge reads.
STATE_PATH = Path(".stella/private/oracle-state.json")

#: Per-probe wall-clock ceiling. The engine allows a Stop hook ten
#: minutes; leaving headroom for several probes plus the judge call.
PROBE_TIMEOUT_SECONDS = 120

#: How much probe output a deny reason carries back to the worker.
OUTPUT_TAIL_CHARS = 1200

#: The steering appended on the second consecutive deny (rung 3).
DIVERSIFY_NOTE = (
    "\n\nYou have now failed verification twice on this approach. Step back: "
    "re-read the task, write down two fundamentally different approaches, "
    "pick the stronger, and reimplement from scratch rather than patching "
    "the current attempt."
)

JUDGE_SYSTEM_PROMPT = (
    "You are a verification oracle for a coding agent. You are given a task "
    "statement, the diff the agent produced, and its final answer. Reply with "
    "ONLY a JSON object: {\"probes\": [{\"cmd\": \"<shell command>\", "
    "\"description\": \"<what it proves>\"}], \"verdict\": \"pass\"|\"fail\", "
    "\"unmet\": [\"<requirement not met>\", ...]}. Probes are shell commands "
    "that exit 0 exactly when the task's acceptance criteria hold — prefer "
    "probes over opinions, and derive them from the task's own stated "
    "requirements. If no runnable probe can express a requirement, leave "
    "probes empty and let verdict carry your judgement."
)


def read_payload(stdin=None) -> dict:
    """The engine's Stop payload: ``{"event", "cwd", "finalText"}``."""
    raw = (stdin or sys.stdin).read()
    return json.loads(raw) if raw.strip() else {}


def load_state() -> dict:
    try:
        return json.loads(STATE_PATH.read_text())
    except (OSError, ValueError):
        return {"denies": 0, "probes": []}


def save_state(state: dict) -> None:
    try:
        STATE_PATH.parent.mkdir(parents=True, exist_ok=True)
        STATE_PATH.write_text(json.dumps(state))
    except OSError:
        pass  # state is an optimization, never a requirement


def run_probe(cmd: str) -> tuple[bool, str]:
    """Run one probe; ``(passed, output tail)``."""
    try:
        proc = subprocess.run(
            cmd,
            shell=True,
            capture_output=True,
            text=True,
            timeout=PROBE_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired:
        return False, f"probe timed out after {PROBE_TIMEOUT_SECONDS}s: {cmd}"
    tail = (proc.stdout + proc.stderr)[-OUTPUT_TAIL_CHARS:]
    return proc.returncode == 0, tail


def workspace_diff() -> str:
    """What changed, for the judge: tracked diff plus untracked names."""
    try:
        diff = subprocess.run(
            ["git", "diff", "HEAD", "--", ".", ":!**/.stella/**"],
            capture_output=True,
            text=True,
            timeout=30,
        ).stdout
        untracked = subprocess.run(
            ["git", "ls-files", "--others", "--exclude-standard"],
            capture_output=True,
            text=True,
            timeout=30,
        ).stdout
    except (OSError, subprocess.TimeoutExpired):
        return ""
    names = [line for line in untracked.splitlines() if not line.startswith(".stella/")]
    if names:
        diff += "\n\nuntracked files created:\n" + "\n".join(names)
    return diff[:60_000]


def call_judge(task: str, diff: str, final_text: str) -> dict | None:
    """One judge call; ``None`` on any transport or parse failure."""
    api_key = os.environ.get("OPENROUTER_API_KEY", "")
    model = os.environ.get("STELLA_ORACLE_JUDGE_MODEL", "")
    if not api_key or not model:
        return None
    body = json.dumps(
        {
            "model": model,
            "messages": [
                {"role": "system", "content": JUDGE_SYSTEM_PROMPT},
                {
                    "role": "user",
                    "content": (
                        f"## Task\n{task}\n\n## Diff\n{diff}\n\n"
                        f"## Agent's final answer\n{final_text}"
                    ),
                },
            ],
        }
    ).encode()
    request = urllib.request.Request(
        os.environ.get(
            "STELLA_ORACLE_JUDGE_URL",
            "https://openrouter.ai/api/v1/chat/completions",
        ),
        data=body,
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=240) as response:
            content = json.loads(response.read())["choices"][0]["message"]["content"]
    except Exception:
        return None
    match = re.search(r"\{.*\}", content, re.DOTALL)
    if not match:
        return None
    try:
        return json.loads(match.group(0))
    except ValueError:
        return None


def decide(payload: dict, state: dict, judge=call_judge, prober=run_probe) -> dict:
    """The whole decision, pure over its injected effects (testable)."""
    # Rung 1: a configured command, or probes carried from an earlier round.
    probes = []
    configured = os.environ.get("STELLA_ORACLE_TEST_COMMAND")
    if configured:
        probes = [{"cmd": configured, "description": "configured test command"}]
    elif state.get("probes"):
        probes = state["probes"]

    # Rung 2: no oracle yet — ask the judge to derive one.
    verdict = None
    if not probes:
        task = os.environ.get("STELLA_ORACLE_TASK", "")
        judged = judge(task, workspace_diff(), payload.get("finalText", ""))
        if judged is None:
            return {"action": "allow"}  # fail-open: no oracle, no hostage
        probes = [p for p in judged.get("probes", []) if p.get("cmd")]
        verdict = judged
        state["probes"] = probes

    failures = []
    for probe in probes:
        passed, tail = prober(probe["cmd"])
        if not passed:
            failures.append((probe, tail))

    if probes and not failures:
        return {"action": "allow"}
    if not probes and verdict is not None:
        # The judge could derive no probe; its prose verdict decides.
        if verdict.get("verdict") == "pass":
            return {"action": "allow"}
        unmet = "; ".join(verdict.get("unmet", [])) or "requirements not met"
        return deny(state, f"verification failed: {unmet}")

    lines = [
        f"- `{probe['cmd']}` ({probe.get('description', 'probe')}) failed:\n{tail}"
        for probe, tail in failures
    ]
    return deny(state, "verification probes failed:\n" + "\n".join(lines))


def deny(state: dict, reason: str) -> dict:
    state["denies"] = state.get("denies", 0) + 1
    if state["denies"] >= 2:
        reason += DIVERSIFY_NOTE
    return {"action": "deny", "reason": reason}


def main() -> int:
    try:
        payload = read_payload()
        state = load_state()
        decision = decide(payload, state)
        save_state(state)
        print(json.dumps(decision))
        return 0
    except Exception as exc:
        print(f"oracle hook internal error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
