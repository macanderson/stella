#!/usr/bin/env python3
"""Guard: a pull request that says it does NOT close an issue must not close it.

See `#6190`. GitHub's own parser reads a closing keyword (`close`, `fix`,
`resolve`, and their inflections) and the issue reference right after it. It
does not read the words in front of the keyword, so a sentence written to
*deny* the close reads to GitHub exactly like one that asks for it.

`#6180` hit this for real: its commit trailer carried `Refs #6155` (the
correct spelling for "this advances the issue but does not finish it"), but
its description also said:

    This does not close `#6155`. Only its first done-item is satisfied here.

GitHub closed `#6155` anyway, because `close #6155` is in that sentence and
"does not" in front of it is invisible to the parser. AGENTS.md's "One
keyword per issue" paragraph already covers the sibling mistake — a keyword
meant for two issues landing on only one (`Closes #A, #B`) — and `#6190` is
the same class of bug reached from the other side: a keyword the author
meant to negate, applied anyway.

What this checks, over the PR body and every commit message on the PR:

    a negation word (not / never / without / doesn't) ... up to a few words
    ... a closing keyword ... up to a few words ... an issue reference

within one sentence, where the reference is not wrapped in backticks (the
safe spelling this guard's own remedy recommends). A sentence carrying only
`Refs #N`, only a backticked `` `#N` ``, or a genuine unnegated `Closes #N`
all pass — none of those match the keyword-after-negation shape.

Sentence boundary is a period/question mark/exclamation point followed by
whitespace, or a newline — a trailer line like `Closes #6190` carries no
terminal punctuation, so treating a newline as a boundary is what keeps that
line's keyword from being read alongside negation words in the paragraph
above it.

Not a `make gate` step: like `check-deleted-tests.sh`, this reads the PR's
description and commit messages, which are not information a single local
tree carries (a PR body is a GitHub-side ask; the description a contributor
is drafting locally is not it). It runs only as a `pull_request`-triggered
CI step. See `scripts/test-closing-keywords.py` for the hermetic self-test
and the two channels' fixture seams.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys

KEYWORD = r"(?:clos(?:e|es|ed)|fix(?:es|ed)?|resolv(?:e|es|ed))"
NEGATION = r"(?:not|never|without|doesn['\u2019]t)"

# `(?:\s+\S+){0,N}?` is a bounded, non-greedy "up to N filler words" — bounded
# so the match cannot stretch past the sentence it is found in (sentences are
# split out before this ever runs, so "past the sentence" already means
# "never"), non-greedy so it prefers the nearest keyword/reference rather than
# the furthest one a longer sentence might also contain.
NEGATED_CLOSE = re.compile(
    r"\b"
    + NEGATION
    + r"\b(?:\s+\S+){0,6}?\s+\b(?P<keyword>"
    + KEYWORD
    + r")\b(?:\s+\S+){0,3}?\s*(?P<ref>#\d+)",
    re.IGNORECASE,
)


def split_sentences(text: str) -> list[str]:
    """Split on sentence-ending punctuation or a bare newline.

    A commit trailer or a `## Definition of done` checkbox line ends with
    neither `.`, `!` nor `?`, so a newline alone has to count too — otherwise
    "This does not close the loop.\\nCloses #99999" would read as one
    sentence and flag a trailer that is nowhere near the negation.
    """
    return [s for s in re.split(r"(?<=[.!?])\s+|\n+", text) if s.strip()]


def is_backticked(sentence: str, match: re.Match[str]) -> bool:
    """True when the `#N` reference itself sits inside a `` `...` `` span.

    Only the reference is checked, not the whole negated phrase, because the
    remedy this guard recommends is "write the issue number in backticks" —
    a rewrite that leaves the surrounding prose (keyword and all) untouched.
    """
    start, end = match.start("ref"), match.end("ref")
    before = sentence[:start].rstrip()
    after = sentence[end:].lstrip()
    return before.endswith("`") and after.startswith("`")


def find_violations(
    text: str, source: str
) -> list[tuple[str, str, re.Match[str]]]:
    """Return (source, sentence, match) for every violation."""
    violations = []
    for sentence in split_sentences(text):
        for match in NEGATED_CLOSE.finditer(sentence):
            if is_backticked(sentence, match):
                continue
            violations.append((source, sentence.strip(), match))
    return violations


def safe_spelling(match: re.Match[str]) -> str:
    ref = match.group("ref")
    return (
        f"write the issue number in backticks (`` {ref} `` ...) or say "
        f'"advances {ref}" instead of using a closing keyword there.'
    )


def fetch_pr_body(pr_number: str | None) -> tuple[str, bool]:
    """Return (body, fetched_live). Mirrors check-deleted-tests.sh's
    `#4495` live-vs-stale channel: a re-run should see an edited description
    without a new push."""
    if not pr_number:
        return os.environ.get("PR_BODY", ""), False
    gh = os.environ.get("GH_CLOSING_KEYWORDS_BIN", "gh")
    args = [gh, "pr", "view", pr_number, "--json", "body", "--jq", ".body"]
    repo = os.environ.get("GH_REPO")
    if repo:
        args += ["--repo", repo]
    try:
        result = subprocess.run(
            args, capture_output=True, text=True, check=True, timeout=30
        )
        return result.stdout, True
    except (
        subprocess.CalledProcessError,
        subprocess.TimeoutExpired,
        FileNotFoundError,
    ):
        return os.environ.get("PR_BODY", ""), False


def fetch_commit_messages(pr_number: str | None) -> tuple[str, bool]:
    """Every commit message on the PR, headline plus body. The GitHub API is
    ground truth here regardless of checkout depth — a shallow `git log`
    cannot see a PR's full history the way `check-deleted-tests.sh` already
    documents for the same job."""
    if not pr_number:
        return "", False
    gh = os.environ.get("GH_CLOSING_KEYWORDS_BIN", "gh")
    args = [
        gh,
        "pr",
        "view",
        pr_number,
        "--json",
        "commits",
        "--jq",
        ".commits[] | (.messageHeadline // \"\") + \"\\n\" + (.messageBody // \"\")",
    ]
    repo = os.environ.get("GH_REPO")
    if repo:
        args += ["--repo", repo]
    try:
        result = subprocess.run(
            args, capture_output=True, text=True, check=True, timeout=30
        )
        return result.stdout, True
    except (
        subprocess.CalledProcessError,
        subprocess.TimeoutExpired,
        FileNotFoundError,
    ):
        return "", False


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--fixture-pr-body",
        help="stand in for a live `gh pr view` body fetch (test-only)",
    )
    parser.add_argument(
        "--fixture-commit-messages",
        help="stand in for a live `gh pr view` commits fetch (test-only)",
    )
    args = parser.parse_args(argv)

    pr_number = os.environ.get("PR_NUMBER")

    if args.fixture_pr_body is not None:
        body, body_live = args.fixture_pr_body, True
    else:
        body, body_live = fetch_pr_body(pr_number)

    if args.fixture_commit_messages is not None:
        commits, commits_live = args.fixture_commit_messages, True
    else:
        commits, commits_live = fetch_commit_messages(pr_number)

    if not body and not commits:
        print(
            "check-closing-keywords: nothing to check — no PR body and no "
            "commit messages were available (not a pull_request event, or "
            "PR_NUMBER/PR_BODY were both unset). Skipping."
        )
        return 0

    violations = find_violations(body, "the PR description") + find_violations(
        commits, "a commit message"
    )

    if not violations:
        channels = []
        if body:
            channels.append("PR description" + ("" if body_live else " (stale snapshot)"))
        if commits:
            channels.append("commit messages" + ("" if commits_live else " (stale snapshot)"))
        print(
            "check-closing-keywords: OK — no negated closing keyword found in "
            + " or ".join(channels)
            + "."
        )
        return 0

    print("check-closing-keywords: FAILED", file=sys.stderr)
    print("", file=sys.stderr)
    print(
        "GitHub's closing-keyword parser reads the keyword and the issue "
        "reference right after it. It does not read the negation in front "
        "of the keyword, so a sentence written to DENY a close reads to "
        "GitHub exactly like one that asks for it.",
        file=sys.stderr,
    )
    print("", file=sys.stderr)
    for source, sentence, match in violations:
        print(f"  in {source}: \"{sentence}\"", file=sys.stderr)
        print(f"    offending phrase: \"{match.group(0)}\"", file=sys.stderr)
        print(f"    fix: {safe_spelling(match)}", file=sys.stderr)
        print("", file=sys.stderr)
    print(
        "AGENTS.md \u00a7 \"Closing the issue on merge\" has the full rule.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
