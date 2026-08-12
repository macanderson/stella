"""Identify a defect by what it *is*, so the same one is never filed twice.

The hard requirement on `make triage-bench-traces` is that its output is never
a duplicate issue. Title similarity cannot deliver that: the repository's own
backlog holds #2929 and #2872 describing one defect — an oracle that fails on
both trees and routes to `Revise` — in almost no shared words, found on
different runs and different SUTs. Anything keyed on prose would have filed
both, and did.

So a defect is identified by a **fingerprint**: a triple, hashed, of the three
things that are properties of the defect rather than of the run that found it.

    fingerprint = sha256("<detector>\\n<site>\\n<variant>")[:12]

**detector** — the event signature that detects it. Not a description of the
bug: the name of the predicate over `stella-events.jsonl` that fires. Two
findings that the same predicate produced are the same *kind* of defect by
construction.

**site** — the code site the detector implicates, as a repo-relative path
declared on the detector, never derived from the run. A detector that
implicates nothing declares `""`; that is honest, and it simply means the
variant carries the whole discrimination.

**variant** — a normalized discriminator taken from the occurrence, so one
detector can still separate genuinely different defects. This is the load
bearing part, and normalization is what makes it stable: every token that
varies between two runs of the same defect is erased before hashing —
quoted spans, absolute paths, the `/tmp/stella_candidate_<n>_<n>` workspace
root, digits, and whitespace — and only the leading skeleton of the string
survives.

Nothing in any of the three components is a run id, a trial id, a task name, a
timestamp, a cost, a model name, or a line number, which is why next week's run
hashes a recurrence to the same value as this week's, and why two different
reasons under one detector do not collide. `witness author produced no
TEST_COMMAND line` and `witness author emitted a test command but created no
file` are two defects and get two fingerprints; the same reason seen in twelve
trials across two runs is one defect and gets one.

The mapping fingerprint -> issue is **persisted** (`fingerprints.json`) rather
than re-derived, for two reasons. It improves: a human who corrects one binding
corrects it for every future run. And it survives search going wrong — a GitHub
search is a fuzzy instrument, and the day it fails to surface an existing issue
is the day the tool would have opened a duplicate.

The ledger is not the only recovery path. Every issue this tool opens carries
an HTML-comment marker in its body:

    <!-- stella-trace-fingerprint: fp_1a2b3c4d5e6f -->

so a lost or reset ledger can be rebuilt from GitHub itself by searching for
the marker. The marker is invisible in rendered Markdown and is not prose, so
it cannot rot the way a title can.
"""

from __future__ import annotations

import hashlib
import json
import re
from collections.abc import Iterable
from dataclasses import dataclass
from dataclasses import field as dc_field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

LEDGER_PATH = Path(__file__).resolve().parent / "fingerprints.json"

MARKER_PREFIX = "<!-- stella-trace-fingerprint: "
MARKER_SUFFIX = " -->"

# How many normalized tokens of the variant survive into the hash. Long enough
# that two distinct reasons diverge (the repository's witness reasons differ by
# their fourth token), short enough that a reason whose tail gains a clause in a
# later release still hashes to the same defect.
_VARIANT_TOKENS = 14

_QUOTED = re.compile(r"[`'\"]{1}[^`'\"]{1,200}[`'\"]{1}")
_CANDIDATE_ROOT = re.compile(r"/tmp/stella_candidate_[0-9]+_[0-9]+[^\s,)`'\"]*")
_ABS_PATH = re.compile(r"(?<![\w/])/[\w./-]{2,}")
_NUMBER = re.compile(r"\b\d[\d_,.]*\b")
_WS = re.compile(r"\s+")


def normalize(text: str) -> str:
    """Reduce an emitted string to the part that is a property of the defect.

    Order matters. Quoted spans go first because they are where run-specific
    nouns hide (`/app/documents/`, `create_witness_test`), and the candidate
    root goes before the general absolute-path rule because its trailing digits
    would otherwise survive as `<path>` plus a stray number.
    """
    text = _QUOTED.sub(" <q> ", text)
    text = _CANDIDATE_ROOT.sub(" <ws> ", text)
    text = _ABS_PATH.sub(" <path> ", text)
    text = _NUMBER.sub(" <n> ", text)
    text = _WS.sub(" ", text).strip().lower()
    return " ".join(text.split(" ")[:_VARIANT_TOKENS])


@dataclass(frozen=True)
class Fingerprint:
    """A defect's identity, and the three readable parts it was built from."""

    detector: str
    site: str
    variant: str

    @property
    def digest(self) -> str:
        raw = f"{self.detector}\n{self.site}\n{self.variant}"
        return "fp_" + hashlib.sha256(raw.encode("utf-8")).hexdigest()[:12]

    @property
    def marker(self) -> str:
        return f"{MARKER_PREFIX}{self.digest}{MARKER_SUFFIX}"

    def readable(self) -> str:
        return f"{self.detector} | {self.site or '(no site)'} | {self.variant or '(no variant)'}"


def fingerprint_of(detector: str, site: str, variant_source: str) -> Fingerprint:
    return Fingerprint(detector=detector, site=site, variant=normalize(variant_source))


def markers_in(text: str) -> list[str]:
    """Every fingerprint digest stamped into an issue body."""
    return re.findall(r"<!--\s*stella-trace-fingerprint:\s*(fp_[0-9a-f]{12})\s*-->", text)


@dataclass
class LedgerEntry:
    """One defect's binding, plus the incidence already on the record.

    `runs`, `tasks` and `peak_*` are not bookkeeping for its own sake: they are
    what lets a comment say *what this occurrence adds* instead of restating the
    issue. A recurrence on a task the issue never named, or at a higher rate
    than it ever recorded, is news; the same trial in the same run is noise, and
    the tool has to be able to tell them apart without a human remembering.
    """

    digest: str
    issue: int | None
    detector: str
    site: str
    variant: str
    first_seen_run: str
    note: str = ""
    source: str = "auto"
    runs: list[str] = dc_field(default_factory=list)
    tasks: list[str] = dc_field(default_factory=list)
    peak_count: int = 0
    peak_denominator: int = 0

    def novelty(self, run_id: str, tasks: Iterable[str], count: int, denominator: int) -> list[str]:
        """What this occurrence adds that the record did not already have."""
        news: list[str] = []
        if run_id not in self.runs:
            news.append(f"a new run (`{run_id}`) — the record held {self.runs or 'no runs'}")
        fresh = sorted(set(tasks) - set(self.tasks))
        if fresh:
            news.append("a new affected task: " + ", ".join(f"`{t}`" for t in fresh))
        if denominator > 0 and self.peak_denominator > 0:
            was = self.peak_count / self.peak_denominator
            now = count / denominator
            if now > was:
                news.append(
                    f"a higher incidence: {count}/{denominator} here against "
                    f"{self.peak_count}/{self.peak_denominator} on the record"
                )
        elif self.peak_denominator == 0:
            news.append(f"the first recorded incidence for this fingerprint: {count}/{denominator}")
        return news

    def observe(self, run_id: str, tasks: Iterable[str], count: int, denominator: int) -> None:
        if run_id and run_id not in self.runs:
            self.runs.append(run_id)
        for task in sorted(set(tasks) - set(self.tasks)):
            self.tasks.append(task)
        if denominator > 0 and (
            self.peak_denominator == 0
            or count / denominator > self.peak_count / self.peak_denominator
        ):
            self.peak_count, self.peak_denominator = count, denominator

    def to_json(self) -> dict[str, Any]:
        return {
            "issue": self.issue,
            "detector": self.detector,
            "site": self.site,
            "variant": self.variant,
            "first_seen_run": self.first_seen_run,
            "note": self.note,
            "source": self.source,
            "runs": self.runs,
            "tasks": self.tasks,
            "peak_count": self.peak_count,
            "peak_denominator": self.peak_denominator,
        }


class Ledger:
    """The persisted fingerprint -> issue mapping, tracked in git.

    Two properties make it safe to let a program write a file a human also
    edits. It **never overwrites a human binding**: an entry whose `source` is
    anything but `auto` keeps its issue number, so a correction survives every
    later run. And it is written sorted and pretty-printed, so a change to it
    reads as a reviewable diff rather than a churned blob.
    """

    def __init__(self, path: Path = LEDGER_PATH) -> None:
        self.path = path
        self.entries: dict[str, LedgerEntry] = {}
        self._load()

    def _load(self) -> None:
        if not self.path.exists():
            return
        try:
            raw = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return
        for digest, value in (raw.get("fingerprints") or {}).items():
            if not isinstance(value, dict):
                continue
            issue = value.get("issue")
            self.entries[digest] = LedgerEntry(
                digest=digest,
                issue=int(issue) if isinstance(issue, int) else None,
                detector=str(value.get("detector") or ""),
                site=str(value.get("site") or ""),
                variant=str(value.get("variant") or ""),
                first_seen_run=str(value.get("first_seen_run") or ""),
                note=str(value.get("note") or ""),
                source=str(value.get("source") or "auto"),
                runs=[str(r) for r in (value.get("runs") or [])],
                tasks=[str(t) for t in (value.get("tasks") or [])],
                peak_count=int(value.get("peak_count") or 0),
                peak_denominator=int(value.get("peak_denominator") or 0),
            )

    def lookup(self, fingerprint: Fingerprint) -> LedgerEntry | None:
        return self.entries.get(fingerprint.digest)

    def bind(
        self,
        fingerprint: Fingerprint,
        issue: int | None,
        run_id: str,
        note: str = "",
        source: str = "auto",
    ) -> LedgerEntry:
        """Record (or complete) a binding, never clobbering a human's."""
        existing = self.entries.get(fingerprint.digest)
        if existing is not None:
            if existing.source != "auto" and existing.issue is not None:
                return existing
            if issue is not None:
                existing.issue = issue
            if note:
                existing.note = note
            return existing
        entry = LedgerEntry(
            digest=fingerprint.digest,
            issue=issue,
            detector=fingerprint.detector,
            site=fingerprint.site,
            variant=fingerprint.variant,
            first_seen_run=run_id,
            note=note,
            source=source,
        )
        self.entries[entry.digest] = entry
        return entry

    def save(self) -> None:
        payload = {
            "schema": 1,
            "updated_at": datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "fingerprints": {
                digest: self.entries[digest].to_json() for digest in sorted(self.entries)
            },
        }
        self.path.write_text(
            json.dumps(payload, indent=2, sort_keys=False) + "\n", encoding="utf-8"
        )
