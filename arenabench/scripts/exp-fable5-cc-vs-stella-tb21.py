#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Assemble the seed experiment document: Stella vs Claude Code on Fable 5.

This is the calculation artifact for one experiment — the seed data the
generic experiments store (``arenabench.experiments``) holds, not product
surface. It reads ground-truth artifacts only (each trial's
``result.json``, ``verifier/reward.txt``, and each arm's *own* journal —
``agent/claude-code.txt`` / ``agent/stella-events.jsonl``), never a
dashboard, summary file, or assembly projection, and emits one canonical
JSON document with every raw value, formula, denominator, and source
recorded. Committing this script is what preserves the calculation
version: rerunning it against the same artifacts reproduces the same
document.

Outcome vocabulary is the experiment's preregistered taxonomy
(verified_solve, verified_failure, solved_then_timeout,
timeout_before_solve, invalid_infrastructure, provider_failure,
budget_rejected, aborted, pending, unverified), with the classification
rules and their precedence written into the document itself. Provider
failures are detected from structured fields (Claude Code's terminal
``api_error_status``; Stella's parsed ``error`` events) plus strict
phrase needles — a bare "402" once matched timestamps, which is why
``score-match.py`` insists on phrases.

Usage: python3 arenabench/scripts/exp-fable5-cc-vs-stella-tb21.py
           [--store] [--out FILE]
"""

from __future__ import annotations

import argparse
import contextlib
import json
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from arenabench import pricing
from arenabench.experiments import store_results
from arenabench.sut import arenabench_home
from arenabench.telemetry import INFRASTRUCTURE_FAILURES

CALCULATION_VERSION = "exp-fable5-cc-vs-stella-tb21/1"
EXPERIMENT_ID = "stella-vs-claude-code-fable5-tb21"

#: Strict phrase needles (never bare numbers) for provider rejections in
#: Stella's stream. Loose needles matched timestamps and task content.
STELLA_PROVIDER_NEEDLES = (
    "HTTP 402",
    "payment required",
    "out of credits",
    "HTTP 401",
)

#: Quota-throttle needles for the Claude Code journal.
CC_THROTTLE_NEEDLES = ('"api_error_status":429', "session limit", "rate limit")

SOLVE_CLASSES = ("verified_solve", "solved_then_timeout")
LOSS_CLASSES = ("verified_failure", "timeout_before_solve")
VALID_CLASSES = SOLVE_CLASSES + LOSS_CLASSES


def _read_json(path: Path) -> dict[str, Any] | None:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def _reward(result: dict[str, Any], trial_dir: Path) -> float | None:
    """The external verifier's reward, cross-checked against reward.txt."""
    nested = (result.get("verifier_result") or {}).get("rewards") or {}
    recorded = nested.get("reward", (result.get("verifier_result") or {}).get("reward"))
    txt = trial_dir / "verifier" / "reward.txt"
    if txt.exists():
        try:
            file_reward = float(txt.read_text().strip())
        except ValueError:
            file_reward = None
        if file_reward is not None:
            return file_reward
    return float(recorded) if recorded is not None else None


def _cc_journal(trial_dir: Path) -> dict[str, Any]:
    """Facts from Claude Code's own stream-json journal."""
    path = trial_dir / "agent" / "claude-code.txt"
    facts: dict[str, Any] = {"present": path.exists(), "path": str(path)}
    if not path.exists():
        return facts
    text = path.read_text(encoding="utf-8", errors="replace")
    facts["throttle_needle_hits"] = sum(text.count(n) for n in CC_THROTTLE_NEEDLES)
    terminal = None
    for line in reversed(text.splitlines()):
        if '"type":"result"' in line:
            with contextlib.suppress(json.JSONDecodeError):
                terminal = json.loads(line)
            break
    if terminal is not None:
        usage = terminal.get("usage") or {}
        facts["terminal"] = {
            "is_error": terminal.get("is_error"),
            "api_error_status": terminal.get("api_error_status"),
            "terminal_reason": terminal.get("terminal_reason"),
            "result_text": str(terminal.get("result", ""))[:200],
            "num_turns": terminal.get("num_turns"),
            "total_cost_usd": terminal.get("total_cost_usd"),
            "duration_ms": terminal.get("duration_ms"),
            "duration_api_ms": terminal.get("duration_api_ms"),
            "input_tokens": usage.get("input_tokens"),
            "cache_read_input_tokens": usage.get("cache_read_input_tokens"),
            "cache_creation_input_tokens": usage.get("cache_creation_input_tokens"),
            "output_tokens": usage.get("output_tokens"),
        }
    return facts


def _stella_journal(trial_dir: Path) -> dict[str, Any]:
    """Facts from Stella's own event stream."""
    path = trial_dir / "agent" / "stella-events.jsonl"
    facts: dict[str, Any] = {"present": path.exists(), "path": str(path)}
    if not path.exists():
        return facts
    usage_by_model: dict[str, dict[str, int | float]] = {}
    error_messages: list[str] = []
    terminal_type = None
    tool_calls = 0
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        etype = event.get("type")
        terminal_type = etype or terminal_type
        if etype == "error":
            error_messages.append(str(event.get("message", ""))[:300])
        elif etype == "tool_start":
            tool_calls += 1
        elif etype == "step_usage":
            model = str(event.get("model", "?"))
            slot = usage_by_model.setdefault(
                model,
                {
                    "calls": 0,
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cached_input_tokens": 0,
                    "cache_write_tokens": 0,
                    "cost_usd": 0.0,
                    "duration_ms": 0,
                },
            )
            slot["calls"] += 1
            for key in (
                "input_tokens",
                "output_tokens",
                "cached_input_tokens",
                "cache_write_tokens",
                "cost_usd",
                "duration_ms",
            ):
                slot[key] += event.get(key) or 0
    provider_hits = [
        m for m in error_messages if any(n in m for n in STELLA_PROVIDER_NEEDLES)
    ]
    if not provider_hits:
        # A reaped stream can lose its terminal error event (#2131); fall
        # back to a raw strict-phrase scan and say the evidence came from it.
        raw = path.read_text(encoding="utf-8", errors="replace")
        if any(n in raw for n in STELLA_PROVIDER_NEEDLES[:3]):
            provider_hits = ["<needle found outside a parsed error event>"]
    facts.update(
        {
            "terminal_event_type": terminal_type,
            "error_messages": error_messages[:3],
            "provider_rejection_evidence": provider_hits[:2],
            "tool_calls": tool_calls,
            "usage_by_model": usage_by_model,
        }
    )
    return facts


def classify(
    *,
    reward: float | None,
    exception_type: str | None,
    exception_message: str,
    cc: dict[str, Any],
    stella: dict[str, Any],
    ran: bool,
) -> tuple[str, list[str]]:
    """Mission taxonomy, precedence top-down. Returns (label, evidence)."""
    if not ran:
        return "pending", ["no trial artifacts: the task was never attempted"]
    if exception_type == "CancelledError":
        return "aborted", [f"exception_info: {exception_type}"]
    if exception_type == "DeliberateStopError":
        if "budget" in exception_message.lower():
            return "budget_rejected", [f"DeliberateStopError: {exception_message[:160]}"]
        return "aborted", [f"DeliberateStopError: {exception_message[:160]}"]
    terminal = cc.get("terminal") or {}
    if terminal.get("is_error") and terminal.get("api_error_status") in (401, 402, 429):
        return "provider_failure", [
            f"claude-code.txt terminal: api_error_status={terminal['api_error_status']}"
            f" result={terminal.get('result_text', '')!r}"
        ]
    if stella.get("provider_rejection_evidence"):
        return "provider_failure", [
            f"stella-events.jsonl error: {m}"
            for m in stella["provider_rejection_evidence"]
        ]
    if (
        exception_type == "AgentTimeoutError"
        and cc.get("present")
        and cc.get("throttle_needle_hits", 0) > 0
    ):
        # Judgment call, recorded as such: a run throttled by quota that then
        # timed out measures the quota, not the agent (fable5-cc-repair.toml
        # states the same rule for these exact trials).
        return "provider_failure", [
            "AgentTimeoutError with "
            f"{cc['throttle_needle_hits']} quota-throttle needle hits in the "
            "agent's own journal (classified as quota, not capability — inference)"
        ]
    if exception_type in INFRASTRUCTURE_FAILURES:
        return "invalid_infrastructure", [f"exception_info: {exception_type}"]
    if reward is not None and reward >= 1.0:
        if exception_type == "AgentTimeoutError":
            return "solved_then_timeout", [
                f"reward {reward} with AgentTimeoutError — solved, then ran out"
            ]
        return "verified_solve", [f"external verifier reward {reward}"]
    if exception_type == "AgentTimeoutError":
        return "timeout_before_solve", ["AgentTimeoutError with reward < 1"]
    if reward is None:
        return "unverified", ["no verifier reward was recorded"]
    return "verified_failure", [
        f"external verifier reward {reward}"
        + (f"; exception {exception_type}" if exception_type else "")
    ]


def read_trial(trial_dir: Path, arm: str) -> dict[str, Any] | None:
    result = _read_json(trial_dir / "result.json")
    if result is None:
        return None
    exc = result.get("exception_info") or {}
    exception_type = exc.get("exception_type")
    exception_message = str(exc.get("exception_message", ""))
    reward = _reward(result, trial_dir)
    cc = _cc_journal(trial_dir) if arm == "claude-code" else {}
    stella = _stella_journal(trial_dir) if arm == "stella" else {}
    label, evidence = classify(
        reward=reward,
        exception_type=exception_type,
        exception_message=exception_message,
        cc=cc,
        stella=stella,
        ran=True,
    )
    envelope = result.get("agent_result") or {}
    metadata = envelope.get("metadata") or {}
    # Harbor's task_name is org-qualified ("terminal-bench/fix-git"); match
    # specs carry the bare name. The bare name is the join key everywhere.
    task_name = str(result.get("task_name") or "").rsplit("/", 1)[-1]
    trial: dict[str, Any] = {
        "task": task_name,
        "trial": result.get("trial_name"),
        "arm": arm,
        "outcome": label,
        "outcome_evidence": evidence,
        "reward": reward,
        "exception_type": exception_type,
        "started_at": result.get("started_at"),
        "finished_at": result.get("finished_at"),
        "raw": {
            "envelope_tokens_in": envelope.get("n_input_tokens"),
            "envelope_tokens_cached": envelope.get("n_cache_tokens"),
            "envelope_tokens_out": envelope.get("n_output_tokens"),
            "envelope_cost_usd": envelope.get("cost_usd"),
            "stella_status": metadata.get("stella_status"),
        },
        "sources": {"result_json": str(trial_dir / "result.json")},
    }
    if arm == "claude-code" and cc.get("terminal"):
        t = cc["terminal"]
        tokens_in = sum(
            t.get(k) or 0
            for k in ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")
        )
        trial["measured"] = {
            "basis": "agent_own_journal (claude-code.txt terminal result)",
            "cost_usd": t.get("total_cost_usd"),
            "tokens_in_total": tokens_in,
            "tokens_cache_read": t.get("cache_read_input_tokens"),
            "tokens_cache_write": t.get("cache_creation_input_tokens"),
            "tokens_out": t.get("output_tokens"),
            "wall_ms": t.get("duration_ms"),
            "model_ms": t.get("duration_api_ms"),
            "turns": t.get("num_turns"),
        }
        trial["sources"]["journal"] = cc["path"]
    if arm == "stella" and stella.get("usage_by_model"):
        by_model = stella["usage_by_model"]
        trial["measured"] = {
            "basis": "agent_own_journal (stella-events.jsonl step_usage sum)",
            "cost_usd": round(sum(m["cost_usd"] for m in by_model.values()), 6),
            "tokens_in_total": sum(m["input_tokens"] for m in by_model.values()),
            "tokens_cache_read": sum(m["cached_input_tokens"] for m in by_model.values()),
            "tokens_cache_write": sum(m["cache_write_tokens"] for m in by_model.values()),
            "tokens_out": sum(m["output_tokens"] for m in by_model.values()),
            "model_ms": sum(m["duration_ms"] for m in by_model.values()),
            "tool_calls": stella.get("tool_calls"),
            "by_model": by_model,
        }
        trial["sources"]["journal"] = stella["path"]
    return trial


def reprice_trial(
    trial: dict[str, Any], route_api: str, model: str = "claude-fable-5"
) -> tuple[float | None, str]:
    """List-price the trial from its own token counts; None when unpriceable."""
    measured = trial.get("measured")
    if not measured:
        return None, "no usage measured"
    by_model = measured.get("by_model")
    if by_model:
        total = 0.0
        for model, usage in by_model.items():
            price = pricing.price_on_route(route_api, model)
            if price is None:
                return None, f"unpriced model {model} (missing beats wrong)"
            total += (
                pricing.trial_cost(
                    price,
                    tokens_in=int(usage["input_tokens"]),
                    tokens_out=int(usage["output_tokens"]),
                    cache_read=int(usage["cached_input_tokens"]),
                    cache_write=int(usage["cache_write_tokens"]),
                )
                or 0.0
            )
        return round(total, 6), "priced per-model on the seat's recorded route"
    price = pricing.price_on_route(route_api, model)
    cost = pricing.trial_cost(
        price,
        tokens_in=int(measured.get("tokens_in_total") or 0),
        tokens_out=int(measured.get("tokens_out") or 0),
        cache_read=int(measured.get("tokens_cache_read") or 0),
        cache_write=int(measured.get("tokens_cache_write") or 0),
    )
    return (round(cost, 6) if cost is not None else None), (
        "priced on the seat's recorded route" if cost is not None else "unpriced"
    )


def aggregate(trials: list[dict[str, Any]]) -> dict[str, Any]:
    counts: dict[str, int] = {}
    for t in trials:
        counts[t["outcome"]] = counts.get(t["outcome"], 0) + 1
    valid = [t for t in trials if t["outcome"] in VALID_CLASSES]
    solves = [t for t in trials if t["outcome"] in SOLVE_CLASSES]
    excluded = [t for t in trials if t["outcome"] not in VALID_CLASSES]
    # A trial without a terminal usage record (a timed-out agent's journal
    # has no result line) is UNMEASURED, not zero. Totals over trials that
    # did measure are therefore lower bounds, and say so.
    unmeasured = sum(
        1 for t in valid if (t.get("measured") or {}).get("cost_usd") is None
    )
    measured_valid = sum(
        (t.get("measured") or {}).get("cost_usd") or 0.0 for t in valid
    )
    measured_excluded = sum(
        (t.get("measured") or {}).get("cost_usd") or 0.0 for t in excluded
    )
    repriced_values = [t.get("repriced_cost_usd") for t in valid]
    repriced_total = (
        None
        if any(v is None for v in repriced_values)
        else round(sum(repriced_values), 6)
    )
    return {
        "attempted_trials": len(trials),
        "outcome_counts": counts,
        "valid_attempts": len(valid),
        "verified_solves": len(solves),
        "solve_rate": (
            round(len(solves) / len(valid), 4) if valid else None
        ),
        "valid_trials_with_unmeasured_usage": unmeasured,
        "measured_cost_usd_valid_trials": round(measured_valid, 6),
        "measured_cost_is_lower_bound": unmeasured > 0,
        "measured_cost_usd_excluded_trials": round(measured_excluded, 6),
        "repriced_cost_usd_valid_trials": repriced_total,
        "cost_per_verified_solve_measured_usd": (
            round(measured_valid / len(solves), 6) if solves else None
        ),
        "cost_per_verified_solve_repriced_usd": (
            round(repriced_total / len(solves), 6)
            if solves and repriced_total is not None
            else None
        ),
    }


def read_arm(
    job_dir: Path, arm: str, route_api: str, expected_tasks: list[str] | None = None
) -> dict[str, Any]:
    trials: list[dict[str, Any]] = []
    seen: set[str] = set()
    for trial_dir in sorted(job_dir.glob("*__*")):
        if not trial_dir.is_dir():
            continue
        trial = read_trial(trial_dir, arm)
        if trial is None:
            continue
        cost, note = reprice_trial(trial, route_api)
        trial["repriced_cost_usd"] = cost
        trial["repriced_note"] = note
        trials.append(trial)
        seen.add(str(trial["task"]))
    for task in expected_tasks or []:
        if task not in seen:
            trials.append(
                {
                    "task": task,
                    "arm": arm,
                    "outcome": "pending",
                    "outcome_evidence": [
                        "listed in the match spec but no trial artifacts exist"
                    ],
                    "reward": None,
                }
            )
    return {"trials": trials, "aggregates": aggregate(trials)}


def read_assembly_run(match_dir: Path, model: str) -> dict[str, Any]:
    """Classify every cell of an assembled (cloud-fanned) match.

    ``assembly.json`` cells carry no outcome — status is derived from each
    cell's own artifacts, which is the point: one classifier over ground
    truth, where today three consumers disagree.
    """
    assembly = _read_json(match_dir / "assembly.json") or {}
    per_arm: dict[str, list[dict[str, Any]]] = {}
    unreadable = 0
    for cell in assembly.get("cells", []):
        artifacts = (match_dir / cell.get("artifacts", "")).resolve()
        arm = "claude-code" if "claude-code" in str(cell.get("seat")) else "stella"
        trial = read_trial(artifacts, arm)
        if trial is None:
            unreadable += 1
            continue
        route = "anthropic" if arm == "claude-code" else "openrouter"
        cost, note = reprice_trial(trial, route, model=model)
        trial["repriced_cost_usd"] = cost
        trial["repriced_note"] = note
        per_arm.setdefault(arm, []).append(trial)
    return {
        "unreadable_cells": unreadable,
        "arms": {
            arm: {"trials": trials, "aggregates": aggregate(trials)}
            for arm, trials in sorted(per_arm.items())
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--store", action="store_true", help="persist into experiments.db")
    parser.add_argument("--out", type=Path, help="also write the document to this file")
    args = parser.parse_args()

    home = arenabench_home()
    matches = home / "matches"

    h2h = matches / "dd52a57a6f49" / "jobs"
    cc_h2h = read_arm(
        h2h / "dd52a57a6f49-claude-code-fable-5", "claude-code", "anthropic"
    )
    stella_h2h = read_arm(
        h2h / "dd52a57a6f49-stella-fable-5-pipeline", "stella", "openrouter"
    )

    cc_panel = read_arm(
        matches / "41c7e447666a" / "jobs" / "41c7e447666a-claude-code-fable-5",
        "claude-code",
        "anthropic",
    )
    spec_96b5 = _read_json(matches / "96b5d7b9710c" / "spec.json") or {}
    stella_panel = read_arm(
        matches
        / "96b5d7b9710c"
        / "jobs"
        / "96b5d7b9710c-stella-fable-5-full-pipeline-xhigh",
        "stella",
        "openrouter",
        expected_tasks=spec_96b5.get("tasks") or [],
    )

    paired = []
    stella_by_task = {t["task"]: t for t in stella_h2h["trials"]}
    for t in cc_h2h["trials"]:
        partner = stella_by_task.get(t["task"])
        if partner is None:
            continue
        both_valid = (
            t["outcome"] in VALID_CLASSES and partner["outcome"] in VALID_CLASSES
        )
        paired.append(
            {
                "task": t["task"],
                "claude_code": t["outcome"],
                "stella": partner["outcome"],
                "matched_usable": both_valid,
            }
        )

    document = {
        "schema": "arenabench-experiment-document/1",
        "calculation_version": CALCULATION_VERSION,
        "experiment": {
            "id": EXPERIMENT_ID,
            "title": (
                "Can Stella reliably outperform Claude Code on Fable 5 across >= 4 "
                "independent matched Terminal-Bench 2.1 repetitions, at materially "
                "lower cost per verified solve?"
            ),
            "status": "hypothesis_untested_evidence_incomplete",
            "verdict": (
                "No claim is made in either direction. Zero matched repetitions "
                "exist against a requirement of four; the single head-to-head has "
                "4 of 6 comparator trials invalidated by provider quota."
            ),
        },
        "subject_resolution": {
            "supplied_label": "Fable 5",
            "resolved_model_id": "claude-fable-5",
            "arms": {
                "claude-code": {
                    "route": "anthropic/claude-fable-5",
                    "provider": "Anthropic first-party (no base_url)",
                    "auth": "Claude subscription OAuth (CLAUDE_CODE_OAUTH_TOKEN)",
                    "source": "matches/dd52a57a6f49/jobs/*/config.json:model_name",
                },
                "stella": {
                    "route": "openrouter/anthropic/claude-fable-5",
                    "provider": "OpenRouter (metered credits)",
                    "source": "matches/dd52a57a6f49/jobs/*/config.json:model_name",
                },
            },
        },
        "taxonomy": {
            "classes": [
                "verified_solve",
                "verified_failure",
                "solved_then_timeout",
                "timeout_before_solve",
                "invalid_infrastructure",
                "provider_failure",
                "budget_rejected",
                "aborted",
                "pending",
                "unverified",
            ],
            "precedence": (
                "pending > aborted/budget_rejected > provider_failure > "
                "invalid_infrastructure > solved_then_timeout/verified_solve > "
                "timeout_before_solve > unverified > verified_failure"
            ),
            "never_a_loss": [
                "aborted",
                "invalid_infrastructure",
                "provider_failure",
                "budget_rejected",
                "pending",
                "unverified",
            ],
        },
        "formulas": {
            "valid_attempts": "count(outcome in {verified_solve, verified_failure, "
            "solved_then_timeout, timeout_before_solve})",
            "verified_solves": "count(outcome in {verified_solve, solved_then_timeout})",
            "solve_rate": "verified_solves / valid_attempts",
            "cost_per_verified_solve": "sum(cost over valid trials) / verified_solves",
            "measured_cost": "each arm's own journal (CC terminal total_cost_usd; "
            "Stella sum of step_usage.cost_usd); pricing bases differ across arms "
            "and are never compared without the label",
            "repriced_cost": "arenabench.pricing.trial_cost over the trial's own "
            "token counts at one list-price table (claude-fable-5: 10/50 in/out, "
            "cache read 1.0, write 12.5 per Mtok); unpriced => null, never a guess",
            "units": {"cost": "USD", "tokens": "tokens", "durations": "ms"},
        },
        "runs": [
            {
                "match_id": "dd52a57a6f49",
                "role": "the only head-to-head",
                "executed": "2026-08-04/05",
                "tasks": 6,
                "comparability_key": {
                    "dataset": "terminal-bench-2.1 (staged path)",
                    "dataset_digest": {
                        "value": None,
                        "status": "unrecorded (provenance.json dataset_digest is "
                        "empty; source: backfilled, measured: false)",
                    },
                    "attempts_per_task": 1,
                    "seed": {"value": None, "status": "no seed concept in harness"},
                    "harbor_version": {
                        "value": "0.6.1",
                        "status": "inference — run-fable5.sh hard-asserts 0.6.1; "
                        "provenance.json records null",
                    },
                    "effort": "medium (both arms)",
                    "reasoning": True,
                    "budget_usd": None,
                    "max_tokens": None,
                    "timeout_multiplier": 1.0,
                    "agent_setup_timeout_multiplier": 4.0,
                    "environment": "docker, DOCKER_DEFAULT_PLATFORM=linux/amd64, "
                    "local macOS host",
                    "runner_image_digest": {
                        "value": None,
                        "status": "unrecorded — no image-digest field exists in any "
                        "provenance schema generation",
                    },
                    "sut_git_sha": {
                        "value": None,
                        "status": "unrecorded in the match; ~/.arenabench/sut/"
                        "sut_commit.txt is mutable state and cannot date this run",
                    },
                    "claude_code_version": {"value": None, "status": "unrecorded"},
                    "stella_roles": {
                        "verifier": "moonshotai/kimi-k3 (high)",
                        "triage": "anthropic/claude-haiku-4.5 (low)",
                    },
                },
                "arms": {"claude-code": cc_h2h, "stella": stella_h2h},
                "paired_cells": paired,
            },
            {
                "match_id": "41c7e447666a",
                "role": "unpaired Claude Code 8-task baseline (xhigh)",
                "executed": "2026-08-05",
                "comparability_key": {
                    "effort": "xhigh",
                    "concurrency": 2,
                    "agent_setup_timeout_multiplier": 6.0,
                    "not_comparable_to": [
                        "dd52a57a6f49 (effort medium)",
                        "96b5d7b9710c (3 days apart, concurrency 1, co-tenant load, "
                        "aborted)",
                    ],
                },
                "arms": {"claude-code": cc_panel},
            },
            {
                "match_id": "96b5d7b9710c",
                "role": "unpaired Stella 8-task run (xhigh) — operator-aborted",
                "executed": "2026-08-08",
                "comparability_key": {
                    "effort": "xhigh",
                    "concurrency": 1,
                    "budget_usd": 5.0,
                    "sut_git_sha": "7edec3b2333e0a8649bb1fcc7190ad97242d9e3e",
                    "dataset_digest": "sha256:7d7bdc1cbedad549fc1140404bd4dc45e5fd0"
                    "ea7c4186773687d177ad3a0699a",
                    "harbor_version": "0.6.1 (measured provenance)",
                },
                "arms": {"stella": stella_panel},
            },
            {
                "match_id": "VOID-ambient-key-095a17dda694",
                "role": "voided by the operator before scoring",
                "excluded_because": "launched accidentally with the ambient "
                "ANTHROPIC_API_KEY instead of the subscription OAuth token; both "
                "in-flight trials cancelled ~2min in, no scored result (VOID.md)",
                "arms": {},
            },
        ],
        "replications": {
            "required_matched_repetitions": 4,
            "completed_matched_repetitions": 0,
            "definition": "one full run of one task set with both arms producing "
            "valid (non-excluded) trials under one comparability key",
            "nearest_evidence": "dd52a57a6f49 with 2 of 6 paired cells usable",
        },
        "related_runs": [
            {
                "match_id": "h2h891-full",
                "subject": "claude-sonnet-5 (NOT the experiment's model)",
                "why_here": "the improvement-history context: the five-tool bare "
                "loop campaign this experiment inherits its harness and fixes "
                "from; classified under this document's single taxonomy where "
                "the existing consumers (score-match.py, metrics-match.py, "
                "arenabench telemetry) disagree three ways about the 402-dead "
                "trials",
                "not_comparable_because": "different model (sonnet-5), different "
                "tool set (five-tool bare loop), different SUT builds across "
                "slices (208 distinct provenance records per assembly warning)",
                **read_assembly_run(matches / "h2h891-full", "claude-sonnet-5"),
            }
        ],
    }

    if args.out:
        args.out.write_text(
            json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        print(f"wrote {args.out}")
    if args.store:
        store_results(document)
        print(f"stored into {home / 'experiments.db'}")
    if not args.out and not args.store:
        json.dump(document, sys.stdout, indent=2, sort_keys=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
