"""Turn findings into GitHub issue activity — and never into a duplicate.

Two halves, kept apart so the decision is testable without a network:

* `GitHubClient` — the only thing here that talks to `gh`. Tests inject a fake.
* `plan_actions` — the de-duplication decision, a pure function of the findings,
  the ledger and whatever the client returned.

## How a duplicate is prevented

Three gates, tried in order, each one a fallback for the previous one failing:

1. **The ledger** (`fingerprints.json`). A fingerprint already bound to an issue
   is that issue, full stop. No search, no similarity, no judgement.
2. **The fingerprint marker.** Every issue this tool opens carries
   `<!-- stella-trace-fingerprint: fp_… -->` in its body, so a search that
   surfaces an issue carrying this finding's digest is an exact hit even when
   the ledger has been lost or reset — and the ledger is repaired from it.
3. **A search across open AND closed**, whose hits are reported as *candidates*
   for a human to confirm. A candidate is never auto-bound: prose similarity is
   the instrument that filed #2929 and #2872 for one defect, and it does not get
   a vote here.

A finding that survives all three is new, and only then may an issue be opened.

## A closed issue that recurs

It is a comment on that issue, never a new ticket. The comment is headed
`RECURRENCE` and says so explicitly, because "this closed issue's defect is back"
is more valuable than either a silent duplicate or a silent omission.

What the comment does **not** do is assert a regression. Whether a recurrence is
a regression depends on whether the run's SUT already contained the fix, and the
trace cannot answer that: a run recorded before the fix merged shows the same
bytes as a run after it. So the comment states the SUT ref the run carried and
names the command that settles it, and leaves the call to a human. Reopening is
likewise a human action — the tool prints the `gh issue reopen` line rather than
running it.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
from collections.abc import Sequence
from dataclasses import dataclass, field
from enum import StrEnum
from typing import Any, Protocol

from detectors import Finding
from fingerprint import Fingerprint, Ledger, LedgerEntry, markers_in
from run_trace import Run

REPO = "macanderson/stella"


class Decision(StrEnum):
    NEW = "NEW ISSUE"
    COMMENT = "COMMENT (open issue)"
    RECURRENCE = "COMMENT (closed issue — recurrence)"
    SUPPRESSED = "SUPPRESSED (new-issue cap reached)"
    NO_NEWS = "NOTHING POSTED (occurrence adds nothing to the record)"


@dataclass
class Action:
    finding: Finding
    fingerprint: Fingerprint
    decision: Decision
    title: str
    body: str
    issue: int | None = None
    issue_state: str = ""
    issue_title: str = ""
    candidates: list[dict[str, Any]] = field(default_factory=list)
    novelty: list[str] = field(default_factory=list)
    matched_by: str = ""


# --------------------------------------------------------------------------
# The GitHub side
# --------------------------------------------------------------------------


class GitHubClient(Protocol):
    def search_issues(self, terms: Sequence[str]) -> list[dict[str, Any]]: ...

    def view_issue(self, number: int) -> dict[str, Any]: ...

    def create_issue(self, title: str, body: str) -> int: ...

    def comment_issue(self, number: int, body: str) -> None: ...


_ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")


def _plain_env() -> dict[str, str]:
    """An environment in which `gh --json` emits JSON and nothing else.

    `CLICOLOR_FORCE` is set in some developer shells, and `gh` honours it even
    when its stdout is a pipe — so `--json` comes back wrapped in ANSI colour
    codes and `json.loads` raises. Clearing it (and setting `NO_COLOR`) is the
    fix; the `_ANSI` strip below is the belt to that braces, because the failure
    mode is a crash in the middle of a run rather than a wrong answer, and it
    cost one debugging round already.
    """
    env = dict(os.environ)
    env.pop("CLICOLOR_FORCE", None)
    env.pop("GH_FORCE_TTY", None)
    env["NO_COLOR"] = "1"
    return env


class GhCli:
    """`gh` over `subprocess`. The only component in this package that writes."""

    def __init__(self, repo: str = REPO) -> None:
        self.repo = repo

    def _json(self, args: list[str]) -> Any:
        result = subprocess.run(args, capture_output=True, text=True, check=False, env=_plain_env())
        if result.returncode != 0:
            raise RuntimeError(f"{' '.join(args)} failed: {result.stderr.strip()}")
        try:
            return json.loads(_ANSI.sub("", result.stdout) or "[]")
        except ValueError as exc:
            raise RuntimeError(f"{' '.join(args)} returned unparseable JSON") from exc

    def search_issues(self, terms: Sequence[str]) -> list[dict[str, Any]]:
        """Search open AND closed.

        `--state` is deliberately absent: `gh search issues` accepts only
        `open` or `closed` there, and *omitting* it is what searches both.
        Passing `--state open` — the reading a reviewer expects to be the safe
        one — would hide every closed issue and turn each closed defect that
        recurs into a duplicate ticket, which is the one outcome this tool
        exists to prevent.
        """
        hits: dict[int, dict[str, Any]] = {}
        for term in terms:
            rows = self._json(
                [
                    "gh",
                    "search",
                    "issues",
                    "--repo",
                    self.repo,
                    "--limit",
                    "20",
                    "--json",
                    "number,title,state,url",
                    term,
                ]
            )
            for row in rows or []:
                if isinstance(row, dict) and isinstance(row.get("number"), int):
                    hits[row["number"]] = row
        return [hits[n] for n in sorted(hits)]

    def view_issue(self, number: int) -> dict[str, Any]:
        return self._json(
            [
                "gh",
                "issue",
                "view",
                str(number),
                "--repo",
                self.repo,
                "--json",
                "number,title,state,body,url",
            ]
        )

    def create_issue(self, title: str, body: str) -> int:
        result = subprocess.run(
            ["gh", "issue", "create", "--repo", self.repo, "--title", title, "--body", body],
            capture_output=True,
            text=True,
            check=False,
            env=_plain_env(),
        )
        if result.returncode != 0:
            raise RuntimeError(f"gh issue create failed: {result.stderr.strip()}")
        url = _ANSI.sub("", result.stdout).strip().splitlines()[-1]
        return int(url.rsplit("/", 1)[-1])

    def comment_issue(self, number: int, body: str) -> None:
        result = subprocess.run(
            ["gh", "issue", "comment", str(number), "--repo", self.repo, "--body", body],
            capture_output=True,
            text=True,
            check=False,
            env=_plain_env(),
        )
        if result.returncode != 0:
            raise RuntimeError(f"gh issue comment failed: {result.stderr.strip()}")


# --------------------------------------------------------------------------
# Rendering
# --------------------------------------------------------------------------


def _evidence_block(run: Run, finding: Finding, limit: int) -> str:
    lines = [
        "### Evidence",
        "",
        f"- run: `{run.run_id}`  ({run.match_name or 'unnamed match'})",
        f"- SUT: `{run.sut_ref or 'not recorded in the artifacts'}`",
        f"- artifacts: `{run.s3_prefix()}`",
        f"- incidence: {finding.rate_line()}",
    ]
    if finding.detail:
        lines.append(f"- {finding.detail}")
    lines += ["", "#### Occurrences", ""]
    for occurrence in finding.occurrences[:limit]:
        lines.append(occurrence.render())
        lines.append("")
    if finding.count > limit:
        lines.append(
            f"_{finding.count - limit} further occurrence(s) not reproduced here; the "
            f"detector `{finding.detector}` reproduces the full list from the run._"
        )
        lines.append("")
    return "\n".join(lines)


def render_new_issue(run: Run, finding: Finding, limit: int = 6) -> str:
    """A handoff a fresh agent could execute with none of this session's context."""
    tasks = sorted({o.task_name for o in finding.occurrences})
    parts = [
        "## Problem",
        "",
        finding.summary,
        "",
        _evidence_block(run, finding, limit),
        "## Where",
        "",
        (
            f"- implicated site: `{finding.site}`"
            if finding.site
            else "- no single code site is implicated by the detector; the evidence above "
            "is the starting point."
        ),
        f"- detector: `bench/trace_triage/detectors.py` -> `{finding.detector}`",
        "",
        "## Reproduce / verify",
        "",
        "```sh",
        f"aws s3 sync {run.s3_prefix()} /tmp/{run.run_id}/ \\",
        "  --exclude '*' --include '*stella-events.jsonl' --include '*reward.txt' \\",
        "  --include '*exception.txt' --include '*result*.json'",
        f"make triage-bench-traces RUN={run.run_id} MIRROR=/tmp/{run.run_id}",
        "```",
        "",
        f"The dry run prints this finding with every occurrence. Affected tasks: "
        f"{', '.join(f'`{t}`' for t in tasks)}.",
        "",
        "## Constraints already discovered",
        "",
        "- `proof` events discriminate on `step.kind`, not the top-level `type`.",
        "- `witness_author` has `output_text: null` — its product is in "
        "`tool_start.call.input.content`, not in `step_usage`.",
        "- `file_change` is observability, never evidence: `bash` names no path, so a "
        "heredoc, `patch`, `make` or `>` mutates the tree silently.",
    ]
    for caveat in finding.caveats:
        parts.append(f"- {caveat}")
    parts += [
        "",
        "## Definition of done",
        "",
        "- A witness test that fails on the current code and passes with the fix.",
        f"- Re-running `make triage-bench-traces` over a run recorded on the fixed SUT "
        f"produces no `{finding.detector}` finding with this fingerprint.",
        "",
        "---",
        "",
        f"Filed by `make triage-bench-traces` from run `{run.run_id}`. "
        "The marker below is how the tool recognises this defect next time — "
        "please leave it in place.",
        "",
        finding.fingerprint.marker,
    ]
    return "\n".join(parts)


def render_comment(
    run: Run,
    finding: Finding,
    entry: LedgerEntry | None,
    recurrence: bool,
    limit: int = 6,
) -> str:
    """A comment that says what is NEW, not what the issue already says."""
    tasks = [o.task_name for o in finding.occurrences]
    news = (
        entry.novelty(run.run_id, tasks, finding.count, finding.denominator)
        if entry
        else [f"a fresh occurrence in run `{run.run_id}`: {finding.count}/{finding.denominator}"]
    )
    if recurrence:
        # On a closed issue the recurrence is itself the news, and always the
        # headline item — otherwise the section reads "adds nothing" directly
        # under a heading announcing that a closed defect came back.
        news.insert(
            0,
            f"the signature fired again on a CLOSED issue, in run `{run.run_id}` "
            f"(SUT `{run.sut_ref or 'unrecorded'}`)",
        )
    header = (
        "## RECURRENCE — this closed issue's defect signature fired again"
        if recurrence
        else "## Fresh evidence from a bench run"
    )
    parts = [header, "", "**What this occurrence adds:**", ""]
    parts += [f"- {item}" for item in (news or ["nothing the record did not already hold"])]
    if not news:
        parts.append(
            "  (kept anyway so the incidence stays auditable — read it as a repeat "
            "observation, not as news.)"
        )
    parts += ["", _evidence_block(run, finding, limit)]
    if recurrence:
        parts += [
            "### Is this a regression?",
            "",
            "**Not decided here, and deliberately not guessed.** This run carries SUT "
            f"`{run.sut_ref or 'unrecorded'}`. If that commit already contains the fix "
            "that closed this issue, the signature firing on it is a regression; if the "
            "run predates the fix, it is additional evidence from before it. The trace "
            "shows the same bytes either way, so it cannot distinguish the two.",
            "",
            "```sh",
            f"git merge-base --is-ancestor <the-fixing-commit> {run.sut_ref or '<sut-ref>'} \\",
            '  && echo "SUT contains the fix — regression" || echo "SUT predates the fix"',
            "```",
            "",
            "Reopening is a human call; this tool does not reopen. If it is a regression:",
            "",
            "```sh",
            "gh issue reopen <this issue> --repo macanderson/stella",
            "```",
            "",
        ]
    for caveat in finding.caveats:
        parts.append(f"> {caveat}")
        parts.append("")
    parts += [
        "---",
        "",
        f"Posted by `make triage-bench-traces` from run `{run.run_id}`.",
        "",
        finding.fingerprint.marker,
    ]
    return "\n".join(parts)


# --------------------------------------------------------------------------
# The decision
# --------------------------------------------------------------------------


def plan_actions(
    run: Run,
    findings: Sequence[Finding],
    ledger: Ledger,
    client: GitHubClient | None,
    max_new_issues: int,
    excerpt_limit: int = 6,
    comment_anyway: bool = False,
) -> list[Action]:
    """Decide, per finding, between commenting and opening — never duplicating.

    Pure with respect to GitHub state changes: it reads (search, view) and
    decides. Nothing is written until `apply_actions`.
    """
    actions: list[Action] = []
    opened = 0
    for finding in sorted(findings, key=lambda f: (f.detector, f.variant_source)):
        fingerprint = finding.fingerprint
        entry = ledger.lookup(fingerprint)
        issue: int | None = entry.issue if entry else None
        matched_by = "ledger" if issue is not None else ""
        candidates: list[dict[str, Any]] = []
        search_failed = False
        search_error = ""

        if issue is None and client is not None:
            try:
                candidates = client.search_issues(finding.search_terms)
            except RuntimeError as exc:
                # Fail CLOSED. A search that did not run is not a search that
                # found nothing, and treating it as one is precisely how a
                # duplicate gets opened — the ledger is the only other gate, and
                # a fingerprint it has never seen is exactly the case where the
                # search was load-bearing.
                search_failed = True
                search_error = str(exc)
            for candidate in candidates:
                body = candidate.get("body")
                if body is None:
                    try:
                        body = client.view_issue(int(candidate["number"])).get("body") or ""
                    except (RuntimeError, KeyError, ValueError):
                        body = ""
                if fingerprint.digest in markers_in(str(body)):
                    issue = int(candidate["number"])
                    matched_by = "fingerprint marker in the issue body"
                    break

        state = ""
        issue_title = ""
        if issue is not None and client is not None:
            try:
                view = client.view_issue(issue)
                state = str(view.get("state") or "").upper()
                issue_title = str(view.get("title") or "")
            except RuntimeError:
                state = ""

        if issue is not None:
            recurrence = state == "CLOSED"
            novelty = (
                entry.novelty(
                    run.run_id,
                    [o.task_name for o in finding.occurrences],
                    finding.count,
                    finding.denominator,
                )
                if entry
                else []
            )
            # An occurrence the record already holds is not evidence, it is a
            # repeat, and a comment that restates the issue is noise on someone
            # else's ticket. A recurrence on a CLOSED issue is always news,
            # because "the closed thing came back" is the finding.
            # `entry is None` means the binding came from the issue's marker
            # rather than the ledger, so there is no record of what was already
            # reported — silence would then be a guess, and the wrong one.
            silent = entry is not None and not novelty and not recurrence and not comment_anyway
            decision = (
                Decision.NO_NEWS
                if silent
                else (Decision.RECURRENCE if recurrence else Decision.COMMENT)
            )
            actions.append(
                Action(
                    finding=finding,
                    fingerprint=fingerprint,
                    decision=decision,
                    title=issue_title,
                    body=render_comment(run, finding, entry, recurrence, excerpt_limit),
                    issue=issue,
                    issue_state=state,
                    issue_title=issue_title,
                    candidates=candidates,
                    novelty=novelty,
                    matched_by=matched_by,
                )
            )
            continue

        blocked = search_failed or opened >= max_new_issues
        if not blocked:
            opened += 1
        actions.append(
            Action(
                finding=finding,
                fingerprint=fingerprint,
                decision=Decision.SUPPRESSED if blocked else Decision.NEW,
                title=finding.title,
                body=render_new_issue(run, finding, excerpt_limit),
                candidates=candidates,
                matched_by=(
                    f"search unavailable ({search_error}) — refusing to open an issue "
                    "it could not de-duplicate"
                    if search_failed
                    else ""
                ),
            )
        )
    return actions


def apply_actions(
    run: Run, actions: Sequence[Action], ledger: Ledger, client: GitHubClient
) -> list[str]:
    """Execute the plan. Only reached behind an explicit `--apply`."""
    log: list[str] = []
    for action in actions:
        finding = action.finding
        tasks = [o.task_name for o in finding.occurrences]
        if action.decision is Decision.SUPPRESSED:
            log.append(
                f"suppressed (cap): {finding.detector} {action.fingerprint.digest} "
                f"— {finding.count} occurrence(s), NOT filed"
            )
            continue
        if action.decision is Decision.NO_NEWS:
            # Still observed: the ledger records that this run saw it, so the
            # next run's novelty check is measured against the truth.
            entry = ledger.bind(action.fingerprint, action.issue, run.run_id)
            entry.observe(run.run_id, tasks, finding.count, finding.denominator)
            log.append(
                f"no news for #{action.issue}: {finding.detector} "
                f"{action.fingerprint.digest} — recorded, not posted"
            )
            continue
        if action.decision is Decision.NEW:
            number = client.create_issue(action.title, action.body)
            entry = ledger.bind(action.fingerprint, number, run.run_id)
            entry.observe(run.run_id, tasks, finding.count, finding.denominator)
            log.append(f"opened #{number}: {action.title}")
            continue
        assert action.issue is not None
        client.comment_issue(action.issue, action.body)
        entry = ledger.bind(action.fingerprint, action.issue, run.run_id)
        entry.observe(run.run_id, tasks, finding.count, finding.denominator)
        log.append(
            f"commented on #{action.issue} "
            f"({'recurrence' if action.decision is Decision.RECURRENCE else 'open'})"
        )
    ledger.save()
    return log
