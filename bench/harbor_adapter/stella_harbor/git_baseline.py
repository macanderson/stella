"""Pre-agent git baseline for bare Terminal-Bench task workspaces.

Terminal-Bench task images are plain directories, and on a plain directory
Stella's diff probe is structurally blind (#973): every empty diff reads as
"cannot read the tree", candidate isolation is unavailable, the witness author
has nothing to snapshot, and the mutation audit cannot restore — most of the
verification ladder degrades on exactly this benchmark. One baseline commit
before the agent starts turns those channels on (issue #1211 item 4).

Its own module rather than more length on the adapter package root, per the
repository's file-size ratchet rule: separable new code becomes a module.
"""

from __future__ import annotations

import shlex
import sys
from typing import Any

# Default ON; set the env to a falsy value to run the old bare-directory
# posture. A scored-behavior change either way — a claim run states which
# posture it used.
GIT_BASELINE_ENV = "STELLA_TB_GIT_BASELINE"

# Skip the baseline when the workspace is larger than this many KiB: git
# objects roughly double a dataset-heavy image's disk usage, and task images
# carry disk limits the baseline must never be the thing that breaches.
GIT_BASELINE_SIZE_CAP_KB = 524_288  # 512 MiB


def git_baseline_script(workdir: str | None) -> str:
    """The guarded, best-effort shell that gives a bare task dir a baseline.

    Every guard exits 0 — this must never fail a trial:

    - no ``git`` in the image → skip. The adapter uploads only the frozen
      Stella binary and provisions no utilities (protocol: container
      utilities), so a git-less image simply keeps the bare-directory posture;
    - a repository already present → skip. That covers a *broken* one
      (``fix-git``'s puzzle state must not be touched) via ``-e .git``, and a
      parent-directory repository via ``git rev-parse --git-dir``;
    - workspace over the size cap → skip rather than double its disk usage.

    The committer identity rides per-invocation ``-c`` flags so no config file
    the task or the agent could observe is written, and ``.stella/`` is
    excluded via ``.git/info/exclude`` (inside ``.git``, invisible to the
    task) so Stella's own state — the code-graph DB the very next install
    step creates — never enters the baseline or a later diff.
    """
    lead = f"cd {shlex.quote(str(workdir))} 2>/dev/null || exit 0; " if workdir else ""
    identity = "-c user.name=stella-baseline -c user.email=baseline@stella.invalid"
    return (
        lead
        + "command -v git >/dev/null 2>&1 || { "
        + "echo 'stella-adapter: git baseline skipped: git is not installed'; "
        + "exit 0; }; "
        + "if [ -e .git ] || git rev-parse --git-dir >/dev/null 2>&1; then "
        + "echo 'stella-adapter: git baseline skipped: a repository is already"
        + " present'; exit 0; fi; "
        + "size_kb=$(du -sk . 2>/dev/null | cut -f1); "
        + f'if [ "${{size_kb:-0}}" -gt {GIT_BASELINE_SIZE_CAP_KB} ]; then '
        + 'echo "stella-adapter: git baseline skipped: workspace ${size_kb}KB '
        + f'exceeds the {GIT_BASELINE_SIZE_CAP_KB}KB cap"; exit 0; fi; '
        + "git init -q && "
        + "printf '.stella/\\n' >> .git/info/exclude && "
        + f"{{ git {identity} add -A . >/dev/null 2>&1 || true; }} && "
        + f"git {identity} commit -q --allow-empty --no-gpg-sign "
        + "-m 'stella adapter: pre-agent baseline' && "
        + "echo 'stella-adapter: git baseline committed'"
    )


async def run_git_baseline(agent: Any, environment: Any) -> None:
    """Give a bare task workspace a git baseline before the agent starts.

    Ordered before the adapter's code-graph step so the ``.stella/``
    exclusion is in place before ``stella init`` writes its DB into the
    workspace. Best-effort by construction: every skip reason is a printed
    line, never a failed trial.
    """
    configured = agent._configured_value(GIT_BASELINE_ENV)
    if configured is not None and not _is_truthy(configured):
        return
    cwd = getattr(environment.task_env_config, "workdir", None)
    try:
        result = await agent.exec_as_agent(
            environment,
            command=git_baseline_script(cwd),
            timeout_sec=120,
        )
    except Exception as exc:  # noqa: BLE001 - a baseline aid, never fatal
        print(f"stella-adapter: git baseline unavailable: {exc}", file=sys.stderr)
        return
    summary = getattr(result, "stdout", None) or ""
    for line in summary.splitlines():
        if line.startswith("stella-adapter: git baseline"):
            print(line, file=sys.stderr)


def _is_truthy(value: str | None) -> bool:
    """Mirror of the adapter root's `_is_truthy` (kept import-free both ways:
    the root imports this module, so this module must not import the root)."""
    if not value:
        return False
    return value.strip().lower() in ("1", "true", "yes", "on")
