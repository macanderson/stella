#!/usr/bin/env python3
"""`stella-research` — the pipeline's research stage, as a plugin.

Standard library only, deliberately: `doc:pipeline-as-plugins` §9 rule 3 is
that "if a plugin CANNOT be written without an SDK, the protocol is too
complicated", and the first *first-party* plugin is the worst possible place
to make an exception. If you are reading this to learn the protocol, the whole
protocol is here.

# The wire

`crates/stella-plugin/src/wire.rs` is the contract; these are its shapes as a
plugin author meets them. The host spawns `[runtime].argv` directly — no shell
— writes one JSON request on stdin, closes it, and reads one JSON response
from stdout:

    {"point": "before_turn", "body": {...BeforeTurnRequest}}
 -> {"point": "before_turn", "body": {...BeforeTurnResponse}}

Every table on that wire **denies unknown fields**, the envelope included, so
this program does too, at every level. A field the host does not know, at a
version the host accepts, is a typo, and a message that quietly does nothing is
worse than one that refuses. `protocol_version` rides on every message and the
contract is additive-only.

There is no error variant in `BeforeTurnResponse`. A plugin that cannot answer
*fails* — non-zero exit, one line on stderr — and the host runs the turn
without the contribution: a wrapper that cannot speak has nothing to say, and
that is not the user's fault. So stdout carries a valid response or nothing.

# What it contributes, and why it is only ever a message

Everything this plugin returns rides as [`VolatileContext`], whose only exit on
the host side is `into_message` — a *user* message after the byte-stable
system-prompt prefix (invariant 7). That is not politeness: prompt-cache hits
are a feature, and a plugin that could write into the stable prefix would make
itself a per-turn cost regression for every user who installed it.

# What it does not do, and what replaced it

The built-in stage this extracts (`crates/stella-pipeline/src/research.rs` and
`pipeline/research_stage.rs`) answers triage's questions by fanning out
**read-only model sub-agents**. This plugin cannot: `doc:wrapper-socket` §7 is
explicit that the socket hands a plugin no engine, no provider and no
credential, and the request carries no `[roles]` call it could spend. Two
consequences, both stated out loud rather than papered over:

  - It does deterministic grounding instead of model research — it reports
    what a literal scan of the granted workspace actually contains, which is
    the half of a research finding that is checkable.
  - It contributes at the `research` stage only. The built-in `recall` stage
    reads the context plane (memories, episodes, the code graph), and the wire
    has no representation for that plane, so this plugin answers `recall` with
    an empty contribution rather than pretending.

It publishes no signals for the same kind of reason: the `Signal` vocabulary
is closed, and `StageName::Research.publishes()` is empty in the host today, so
there is no signal a research stage may honestly publish.

# Every capability arrives in the request

`candidate` is a `CandidateGrant`: the handle, the canonical workspace `root`,
and the test the host would run there (which this plugin ignores — it runs
nothing). This program reaches for nothing else: no environment, no working
directory, no git checkout, no terminal. That is what lets it run unchanged
under `stella-cli`, under `stella-serve`, and inside an application that
embedded the loop. With no grant there is no root, and a stage with nothing to
read contributes nothing.
"""

import json
import os
import re
import sys

# The version every message on this wire carries (`wire::PROTOCOL_VERSION`).
PROTOCOL_VERSION = 1

# The one point this plugin answers, matching `[loop].points`. `WrapperPoint`
# has exactly two, and an undeclared point is never dispatched — but a host
# that asks anyway gets a refusal rather than an answer to a question this
# plugin did not agree to answer.
POINT = "before_turn"

# The one stage it contributes at. Every other declared stage gets an empty
# contribution, which must be byte-identical to the one a host that never
# installed this plugin would have used.
CONTRIBUTING_STAGE = "research"

# The fields each table on the request declares. Anything else is a typo, per
# the deny-unknown-fields rule the whole wire contract is written under.
BEFORE_TURN_REQUEST_FIELDS = {
    "protocol_version",
    "wrapper",
    "stage",
    "round",
    "goal",
    "candidate",
    "published",
}
CANDIDATE_GRANT_FIELDS = {"handle", "root", "test"}
PUBLISHED_SIGNAL_FIELDS = {"signal", "value"}

# `StageName`, closed. A stage the host cannot dispatch is exactly the manifest
# that quietly does nothing, and a stage *name* this plugin does not recognise
# is the same defect arriving over the wire: it would be indistinguishable from
# "some stage that is not research", and this plugin would silently contribute
# nothing to a turn it was installed to ground.
STAGE_NAMES = (
    "triage",
    "recall",
    "research",
    "plan",
    "scope",
    "execute",
    "witness",
    "verify",
    "verdict",
    "reflect",
    "contextwrite",
    "complete",
)

# ── The bounds, and why each one is where it is ──────────────────────────────
#
# The built-in stage bounds its findings by characters — 4,000 per finding and
# 12,000 per turn (`RESEARCH_FINDING_CHARS`, `RESEARCH_PROMPT_BUDGET_CHARS` in
# `crates/stella-pipeline/src/research.rs`), because a model's answer has no
# other shape to bound. A deterministic scan does: the caps below bound the
# contribution *structurally*, which is stronger, because the ceiling is
# reached the same way on every run instead of depending on how chatty a
# sub-agent was. Their product is deliberately inside the built-in's per-turn
# budget: 6 terms x 5 matches x 200 chars is under 7,000 characters, so this
# stage cannot displace the work it is grounding.
#
# Every cap is *visible* when it binds. A bounded findings section that reads
# as the whole story is the failure the built-in's drop markers exist to
# prevent, and it is worse here — absence in a scan reads as proof of absence.
MAX_TERMS = 6
MAX_MATCHES_PER_TERM = 5
MAX_LINE_CHARS = 200
MAX_ORIENTATION_ENTRIES = 40
# Files opened before the scan stops and says so. A backstop for a tree nobody
# expected, not the thing that makes the output small.
MAX_FILES_SCANNED = 2000
# A file larger than this is not grounding, it is a dataset.
MAX_FILE_BYTES = 262_144
# A NUL in the head of a file is the portable "this is not text" test.
BINARY_SNIFF_BYTES = 4096

# Directory names the scan never descends into: build output and dependency
# trees restate the source in generated form, so a match inside one is noise
# that reads exactly like a finding. Hidden entries (`.git`, `.venv`, and the
# rest) are skipped by the leading-dot rule beside this, which is why they are
# not listed here.
SKIP_DIRS = frozenset(
    (
        "target",
        "node_modules",
        "dist",
        "build",
        "vendor",
        "venv",
        "__pycache__",
    )
)

# A token that is only a word has to be long enough and ordinary enough to be
# worth searching for. Anything that looks like an identifier or a path skips
# this list entirely — `id` is a stopword, `id_map` is a symbol.
MIN_TERM_CHARS = 3
MIN_WORD_CHARS = 5
STOPWORDS = frozenset(
    (
        "about",
        "after",
        "again",
        "against",
        "along",
        "already",
        "also",
        "always",
        "another",
        "because",
        "been",
        "before",
        "being",
        "below",
        "between",
        "both",
        "cannot",
        "could",
        "currently",
        "does",
        "doing",
        "done",
        "during",
        "each",
        "either",
        "else",
        "enough",
        "even",
        "ever",
        "every",
        "from",
        "further",
        "hence",
        "here",
        "however",
        "instead",
        "into",
        "itself",
        "just",
        "like",
        "make",
        "makes",
        "many",
        "might",
        "more",
        "most",
        "much",
        "must",
        "need",
        "needs",
        "never",
        "not",
        "nothing",
        "only",
        "other",
        "over",
        "please",
        "rather",
        "really",
        "same",
        "should",
        "since",
        "some",
        "still",
        "such",
        "than",
        "that",
        "their",
        "them",
        "then",
        "there",
        "these",
        "they",
        "thing",
        "things",
        "this",
        "those",
        "through",
        "under",
        "until",
        "upon",
        "very",
        "want",
        "wants",
        "were",
        "what",
        "when",
        "where",
        "whether",
        "which",
        "while",
        "will",
        "with",
        "within",
        "without",
        "would",
        "your",
    )
)

# A token starts with a letter or underscore and may carry the punctuation a
# symbol or a path carries. Trailing punctuation is prose, not part of the
# name, so it is stripped after the match.
TOKEN_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_./:-]*")
TOKEN_TRIM = ".:/-"
# A dotted token is a filename when its tail looks like an extension.
EXTENSION_RE = re.compile(r"^[A-Za-z]{1,5}$")


class Refusal(Exception):
    """The plugin cannot answer. Exits non-zero; the host runs without it."""


def refuse(reason):
    raise Refusal(reason)


def deny_unknown(table, allowed):
    """Refuse a table carrying a key the contract does not declare.

    One message shape for every table, matching the reference plugins in
    `stella-examples`: a refusal line is part of the contract a conformance
    harness grades, and Rust's `serde` reports the offending key without a
    path to the table it was found in, so neither does this.
    """
    unknown = sorted(set(table) - allowed)
    if unknown:
        refuse("the request denies unknown fields; got {}".format(", ".join(unknown)))
    return table


def read_request():
    """Decode `{"point": ..., "body": ...}` and return the `before_turn` body."""
    try:
        envelope = json.loads(sys.stdin.read())
    except ValueError:
        refuse("stdin was not a single JSON object")
    if not isinstance(envelope, dict):
        refuse("stdin was not a single JSON object")
    if set(envelope) - {"point", "body"}:
        refuse("the request envelope carried a field outside {point, body}")

    point = envelope.get("point")
    if point != POINT:
        refuse(
            "this plugin answers {} only; the host asked for {}".format(
                POINT, json.dumps(point)
            )
        )

    body = envelope.get("body")
    if not isinstance(body, dict):
        refuse("the request envelope carried no object body")
    deny_unknown(body, BEFORE_TURN_REQUEST_FIELDS)

    version = body.get("protocol_version")
    # `isinstance(True, int)` is True in Python, so a JSON `true` would
    # otherwise compare equal to version 1. It does not, and this says so.
    if isinstance(version, bool) or version != PROTOCOL_VERSION:
        refuse(
            "this plugin speaks wrapper protocol_version {}; the host asked for {}".format(
                PROTOCOL_VERSION, json.dumps(version)
            )
        )

    stage = body.get("stage")
    if stage not in STAGE_NAMES:
        refuse(
            "StageName is a closed set {{{}}}; got {}".format(
                ", ".join(STAGE_NAMES), json.dumps(stage)
            )
        )

    for published in body.get("published", []) or []:
        if not isinstance(published, dict):
            refuse("published must be an array of PublishedSignal objects")
        deny_unknown(published, PUBLISHED_SIGNAL_FIELDS)

    goal = body.get("goal")
    if not isinstance(goal, str):
        refuse("the request carried no goal")
    return body


def grant_root(body):
    """The candidate workspace's root, or `None` when the host granted none.

    `CandidateGrant` is the capability: `root` is where this plugin's own reads
    happen, and there is no other path it may use. A stage with no grant has
    nothing to read and contributes nothing — reaching for a working directory
    instead is exactly the ambient authority the socket exists to refuse.
    """
    candidate = body.get("candidate")
    if candidate is None:
        return None
    if not isinstance(candidate, dict):
        refuse("candidate must be a CandidateGrant object")
    deny_unknown(candidate, CANDIDATE_GRANT_FIELDS)
    root = candidate.get("root")
    if not isinstance(root, str) or not root:
        refuse("the candidate grant carried no root")
    return root


def is_symbolish(token):
    """Whether a token is a symbol or a path rather than an English word.

    Symbols and paths are searched whatever their length; ordinary words have
    to clear [`MIN_WORD_CHARS`] and the stopword list. The test is deliberately
    syntactic — a plugin that tried to *understand* the goal would be doing the
    job the model does, at the point of least evidence.
    """
    if "_" in token or "/" in token or "::" in token:
        return True
    if token != token.lower() and token != token.upper():
        return True  # camelCase or CamelCase
    head, _, tail = token.rpartition(".")
    return bool(head) and bool(EXTENSION_RE.match(tail))


def search_terms(goal):
    """The terms to look for, in the order they will be reported.

    Symbols and paths first, then ordinary words, each in the order the goal
    named them — a stable ranking, because a contribution that reordered itself
    between two runs on the same goal would be a prompt-cache miss with no
    cause a reader could see.
    """
    ranked = {}
    for raw in TOKEN_RE.findall(goal):
        token = raw.strip(TOKEN_TRIM)
        if len(token) < MIN_TERM_CHARS:
            continue
        key = token.lower()
        if key in ranked:
            continue
        symbolish = is_symbolish(token)
        if not symbolish and (key in STOPWORDS or len(token) < MIN_WORD_CHARS):
            continue
        ranked[key] = (0 if symbolish else 1, len(ranked), token)
    return [token for _, _, token in sorted(ranked.values())[:MAX_TERMS]]


def scannable_files(root):
    """Every file the scan will read, workspace-relative, in sorted order.

    Returns `(paths, truncated)`. Deterministic by construction: `os.walk` is
    handed sorted directory and file names at every level, so two runs over the
    same tree visit the same files in the same order — which is what makes a
    golden possible at all.

    Symlinks are skipped rather than followed. The host fences the paths a
    plugin *names* on the way back; a link is how a read would leave the
    granted root without either side saying so.
    """
    paths = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(
            name
            for name in dirnames
            if not name.startswith(".")
            and name not in SKIP_DIRS
            and not os.path.islink(os.path.join(dirpath, name))
        )
        for name in sorted(filenames):
            if name.startswith("."):
                continue
            full = os.path.join(dirpath, name)
            if os.path.islink(full):
                continue
            if len(paths) == MAX_FILES_SCANNED:
                return paths, True
            paths.append(os.path.relpath(full, root).replace(os.sep, "/"))
    return paths, False


def read_text(path):
    """A file's text, or `None` when it is not text this stage should read."""
    try:
        with open(path, "rb") as handle:
            blob = handle.read(MAX_FILE_BYTES + 1)
    except OSError:
        return None
    if len(blob) > MAX_FILE_BYTES:
        return None
    if b"\0" in blob[:BINARY_SNIFF_BYTES]:
        return None
    return blob.decode("utf-8", "replace")


def clamp_line(line):
    """One matched line, trimmed and clamped, with the clamp made visible."""
    line = line.strip()
    if len(line) > MAX_LINE_CHARS:
        return line[:MAX_LINE_CHARS] + " […]"
    return line


def scan(root, terms):
    """Match `terms` against the granted tree.

    Returns `(found, truncated)`, where `found[term]` is
    `(matches, total, files)`: the first [`MAX_MATCHES_PER_TERM`] matches as
    `(path, line number, line)`, the *total* number of matches, and the number
    of distinct files carrying one. The totals are counted past the cap on
    purpose — "5 of 41" and "5 of 5" are different facts, and a reader who
    cannot tell them apart has been told the truncated half was the whole
    story. `files` is counted over every match rather than over the shown ones
    for the same reason.
    """
    matches = {term: [] for term in terms}
    totals = {term: 0 for term in terms}
    files = {term: set() for term in terms}
    needles = [(term, term.lower()) for term in terms]
    paths, truncated = scannable_files(root)
    for relative in paths:
        text = read_text(os.path.join(root, relative))
        if text is None:
            continue
        for number, line in enumerate(text.splitlines(), start=1):
            lowered = line.lower()
            for term, needle in needles:
                if needle in lowered:
                    totals[term] += 1
                    files[term].add(relative)
                    if len(matches[term]) < MAX_MATCHES_PER_TERM:
                        matches[term].append((relative, number, clamp_line(line)))
    found = {
        term: (matches[term], totals[term], len(files[term])) for term in terms
    }
    return found, truncated


def counted(count, singular, plural):
    """`3 matches`, `1 file`. English, spelled out rather than derived, because
    a contribution a person reads should not say "1 matches"."""
    return "{} {}".format(count, singular if count == 1 else plural)


def orientation(root):
    """What the granted root holds at its top level, or `None` if unreadable.

    The first question anyone asks of an unfamiliar workspace, answered without
    a model call. It is also the honest floor of this stage: a turn whose goal
    names nothing findable still learns where it is.
    """
    try:
        entries = sorted(os.listdir(root))
    except OSError:
        return None
    lines = []
    for name in entries:
        if name.startswith("."):
            continue
        suffix = "/" if os.path.isdir(os.path.join(root, name)) else ""
        lines.append("- {}{}".format(name, suffix))
    shown = lines[:MAX_ORIENTATION_ENTRIES]
    if len(lines) > len(shown):
        shown.append(
            "- [… {} not shown …]".format(
                counted(len(lines) - len(shown), "further entry", "further entries")
            )
        )
    if not shown:
        return None
    return {
        "label": "research:workspace",
        "text": "## Research: workspace orientation\n\n"
        "The candidate root holds these top-level entries (hidden entries "
        "omitted):\n"
        + "\n".join(shown)
        # Said every time, because the alternative is a listing that shows a
        # directory beside findings that never looked inside it: generated
        # trees restate the source, and a match in one reads exactly like a
        # finding. Sorted, because a frozenset has no order and a contribution
        # that reordered itself between runs is a cache miss with no cause.
        + "\n\nNot scanned, wherever they appear: hidden entries, symlinks, "
        "and {}.".format(", ".join(sorted(SKIP_DIRS))),
    }


def term_context(term, hits, count, files):
    """One term's findings, as the contribution the model will read."""
    header = (
        "## Research: `{}`\n\n{} in {} under the candidate root, as "
        "workspace-relative paths:".format(
            term,
            counted(count, "match", "matches"),
            counted(files, "file", "files"),
        )
    )
    lines = ["- {}:{}: {}".format(path, number, line) for path, number, line in hits]
    if count > len(hits):
        lines.append(
            "[… {} not shown; this stage reports the first {} …]".format(
                counted(count - len(hits), "further match", "further matches"),
                MAX_MATCHES_PER_TERM,
            )
        )
    return {
        "label": "research:{}".format(term),
        "text": header + "\n" + "\n".join(lines),
    }


def unmatched_context(terms):
    """The terms nothing matched, said plainly.

    The built-in stage's own system prompt tells its sub-agents to "say plainly
    when you could not find an answer", and a deterministic scan can say it
    with more authority than a model can: nothing under this root contains this
    string.
    """
    return {
        "label": "research:unmatched",
        "text": "## Research: nothing found\n\n"
        "No occurrences under the candidate root of: {}.".format(
            ", ".join("`{}`".format(term) for term in terms)
        ),
    }


def bounded_context():
    """Said whenever the file cap bound, because a bounded scan must never read
    as the whole story — absence is otherwise indistinguishable from proof of
    absence."""
    return {
        "label": "research:scan-bounded",
        "text": "## Research: the scan was bounded\n\n"
        "This stage read the first {} files under the candidate root and "
        "stopped. Anything reported above is present; nothing above is "
        "evidence that something is absent.".format(MAX_FILES_SCANNED),
    }


def contribute(body):
    """The context this stage contributes, which may be nothing.

    Nothing is a complete answer and the common case: a stage that is not
    `research`, a request with no candidate grant, a root that cannot be read.
    Every one of those returns an empty contribution — byte-identical to the
    one a host that never installed this plugin would have used, which is the
    advisory contract the built-in stage holds too (zero findings leave the
    prompt exactly as it was).
    """
    if body["stage"] != CONTRIBUTING_STAGE:
        return []
    root = grant_root(body)
    if root is None or not os.path.isdir(root):
        return []

    context = []
    where = orientation(root)
    if where is not None:
        context.append(where)

    terms = search_terms(body["goal"])
    found, truncated = scan(root, terms) if terms else ({}, False)
    unmatched = []
    for term in terms:
        hits, count, files = found[term]
        if count:
            context.append(term_context(term, hits, count, files))
        else:
            unmatched.append(term)
    if unmatched:
        context.append(unmatched_context(unmatched))
    if truncated:
        context.append(bounded_context())
    return context


def main():
    try:
        body = read_request()
        context = contribute(body)
    except Refusal as refusal:
        sys.stderr.write("stella-research: {}\n".format(refusal))
        return 1

    response = {"protocol_version": PROTOCOL_VERSION}
    if context:
        # Absent rather than empty when there is nothing to say: the host's own
        # `BeforeTurnResponse` skips empty collections when it serializes, so
        # an empty contribution is the same bytes on both sides of the wire.
        response["context"] = context
    sys.stdout.write(json.dumps({"point": POINT, "body": response}) + "\n")
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
