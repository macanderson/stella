"""`make triage-bench-traces` — a bench run in, GitHub issue activity out.

    python3 bench/trace_triage/triage_bench_traces.py --run w10p            # dry run
    python3 bench/trace_triage/triage_bench_traces.py --run w10p --apply    # writes

The default is a **dry run**, and that is not a convenience: this writes to a
real tracker that other people read. A dry run prints, for every finding, the
exact title and the exact body it would post, the de-duplication decision and
what that decision was made on, so the plan can be read before anything reaches
GitHub. `--apply` is the only thing that writes, and even then it will not open
more than `--max-new-issues` issues in one invocation — anything past the cap is
printed as SUPPRESSED with its full evidence, never silently dropped.

Artifacts are read from a local mirror. `--fetch` populates one with `aws s3
sync`, excluding the multi-hundred-megabyte `stella-run.json` and stdout copies
that are re-renderings of the event stream:

    --run w10p --fetch                      # mirror to ./.trace-mirror/w10p
    --run w10p --mirror ~/w10p              # a mirror you already have
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from detectors import DETECTORS, Finding, run_all  # noqa: E402
from fingerprint import LEDGER_PATH, Ledger  # noqa: E402
from issues import (  # noqa: E402
    REPO,
    Action,
    Decision,
    GhCli,
    apply_actions,
    plan_actions,
)
from run_trace import S3_BUCKET, Run, load_run  # noqa: E402

DEFAULT_MIRROR = Path(".trace-mirror")

# What a mirror needs. `stella-run.json` and `stella-run.stdout.txt` are
# re-renderings of the event stream and can each exceed 70 MB per trial, so
# fetching them would cost gigabytes to answer questions the JSONL answers.
_INCLUDES = (
    "*stella-events.jsonl",
    "*reward.txt",
    "*exception.txt",
    "*/result.json",
    "*/results.json",
    "*config.json",
    "*spec.json",
    "*seat.json",
    "*provenance.json",
)


def fetch_run(run_id: str, mirror: Path) -> Path:
    destination = mirror / run_id
    destination.mkdir(parents=True, exist_ok=True)
    args = [
        "aws",
        "s3",
        "sync",
        f"s3://{S3_BUCKET}/runs/{run_id}/",
        str(destination) + "/",
        "--exclude",
        "*",
        "--no-progress",
    ]
    for pattern in _INCLUDES:
        args += ["--include", pattern]
    result = subprocess.run(args, check=False)
    if result.returncode != 0:
        raise SystemExit(f"aws s3 sync failed for run {run_id} (exit {result.returncode})")
    return destination


def _print_action(action: Action, index: int, total: int) -> None:
    finding = action.finding
    bar = "=" * 78
    print(f"\n{bar}")
    print(f"[{index}/{total}] {action.decision.value}")
    print(bar)
    print(f"detector    : {finding.detector}")
    print(f"fingerprint : {finding.fingerprint.digest}")
    print(f"              {finding.fingerprint.readable()}")
    print(f"incidence   : {finding.rate_line()}")
    if action.issue is not None:
        print(
            f"issue       : #{action.issue} [{action.issue_state or 'state unknown'}] "
            f"{action.issue_title}"
        )
        print(f"matched by  : {action.matched_by}")
    if action.candidates:
        print("candidates  : (search hits — NOT auto-bound; confirm before trusting)")
        for candidate in action.candidates[:8]:
            print(
                f"              #{candidate.get('number')} "
                f"[{str(candidate.get('state') or '?').upper()}] {candidate.get('title')}"
            )
    if action.novelty:
        print("adds        : " + "; ".join(action.novelty))
    if action.decision is Decision.NEW:
        print(f"title       : {action.title}")
    if action.decision is Decision.NO_NEWS:
        print(
            "\nNothing would be posted: this occurrence adds nothing the record does not\n"
            "already hold, and a comment restating the issue is noise on it. The run and\n"
            "task list are still recorded in the ledger under --apply, so the next run's\n"
            "novelty check is measured against the truth. Force it with --comment-anyway."
        )
        print("-" * 78)
        return
    print(f"\n--- body it would post {'-' * 55}")
    print(action.body)
    print("-" * 78)


def _summarise(actions: list[Action], apply: bool) -> None:
    counts: dict[str, int] = {}
    for action in actions:
        counts[action.decision.value] = counts.get(action.decision.value, 0) + 1
    print("\n" + "=" * 78)
    print("SUMMARY")
    print("=" * 78)
    for decision in Decision:
        if counts.get(decision.value):
            print(f"  {counts[decision.value]:3d}  {decision.value}")
    if not counts:
        print("  no findings — nothing to file or comment on")
    suppressed = [a for a in actions if a.decision is Decision.SUPPRESSED]
    if suppressed:
        # Two reasons land here and they mean different things: the cap is a
        # budget a human can raise, a failed search is a de-duplication the tool
        # could not perform. Printing both under one heading would invite
        # "just raise the cap" at exactly the moment that is the wrong move.
        print("\nSuppressed — not filed, and listed here rather than silently dropped:")
        for action in suppressed:
            reason = action.matched_by or "the new-issue cap (raise --max-new-issues to file it)"
            print(
                f"  {action.fingerprint.digest}  {action.finding.detector}  "
                f"{action.finding.count} occurrence(s)  — {action.finding.title}"
            )
            print(f"      reason: {reason}")
    if not apply:
        print(
            "\nDRY RUN — nothing was written to GitHub and the fingerprint ledger was "
            "not updated.\nRe-run with --apply to post exactly the bodies printed above."
        )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="triage-bench-traces",
        description="Triage a bench run's traces into GitHub issue activity.",
    )
    parser.add_argument("--run", required=True, help="run id, e.g. w10p")
    parser.add_argument("--mirror", type=Path, help="local mirror of runs/<run_id>/")
    parser.add_argument(
        "--fetch",
        action="store_true",
        help=f"populate the mirror with aws s3 sync (default {DEFAULT_MIRROR})",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="actually create issues and post comments (default: dry run)",
    )
    parser.add_argument(
        "--max-new-issues",
        type=int,
        default=5,
        help="cap on issues one invocation may open; the rest are printed as SUPPRESSED",
    )
    parser.add_argument("--repo", default=REPO)
    parser.add_argument(
        "--detector",
        action="append",
        default=[],
        help="restrict to these detector codes (repeatable)",
    )
    parser.add_argument(
        "--offline",
        action="store_true",
        help="skip every GitHub read; ledger-only de-duplication. Implies a dry run.",
    )
    parser.add_argument(
        "--comment-anyway",
        action="store_true",
        help=(
            "post a comment even when the occurrence adds nothing the record did not "
            "already hold (default: record it and stay quiet)"
        ),
    )
    parser.add_argument("--ledger", type=Path, default=LEDGER_PATH)
    parser.add_argument("--list-detectors", action="store_true", help="print the registry and exit")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    if args.list_detectors:
        for det in DETECTORS:
            print(f"{det.code:32s} site={det.site or '-'}")
            print(f"{'':32s} {det.title}")
        return 0

    if args.offline and args.apply:
        raise SystemExit("--offline cannot write: it never reads GitHub, so it cannot de-duplicate")

    if args.fetch:
        root = fetch_run(args.run, args.mirror or DEFAULT_MIRROR)
    elif args.mirror:
        root = args.mirror
    else:
        raise SystemExit("give --mirror <path> or --fetch")
    if not root.is_dir():
        raise SystemExit(f"no such mirror directory: {root}")

    run: Run = load_run(root, args.run)
    if not run.trials:
        raise SystemExit(f"no trials found under {root} — is this a mirror of runs/{args.run}/ ?")

    unreadable = sum(t.malformed_lines for t in run.trials)
    print(f"run        : {run.run_id}  ({run.match_name or 'unnamed match'})")
    print(f"mirror     : {root}")
    print(f"SUT        : {run.sut_ref or 'not recorded'}")
    print(
        "arm        : "
        + (
            "bare turn loop — the pipeline detectors are silent, because a bare arm "
            "has no verify stage and its absences are the design"
            if run.bare_loop
            else "staged pipeline"
        )
    )
    print(f"trials     : {len(run.trials)} ({len(run.graded_trials)} with a reward)")
    if unreadable:
        print(
            f"unreadable : {unreadable} malformed JSONL line(s) skipped — a run still "
            "streaming has a half-written tail; absence of an event below may be that."
        )

    findings: list[Finding] = run_all(run)
    if args.detector:
        findings = [f for f in findings if f.detector in set(args.detector)]

    ledger = Ledger(args.ledger)
    client = None if args.offline else GhCli(args.repo)
    actions = plan_actions(
        run,
        findings,
        ledger,
        client,
        args.max_new_issues,
        comment_anyway=args.comment_anyway,
    )

    for index, action in enumerate(actions, start=1):
        _print_action(action, index, len(actions))

    if args.apply:
        assert client is not None
        for line in apply_actions(run, actions, ledger, client):
            print("applied:", line)

    _summarise(actions, args.apply)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
