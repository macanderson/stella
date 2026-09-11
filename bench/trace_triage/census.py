"""The share of a run's wall clock that went to tool execution, and what sat inside it.

`#3753` measured one match and reported tool execution at 75% of agent wall
clock. Its definition of done asks for that number again on a comparable panel,
"on the same clean-basis method". That method was an uncommitted `jq`
incantation, so the next run would have produced a number nobody could compare
to the last one. Here it is written down.

Wall clock is the first event to the last, per trial. Tool time is
`tool_result.duration_ms` summed over the join `run_trace` already performs. A
share is a ratio of two sums over one named trial set, never a mean of ratios.

**The basis travels with the number or the number says nothing.** `#3753`'s body
reports 75% over a clean basis of nine tasks; a later comment on the same match
reports 52% over the twelve trials that carried events. Neither corrects the
other. They are two populations, and a reader handed one figure alone cannot
tell which. `Census.trials` and `Census.untimed_trials` are therefore on every
result this module returns.

**A trace written before the event schema carried `ts` has no wall clock.** Its
tool durations are still readable. Counting it as a zero would deflate every
share it entered, so it lands in `untimed_trials` and leaves the ratio alone.

**A trial that recorded nothing is a third thing again**, and `empty_trials`
keeps it apart from the old schema. A run whose credential never authenticated
writes empty traces, and folding those into "no timestamp" would report a
broken run as an old one.

Two costs inside the tool time can be named from the trace with no rig and no
spend. `bash` answers `command timed out after Ns` when its backstop fires, so a
killed call is measured time against work that is gone. When a later call in the
same trial repeats a killed one, that first attempt bought nothing at all —
which is the shape `pytorch-model-cli` died in, with an install killed at 120s,
the same install killed again at 300s, and a third attempt that ran.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from difflib import SequenceMatcher
from pathlib import Path
from typing import Iterable

from run_trace import Trial, load_run, load_trial

__all__ = [
    "Census",
    "TrialCensus",
    "census_of",
    "main",
    "render",
    "summarize",
]

#: `bash` opens its timeout refusal with this, and no other tool answers it.
TIMEOUT_MARKER = "command timed out after"

#: How alike two commands must read before the later one counts as a repeat of
#: the earlier. Tuned on the pair `pytorch-model-cli` actually sent —
#: `timeout 300 pip install torch --quiet` against `pip install torch --quiet`,
#: which score 0.62 — and reported beside `reattempted_exact_ms`, the floor that
#: needs no threshold at all.
REPEAT_SIMILARITY = 0.6

_WHITESPACE = re.compile(r"\s+")


def _normalized(command: str) -> str:
    return _WHITESPACE.sub(" ", command).strip()


@dataclass(frozen=True)
class TrialCensus:
    """One trial's wall clock and the tool time inside it."""

    task_id: str
    events: int
    wall_ms: int | None
    tool_ms: int
    bash_ms: int
    model_ms: int
    timed_out_ms: int
    reattempted_ms: int
    reattempted_exact_ms: int
    tool_calls: int


@dataclass(frozen=True)
class Census:
    """A trial set's totals, and the shares they support."""

    trials: int
    untimed_trials: int
    empty_trials: int
    wall_ms: int
    tool_ms: int
    bash_ms: int
    model_ms: int
    timed_out_ms: int
    reattempted_ms: int
    reattempted_exact_ms: int
    tool_calls: int

    @property
    def timed_trials(self) -> int:
        """Trials that carried a timestamp, which is what a share is over."""
        return self.trials - self.untimed_trials - self.empty_trials

    def _share_of_wall(self, part_ms: int) -> float | None:
        return None if self.wall_ms <= 0 else 100.0 * part_ms / self.wall_ms

    def _share_of_bash(self, part_ms: int) -> float | None:
        return None if self.bash_ms <= 0 else 100.0 * part_ms / self.bash_ms

    @property
    def tool_share(self) -> float | None:
        """The figure `#3753` asks to move, as a percentage, or `None` when no
        trial in the set carried a wall clock."""
        return self._share_of_wall(self.tool_ms)

    @property
    def model_share(self) -> float | None:
        return self._share_of_wall(self.model_ms)

    @property
    def timed_out_share(self) -> float | None:
        return self._share_of_bash(self.timed_out_ms)

    @property
    def reattempted_share(self) -> float | None:
        return self._share_of_bash(self.reattempted_ms)


def _wall_ms(trial: Trial) -> int | None:
    stamps = [e["ts"] for e in trial.events if isinstance(e.get("ts"), int)]
    return max(stamps) - min(stamps) if stamps else None


def _model_ms(trial: Trial) -> int:
    return sum(int(e.get("duration_ms") or 0) for e in trial.of_type("step_usage"))


def _result_text(call) -> str:
    return "" if call.result is None else str(call.result.get("output", ""))


def census_of(trial: Trial, *, similarity: float = REPEAT_SIMILARITY) -> TrialCensus:
    """Read one trial's clock, without reaching for anything but its events."""
    tool_ms = bash_ms = timed_out_ms = reattempted_ms = exact_ms = 0
    bash_commands: list[str] = []
    killed: list[tuple[int, int, str]] = []

    for call in trial.tool_calls:
        duration = int((call.result or {}).get("duration_ms") or 0)
        tool_ms += duration
        if call.name != "bash":
            continue
        command = call.input.get("command")
        if not isinstance(command, str):
            continue
        bash_ms += duration
        bash_commands.append(_normalized(command))
        if TIMEOUT_MARKER in _result_text(call):
            timed_out_ms += duration
            killed.append((len(bash_commands) - 1, duration, _normalized(command)))

    for index, duration, command in killed:
        later = bash_commands[index + 1 :]
        if command in later:
            exact_ms += duration
        if any(
            SequenceMatcher(None, command, other).ratio() >= similarity
            for other in later
        ):
            reattempted_ms += duration

    return TrialCensus(
        task_id=trial.task_id,
        events=len(trial.events),
        wall_ms=_wall_ms(trial),
        tool_ms=tool_ms,
        bash_ms=bash_ms,
        model_ms=_model_ms(trial),
        timed_out_ms=timed_out_ms,
        reattempted_ms=reattempted_ms,
        reattempted_exact_ms=exact_ms,
        tool_calls=len(trial.tool_calls),
    )


def summarize(censuses: Iterable[TrialCensus]) -> Census:
    """Total a trial set, holding every share to the trials that carry a clock.

    A trial with no wall clock keeps its tool time out of the numerator as well
    as the denominator, so the share is a statement about one population rather
    than a ratio between two.
    """
    rows = list(censuses)
    timed = [row for row in rows if row.wall_ms is not None]
    empty = [row for row in rows if not row.events]
    return Census(
        trials=len(rows),
        untimed_trials=len(rows) - len(timed) - len(empty),
        empty_trials=len(empty),
        wall_ms=sum(row.wall_ms or 0 for row in timed),
        tool_ms=sum(row.tool_ms for row in timed),
        bash_ms=sum(row.bash_ms for row in rows),
        model_ms=sum(row.model_ms for row in timed),
        timed_out_ms=sum(row.timed_out_ms for row in rows),
        reattempted_ms=sum(row.reattempted_ms for row in rows),
        reattempted_exact_ms=sum(row.reattempted_exact_ms for row in rows),
        tool_calls=sum(row.tool_calls for row in rows),
    )


def _percent(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.1f}%"


def _hours(milliseconds: int) -> str:
    return f"{milliseconds / 3_600_000:.2f}h"


def render(census: Census, *, label: str) -> str:
    """The census as text, with its basis on the first line."""
    basis = f"{census.timed_trials} trials with a wall clock"
    if census.untimed_trials:
        basis += f", {census.untimed_trials} on a schema without one"
    if census.empty_trials:
        basis += f", {census.empty_trials} that recorded nothing"
    lines = [
        f"{label}: {basis}",
        f"  wall clock       {_hours(census.wall_ms)}",
        f"  tool execution   {_hours(census.tool_ms)}  {_percent(census.tool_share)} of wall clock",
        f"  model            {_hours(census.model_ms)}  {_percent(census.model_share)} of wall clock",
        f"  bash             {_hours(census.bash_ms)}  over {census.tool_calls} tool calls",
        f"    killed by the timeout      {_hours(census.timed_out_ms)}  "
        f"{_percent(census.timed_out_share)} of bash",
        f"    killed and tried again     {_hours(census.reattempted_ms)}  "
        f"{_percent(census.reattempted_share)} of bash",
        f"    ...of which run verbatim   {_hours(census.reattempted_exact_ms)}",
    ]
    return "\n".join(lines)


def _load(root: Path) -> list[Trial]:
    """Every trial under `root`, in either layout the mirror is written in.

    The cloud runner writes `runs/<run_id>/<trial>/ws/matches/<match>/jobs/...`
    and `load_run` reads that. A match pulled to `~/.arenabench/matches/` starts
    at the `jobs/` level instead, whether `root` is one match or the directory
    holding many, so both depths are walked before the answer is empty.

    A match holds every contestant's seat, and the other one does not write
    `stella-events.jsonl`. Its directories are skipped rather than counted as
    trials that recorded nothing, which would report an opponent as a failure.
    """
    trials = list(load_run(root).trials)
    if trials:
        return trials
    for pattern in ("jobs/*/*", "*/jobs/*/*"):
        for task_dir in sorted(root.glob(pattern)):
            if (task_dir / "agent" / "stella-events.jsonl").is_file():
                trials.append(
                    load_trial(root.name, root, task_dir.parent.name, task_dir)
                )
        if trials:
            break
    return trials


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "root", type=Path, help="a mirrored run, or a directory of matches"
    )
    parser.add_argument(
        "--label", default="", help="what the trial set is, for the first line"
    )
    args = parser.parse_args(argv)

    trials = _load(args.root.resolve())
    if not trials:
        print(f"no trials under {args.root}", file=sys.stderr)
        return 1
    print(
        render(
            summarize(census_of(trial) for trial in trials),
            label=args.label or args.root.name,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
