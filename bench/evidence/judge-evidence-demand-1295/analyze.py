#!/usr/bin/env python3
"""Measure how often a model judge's pass is the ONLY thing behind a turn.

This is the number #1295 asks for, and it is deliberately computed from the
event stream rather than from a counter the pipeline keeps. Every
``judge_verdict`` event carries the ladder snapshot the verdict was decided
from (#865/#1043), and ``judge_pass_stands_alone`` is a pure function of two
of its fields — so the rate is derivable from artifacts that already exist,
for runs recorded before anyone thought to ask, and cannot drift from the
predicate the pipeline actually applies as long as this file restates it
correctly:

    stands_alone := not flip_achieved and touched_tests_passed is not True

Two commands, split for the same reason `score_dev_baseline.py` splits its
two:

* ``extract`` — read a Harbor job directory into ``turns.jsonl``, one row per
  verification turn. Needs the (large, uncommitted) job tree.
* ``report``  — read one or more ``turns.jsonl`` and print the rates. Needs
  only the committed evidence, so a reader reproduces the number without the
  job tree.

Usage::

    python analyze.py extract <job-dir> --arm on  -o <run>/on/turns.jsonl
    python analyze.py extract <job-dir> --arm off -o <run>/off/turns.jsonl
    python analyze.py report <run>/on/turns.jsonl <run>/off/turns.jsonl
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

# The event the ladder emits for every verification outcome, deterministic or
# judged. One per verification round, so a candidate that revised once emits
# two.
_VERDICT = "judge_verdict"


def _dict(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def stands_alone(ladder: dict[str, Any]) -> bool:
    """Restate ``LadderInputs::judge_pass_stands_alone`` over a wire snapshot.

    Deliberately narrow, exactly as the pipeline's version is: a readable diff
    and a recorded file touch prove the tree *changed*, and neither says the
    change is *right*. Only the second claim is the one a pass makes, so only
    test-shaped evidence can corroborate one.
    """
    return not ladder.get("flip_achieved") and ladder.get("touched_tests_passed") is not True


def read_events(path: Path) -> list[dict[str, Any]]:
    """Parse a `stella-events.jsonl`, skipping unparseable lines.

    A truncated final line is normal — the stream is written live and a trial
    can be killed mid-write by the harness agent timeout — so a bad line is
    dropped rather than failing the trial's whole row.
    """
    events: list[dict[str, Any]] = []
    with path.open(errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return events


def extract_trial(trial_dir: Path, arm: str, layout: str = "harbor") -> list[dict[str, Any]]:
    """One row per verification turn in this trial (possibly none).

    Two layouts, because the measurement can be produced two ways. `harbor` is
    a real trial directory, with the external verifier's reward beside the
    stream. `host` is the same agent run outside a task container, where there
    is no verifier at all — so `reward` stays `None` there, and a reader must
    not read its absence as a zero.
    """
    if layout == "host":
        events_path = trial_dir / "stella-events.jsonl"
        reward_path = None
    else:
        events_path = trial_dir / "agent" / "stella-events.jsonl"
        reward_path = trial_dir / "verifier" / "reward.txt"
    if not events_path.is_file():
        return []
    reward: float | None = None
    if reward_path is not None and reward_path.is_file():
        try:
            reward = float(reward_path.read_text().strip())
        except ValueError:
            reward = None

    task = trial_dir.name.split("__")[0]
    rows: list[dict[str, Any]] = []
    turn = 0
    for event in read_events(events_path):
        if event.get("type") != _VERDICT:
            continue
        evidence = _dict(event.get("evidence"))
        ladder = _dict(evidence.get("ladder"))
        rows.append(
            {
                "arm": arm,
                "task": task,
                "trial": trial_dir.name,
                "turn": turn,
                "reward": reward,
                "passed": bool(event.get("passed")),
                "deterministic": bool(evidence.get("deterministic")),
                # `None` for a stream recorded before the rung was on the wire
                # (#1043). Reported as `unknown` rather than guessed — see
                # `LadderRung` for why the guess is not available.
                "rung": ladder.get("rung"),
                # The precondition the whole feature turns on: with no tracked
                # command, no worker on any turn can clear `stands_alone`.
                "tracked_command": ladder.get("tracked_command"),
                "flip_achieved": bool(ladder.get("flip_achieved")),
                "touched_tests_passed": ladder.get("touched_tests_passed"),
                "stands_alone": stands_alone(ladder) if ladder else None,
                "ladder_present": bool(ladder),
                # The one thing a PRE-ladder stream can still say about the
                # precondition, and it says it in prose: `unverifiable_evidence`
                # spells out "flip oracle not armed (no test command)". Read as
                # a weaker, explicitly-labelled fallback — it is the verdict
                # summary's wording, not a recorded field, so it is never mixed
                # into the snapshot-derived rates.
                "oracle_unarmed_stated": "flip oracle not armed" in (
                    evidence.get("summary") or ""
                ),
            }
        )
        turn += 1
    return rows


def find_trials(job_dir: Path, layout: str = "harbor") -> list[Path]:
    """Locate per-run directories under `job_dir`.

    Harbor 0.6.1 writes `<task>__<suffix>/` per trial under the job dir (later
    releases nest them under `trials/`). The host layout writes one directory
    per task, named for the task alone — there is no attempt suffix because
    there is one run.
    """
    if layout == "host":
        return sorted(path for path in job_dir.iterdir() if path.is_dir())
    nested = job_dir / "trials"
    root = nested if nested.is_dir() else job_dir
    return sorted(path for path in root.iterdir() if path.is_dir() and "__" in path.name)


def cmd_extract(args: argparse.Namespace) -> int:
    job_dir = Path(args.job_dir)
    trials = find_trials(job_dir, args.layout)
    if not trials:
        print(f"no trial directories under {job_dir}", file=sys.stderr)
        return 1
    rows = [row for trial in trials for row in extract_trial(trial, args.arm, args.layout)]
    out = Path(args.output)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as handle:
        for row in rows:
            handle.write(json.dumps(row, sort_keys=True) + "\n")
    silent = sum(1 for trial in trials if not extract_trial(trial, args.arm, args.layout))
    print(
        f"{len(rows)} verification turn(s) from {len(trials)} trial(s) -> {out}"
        + (f"  ({silent} trial(s) emitted no verdict at all)" if silent else "")
    )
    return 0


def _pct(part: int, whole: int) -> str:
    return f"{100.0 * part / whole:5.1f}%" if whole else "    — "


def summarize(rows: list[dict[str, Any]]) -> dict[str, Any]:
    # A turn recorded before the ladder snapshot reached the wire (#865/#1043)
    # carries no `ladder`, and neither `stands_alone` nor `tracked_command` can
    # be derived from it. Those turns are counted SEPARATELY and excluded from
    # every denominator below rather than defaulted: reporting "0% standalone"
    # for a corpus that simply never recorded the fields is the same error the
    # rung vocabulary refuses to make, and in the more flattering direction.
    known = [r for r in rows if r.get("ladder_present")]
    unknown = len(rows) - len(known)
    passes = [r for r in known if r["passed"]]
    judge_passes = [r for r in passes if not r["deterministic"]]
    alone = [r for r in judge_passes if r.get("stands_alone")]
    with_cmd = [r for r in known if r.get("tracked_command")]
    alone_with_cmd = [r for r in alone if r.get("tracked_command")]
    trials = {r["trial"] for r in rows}
    solved = {r["trial"] for r in rows if r.get("reward") == 1.0}
    return {
        "turns": len(rows),
        "turns_scorable": len(known),
        "turns_no_ladder": unknown,
        "trials": len(trials),
        "solved": len(solved),
        "passes": len(passes),
        "judge_passes": len(judge_passes),
        "standalone_judge_passes": len(alone),
        "turns_with_tracked_command": len(with_cmd),
        "standalone_with_tracked_command": len(alone_with_cmd),
        "revised_trials": len({r["trial"] for r in rows if r["turn"] > 0}),
        "oracle_unarmed_stated": sum(1 for r in rows if r.get("oracle_unarmed_stated")),
    }


def cmd_report(args: argparse.Namespace) -> int:
    arms: dict[str, list[dict[str, Any]]] = {}
    for path in args.turns:
        for line in Path(path).read_text().splitlines():
            if not line.strip():
                continue
            row = json.loads(line)
            arms.setdefault(row.get("arm") or Path(path).stem, []).append(row)
    if not arms:
        print("no rows", file=sys.stderr)
        return 1

    for arm, rows in sorted(arms.items()):
        s = summarize(rows)
        print(f"\n=== arm: {arm} ===")
        print(f"  trials                        {s['trials']}")
        print(f"  solved (reward 1.0)           {s['solved']}  {_pct(s['solved'], s['trials'])}")
        print(f"  verification turns            {s['turns']}")
        if s["turns_no_ladder"]:
            # Stated, never folded into a rate. These turns predate the ladder
            # snapshot; the fields this report is about do not exist on them.
            print(
                f"  ... no ladder recorded        {s['turns_no_ladder']}"
                f"  (pre-#865 — NOT scorable, excluded from every rate below)"
            )
            print(
                f"  ... of those, verdict prose says the flip oracle was NOT "
                f"ARMED (no test command): {s['oracle_unarmed_stated']}"
            )
        print(f"  scorable verification turns   {s['turns_scorable']}")
        print(f"  trials that revised           {s['revised_trials']}")
        print(f"  passing turns                 {s['passes']}")
        print(f"  ... of which judge-only       {s['judge_passes']}")
        # THE number #1295 asks for: how often is a judge's word the only
        # thing behind the turn, as a share of every verification turn.
        print(
            f"  STANDALONE judge passes       {s['standalone_judge_passes']}"
            f"  {_pct(s['standalone_judge_passes'], s['turns_scorable'])} of scorable turns,"
            f" {_pct(s['standalone_judge_passes'], s['judge_passes'])} of judge passes"
        )
        # And the precondition that decides whether asking is affordable: a
        # standalone pass with no tracked command is one no ask could fix.
        print(
            f"  turns with a tracked command  {s['turns_with_tracked_command']}"
            f"  {_pct(s['turns_with_tracked_command'], s['turns_scorable'])}"
        )
        print(
            f"  ... standalone AND answerable {s['standalone_with_tracked_command']}"
            f"  {_pct(s['standalone_with_tracked_command'], s['turns_scorable'])} of scorable turns"
        )
    print()
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    extract = sub.add_parser("extract", help="read a Harbor job dir into turns.jsonl")
    extract.add_argument("job_dir")
    extract.add_argument("--arm", required=True, help="label for this run's arm")
    extract.add_argument(
        "--layout",
        choices=("harbor", "host"),
        default="harbor",
        help="harbor trial directories (with a verifier reward) or host runs (without)",
    )
    extract.add_argument("-o", "--output", required=True)
    extract.set_defaults(func=cmd_extract)

    report = sub.add_parser("report", help="print the rates from one or more turns.jsonl")
    report.add_argument("turns", nargs="+")
    report.set_defaults(func=cmd_report)

    args = parser.parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
