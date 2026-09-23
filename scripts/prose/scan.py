"""What counts as prose, and which constructions in it are banned.

Bans two kinds of prose. The first is constructions that announce writing
instead of doing it. The canonical specimen, and the one that named this
guard:

    Two things stated rather than hidden: ...

Strip that clause and nothing is lost. The sentence after it still says
whatever it said. That is the test every pattern here has to pass -- a
construction earns a ban only when deleting it costs the reader nothing.

The second is words a reader outside this repository cannot parse, or filler
that adds nothing: declarations of honesty ("the honest claim" -- honesty is
assumed; state the fact), interface jargon ("TUI", "REPL", "Command Deck" --
say interactive mode or non-interactive mode), and Rust-internal terms in
prose ("variant" -- say option; "invariant" -- say rule). The bar for every
sentence a human reads here: an 8th grader can follow it.

Scope is every tracked text file: prose, Rust doc comments, Python
docstrings, shell headers. A comment is text a human reads, so it is held to
the same bar as a `.md` file. Fenced blocks and backticked spans are skipped
-- a document that names a banned construction in order to ban it is citing
it, and has to be able to spell it.

Pure functions over text: nothing here reads git, and `tracked_files` in
`git_pairing` decides which paths reach them.
"""

from __future__ import annotations

import re
from pathlib import Path


# Extensions whose contents a human reads as prose or comments.
SCANNED = (".md", ".mdx", ".rs", ".py", ".sh", ".ts", ".tsx", ".toml")

# Paths that are generated, vendored, or are this guard's own subject matter.
# A pattern list cannot scan the file that defines it without matching itself,
# the guard's other modules quote the constructions they handle, and the test
# suite has to spell every banned construction to feed one to the guard -- its
# heredocs are fixtures, not prose anyone reads for meaning.
EXCLUDED_PREFIXES = (
    "scripts/check-prose.py",
    "scripts/prose/",
    "scripts/test-prose-guard.sh",
    "scripts/prose-baseline.txt",
    "docs/wire/",
    ".claude/skills/oxagen-branding/",  # vendored; Stella does not own its grade
)
EXCLUDED_SUBSTRINGS = ("/snapshots/", "/fixtures/")


def is_scanned(path: str) -> bool:
    """Whether the guard reads `path` at all: a scanned extension, not excluded."""
    return (
        path.endswith(SCANNED)
        and not path.startswith(EXCLUDED_PREFIXES)
        and not any(s in path for s in EXCLUDED_SUBSTRINGS)
    )

# (name, regex, remedy). Deletion-safe: cut the clause; the sentence still holds.
PATTERNS: list[tuple[str, re.Pattern[str], str]] = [
    (
        "enumerative-announcement",
        # "Two things follow", "Both halves matter", "Three reasons to know".
        # Announcing a list instead of writing it. Spared when the phrase is a
        # real subject with a real object -- "Both halves of the rev-range are
        # validated" says something; "Both halves matter" does not.
        re.compile(r"\b(?:Two|Three|Four|Five|Six|Both)\s+(?:things?|halves|reasons?|consequences?|problems?|causes?|claims?|facts?|parts?)\b(?!\s+of\b)"),
        "delete the announcement; write the list",
    ),
    (
        "ranking-flourish",
        # Telling the reader which item to care about, instead of ordering the
        # list so the important one comes first.
        re.compile(r"\bthe (?:one|part|half) that (?:matters|counts)\b|\band the (?:second|first) is the\b|\bwhich is the (?:part|half) that\b"),
        "delete it; put the important item first",
    ),
    (
        "meta-writing",
        # Prose about the prose: narrating that a thing is being stated
        # rather than stating it.
        re.compile(r"\b(?:stated|named|said|spelled out|written|declared)\s+(?:out loud\s+)?rather than\b|\bworth (?:naming|saying|stating)\b"),
        "delete it; just state the thing",
    ),
    (
        "empty-epigram",
        # The "X, not Y" tail where Y is a rhetorical foil, not an alternative
        # anyone proposed.
        re.compile(r",\s+not (?:decoration|prose|a slogan|an afterthought|theater|theatre|an accident|a coincidence)\b|\bis a feature, not a bug\b"),
        "delete the tail; the claim stands alone",
    ),
    (
        "tired-metaphor",
        re.compile(r"\b(?:load-bearing|belt and braces|in the same breath)\b"),
        "say what it does: required, checked twice, in the same PR",
    ),
    (
        "honesty-declaration",
        # "the honest number", "measured honestly", "N honest claims about".
        # Honesty is assumed; declaring it is filler. State the fact.
        re.compile(r"\b[Hh]onest(?:ly|y)?\b"),
        "delete it; honesty is assumed -- state the fact",
    ),
    (
        "interface-jargon",
        # The interface has one public name per mode. "TUI", "REPL" and
        # "Command Deck" are insider names; a reader should see "interactive
        # mode" or "non-interactive mode". File and crate names like
        # `stella-tui` stay in backticks, which this guard already exempts.
        re.compile(r"\bTUI\b|\bREPL\b|\bCommand Deck\b|\b[Dd]eck consumers?\b|\b[Hh]eadless\b"),
        "say interactive mode / non-interactive mode",
    ),
    (
        "rust-jargon",
        # Rust terms in prose a non-Rust reader has to decode. An enum
        # "variant" is an option in a list; an "invariant" is a rule.
        re.compile(r"\b[Ii]nvariants?\b|\b[Vv]ariants?\b"),
        "say option (for variant) or rule (for invariant)",
    ),
    (
        "filler-adverb",
        # "deliberately" sounds like it is carrying a reason and never is.
        # Either the reason follows, in which case the word adds nothing to
        # it, or no reason follows, in which case the word stands in for one.
        # #4392's audit found it 2,500 times across 1,100 files, which is what
        # a word means when it means nothing.
        re.compile(r"\b[Dd]eliberate(?:ly)?\b"),
        "delete it; state the reason, or state nothing",
    ),
    (
        "bare-issue-citation",
        # An issue number is not an explanation. A reader outside the tracker
        # cannot follow it, and a reader inside it should not have to in order
        # to understand the line they are looking at. Narrow on purpose: this
        # matches only a sentence whose entire content is the citation, never
        # a number appended to a sentence that says something.
        re.compile(r"(?:^|[.;:!?]\s+)[Ss]ee #\d+\.?\s*$"),
        "say what the issue decided; keep the number beside it",
    ),
    (
        "issue-reference",
        # An issue number in prose sends the reader to a tracker to find out
        # what the sentence means. The sentence must say it instead. Tracking
        # markers (TODO and friends) keep theirs: a gate requires them there.
        # Two more are not prose. A CSS hex reads as one -- `#09090B` as issue
        # 10100 -- so the six- and eight-digit forms are exempt. And an
        # `issue:` field is a value a type requires, with no sentence in it.
        re.compile(r"^(?!.*(?:TODO|FIXME|XXX|HACK|Closes #|Refs #|issue: \"#)).*?"
                   r"(#(?![\da-fA-F]{6}\b)(?![\da-fA-F]{8}\b)\d{2,})"),
        "say the fact; drop the issue number from the prose",
    ),
    (
        "historical-reference",
        # Prose about what the code did before. The reader has today's code;
        # yesterday's belongs in git history and the tracker, not in comments
        # they must read past.
        re.compile(
            r"\b[Nn]o longer\b|\b[Pp]reviously\b|\b[Hh]istorically\b"
            r"|\b[Ww]as once\b|\b[Bb]ack when\b"
            r"|\b[Tt]he old (?:behaviou?r|way|code|shape|design)\b"
            r"|(?<!is )(?<!be )(?<!are )(?<!was )(?<!were )(?<!een )\b[Uu]sed to\b"
        ),
        "delete the history; describe what the code does now",
    ),
    (
        "complex-word",
        # Words with a plain replacement. Say dependency, rule, unrelated.
        re.compile(r"\b[Cc]oncretions?\b|\b[Rr]eif(?:y|ies|ied|ication)\b|\b[Oo]rthogonal(?:ly|ity)?\b"),
        "use the plain word: dependency, rule, unrelated",
    ),
]


# A backticked span is a citation, not a use: a document that names a banned
# construction in order to ban it must be able to spell it. A span may wrap
# across lines, so both passes run over the whole file and blank their matches
# to spaces -- newlines survive, which keeps every reported line number the
# one a reader will find in their editor.
CODE_SPAN = re.compile(r"`[^`]*`", re.DOTALL)
FENCE = re.compile(r"^\s*(?:```|~~~)")


def _blank(text: str) -> str:
    return "".join(c if c == "\n" else " " for c in text)


def prose_only(text: str) -> list[str]:
    """The file's lines with fenced blocks and code spans blanked out."""
    lines = text.splitlines()
    in_fence = False
    for i, line in enumerate(lines):
        if FENCE.match(line):
            in_fence = not in_fence
            lines[i] = ""
            continue
        if in_fence:
            lines[i] = ""
    return CODE_SPAN.sub(lambda m: _blank(m.group(0)), "\n".join(lines)).split("\n")


def scan_text(text: str) -> list[tuple[int, str, str, str]]:
    """Return (lineno, pattern name, matched text, remedy) for one file's text."""
    hits: list[tuple[int, str, str, str]] = []
    for lineno, line in enumerate(prose_only(text), start=1):
        for name, rx, remedy in PATTERNS:
            for m in rx.finditer(line):
                hits.append((lineno, name, m.group(0), remedy))
    return hits


def scan(root: Path, path: str) -> list[tuple[int, str, str, str]]:
    """[`scan_text`] over one file in the working tree."""
    try:
        text = (root / path).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return []
    return scan_text(text)


def prose_lines(path: str, lines: list[str]) -> list[str]:
    """The lines a reader reads as prose.

    In a code file that means comment lines only -- a shell pipeline or a
    match arm is not a sentence, and counting it as one turns the grade
    into noise. In a markdown file every line is prose.
    """
    if path.endswith((".md", ".mdx")):
        return lines
    marker = "#" if path.endswith((".py", ".sh", ".toml")) else "//"
    kept = []
    for line in lines:
        stripped = line.lstrip()
        if stripped.startswith(marker) and not stripped.startswith("#!"):
            kept.append(stripped)
    return kept
