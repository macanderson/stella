"""Build a run directory on disk that looks exactly like a mirrored S3 run.

Every test here is offline by construction: no test may reach S3 or GitHub, so
the traces are written rather than fetched. The shapes below are transcribed
from real artifacts under
`s3://arenabench-artifacts-578673726240/runs/w10p/` — the field spellings, the
`proof`/`step.kind` nesting, the `output.ok.content` / `output.error.message`
envelopes and harbor's `exception_stats` bucket are copied, not invented,
because a fixture that disagrees with the wire tests the fixture.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

MATCH_ID = "abc123abc123"
JOB = f"{MATCH_ID}-stella-glm-5-2-pipeline-openrouter"


def event(**fields: Any) -> dict[str, Any]:
    return fields


def proof(kind: str, **step: Any) -> dict[str, Any]:
    """A `proof` event. Note the nesting: the kind is on `step`, not the top."""
    return {"ts": 1, "type": "proof", "step": {"kind": kind, **step}}


def tool_pair(call_id: str, name: str, args: dict[str, Any], output: dict[str, Any]):
    return [
        {"ts": 1, "type": "tool_start", "call": {"call_id": call_id, "name": name, "input": args}},
        {"ts": 2, "type": "tool_result", "call_id": call_id, "output": output},
    ]


def ok(content: str) -> dict[str, Any]:
    return {"ok": {"content": content}}


def err(message: str, *, cls: str | None = None) -> dict[str, Any]:
    """An error envelope; `cls` adds the wire's optional `class` token.

    The default omits the key entirely, matching what an old trace or an
    unclassified error puts on the wire (`skip_serializing_if` in
    `crates/stella-protocol/src/tool.rs`).
    """
    envelope: dict[str, Any] = {"message": message}
    if cls is not None:
        envelope["class"] = cls
    return {"error": envelope}


def write_run(
    root: Path,
    trials: list[dict[str, Any]],
    *,
    declared_worker: str = "openrouter/z-ai/glm-5.2",
    declared_roles: dict[str, str] | None = None,
    sut_ref: str = "554bb4f774515297273b25a919bfbfe10bea6974",
    bare_loop: bool = False,
) -> Path:
    """Write `runs/<run_id>/` from a list of trial descriptors.

    Each descriptor: `{"task": str, "events": [...], "reward": float|None,
    "exception": str|None, "raw_tail": str|None}`. `raw_tail` appends a line
    verbatim, which is how the half-written-JSON case is exercised.
    """
    declared_roles = declared_roles or {
        "triage": "openrouter/z-ai/glm-4.7-flash",
        "plan": "openrouter/deepseek/deepseek-chat",
    }
    root.mkdir(parents=True, exist_ok=True)
    for index, spec in enumerate(trials):
        uuid = f"0000000{index}-0000-4000-8000-00000000000{index}"
        task_id = spec["task"]
        job_dir = root / uuid / "ws" / "matches" / MATCH_ID / "jobs" / JOB
        task_dir = job_dir / task_id
        (task_dir / "agent").mkdir(parents=True, exist_ok=True)
        (task_dir / "verifier").mkdir(parents=True, exist_ok=True)

        lines = [json.dumps(e) for e in spec.get("events", [])]
        if spec.get("raw_tail"):
            lines.append(spec["raw_tail"])
        (task_dir / "agent" / "stella-events.jsonl").write_text("\n".join(lines) + "\n")

        if spec.get("reward") is not None:
            (task_dir / "verifier" / "reward.txt").write_text(f"{spec['reward']:g}\n")
        if spec.get("exception"):
            (task_dir / "exception.txt").write_text("Traceback\n  ...\n" + spec["exception"] + "\n")

        # Harbor's job-level result.json — where the exception CLASS lives. Not
        # the trial-root results.json below, which is arenabench's.
        exception_stats: dict[str, list[str]] = {}
        if spec.get("exception_class"):
            exception_stats[spec["exception_class"]] = [task_id]
        (job_dir / "result.json").write_text(
            json.dumps({"stats": {"evals": {"e": {"exception_stats": exception_stats}}}})
        )

        (root / uuid / "results.json").write_text(
            json.dumps(
                {
                    "match": {
                        "id": MATCH_ID,
                        "name": "fixture match",
                        "sut_ref": sut_ref,
                        "contestants": [
                            {
                                "engine": {
                                    "qualified_model": declared_worker,
                                    "bare_loop": bare_loop,
                                    "roles": {
                                        role: {"model": model}
                                        for role, model in declared_roles.items()
                                    },
                                }
                            }
                        ],
                    }
                }
            )
        )
    return root
